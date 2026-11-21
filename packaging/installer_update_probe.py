#!/usr/bin/env python3
"""Exercise a real Velopack update in an ephemeral Windows/macOS CI prefix.

This does not install bilibilive. Two tiny, uniquely named test packages are
built below --work, and the official helper updates only their private portable
installation. A successful helper spawn is not a successful test: the restarted
0.0.2 executable must write a receipt and match the new package's executable.
"""

import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import plistlib
import re
import shutil
import signal
import stat
import subprocess
import sys
import time
import uuid
import xml.etree.ElementTree as ET
import zipfile

SDK_VERSION = "1.2.161"
SERDE_JSON_VERSION = "1.0.154"
VERSIONS = ("0.0.1", "0.0.2")
PROGRAM = "bilibilive-update-probe"
TARGETS = {
    "x86_64-pc-windows-msvc": ("win", "x64", "win-x64", "win32"),
    "x86_64-apple-darwin": ("osx", "x64", "osx-x64", "darwin"),
    "aarch64-apple-darwin": ("osx", "arm64", "osx-arm64", "darwin"),
}

# No app credentials, network source, UI input, or default SDK locator is used.
# Hook callbacks exit inside Velopack before it tries to create a locator.
RUST_SOURCE = r'''use serde_json::{json, Value};
use std::{env, error::Error, fs, path::{Path, PathBuf}, process::ExitCode,
    thread, time::{Duration, Instant}};
use velopack::{locator::{VelopackLocator, VelopackLocatorConfig},
    sources::FileSource, UpdateCheck, UpdateManager, VelopackApp};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn require(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}

fn path(value: &Value, key: &str) -> Result<PathBuf> {
    let raw = value[key].as_str().ok_or("missing context path")?;
    let result = PathBuf::from(raw);
    require(result.is_absolute(), "context path is not absolute")?;
    Ok(result)
}

fn existing_under(root: &Path, value: &Path) -> Result<PathBuf> {
    let value = value.canonicalize()?;
    require(value.starts_with(root) && value != root, "path leaves private work")?;
    Ok(value)
}

fn write_receipt(value: &Value, filename: &str, receipt: Value) -> Result<()> {
    let work = path(value, "work")?.canonicalize()?;
    let directory = existing_under(&work, &path(value, "receipts")?)?;
    let target = directory.join(filename);
    require(!target.exists(), "receipt already exists")?;
    let temporary = directory.join(format!("{}.{}.tmp", filename, std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(&receipt)?)?;
    fs::rename(temporary, target)?;
    Ok(())
}

fn run(args: &[String]) -> Result<()> {
    require(cfg!(any(target_os = "windows", target_os = "macos")),
        "the live probe supports only Windows and macOS")?;
    require(args.len() == 3 && args[1] == "--context"
        && matches!(args[0].as_str(), "--update" | "--confirm"), "invalid probe arguments")?;
    let context_file = PathBuf::from(&args[2]).canonicalize()?;
    require(fs::metadata(&context_file)?.len() <= 32 * 1024, "context too large")?;
    let value: Value = serde_json::from_slice(&fs::read(&context_file)?)?;
    let work = path(&value, "work")?.canonicalize()?;
    existing_under(&work, &context_file)?;
    let marker: Value = serde_json::from_slice(&fs::read(work.join("probe-owner.json"))?)?;
    require(value["nonce"].as_str().is_some_and(|v| v.len() == 32)
        && marker["nonce"] == value["nonce"], "private work ownership mismatch")?;
    let root = existing_under(&work, &path(&value, "install_root")?)?;
    let packages = existing_under(&work, &path(&value, "packages")?)?;
    let feed = existing_under(&work, &path(&value, "feed")?)?;
    let exe = env::current_exe()?.canonicalize()?;
    require(exe == path(&value, "executable")?.canonicalize()? && exe.starts_with(&root),
        "probe executable is outside its private installation")?;
    let update_exe = existing_under(&root, &path(&value, "helper")?)?;
    let manifest = existing_under(&root, &path(&value, "manifest")?)?;
    let binary_dir = path(&value, "binary_dir")?.canonicalize()?;
    require(binary_dir.starts_with(&root), "binary directory leaves installation")?;
    let config = VelopackLocatorConfig {
        RootAppDir: root.clone(), UpdateExePath: update_exe,
        PackagesDir: packages.clone(), ManifestPath: manifest,
        CurrentBinaryDir: binary_dir, IsPortable: true,
    };
    let locator = VelopackLocator::new(&config)?;
    require(locator.get_packages_dir().canonicalize()? == packages,
        "SDK substituted a different package cache")?;
    require(locator.get_manifest_id() == value["package_id"].as_str().unwrap_or("")
        && locator.get_manifest_channel() == value["channel"].as_str().unwrap_or("")
        && locator.get_manifest_version_full_string() == env!("CARGO_PKG_VERSION"),
        "installed manifest differs from the executing probe")?;
    VelopackApp::build().set_locator(config.clone()).set_auto_apply_on_startup(false).run();
    if args[0] == "--confirm" {
        require(env!("CARGO_PKG_VERSION") == "0.0.2", "the old version was restarted")?;
        write_receipt(&value, "confirmed.json", json!({
            "phase":"confirmed", "version":env!("CARGO_PKG_VERSION"),
            "nonce":value["nonce"], "package_id":value["package_id"],
            "pid":std::process::id(), "executable":exe, "packages":packages,
            "manifest_version":locator.get_manifest_version_full_string(),
            "installation_portable":locator.get_is_portable()
        }))?;
        return Ok(());
    }
    require(env!("CARGO_PKG_VERSION") == "0.0.1", "only the old probe may request an update")?;
    // The Python parent assigns the Windows job before releasing this barrier.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !work.join("begin-update").is_file() {
        require(Instant::now() < deadline, "parent did not release private start barrier")?;
        thread::sleep(Duration::from_millis(25));
    }
    let manager = UpdateManager::new(FileSource::new(feed), None, Some(config))?;
    let update = match manager.check_for_updates()? {
        UpdateCheck::UpdateAvailable(value) => value,
        _ => return Err("private 0.0.2 update was not available".into()),
    };
    require(update.TargetFullRelease.Version == "0.0.2"
        && update.TargetFullRelease.PackageId == value["package_id"].as_str().unwrap_or("")
        && update.TargetFullRelease.SHA256.eq_ignore_ascii_case(value["package_sha256"].as_str().unwrap_or(""))
        && !update.IsDowngrade, "unexpected update identity")?;
    manager.download_updates(&update, None)?;
    let package = existing_under(&packages, &packages.join(&update.TargetFullRelease.FileName))?;
    require(fs::metadata(&package)?.len() == update.TargetFullRelease.Size,
        "downloaded package size mismatch")?;
    write_receipt(&value, "downloaded.json", json!({
        "phase":"downloaded", "version":env!("CARGO_PKG_VERSION"),
        "nonce":value["nonce"], "package":package,
        "asset":update.TargetFullRelease, "source":"FileSource"
    }))?;
    manager.wait_exit_then_apply_updates(&*update, true, true, [
        "--confirm".to_owned(), "--context".to_owned(), context_file.to_string_lossy().into_owned()
    ])?;
    write_receipt(&value, "helper-started.json", json!({
        "phase":"helper_started", "version":env!("CARGO_PKG_VERSION"),
        "pid":std::process::id(), "nonce":value["nonce"],
        "exit":"natural main return", "updated":false
    }))?;
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() == 2 && matches!(args[0].as_str(), "--veloapp-install"
        | "--veloapp-updated" | "--veloapp-obsolete" | "--veloapp-uninstall") {
        VelopackApp::build().set_auto_apply_on_startup(false).run();
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => { eprintln!("private update probe failed: {error}"); ExitCode::FAILURE }
    }
}
'''


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def write_json(path, value):
    path = Path(path)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    temporary.replace(path)


def read_json(path, limit=1024 * 1024):
    path = Path(path)
    require(path.is_file() and path.stat().st_size <= limit, f"Missing or oversized JSON: {path}")
    return json.loads(path.read_text(encoding="utf-8-sig"))


def contained(path, root):
    path, root = Path(path).resolve(), Path(root).resolve()
    if path != root and path.is_relative_to(root):
        return True
    # Windows preserves an existing \\?\ prefix in resolve(). Compare the
    # actual directories when Rust and Python used different path spellings.
    # The lexical check above also supports work/output paths not yet created.
    if root.exists():
        for parent in path.parents:
            try:
                if parent.samefile(root):
                    return True
            except FileNotFoundError:
                continue
    return False


def validate_downloaded_package(package, packages, expected_sha256):
    package = Path(package).resolve(strict=True)
    require(contained(package, packages), "Downloaded package left its private cache")
    require(digest(package) == expected_sha256, "Downloaded package bytes differ")


def private_work(path, runner_temp):
    path = Path(path)
    require(path.is_absolute(), "--work must be an absolute path")
    require(runner_temp, "RUNNER_TEMP must name the ephemeral CI directory")
    require(not path.exists() and not path.is_symlink(), "--work must not already exist")
    require(contained(path, runner_temp), "--work must be below RUNNER_TEMP")
    return path.resolve()


def archive_entries(archive, allow_symlinks=False):
    entries = {}
    size = 0
    for entry in archive.infolist():
        name = entry.filename
        parts = PurePosixPath(name).parts
        require(name and "\\" not in name and not name.startswith("/")
                and all(part not in ("..", ".") and ":" not in part for part in parts),
                f"Unsafe archive name: {name!r}")
        require(name not in entries, f"Duplicate archive name: {name!r}")
        require(not entry.flag_bits & 1, "Encrypted archive entry")
        size += entry.file_size
        require(entry.file_size <= 128 * 1024**2 and size <= 512 * 1024**2
                and len(entries) < 4096, "Probe archive exceeds its bounds")
        if stat.S_ISLNK(entry.external_attr >> 16):
            require(allow_symlinks and entry.file_size <= 4096, "Unexpected archive symlink")
            target = archive.read(entry).decode("utf-8")
            require(target and not target.startswith("/") and "\\" not in target
                    and ":" not in target, "Unsafe symlink target")
            depth = len(parts) - 1
            for part in PurePosixPath(target).parts:
                if part == "..":
                    depth -= 1
                    require(depth > 0, "Archive symlink leaves its application bundle")
                elif part != ".":
                    depth += 1
        entries[name] = entry
    return entries


def inspect_release(directory, package_id, version, target):
    system, arch, channel, _ = TARGETS[target]
    directory = Path(directory)
    feed = read_json(directory / f"releases.{channel}.json")
    assets = feed.get("Assets", [])
    require(len(assets) == 1, "Probe feed must contain exactly one full release")
    asset = assets[0]
    filename = f"{package_id}-{version}-{channel}-full.nupkg"
    require((asset.get("PackageId"), asset.get("Version"), asset.get("Type"), asset.get("FileName"))
            == (package_id, version, "Full", filename), "Probe feed identity mismatch")
    package = directory / filename
    require(re.fullmatch(r"[0-9a-fA-F]{64}", asset.get("SHA256", "")), "Missing package SHA-256")
    require(package.stat().st_size == asset["Size"] and digest(package).lower() == asset["SHA256"].lower(),
            "Probe package differs from the feed digest or size")
    exe_name = PROGRAM + (".exe" if system == "win" else "")
    executable_entry = "lib/app/" + (exe_name if system == "win" else f"Contents/MacOS/{exe_name}")
    with zipfile.ZipFile(package) as archive:
        entries = archive_entries(archive, system == "osx")
        specs = [name for name in entries if name.endswith(".nuspec")]
        require(specs == [f"{package_id}.nuspec"], "Unexpected package manifest")
        tree = ET.fromstring(archive.read(specs[0]))
        metadata = {node.tag.rsplit("}", 1)[-1]: node.text or "" for node in tree.iter()}
        require((metadata.get("id"), metadata.get("version"), metadata.get("channel"), metadata.get("os"))
                == (package_id, version, channel, system), "Probe NuSpec identity mismatch")
        require(metadata.get("machineArchitecture", "").lower() == arch, "Probe architecture mismatch")
        executable_sha256 = hashlib.sha256(archive.read(executable_entry)).hexdigest()
    portable = directory / f"{package_id}-{channel}-Portable.zip"
    with zipfile.ZipFile(portable) as archive:
        entries = archive_entries(archive, system == "osx")
        exe_entry = f"current/{exe_name}" if system == "win" else f"{package_id}.app/Contents/MacOS/{exe_name}"
        require(hashlib.sha256(archive.read(exe_entry)).hexdigest() == executable_sha256,
                "Portable executable differs from the full package")
        helper = "Update.exe" if system == "win" else f"{package_id}.app/Contents/MacOS/UpdateMac"
        require(helper in entries, "Portable has no official update helper")
        if system == "win":
            require(".portable" in entries and "current/sq.version" in entries,
                    "Windows archive is not a complete portable installation")
    return {"version": version, "package": str(package), "package_sha256": digest(package),
            "portable": str(portable), "portable_sha256": digest(portable),
            "executable_sha256": executable_sha256, "channel": channel, "asset": asset}


def validate_confirmation(receipt, context, expected_sha256, old_pid):
    require(receipt.get("phase") == "confirmed" and receipt.get("version") == VERSIONS[1]
            and receipt.get("manifest_version") == VERSIONS[1], "The new version did not confirm its identity")
    require(receipt.get("nonce") == context["nonce"] and receipt.get("package_id") == context["package_id"],
            "Restarted receipt belongs to another probe")
    require(type(receipt.get("pid")) is int and receipt["pid"] > 0 and receipt["pid"] != old_pid,
            "Confirmation did not come from a new process")
    executable = Path(receipt["executable"]).resolve(strict=True)
    require(executable.samefile(Path(context["executable"]).resolve(strict=True))
            and contained(executable, context["install_root"]), "Restarted executable left its private installation")
    require(Path(receipt["packages"]).resolve(strict=True).samefile(Path(context["packages"]).resolve(strict=True)),
            "Restarted SDK cache is not private")
    require(receipt.get("installation_portable") is True, "Probe lost portable installation identity")
    require(digest(executable) == expected_sha256, "Restarted executable differs from the new update package")
    return {**receipt, "executable_sha256": expected_sha256}


class WindowsJob:
    """Keep the test process and its official helper in a kill-on-close job."""

    def __init__(self):
        from ctypes import wintypes

        class Limits(ctypes.Structure):
            _fields_ = [("process_time", ctypes.c_int64), ("job_time", ctypes.c_int64),
                        ("flags", wintypes.DWORD), ("min_working", ctypes.c_size_t),
                        ("max_working", ctypes.c_size_t), ("active_limit", wintypes.DWORD),
                        ("affinity", ctypes.c_size_t), ("priority", wintypes.DWORD),
                        ("scheduling", wintypes.DWORD)]

        class Extended(ctypes.Structure):
            _fields_ = [("basic", Limits), ("io", ctypes.c_uint64 * 6),
                        ("process_memory", ctypes.c_size_t), ("job_memory", ctypes.c_size_t),
                        ("peak_process", ctypes.c_size_t), ("peak_job", ctypes.c_size_t)]

        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
        self.kernel.CreateJobObjectW.restype = wintypes.HANDLE
        self.kernel.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int,
                                                       ctypes.c_void_p, wintypes.DWORD]
        self.kernel.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
        self.kernel.QueryInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int,
                                                         ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p]
        self.kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        self.handle = self.kernel.CreateJobObjectW(None, None)
        require(self.handle, "Could not create private process job")
        limits = Extended()
        limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not self.kernel.SetInformationJobObject(self.handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
            self.close()
            raise ctypes.WinError(ctypes.get_last_error())

    def assign(self, process):
        if not self.kernel.AssignProcessToJobObject(self.handle, int(process._handle)):
            raise ctypes.WinError(ctypes.get_last_error())

    def active(self):
        # JOBOBJECT_BASIC_ACCOUNTING_INFORMATION: four 64-bit times, four DWORDs.
        values = ctypes.create_string_buffer(48)
        if not self.kernel.QueryInformationJobObject(self.handle, 1, values, len(values), None):
            raise ctypes.WinError(ctypes.get_last_error())
        return int.from_bytes(values.raw[40:44], "little")

    def close(self):
        if self.handle:
            self.kernel.CloseHandle(self.handle)
            self.handle = None


def mac_owned_processes(work):
    output = subprocess.run(["/bin/ps", "-axo", "pid=,comm="], check=True,
                            capture_output=True, text=True, timeout=5).stdout
    found = []
    for line in output.splitlines():
        fields = line.strip().split(None, 1)
        if len(fields) == 2 and fields[0].isdigit() and Path(fields[1]).is_absolute():
            if contained(fields[1], work):
                found.append(int(fields[0]))
    return found


class Probe:
    def __init__(self, args):
        self.args = args
        self.work = private_work(args.work, os.environ.get("RUNNER_TEMP"))
        self.output = Path(args.output).resolve()
        require(Path(args.output).is_absolute() and contained(self.output, self.work),
                "--output must be an absolute file path below the new --work")
        require(not self.output.exists(), "--output already exists")
        system, arch, channel, required_platform = TARGETS[args.target]
        machine = {"amd64": "x64", "x86_64": "x64", "aarch64": "arm64", "arm64": "arm64"}.get(platform.machine().lower())
        require(sys.platform == required_platform and machine == arch,
                "Run this probe on the matching native Windows/macOS runner; cross execution is not supported")
        self.system, self.channel = system, channel
        self.work.mkdir(parents=True, mode=0o700)
        self.output.parent.mkdir(parents=True, exist_ok=True)
        self.nonce = uuid.uuid4().hex
        self.package_id = "bilibilive-probe-" + self.nonce[:16]
        self.result = {"status": "preparing", "sdk_version": SDK_VERSION, "target": args.target,
                       "work": str(self.work), "package_id": self.package_id, "commands": [],
                       "isolation": {"system_install": False, "profile_settings_changed": False,
                                     "application_accounts": False, "update_source": "private FileSource"}}
        write_json(self.work / "probe-owner.json", {"nonce": self.nonce, "package_id": self.package_id})
        for name in ("logs", "tmp", "receipts", "packages"):
            (self.work / name).mkdir()
        self.env = os.environ.copy()
        self.env.update({"TMPDIR": str(self.work / "tmp"), "TEMP": str(self.work / "tmp"),
                         "TMP": str(self.work / "tmp"), "CARGO_TARGET_DIR": str(self.work / "cargo-target"),
                         "PYTHONDONTWRITEBYTECODE": "1"})
        # Disable SDK debug mode: fast installer hooks must return normally.
        self.env.pop("VELOPACK_DEBUG", None)
        self.env.pop("VELOPACK_RESTART", None)
        self.env.pop("VELOPACK_FIRSTRUN", None)
        self.child = None
        self.job = None
        self.default_log = None

    def command(self, label, command, timeout=600):
        command = [str(part) for part in command]
        log = self.work / "logs" / f"{label}.log"
        started = time.monotonic()
        record = {"label": label, "command": command, "log": str(log)}
        self.result["commands"].append(record)
        print(f"[{label}] running", flush=True)
        try:
            with log.open("wb") as stream:
                process = subprocess.run(command, cwd=self.work, env=self.env, stdout=stream,
                                         stderr=subprocess.STDOUT, timeout=timeout, check=False)
            record["exit_code"] = process.returncode
            require(process.returncode == 0, f"{label} failed with exit {process.returncode}; see {log}")
        finally:
            record["seconds"] = round(time.monotonic() - started, 3)
            if log.exists():
                record["log_sha256"] = digest(log)
        return log.read_text(encoding="utf-8", errors="replace")

    def build_release(self, version, project):
        manifest = f'''[package]
name = "{PROGRAM}"
version = "{version}"
edition = "2021"
publish = false

[workspace]

[dependencies]
velopack = {{ version = "={SDK_VERSION}", features = ["public-utils"] }}
serde_json = "={SERDE_JSON_VERSION}"

[profile.dev]
debug = 0
incremental = false
'''
        (project / "Cargo.toml").write_text(manifest, encoding="utf-8")
        self.command(f"build-{version}", ["cargo", "build", "--manifest-path", project / "Cargo.toml",
                                        "--target", self.args.target], timeout=1200)
        built = self.work / "cargo-target" / self.args.target / "debug" / (PROGRAM + (".exe" if self.system == "win" else ""))
        require(built.is_file(), "Cargo did not produce the expected test executable")
        version_dir = self.work / f"version-{version}"
        version_dir.mkdir()
        shutil.copy2(project / "Cargo.lock", version_dir / "Cargo.lock")
        if self.system == "win":
            payload = version_dir / "payload"
            payload.mkdir()
            shutil.copy2(built, payload / built.name)
        else:
            payload = version_dir / f"{self.package_id}.app"
            (payload / "Contents/MacOS").mkdir(parents=True)
            (payload / "Contents/Resources").mkdir()
            shutil.copy2(built, payload / "Contents/MacOS" / built.name)
            info = {"CFBundleName": self.package_id, "CFBundleExecutable": PROGRAM,
                    "CFBundleIdentifier": "io.github.lightjunction.bilibilive.probe." + self.nonce,
                    "CFBundlePackageType": "APPL", "CFBundleShortVersionString": version,
                    "CFBundleVersion": version, "LSBackgroundOnly": True, "LSUIElement": True}
            (payload / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
        releases = version_dir / "releases"
        command = [self.args.vpk, "pack", "--packId", self.package_id, "--packVersion", version,
                   "--packTitle", self.package_id, "--packAuthors", "bilibilive CI fixture",
                   "--packDir", payload, "--mainExe", built.name, "--runtime", self.channel,
                   "--channel", self.channel, "--delta", "None", "--noInst", "--outputDir", releases,
                   "--yes", "--skip-updates", "--legacyConsole"]
        if self.system == "osx":
            command += ["--signAppIdentity", "-"]
        pack_log = self.command(f"pack-{version}", command)
        require(re.search(r"Velopack CLI\s+" + re.escape(SDK_VERSION) + r"(?![0-9.])", pack_log),
                f"Use vpk {SDK_VERSION} to match the Rust SDK; see the pack log")
        result = inspect_release(releases, self.package_id, version, self.args.target)
        result["build_lock_sha256"] = digest(version_dir / "Cargo.lock")
        result["built_executable_sha256"] = digest(built)
        return result

    def cleanup(self):
        # No process-name-wide kill: Windows job membership or a full private
        # executable path identifies every process eligible for termination.
        if self.job:
            self.job.close()
        if self.child and self.child.poll() is None:
            self.child.kill()
            self.child.wait(timeout=5)
        if self.system == "osx":
            pids = mac_owned_processes(self.work)
            for pid in pids:
                try:
                    os.kill(pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            if pids:
                time.sleep(0.2)
                for pid in mac_owned_processes(self.work):
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
        self.result["cleanup"] = {"private_processes_only": True, "artifacts_retained": True}
        default_log = self.default_log
        if default_log and default_log.is_file() and default_log.stat().st_size <= 8 * 1024**2:
            copied = self.work / "logs/official-helper.log"
            shutil.copy2(default_log, copied)
            exception = self.result["isolation"]["sdk_log_exception"]
            exception["copied_to"] = str(copied)
            exception["log_sha256"] = digest(copied)
            # Only this unpredictable, unique package's own diagnostic file.
            default_log.unlink()
            exception["removed_own_log"] = True

    def run(self):
        project = self.work / "project"
        (project / "src").mkdir(parents=True)
        (project / "src/main.rs").write_text(RUST_SOURCE, encoding="utf-8")
        self.result["probe_source_sha256"] = hashlib.sha256(RUST_SOURCE.encode()).hexdigest()
        old, new = [self.build_release(version, project) for version in VERSIONS]
        self.result["releases"] = [old, new]
        installation = self.work / "private-install"
        installation.mkdir()
        if self.system == "osx":
            self.command("extract-portable", ["/usr/bin/ditto", "-x", "-k", old["portable"], installation], timeout=60)
            root = installation / f"{self.package_id}.app"
            binary_dir = root / "Contents/MacOS"
            executable, helper = binary_dir / PROGRAM, binary_dir / "UpdateMac"
            manifest = root / "Contents/Resources/sq.version"
        else:
            with zipfile.ZipFile(old["portable"]) as archive:
                archive_entries(archive)
                archive.extractall(installation)
            root, binary_dir = installation, installation / "current"
            executable, helper = binary_dir / (PROGRAM + ".exe"), root / "Update.exe"
            manifest = binary_dir / "sq.version"
        require(digest(executable) == old["executable_sha256"], "Extracted old executable mismatch")
        require(helper.is_file() and manifest.is_file(), "Incomplete private portable installation")
        context = {"nonce": self.nonce, "package_id": self.package_id, "channel": self.channel,
                   "work": str(self.work), "install_root": str(root), "binary_dir": str(binary_dir),
                   "executable": str(executable), "helper": str(helper), "manifest": str(manifest),
                   "packages": str(self.work / "packages"), "feed": str(Path(new["package"]).parent),
                   "receipts": str(self.work / "receipts"), "package_sha256": new["package_sha256"]}
        context_path = self.work / "context.json"
        write_json(context_path, context)
        self.result["isolation"].update({"install_root": str(root), "packages_dir": context["packages"],
                                        "official_helper_sha256_before": digest(helper)})
        if self.system == "win":
            default_log = Path(os.environ.get("LOCALAPPDATA", str(self.work / "tmp"))) / "velopack" / f"velopack_{self.package_id}.log"
        else:
            log_directory = Path.home() / "Library/Logs"
            default_log = (log_directory if log_directory.is_dir() else self.work / "tmp") / f"velopack_{self.package_id}.log"
        self.result["isolation"]["sdk_log_exception"] = {
            "expected_path": str(default_log), "scope": "unique probe package on ephemeral CI runner",
            "reason": "The official SDK helper uses the runner account's default diagnostic log location.",
            "home_changed": False}
        self.default_log = default_log
        write_json(self.output, self.result)
        deadline = time.monotonic() + 90
        with (self.work / "logs/probe.log").open("wb") as log:
            self.child = subprocess.Popen([str(executable), "--update", "--context", str(context_path)],
                                          cwd=root, env=self.env, stdout=log, stderr=subprocess.STDOUT,
                                          start_new_session=self.system == "osx")
            if self.system == "win":
                self.job = WindowsJob()
                self.job.assign(self.child)
            (self.work / "begin-update").write_text(self.nonce, encoding="ascii")
            confirmation = self.work / "receipts/confirmed.json"
            while time.monotonic() < deadline:
                code = self.child.poll()
                require(code in (None, 0), f"Old probe exited with {code}; see logs/probe.log")
                if confirmation.exists() and code == 0:
                    break
                time.sleep(0.1)
            require(confirmation.exists() and self.child.poll() == 0,
                    "No verified restart within 90 seconds; inspect helper/probe logs")
            downloaded = read_json(self.work / "receipts/downloaded.json")
            started = read_json(self.work / "receipts/helper-started.json")
            require(started.get("pid") == self.child.pid and started.get("nonce") == self.nonce,
                    "Helper receipt did not originate from the old probe")
            require(downloaded.get("nonce") == self.nonce and downloaded.get("source") == "FileSource",
                    "Download receipt is not from the private source")
            validate_downloaded_package(downloaded["package"], context["packages"], new["package_sha256"])
            confirmed = validate_confirmation(read_json(confirmation), context, new["executable_sha256"], self.child.pid)
            # Give the official helper and the short-lived new process time to
            # return; all runtime waiting remains inside the same 90s deadline.
            active = self.job.active() if self.job else len(mac_owned_processes(self.work))
            while active and time.monotonic() < deadline:
                active = self.job.active() if self.job else len(mac_owned_processes(self.work))
                if active == 0:
                    break
                time.sleep(0.1)
            require(active == 0, "Private update processes did not finish within 90 seconds")
        self.result["flow"] = {"old_pid": self.child.pid, "old_exit_code": 0,
                               "downloaded": downloaded, "helper_started": started, "confirmed": confirmed,
                               "private_processes_exited": True, "update_wait_seconds": round(90 - (deadline - time.monotonic()), 3)}
        self.result["status"] = "passed"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", required=True, help="New absolute directory below RUNNER_TEMP")
    parser.add_argument("--target", required=True, choices=sorted(TARGETS))
    parser.add_argument("--vpk", default="vpk", help=f"Installed vpk {SDK_VERSION} executable")
    parser.add_argument("--output", required=True, help="Absolute receipt JSON path below --work")
    args = parser.parse_args()
    probe = None
    try:
        probe = Probe(args)
        probe.run()
    except (Exception, KeyboardInterrupt) as error:
        if probe:
            probe.result.update(status="failed", error=str(error) or type(error).__name__)
        print(f"installer update probe failed: {error}", file=sys.stderr)
    finally:
        if probe:
            try:
                probe.cleanup()
            except Exception as error:
                probe.result.update(status="failed", cleanup_error=str(error))
            write_json(probe.output, probe.result)
            print(json.dumps({"status": probe.result["status"], "receipt": str(probe.output)}), flush=True)
    return 0 if probe and probe.result["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
