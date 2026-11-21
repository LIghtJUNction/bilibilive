#!/usr/bin/env python3
"""Build and verify native distribution assets in an external staging directory.

The Rust source and Cargo publication file list are never changed by this script.
Native installers use the official Velopack CLI. The checked feed, full package,
portable installation, and extracted macOS installer must contain identical
versioned runtime payload bytes.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import mmap
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import time
import tomllib
import xml.etree.ElementTree as ET
import zipfile
sys.dont_write_bytecode = True

VPK_VERSION = "1.2.161"
TARGETS = {
    "x86_64-unknown-linux-gnu": ("linux", "x64", None),
    "x86_64-pc-windows-msvc": ("win", "x64", "win-x64"),
    "aarch64-apple-darwin": ("osx", "arm64", "osx-arm64"),
    "x86_64-apple-darwin": ("osx", "x64", "osx-x64"),
}
MAX_MEMBER = 2 * 1024**3
MAX_TOTAL = 6 * 1024**3
# The official Velopack 1.2.161 SetupBundle marker. Its two preceding signed
# little-endian int64 fields identify the exact embedded full NuGet package.
SETUP_BUNDLE_SIGNATURE = bytes.fromhex("94f0b17b6893e02937eb34ef53aae7d42b54f5707ef5d6f57854983e5e94ed7d")


def require(value, message):
    if not value:
        raise ValueError(message)


def run(command, *, env=None, timeout=1800, capture=False):
    print("+", " ".join(map(str, command)), flush=True)
    return subprocess.run(list(map(str, command)), check=True, env=env,
                          timeout=timeout, capture_output=capture, text=capture)


def digest(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def json_write(path, value):
    Path(path).write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def relative(name):
    require(isinstance(name, str) and 0 < len(name) <= 1024, "Invalid archive path length")
    require(not any(x in name for x in ("\\", ":", "\0")), f"Invalid archive path: {name}")
    parts = name.rstrip("/").split("/")
    require(all(p not in ("", ".", "..") for p in parts), f"Unsafe archive path: {name}")
    require(not PurePosixPath(name).is_absolute(), f"Absolute archive path: {name}")
    return PurePosixPath(name)


def file_map(root):
    result = {}
    for path in sorted(Path(root).rglob("*")):
        require(not path.is_symlink(), f"Native payload cannot contain a symbolic link: {path}")
        if path.is_dir():
            continue
        require(path.is_file(), f"Native payload contains a special file: {path}")
        name = path.relative_to(root).as_posix()
        relative(name)
        require(name.casefold() not in {n.casefold() for n in result}, "Case-insensitive duplicate payload path")
        result[name] = {"size": path.stat().st_size, "sha256": digest(path)}
    return result


def payload(root, version, target):
    root = Path(root)
    manifest_path = root / "bundle-manifest.json"
    require(manifest_path.is_file() and manifest_path.stat().st_size <= 1024**2, "Native payload manifest is missing or too large")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    require((manifest.get("schema"), manifest.get("package"), manifest.get("version"), manifest.get("target"))
            == (1, "bilibilive", version, target), "Native payload manifest identifies a different build")
    records = manifest.get("files")
    require(isinstance(records, dict) and records, "Native payload must contain registered files")
    actual = file_map(root)
    require(set(actual) == set(records) | {"bundle-manifest.json"}, "Native payload has missing or unregistered files")
    for name, record in records.items():
        relative(name)
        require(name.startswith(("resources/", "runtime/")), "Payload file lies outside its declared directories")
        require(isinstance(record, dict) and set(record) <= {"size", "sha256", "executable"}, "Invalid payload file record")
        require(actual[name] == {"size": record["size"], "sha256": record["sha256"]}, f"Native payload bytes changed: {name}")
        require(re.fullmatch(r"[0-9a-f]{64}", record["sha256"]), "Invalid manifest SHA-256")
    plugin = manifest.get("plugin", {})
    plugin_name = str(relative(plugin.get("root", "")) / relative(plugin.get("entrypoint", "")))
    require(plugin_name in records, "Native OBS plugin entrypoint is absent from the manifest")
    runtime = manifest.get("runtime", {})
    if TARGETS[target][0] == "linux":
        require(runtime.get("mode") == "system", "Linux package must explicitly declare its system media dependencies")
    else:
        require(runtime.get("mode") == "bundled", "Windows/macOS distribution must include its native media runtime")
        for key in ("ffmpeg", "ffplay"):
            require(runtime.get("executables", {}).get(key) in records, f"Bundled {key} is missing")
        require(runtime.get("libraries", {}).get("datachannel") in records, "Bundled libdatachannel is missing")
    return manifest


def native_header(data, target):
    platform, arch, _ = TARGETS[target]
    if platform == "win":
        require(data[:2] == b"MZ" and len(data) >= 64, "Windows executable has no DOS/PE header")
        offset = struct.unpack_from("<I", data, 60)[0]
        require(offset + 26 <= len(data) and data[offset:offset + 4] == b"PE\0\0", "Windows executable has no PE signature")
        machine = struct.unpack_from("<H", data, offset + 4)[0]
        magic = struct.unpack_from("<H", data, offset + 24)[0]
        require((machine, magic) == (0x8664, 0x20b), "Windows executable is not x86_64 PE32+")
        return {"format": "PE32+", "architecture": arch}
    if platform == "linux":
        require(data[:6] == b"\x7fELF\x02\x01", "Linux executable is not little-endian ELF64")
        require(struct.unpack_from("<H", data, 18)[0] == 62, "Linux executable is not x86_64")
        return {"format": "ELF64", "architecture": arch}
    expected = 0x0100000c if arch == "arm64" else 0x01000007
    if data[:4] == b"\xcf\xfa\xed\xfe":
        require(struct.unpack_from("<I", data, 4)[0] == expected, "Mach-O executable has the wrong architecture")
        return {"format": "Mach-O64", "architecture": arch}
    if data[:4] in (b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"):
        count = struct.unpack_from(">I", data, 4)[0]
        stride = 32 if data[:4] == b"\xca\xfe\xba\xbf" else 20
        require(0 < count <= 8 and 8 + count * stride <= len(data), "Invalid universal Mach-O architecture table")
        cpus = [struct.unpack_from(">I", data, 8 + i * stride)[0] for i in range(count)]
        require(expected in cpus, "Universal Mach-O does not contain the required architecture")
        return {"format": "Mach-O universal", "architecture": arch, "cpus": cpus}
    raise ValueError("macOS executable has no supported Mach-O header")


def inspect_binary(path, target):
    with Path(path).open("rb") as stream:
        header = stream.read(64 * 1024)
    return {"size": Path(path).stat().st_size, "sha256": digest(path), **native_header(header, target)}


def zip_entries(archive):
    entries = {}
    folded = set()
    total = 0
    for item in archive.infolist():
        relative(item.filename)
        name = item.filename.rstrip("/")
        require(name not in entries and name.casefold() not in folded, f"Duplicate archive path: {name}")
        folded.add(name.casefold())
        require(item.file_size <= MAX_MEMBER and not item.flag_bits & 1, "Oversized or encrypted archive member")
        total += item.file_size
        require(total <= MAX_TOTAL, "Archive expands beyond its size limit")
        kind = stat.S_IFMT(item.external_attr >> 16)
        require(kind in (0, stat.S_IFREG, stat.S_IFDIR, stat.S_IFLNK), "Archive contains a special file")
        if kind == stat.S_IFLNK:
            require(item.file_size <= 1024, "Archive symlink target is too long")
            target = archive.read(item).decode("utf-8")
            require(not target.startswith("/") and "\\" not in target and ":" not in target, "Archive symlink is absolute")
            parts = list(PurePosixPath(name).parent.parts)
            for part in target.split("/"):
                if part == "..":
                    require(parts, "Archive symlink escapes its root")
                    parts.pop()
                elif part not in ("", "."):
                    parts.append(part)
            require(parts, "Archive symlink resolves outside its root")
        entries[name] = item
    return entries


def archive_payload(archive, entries, prefix, manifest, manifest_bytes):
    expected = set(manifest["files"]) | {"bundle-manifest.json"}
    actual = {name[len(prefix):] for name, info in entries.items()
              if name.startswith(prefix) and not info.is_dir()}
    # The application and docs are alongside the payload on Windows. Its two
    # declared resource subtrees and manifest are nevertheless a complete set.
    actual = {name for name in actual if name == "bundle-manifest.json" or name.startswith(("resources/", "runtime/"))}
    require(actual == expected, "Archive native payload has missing or unregistered files")
    for name in expected:
        info = entries[prefix + name]
        require(stat.S_IFMT(info.external_attr >> 16) != stat.S_IFLNK, "Native payload became a symbolic link")
        data = archive.read(info)
        if name == "bundle-manifest.json":
            require(data == manifest_bytes, "Archive rewrote the embedded native manifest")
        else:
            record = manifest["files"][name]
            require(len(data) == record["size"] and hashlib.sha256(data).hexdigest() == record["sha256"],
                    f"Packaging or signing changed native payload bytes: {name}")


def nuspec(data):
    require(len(data) <= 1024**2, "Package manifest is too large")
    root = ET.fromstring(data)
    metadata = next((node for node in root if node.tag.rsplit("}", 1)[-1] == "metadata"), None)
    require(metadata is not None, "Package manifest has no metadata")
    result = {}
    for node in metadata:
        key = node.tag.rsplit("}", 1)[-1]
        require(key not in result, "Package manifest has duplicate fields")
        result[key] = node.text or ""
    return result


def windows_setup_package(installer, package):
    with Path(installer).open("rb") as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as mapped:
        position = mapped.find(SETUP_BUNDLE_SIGNATURE)
        require(position >= 16, "Windows installer has no official Velopack bundle marker")
        offset, length = struct.unpack_from("<qq", mapped, position - 16)
        require(offset >= position + len(SETUP_BUNDLE_SIGNATURE) and length == Path(package).stat().st_size
                and offset + length <= len(mapped), "Windows installer embedded package has an invalid extent")
        h = hashlib.sha256()
        for start in range(offset, offset + length, 1024 * 1024):
            h.update(mapped[start:min(start + 1024 * 1024, offset + length)])
    require(h.hexdigest() == digest(package), "Windows installer embeds a different full update package")
    return {"offset": offset, "size": length, "sha256": h.hexdigest()}


def validate_velopack(directory, version, target, manifest, manifest_bytes):
    directory = Path(directory)
    platform, arch, channel = TARGETS[target]
    require(channel, "Linux assets do not use a Velopack feed")
    feed_path = directory / f"releases.{channel}.json"
    feed = json.loads(feed_path.read_text(encoding="utf-8-sig"))
    assets = feed.get("Assets")
    require(isinstance(assets, list) and len(assets) == 1, "Clean release staging must contain exactly one full package")
    asset = assets[0]
    package_name = f"bilibilive-{version}-{channel}-full.nupkg"
    require((asset.get("PackageId"), asset.get("Version"), asset.get("Type"), asset.get("FileName"))
            == ("bilibilive", version, "Full", package_name), "Feed identity differs from the requested build")
    package = directory / package_name
    require(package.stat().st_size == asset["Size"] and digest(package).upper() == asset["SHA256"].upper(), "Full package differs from its feed digest or size")
    require(re.fullmatch(r"[0-9a-fA-F]{64}", asset["SHA256"]), "Feed does not contain a SHA-256")
    if asset.get("SHA1"):
        h = hashlib.sha1()
        with package.open("rb") as stream:
            for block in iter(lambda: stream.read(1024**2), b""): h.update(block)
        require(h.hexdigest().upper() == asset["SHA1"].upper(), "Full package SHA-1 differs from its feed")
    app_entry = "bilibilive.exe" if platform == "win" else "Contents/MacOS/bilibilive"
    runtime_prefix = "lib/app/" if platform == "win" else "lib/app/Contents/Resources/bilibilive/"
    with zipfile.ZipFile(package) as archive:
        entries = zip_entries(archive)
        specs = [name for name in entries if name.endswith(".nuspec")]
        require(specs == ["bilibilive.nuspec"], "Full package must have one canonical application manifest")
        spec = nuspec(archive.read(specs[0]))
        require((spec.get("id"), spec.get("version"), spec.get("channel"), spec.get("mainExe"), spec.get("os"))
                == ("bilibilive", version, channel, app_entry, platform), "NuSpec identifies a different platform, entrypoint or release")
        require(spec.get("machineArchitecture", "").lower() == arch, "NuSpec architecture differs from its channel")
        archive_payload(archive, entries, runtime_prefix, manifest, manifest_bytes)
        application = archive.read("lib/app/" + app_entry)
        native = {**native_header(application[:65536], target), "size": len(application), "sha256": hashlib.sha256(application).hexdigest()}
        helper = "lib/app/Squirrel.exe" if platform == "win" else "lib/app/Contents/MacOS/UpdateMac"
        require(helper in entries, "Full update package has no official update helper")
        native_header(archive.read(helper)[:65536], target)
    portable = directory / f"bilibilive-{channel}-Portable.zip"
    with zipfile.ZipFile(portable) as archive:
        entries = zip_entries(archive)
        prefix = "current/" if platform == "win" else "bilibilive.app/Contents/Resources/bilibilive/"
        archive_payload(archive, entries, prefix, manifest, manifest_bytes)
        entry = "current/bilibilive.exe" if platform == "win" else "bilibilive.app/Contents/MacOS/bilibilive"
        actual = archive.read(entry)
        require(hashlib.sha256(actual).hexdigest() == native["sha256"], "Portable and full update packages contain different executables")
        helper = "Update.exe" if platform == "win" else "bilibilive.app/Contents/MacOS/UpdateMac"
        require(helper in entries, "Portable installation has no update helper")
        native_header(archive.read(helper)[:65536], target)
        if platform == "win": require(".portable" in entries, "Windows portable installation has no Velopack marker")
    installer = directory / f"bilibilive-{channel}-Setup.{'exe' if platform == 'win' else 'pkg'}"
    require(installer.is_file() and installer.stat().st_size > 0, "Native installer is missing")
    with installer.open("rb") as stream: header = stream.read(65536)
    embedded = None
    if platform == "win":
        native_header(header, target)
        embedded = windows_setup_package(installer, package)
    else: require(header[:4] == b"xar!", "macOS installer is not an XAR package")
    return {"channel": channel, "native_executable": native, "feed": feed_path.name,
            "package": package.name, "portable": portable.name, "installer": installer.name,
            "windows_embedded_package": embedded,
            "payload_files_verified": len(manifest["files"]), "sdk": VPK_VERSION}


def make_app(flat, bundle_dir, version, vpk, icons):
    plist = {
        "CFBundleName": "bilibilive", "CFBundleDisplayName": "bilibilive",
        "CFBundleExecutable": "bilibilive", "CFBundleIdentifier": "io.github.lightjunction.bilibilive",
        "CFBundlePackageType": "APPL", "CFBundleShortVersionString": version, "CFBundleVersion": version,
        "CFBundleIconFile": "bilibilive.icns", "NSPrincipalClass": "NSApplication",
        "NSHighResolutionCapable": True,
        "NSMicrophoneUsageDescription": "Capture selected audio only when you start a live interaction.",
        "NSScreenCaptureUsageDescription": "Share your selected screen during a live interaction.",
    }
    info = Path(bundle_dir).parent / "Info.plist"
    info.write_bytes(plistlib.dumps(plist))
    run([vpk, "bundle", "--packId", "bilibilive", "--packVersion", version,
         "--packTitle", "bilibilive", "--packAuthors", "LIghtJUNction", "--packDir", flat,
         "--mainExe", "bilibilive", "--plist", info,
         "--icon", icons / "bilibilive.icns", "--outputDir", bundle_dir, "--yes", "--skip-updates"])
    app = Path(bundle_dir) / "bilibilive.app"
    macos = app / "Contents/MacOS"
    resources = app / "Contents/Resources"
    payload_root = resources / "bilibilive"
    payload_root.mkdir()
    for name in ("bundle-manifest.json", "resources", "runtime"):
        source = macos / name
        if source.exists(): shutil.move(str(source), payload_root / name)
    for name in ("LICENSE", "NOTICE", "README.md", "bilibilive.1"):
        if (macos / name).exists(): shutil.move(str(macos / name), resources / name)
    return app


def smoke(binary, profile, output, target, version):
    profile.mkdir(parents=True)
    env = dict(os.environ, BILIBILIVE_GUI_FRAMES="16", LIBGL_ALWAYS_SOFTWARE="1")
    env.pop("BILIBILIVE_COOKIE", None)
    # Every profile is disposable; demo rendering does not authenticate, connect
    # OBS, join RTC, check updates, or change the user's normal preferences.
    command = [str(binary), "--config-dir", str(profile), "gui", "--demo", "--noupdate"]
    if TARGETS[target][0] == "linux": command = ["xvfb-run", "-a", *command]
    hook_result = None
    encoder_results = []
    if TARGETS[target][0] in ("win", "osx"):
        hook = subprocess.run([str(binary), "--veloapp-updated", version], env=env, timeout=15, capture_output=True)
        hook_result = {"exit_code": hook.returncode, "stderr": hook.stderr.decode("utf-8", "replace")[-2000:]}
        require(hook.returncode == 0, "Packaged Rust application does not handle the native Velopack startup hook")
        for codec, encoder in (("h264", "libx264"), ("av1", "libsvtav1")):
            probe = subprocess.run([str(binary), "--config-dir", str(profile), "call", "rtc.encoder.probe", "--args",
                                    json.dumps({"codec": codec, "encoder": encoder}), "--json"],
                                   env=env, timeout=30, capture_output=True)
            record = {"codec": codec, "encoder": encoder, "exit_code": probe.returncode,
                      "stdout": probe.stdout.decode("utf-8", "replace")[-16000:],
                      "stderr": probe.stderr.decode("utf-8", "replace")[-8000:]}
            encoder_results.append(record)
            json_write(output.with_name(output.stem + "-media.json"), encoder_results)
            require(probe.returncode == 0, f"Installed {codec} encoder probe failed; inspect its media receipt")
            value = json.loads(record["stdout"])
            value = value.get("data", value)
            require(value.get("available") is True and value.get("codec") == codec and value.get("encoder") == encoder
                    and isinstance(value.get("rtp_packets"), int) and value["rtp_packets"] > 0,
                    "Installed encoder did not confirm actual loopback RTP output")
    started = time.time()
    completed = subprocess.run(command, env=env, timeout=60, capture_output=True)
    result = {"command": command, "exit_code": completed.returncode, "seconds": round(time.time() - started, 3),
              "profile": str(profile), "startup_hook": hook_result, "encoder_probes": encoder_results,
              "stdout": completed.stdout.decode("utf-8", "replace")[-8000:],
              "stderr": completed.stderr.decode("utf-8", "replace")[-8000:]}
    json_write(output, result)
    require(completed.returncode == 0, "Private native GUI launch/exit failed; inspect its smoke receipt")
    return result


def write_checksums(directory):
    files = sorted(p for p in Path(directory).iterdir() if p.is_file() and not p.name.endswith(".sha256"))
    for path in files:
        path.with_name(path.name + ".sha256").write_text(digest(path) + "  " + path.name + "\n", encoding="ascii")


def package(args):
    source, binary, payload_root, work, output = (Path(v).resolve() for v in
        (args.source, args.binary, args.payload, args.work, args.output))
    version = tomllib.loads((source / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    require(re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version), "Installers require a stable version")
    require(not work.exists(), "Installer staging must be a new directory")
    for included in ("src", "tests", "examples", "docs", "scripts", "packaging", "plugins"):
        require(not work.is_relative_to(source / included) and not output.is_relative_to(source / included),
                "Generated artifacts must stay outside the Cargo publication source directories")
    work.mkdir(parents=True); output.mkdir(parents=True, exist_ok=True)
    manifest = payload(payload_root, version, args.target)
    manifest_bytes = (payload_root / "bundle-manifest.json").read_bytes()
    expected_hash = os.environ.get("BILIBILIVE_BUNDLE_MANIFEST_SHA256")
    require(expected_hash == hashlib.sha256(manifest_bytes).hexdigest(), "Build environment does not bind this runtime manifest")
    before = inspect_binary(binary, args.target)
    platform, _, channel = TARGETS[args.target]
    flat = work / "payload"
    shutil.copytree(payload_root, flat)
    exe_name = "bilibilive.exe" if platform == "win" else "bilibilive"
    shutil.copy2(binary, flat / exe_name)
    for name in ("README.md", "LICENSE", "NOTICE"):
        shutil.copy2(source / name, flat / name)
    if platform != "win": shutil.copy2(source / "packaging/bilibilive.1", flat / "bilibilive.1")
    verification = {"schema": 1, "version": version, "target": args.target, "input_executable": before,
                    "bundle_manifest_sha256": expected_hash, "payload_files": len(manifest["files"])}
    if platform == "linux":
        name = f"bilibilive-v{version}-{args.target}"
        archive = output / (name + ".tar.gz")
        require(not archive.exists(), "Refusing to overwrite an existing release asset")
        with tarfile.open(archive, "w:gz") as stream: stream.add(flat, arcname=name)
        smoke(flat / exe_name, work / "gui-profile", output / f"{name}-gui-smoke.json", args.target, version)
        verification.update({"archive": archive.name, "native_executable": before,
                             "installation": "portable", "runtime": "distribution packages"})
    else:
        from installer_icons import create_icons
        icons = work / "icons"; create_icons(source / "packaging/bilibilive.svg", icons)
        releases = work / "velopack-release"
        pack_dir = flat
        signing = []
        if platform == "osx":
            pack_dir = make_app(flat, work / "bundle", version, args.vpk, icons)
            identity = args.sign_app_identity or "-"
            sign_main = ["codesign", "--force", "--sign", identity, "--options", "runtime", "--entitlements",
                         source / "packaging/installer.entitlements"]
            if args.keychain: sign_main.extend(["--keychain", args.keychain])
            run([*sign_main, pack_dir / "Contents/MacOS/bilibilive"])
            signing = ["--signAppIdentity", identity, "--signDisableDeep", "--signEntitlements",
                       str(source / "packaging/installer.entitlements")]
            if args.sign_installer_identity: signing += ["--signInstallIdentity", args.sign_installer_identity]
            if args.notary_profile: signing += ["--notaryProfile", args.notary_profile]
            if args.keychain: signing += ["--keychain", args.keychain]
            verification["signing"] = {"application": "ad-hoc" if identity == "-" else identity,
                                       "installer": args.sign_installer_identity or "unsigned",
                                       "notarized": bool(args.notary_profile and identity != "-" and args.sign_installer_identity)}
        else:
            verification["signing"] = {"application": "unsigned", "installer": "unsigned", "notarized": False}
            signing = ["--framework", "vcredist145-x64"]
        run([args.vpk, "pack", "--packId", "bilibilive", "--packVersion", version,
             "--packTitle", "bilibilive", "--packAuthors", "LIghtJUNction", "--packDir", pack_dir,
             "--mainExe", exe_name, "--runtime", channel, "--channel", channel,
             "--icon", icons / ("bilibilive.ico" if platform == "win" else "bilibilive.icns"),
             "--delta", "None", "--outputDir", releases, "--yes", "--skip-updates", "--legacyConsole", *signing])
        verified = validate_velopack(releases, version, args.target, manifest, manifest_bytes)
        verification.update(verified)
        installed = work / "portable-installed"; installed.mkdir()
        portable = releases / verified["portable"]
        if platform == "osx":
            run(["ditto", "-x", "-k", portable, installed])
            app = installed / "bilibilive.app"
            payload(app / "Contents/Resources/bilibilive", version, args.target)
            run(["codesign", "--verify", "--strict", app])
            run(["pkgutil", "--expand-full", releases / verified["installer"], work / "pkg-expanded"])
            apps = list((work / "pkg-expanded").rglob("bilibilive.app"))
            require(len(apps) == 1, "Installer must contain exactly one bilibilive.app")
            payload(apps[0] / "Contents/Resources/bilibilive", version, args.target)
            require(digest(apps[0] / "Contents/MacOS/bilibilive") == verified["native_executable"]["sha256"], "Installer application differs from the verified update package")
            archive = output / f"bilibilive-v{version}-{args.target}.app.tar.gz"
            require(not archive.exists(), "Refusing to overwrite an existing app archive")
            with tarfile.open(archive, "w:gz", dereference=False) as stream: stream.add(app, arcname="bilibilive.app")
            executable = app / "Contents/MacOS/bilibilive"
        else:
            with zipfile.ZipFile(portable) as stream:
                entries = zip_entries(stream)
                require(all(stat.S_IFMT(i.external_attr >> 16) != stat.S_IFLNK for i in entries.values()), "Windows portable archive contains a symbolic link")
                stream.extractall(installed)
            executable = installed / "current/bilibilive.exe"
        smoke(executable, work / "gui-profile", output / f"bilibilive-v{version}-{args.target}-gui-smoke.json", args.target, version)
        publish_names = [verified[key] for key in ("feed", "package", "portable", "installer")]
        for name in publish_names:
            require(not (output / name).exists(), f"Refusing to overwrite release asset {name}")
            shutil.copy2(releases / name, output / name)
    json_write(output / f"bilibilive-v{version}-{args.target}-installer-verification.json", verification)
    write_checksums(output)
    print(json.dumps(verification), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", default=str(Path(__file__).resolve().parent.parent))
    parser.add_argument("--binary", required=True)
    parser.add_argument("--payload", required=True)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--work", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--vpk", default="vpk")
    parser.add_argument("--sign-app-identity")
    parser.add_argument("--sign-installer-identity")
    parser.add_argument("--notary-profile")
    parser.add_argument("--keychain")
    package(parser.parse_args())


if __name__ == "__main__":
    try: main()
    except (OSError, ValueError, subprocess.SubprocessError, KeyError, zipfile.BadZipFile) as error:
        raise SystemExit(f"Native distribution preparation failed: {error}") from error
