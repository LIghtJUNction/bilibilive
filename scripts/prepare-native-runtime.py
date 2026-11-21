#!/usr/bin/env python3
"""Build and verify the native payload before compiling the application binary.

Downloads/builds belong to --cache and --output, never to included source paths.
Windows/macOS OBS/Qt use the latest OBS release's published SDK checksums;
Linux uses its installed SDK and media packages. Bundled FFmpeg and
libdatachannel are built from their latest official stable release sources.
No downloaded application binary is accepted in place of a source build.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shlex
import shutil
import signal
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
import urllib.parse
import urllib.request
import zipfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
TARGETS = {"x86_64-unknown-linux-gnu": ("Linux", "x86_64"),
           "x86_64-pc-windows-msvc": ("Windows", "x86_64"),
           "aarch64-apple-darwin": ("Darwin", "arm64"),
           "x86_64-apple-darwin": ("Darwin", "x86_64")}
REPOS = {"obs": "https://github.com/obsproject/obs-studio.git",
         "ffmpeg": "https://github.com/FFmpeg/FFmpeg.git",
         "datachannel": "https://github.com/paullouisageneau/libdatachannel.git",
         "openssl": "https://github.com/openssl/openssl.git",
         "x264": "https://code.videolan.org/videolan/x264.git",
         "svtav1": "https://gitlab.com/AOMediaCodec/SVT-AV1.git",
         "opus": "https://github.com/xiph/opus.git",
         # VideoLAN's official read-only mirror keeps the same tags and commits
         # available when its primary GitLab host cannot be reached by a runner.
         "dav1d": "https://github.com/videolan/dav1d.git",
         "sdl2": "https://github.com/libsdl-org/SDL.git",
         "nvcodec": "https://github.com/FFmpeg/nv-codec-headers.git",
         "amf": "https://github.com/GPUOpen-LibrariesAndSDKs/AMF.git",
         "libvpl": "https://github.com/intel/libvpl.git"}
MAX_DOWNLOAD = 3 * 1024**3
COMMANDS: list[dict] = []


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def write_json(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    partial = path.with_suffix(path.suffix + ".tmp")
    with partial.open("w", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    partial.replace(path)


def request(url: str, limit=4 * 1024 * 1024) -> tuple[bytes, str]:
    require(url.startswith("https://"), "Only HTTPS public source URLs are accepted")
    req = urllib.request.Request(url, headers={"User-Agent": "bilibilive-native-builder", "Cache-Control": "no-cache"})
    with urllib.request.urlopen(req, timeout=45) as response:
        final = response.geturl()
        require(final.startswith("https://"), "Public source redirected away from HTTPS")
        body = response.read(limit + 1)
    require(len(body) <= limit, "Public source metadata exceeds its size bound")
    return body, final


def run(argv, cwd=None, env=None, timeout=180, log: Path | None = None) -> str:
    argv = [str(arg) for arg in argv]
    started = time.monotonic()
    execution_env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1", GIT_TERMINAL_PROMPT="0")
    if env:
        execution_env.update({str(k): str(v) for k, v in env.items()})
    record = {"argv": argv, "cwd": str(cwd or ROOT), "timeout_seconds": timeout}
    COMMANDS.append(record)
    if log: log.parent.mkdir(parents=True, exist_ok=True)
    if log: print(f"Preparing native payload: {log.stem}", flush=True)
    with (log.open("w+b") if log else tempfile.TemporaryFile()) as stdout, tempfile.TemporaryFile() as stderr:
        process = subprocess.Popen(argv, cwd=cwd, env=execution_env, stdout=stdout,
                                   stderr=subprocess.STDOUT if log else stderr,
                                   start_new_session=os.name != "nt",
                                   creationflags=subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0)
        try:
            code = process.wait(timeout=timeout)
        except BaseException:
            # Only this invocation's process group/tree is cancelled. No
            # sibling build, OBS process or user's application is signalled.
            if os.name == "nt":
                subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=False)
            else:
                try: os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError: pass
            process.wait(timeout=30)
            record.update(status="cancelled-or-timeout", elapsed_seconds=round(time.monotonic() - started, 3))
            raise
        record.update(exit_code=code, elapsed_seconds=round(time.monotonic() - started, 3))
        if log:
            stdout.flush(); record.update(log=str(log), log_sha256=sha(log))
            if code:
                stdout.seek(max(0,stdout.tell()-12000))
                raise RuntimeError(f"Native command failed ({code}); log: {log}\n{stdout.read().decode(errors='replace')}")
            return ""
        stdout.seek(0); stderr.seek(0)
        out=stdout.read(8*1024*1024+1);err=stderr.read(8*1024*1024+1)
        require(len(out)+len(err)<=8*1024*1024,"Native metadata command exceeded its output bound")
        if code: raise RuntimeError(f"Native command failed ({code}): {shlex.join(argv)}\n{err[-8000:].decode(errors='replace')}")
        return out.decode(errors="strict").strip()


def latest_github(repo: str) -> str:
    _, final = request(repo.removesuffix(".git") + "/releases/latest", 8 * 1024 * 1024)
    marker = repo.removesuffix(".git") + "/releases/tag/"
    require(final.startswith(marker), f"Cannot determine the latest stable release of {repo}")
    tag = urllib.parse.unquote(final[len(marker):])
    require(re.fullmatch(r"[A-Za-z0-9._-]+", tag), "Unsafe official release tag")
    return tag


def latest_ffmpeg() -> str:
    body, _ = request("https://ffmpeg.org/download.html")
    versions = re.findall(r"ffmpeg-([0-9]+\.[0-9]+(?:\.[0-9]+)?)\.tar\.(?:xz|bz2|gz)", body.decode())
    require(versions, "FFmpeg's official download page does not identify a stable source archive")
    return max(versions, key=lambda v: tuple(map(int, v.split("."))))


def latest_tag(repo: str, pattern: str) -> str:
    text = run(["git", "ls-remote", "--tags", repo], timeout=60)
    candidates = []
    for line in text.splitlines():
        fields = line.split()
        require(len(fields) == 2 and re.fullmatch(r"[0-9a-f]{40}", fields[0]), "Malformed official Git tag metadata")
        tag = fields[1].removeprefix("refs/tags/")
        match = re.fullmatch(pattern, tag)
        if match: candidates.append((tuple(map(int, match[1].split("."))), tag))
    require(candidates, f"No stable release tags found at {repo}")
    return max(candidates)[1]


def resolve() -> dict:
    tasks = {"obs": lambda: latest_github(REPOS["obs"]),
             "datachannel": lambda: latest_github(REPOS["datachannel"]),
             "ffmpeg": latest_ffmpeg,
             "openssl": lambda: latest_github(REPOS["openssl"]),
             "svtav1": lambda: latest_tag(REPOS["svtav1"], r"v([0-9]+\.[0-9]+\.[0-9]+)"),
             "opus": lambda: latest_tag(REPOS["opus"], r"v([0-9]+\.[0-9]+(?:\.[0-9]+)?)"),
             "dav1d": lambda: latest_tag(REPOS["dav1d"], r"([0-9]+\.[0-9]+\.[0-9]+)"),
             "sdl2": lambda: latest_tag(REPOS["sdl2"], r"release-(2\.[0-9]+\.[0-9]+)"),
             "nvcodec": lambda: latest_github(REPOS["nvcodec"]),
             "amf": lambda: latest_github(REPOS["amf"]),
             "libvpl": lambda: latest_github(REPOS["libvpl"])}
    with concurrent.futures.ThreadPoolExecutor(4) as pool:
        futures = {name: pool.submit(fn) for name, fn in tasks.items()}
        tags = {name: future.result() for name, future in futures.items()}
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", tags["obs"]), "OBS latest release is not stable SemVer")
    require(re.fullmatch(r"v0\.24\.[0-9]+", tags["datachannel"]),
            "The latest libdatachannel changed its 0.24 ABI; audit the Rust ABI before building it")
    tags["ffmpeg"] = "n" + tags["ffmpeg"]
    # VideoLAN publishes x264's current source as master, without numbered
    # releases. Freeze that exact public snapshot and describe it honestly.
    tags["x264"] = "master"
    def identity(item):
        name,tag=item
        reference = f"refs/{'heads' if name == 'x264' else 'tags'}/{tag}"
        text = run(["git", "ls-remote", REPOS[name], reference, reference + "^{}"], timeout=60)
        refs = {ref: commit for commit, ref in (line.split() for line in text.splitlines())}
        commit = refs.get(reference + "^{}", refs.get(reference))
        require(commit and re.fullmatch(r"[0-9a-f]{40}", commit), f"Official source tag not found: {name} {tag}")
        return name,{"repository": REPOS[name], "tag": tag, "commit": commit,
                     "selection": "official master snapshot (no numbered upstream releases)" if name == "x264" else "latest stable release compatible with the required API"}
    with concurrent.futures.ThreadPoolExecutor(4) as pool:
        sources=dict(pool.map(identity,tags.items()))
    return {"schema": 1, "observed_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "sources": sources, "ffmpeg_release_page": "https://ffmpeg.org/download.html",
            "obs_plugin_locations": "https://obsproject.com/kb/legacy-plugin-developer-info"}


def source_checkout(name: str, identity: dict, cache: Path) -> Path:
    path = cache / "sources" / f"{name}-{identity['commit'][:16]}"
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        run(["git", "clone", "--depth", "1", *(["--filter=blob:none","--sparse"] if name=="amf" else []),
             "--branch", identity["tag"], identity["repository"], path], timeout=300)
    if name=="amf":
        # Only SDK headers and root license text are needed; do not download
        # example movies or upstream prebuilt application binaries.
        run(["git","sparse-checkout","set","amf/public/include"],cwd=path,timeout=120)
    require(run(["git", "remote", "get-url", "origin"], cwd=path) == identity["repository"], "Native source cache has a different origin")
    require(run(["git", "rev-parse", "HEAD"], cwd=path) == identity["commit"], "Native source cache has a different commit")
    require(not run(["git", "status", "--porcelain", "--untracked-files=no"], cwd=path), "Native source cache has tracked modifications")
    if name == "datachannel":
        run(["git", "-c", "protocol.file.allow=never", "submodule", "update", "--init", "--recursive", "--depth", "1"], cwd=path, timeout=600)
        identity["submodules"] = run(["git", "submodule", "status", "--recursive"], cwd=path).splitlines()
        require(all(not line.startswith(("-", "+", "U")) and re.fullmatch(r"[0-9a-f]{40}",line.split()[0]) for line in identity["submodules"]), "A native source submodule differs from its recorded Git tree")
    return path


def download(url: str, destination: Path, expected: str) -> None:
    require(re.fullmatch(r"[0-9a-f]{64}", expected), "An official archive SHA-256 is required")
    if destination.is_file() and sha(destination) == expected:
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    partial = destination.with_suffix(destination.suffix + ".part")
    req = urllib.request.Request(url, headers={"User-Agent": "bilibilive-native-builder"})
    digest = hashlib.sha256(); size = 0; started=time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=90) as response, partial.open("wb") as stream:
            require(response.geturl().startswith("https://"), "Archive redirected away from HTTPS")
            for block in iter(lambda: response.read(1024 * 1024), b""):
                size += len(block)
                require(size <= MAX_DOWNLOAD, "Native archive exceeds its download bound")
                require(time.monotonic()-started<=1800,"Native archive download exceeded its deadline")
                digest.update(block); stream.write(block)
            length = response.headers.get("Content-Length")
            require(length is None or int(length) == size, "Native archive HTTP length mismatch")
        require(digest.hexdigest() == expected, f"Official archive checksum mismatch: {destination.name}")
        partial.replace(destination)
    finally:
        partial.unlink(missing_ok=True)


def archive_name(name: str) -> PurePosixPath:
    require("\\" not in name and "\x00" not in name, "Unsafe archive member name")
    path = PurePosixPath(name)
    require(not path.is_absolute() and ".." not in path.parts and not any(":" in p for p in path.parts), "Archive member escapes extraction root")
    return path


def sdk_tar_filter(member: tarfile.TarInfo, destination: str, *, windows=os.name == "nt"):
    # Keep Python's data safety checks, including resolution through links that
    # earlier members created. Python 3.12.10 leaves POSIX separators in Windows
    # symlink targets; CreateSymbolicLink then creates an unreadable link.
    # CPython gh-57911 / PR 138309 fixes the same path-separator conversion.
    filtered = tarfile.data_filter(member, destination)
    if windows and filtered.issym():
        return filtered.replace(linkname=filtered.linkname.replace("/", "\\"), deep=False)
    return filtered


def extract(archive: Path, destination: Path) -> None:
    require(not destination.exists(), "Native SDK extraction destination already exists")
    destination.mkdir(parents=True)
    total = 0; names = set()
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as source:
            require(len(source.infolist()) <= 200000, "SDK archive has too many members")
            for info in source.infolist():
                name = archive_name(info.filename)
                require(str(name).lower() not in names, "SDK archive has duplicate/case-colliding names")
                names.add(str(name).lower()); total += info.file_size
                require(total <= 12 * 1024**3, "SDK archive uncompressed size exceeds its bound")
                require(stat.S_IFMT(info.external_attr >> 16) != stat.S_IFLNK, "ZIP symbolic links are not accepted")
            source.extractall(destination)
    else:
        with tarfile.open(archive) as source:
            members = source.getmembers()
            require(len(members) <= 200000, "SDK archive has too many members")
            for info in members:
                name = archive_name(info.name)
                require(str(name) not in names, "SDK archive has duplicate names")
                names.add(str(name)); total += info.size
                require(total <= 12 * 1024**3, "SDK archive uncompressed size exceeds its bound")
                require(info.isfile() or info.isdir() or info.issym() or info.islnk(), "SDK archive contains a device or special file")
                if info.issym() or info.islnk():
                    require("\\" not in info.linkname and "\x00" not in info.linkname,
                            "SDK archive has an invalid POSIX link target")
                    link = PurePosixPath(info.linkname)
                    require(not link.is_absolute() and not any(":" in part for part in link.parts),
                            "SDK archive has an absolute link or Windows drive/stream target")
                    base = destination / (name.parent if info.issym() else PurePosixPath())
                    require((base / str(link)).resolve().is_relative_to(destination.resolve()), "SDK archive link escapes its root")
            source.extractall(destination, filter=sdk_tar_filter)


def obs_dependencies(source: Path, target: str) -> tuple[list[Path], list[dict], str]:
    presets = json.loads((source / "CMakePresets.json").read_text())
    spec = next(p for p in presets["configurePresets"] if p["name"] == "dependencies")["vendor"]["obsproject.com/obs-studio"]["dependencies"]
    windows = "windows" in target
    tuples = [("prebuilt", "windows-x64" if windows else "macos-universal"),
              ("qt6", "windows-x64" if windows else "macos-universal")]
    if windows: tuples.append(("prebuilt", "windows-x86"))
    prefixes = []; receipts = []
    for kind, key in tuples:
        item = spec[kind]; os_name, arch = key.split("-", 1)
        revision = item.get("revision", {}).get(key)
        suffix = f"-v{revision}" if revision else ""
        prefix = f"{os_name}-deps" + ("-qt6" if kind == "qt6" else "")
        name = f"{prefix}-{item['version']}-{arch}{suffix}" + (".zip" if windows else ".tar.xz")
        require(item["baseUrl"] == "https://github.com/obsproject/obs-deps/releases/download", "Unexpected official OBS dependency host")
        url = f"{item['baseUrl']}/{item['version']}/{name}"
        digest = item["hashes"][key]
        archive = source / ".deps" / name
        download(url, archive, digest)
        directory = source / ".deps" / (f"obs-deps{'-qt6' if kind == 'qt6' else ''}-{item['version']}-{arch}")
        marker = directory.with_name(directory.name + ".bilibilive-sha256")
        if not directory.exists():
            extract(archive, directory); marker.write_text(digest)
        require(marker.is_file() and marker.read_text() == digest, "Unverified cached OBS SDK extraction")
        (source / ".deps" / f".dependency_{kind}_{arch}.sha256").write_text(digest)
        receipts.append({"component":kind,"platform":key,"url":url,"sha256":digest,"bundle_version":item["version"]})
        if arch != "x86": prefixes.append(directory)
    qt = next(p for p in prefixes if "qt6" in p.name)
    config = qt / "mkspecs/qconfig.pri"
    version = re.search(r"^QT_VERSION\s*=\s*(\S+)", config.read_text(), re.M)
    require(version, "Official OBS Qt SDK does not identify its Qt version")
    return prefixes, receipts, version[1]


def cmake_build(source: Path, build: Path, prefix: Path, options: list[str], log_root: Path,
                name: str, jobs: int, generator="Ninja", targets=(), component=None, env=None):
    run(["cmake", "-S", source, "-B", build, "-G", generator,
         "-DCMAKE_BUILD_TYPE=Release", f"-DCMAKE_INSTALL_PREFIX={prefix}", *options], env=env,
        timeout=900, log=log_root / f"{name}-configure.log")
    args = ["cmake", "--build", build, "--config", "Release", "--parallel", str(jobs)]
    if targets: args += ["--target", *targets]
    run(args, env=env, timeout=3600, log=log_root / f"{name}-build.log")
    args = ["cmake", "--install", build, "--config", "Release"]
    if component: args += ["--component", component]
    run(args, env=env, timeout=300, log=log_root / f"{name}-install.log")


def prepare_plugin(plan: dict, target: str, cache: Path, output: Path, jobs: int) -> dict:
    logs = cache / "logs" / target
    build = cache / "build" / target
    prefixes = []; sdk_receipt = {}
    generator = "Ninja"
    options = []
    if "linux" not in target:
        source = source_checkout("obs", plan["sources"]["obs"], cache)
        prefixes, archives, qt_version = obs_dependencies(source, target)
        if "windows" in target:
            run(["git","-c","protocol.file.allow=never","submodule","update","--init","--depth","1","deps/libdshowcapture/src"],cwd=source,timeout=300)
            presets = json.loads((source / "CMakePresets.json").read_text())
            generator = next(p["generator"] for p in presets["configurePresets"] if p["name"] == "windows-x64")
            options = ["-A", "x64"]
        else:
            generator = "Xcode"
            options = [f"-DCMAKE_OSX_ARCHITECTURES={TARGETS[target][1]}", "-DCMAKE_OSX_DEPLOYMENT_TARGET=13.0",
                       "-DCMAKE_XCODE_ATTRIBUTE_CODE_SIGNING_ALLOWED=NO", "-DOBS_CODESIGN_LINKER=OFF"]
        sdk = build / "obs-sdk"
        cmake_build(source, build / "obs", sdk,
                    [*options, "-DENABLE_FRONTEND=OFF", "-DENABLE_CORE_MODULES=OFF", "-DENABLE_SCRIPTING=OFF", "-DENABLE_BROWSER=OFF",
                     f"-DOBS_VERSION_OVERRIDE={plan['sources']['obs']['tag']}", f"-DCMAKE_PREFIX_PATH={';'.join(map(str,prefixes))}"],
                    logs, "obs-sdk", jobs, generator, ("libobs", "obs-frontend-api"), "Development")
        prefixes = [sdk, sdk / "Frameworks", *prefixes]
        sdk_receipt = {"obs":plan["sources"]["obs"],"archives":archives,"qt_version":qt_version,
                       "qt_selection":"official SDK shipped by the latest stable OBS release; no privately rebuilt Qt is loaded into OBS"}
    plugin_stage = build / "plugin-stage"
    require(not plugin_stage.exists(), "Plugin stage already exists; use a new build cache or inspect the previous failed build")
    plugin_options = list(options)
    if prefixes: plugin_options += [f"-DCMAKE_PREFIX_PATH={';'.join(map(str,prefixes))}"]
    if "linux" in target: plugin_options += ["-DCMAKE_INSTALL_LIBDIR=lib"]
    cmake_build(ROOT / "plugins/obs-bilibilive-pk", build / "plugin", plugin_stage, plugin_options,
                logs, "obs-plugin", jobs, generator)
    if "windows" in target:
        name="obs-bilibilive-pk"; entry="obs-bilibilive-pk.dll"
        artifact = plugin_stage / "obs-plugins/64bit" / entry
        require(artifact.is_file(), "Windows plugin DLL was not produced")
        destination = output / "resources/obs-plugin" / name
        destination.mkdir(parents=True); shutil.copy2(artifact, destination / entry)
    elif "darwin" in target:
        name="obs-bilibilive-pk.plugin"; entry="Contents/MacOS/obs-bilibilive-pk"
        artifact = plugin_stage / name
        require((artifact / entry).is_file() and (artifact / "Contents/Info.plist").is_file(), "macOS plugin bundle is incomplete")
        import plistlib
        require(plistlib.loads((artifact / "Contents/Info.plist").read_bytes())["CFBundleExecutable"] == "obs-bilibilive-pk", "Plugin bundle executable identity differs")
        destination = output / "resources/obs-plugin" / name
        shutil.copytree(artifact, destination)
    else:
        name="obs-bilibilive-pk"; entry="bin/64bit/obs-bilibilive-pk.so"
        artifact = plugin_stage / "lib/obs-plugins/obs-bilibilive-pk.so"
        require(artifact.is_file(), "Linux plugin module was not produced")
        destination = output / "resources/obs-plugin" / name
        (destination / "bin/64bit").mkdir(parents=True); shutil.copy2(artifact, destination / entry)
    shutil.copy2(ROOT / "LICENSE", destination / "LICENSE")
    shutil.copy2(ROOT / "plugins/obs-bilibilive-pk/UPSTREAM.md", destination / "UPSTREAM.md")
    if "darwin" not in target: (destination / "data/locale").mkdir(parents=True)
    info = inspect_binary(destination / entry, target)
    if "windows" in target:
        imports=run(["dumpbin","/DEPENDENTS",destination/entry])
        names=re.findall(r"^\s+([A-Za-z0-9_.-]+\.dll)\s*$",imports,re.M|re.I)
        require(names,"Cannot inspect OBS plugin DLL imports")
        info["dependencies"]={"provided_by":"matching OBS installation and Windows runtime","names":names}
    elif "darwin" in target:
        imports=run(["otool","-L",destination/entry])
        names=[line.strip().split(" (",1)[0] for line in imports.splitlines()[1:]]
        require(names and all(name.startswith(("@rpath/","@executable_path/../Frameworks/","/System/Library/","/usr/lib/")) for name in names),"OBS plugin retains a nonportable SDK dependency path")
        info["dependencies"]={"provided_by":"matching OBS.app Frameworks and macOS","names":names}
    else:
        imports=run(["readelf","-d",destination/entry])
        info["dependencies"]={"provided_by":"distribution packages","names":re.findall(r"\(NEEDED\).*?\[(.*?)\]",imports)}
    return {"root":destination.relative_to(output).as_posix(),"entrypoint":entry,"build":sdk_receipt,"binary":info}


def msys_path(path: Path) -> str:
    text = str(path.resolve()).replace("\\", "/")
    if re.match(r"^[A-Za-z]:/", text): return "/" + text[0].lower() + text[2:]
    return text


def codec_run(argv, cwd: Path, env: dict, msys: Path | None, log: Path | None = None,
              timeout=3600) -> str:
    if msys:
        script = "export PATH=/ucrt64/bin:/usr/bin; cd " + shlex.quote(msys_path(cwd)) + "; " + shlex.join(map(str, argv))
        return run([msys / "usr/bin/bash.exe", "--login", "-c", script],
                   env=dict(env, MSYSTEM="UCRT64", CHERE_INVOKING="1", MSYS2_PATH_TYPE="minimal"),
                   timeout=timeout, log=log)
    return run(argv, cwd=cwd, env=env, timeout=timeout, log=log)


def prepare_codecs(plan: dict, target: str, cache: Path, jobs: int,
                   msys: Path | None) -> tuple[Path, dict, dict]:
    """Build the actual software codecs and SDL, never silently use old host libs."""
    windows = "windows" in target
    require(not windows or msys is not None, "Windows codec builds require the UCRT64 toolchain")
    toolchain = msys if windows else None
    path = msys_path if windows else str
    root = cache / "build" / target
    prefix = root / "codec-prefix"
    prefix.mkdir(parents=True, exist_ok=True)
    logs = cache / "logs" / target
    pkg = path(prefix / "lib/pkgconfig")
    env = {"PKG_CONFIG_LIBDIR": pkg, "PKG_CONFIG_PATH": pkg,
           "CC": "gcc" if windows else "clang", "CXX": "g++" if windows else "clang++"}
    if not windows: env["MACOSX_DEPLOYMENT_TARGET"] = "13.0"
    sources = {name: source_checkout(name, plan["sources"][name], cache)
               for name in ("x264", "svtav1", "opus", "dav1d", "sdl2")}
    for name, options in (
        ("svtav1", ["-DBUILD_APPS=OFF", "-DBUILD_TESTING=OFF", "-DNATIVE=OFF", "-DSVT_AV1_LTO=OFF"]),
        ("opus", ["-DOPUS_BUILD_TESTING=OFF", "-DOPUS_BUILD_PROGRAMS=OFF", "-DOPUS_INSTALL_PKG_CONFIG_MODULE=ON"]),
        ("sdl2", ["-DSDL_SHARED=ON", "-DSDL_STATIC=OFF", "-DSDL_TEST=OFF", "-DSDL_TESTS=OFF", "-DSDL2_DISABLE_SDL2MAIN=ON"]),
    ):
        build = root / name
        options = [*options, "-DBUILD_SHARED_LIBS=ON", "-DCMAKE_INSTALL_LIBDIR=lib", "-DCMAKE_POSITION_INDEPENDENT_CODE=ON"]
        if not windows:
            options += [f"-DCMAKE_OSX_ARCHITECTURES={TARGETS[target][1]}", "-DCMAKE_OSX_DEPLOYMENT_TARGET=13.0"]
        configure = ["cmake", "-S", path(sources[name]), "-B", path(build), "-G", "Ninja",
                     "-DCMAKE_BUILD_TYPE=Release", f"-DCMAKE_INSTALL_PREFIX={path(prefix)}", *options]
        for stage, args in (("configure", configure),
                            ("build", ["cmake", "--build", path(build), "--parallel", str(jobs)]),
                            ("install", ["cmake", "--install", path(build)])):
            codec_run(args, root, env, toolchain, logs / f"codec-{name}-{stage}.log")
    build = root / "x264"
    build.mkdir(exist_ok=True)
    configure = ["sh", path(sources["x264"] / "configure"), f"--prefix={path(prefix)}",
                 "--enable-shared", "--enable-pic", "--disable-cli"]
    for stage, args in (("configure", configure), ("build", ["make", f"-j{jobs}"]), ("install", ["make", "install"])):
        codec_run(args, build, env, toolchain, logs / f"codec-x264-{stage}.log")
    build = root / "dav1d"
    configure = ["meson", "setup", path(build), path(sources["dav1d"]), f"--prefix={path(prefix)}",
                 "--libdir=lib", "--buildtype=release", "--default-library=shared",
                 "-Denable_tools=false", "-Denable_tests=false", "-Denable_examples=false"]
    for stage, args in (("configure", configure), ("build", ["meson", "compile", "-C", path(build), "-j", str(jobs)]),
                        ("install", ["meson", "install", "-C", path(build)])):
        codec_run(args, root, env, toolchain, logs / f"codec-dav1d-{stage}.log")
    versions = {}
    for name, module in {"x264": "x264", "svtav1": "SvtAv1Enc", "opus": "opus", "dav1d": "dav1d", "sdl2": "sdl2"}.items():
        pcfile = prefix / "lib/pkgconfig" / (module + ".pc")
        require(pcfile.is_file(), f"The private {name} source build did not install its pkg-config identity")
        version = codec_run(["pkg-config", "--modversion", module], root, env, toolchain, timeout=30)
        if name != "x264":
            expected = plan["sources"][name]["tag"].removeprefix("release-").removeprefix("v")
            require(version == expected, f"Built {name} {version} differs from its resolved official source {expected}")
        versions[name] = {"version": version, "source": plan["sources"][name], "pkg_config_sha256": sha(pcfile)}
    return prefix, versions, env


def verify_nvcodec_package(source: Path, prefix: Path, reported_version: str) -> dict:
    # nv-codec-headers' pkg-config revision can differ from its Git tag
    # (n13.1.15.0 publishes Version: 13.1.15.0.1). Compare the installed
    # package to the exact verified source template, without truncating it.
    template=source/"ffnvcodec.pc.in";installed=prefix/"lib/pkgconfig/ffnvcodec.pc"
    versions=[]
    for path in (template,installed):
        require(path.is_file() and path.stat().st_size<=64*1024,"Missing or oversized private NVENC pkg-config identity")
        lines=[line for line in path.read_text(encoding="utf-8").splitlines() if line.startswith("Version:")]
        require(len(lines)==1,"NVENC pkg-config identity must have exactly one Version field")
        version=lines[0].partition(":")[2].strip()
        require(re.fullmatch(r"[0-9]+(?:\.[0-9]+)+",version),"Unsupported NVENC pkg-config Version field")
        versions.append(version)
    require(versions[0]==versions[1]==reported_version,
            f"NVENC pkg-config version {reported_version} / installed {versions[1]} differs from official source {versions[0]}")
    return {"version":reported_version,"pkg_config_sha256":sha(installed),"source_pkg_config_sha256":sha(template)}


def prepare_libvpl_mingw_source(source: Path, output: Path) -> tuple[Path, dict]:
    """Backport Intel's reviewed MinGW guard fix without changing the Git cache."""
    relative=Path("libvpl/src/windows/mfx_dispatcher_defs.h")
    header=source/relative
    original=header.read_bytes()
    canonical=original.replace(b"\r\n",b"\n")
    old=b"#if _MSC_VER < 1400"
    fixed=b"#if defined(_MSC_VER) && _MSC_VER < 1400"
    upstream="412c5cffe3022d5ebb8a955c3b30afe7bd2e3929"
    identity={"file":relative.as_posix(),"upstream_fix_commit":upstream,
              "source_header_sha256":hashlib.sha256(original).hexdigest()}
    if canonical.count(fixed)==1 and old not in canonical:
        return source,{**identity,"status":"already-fixed-upstream","source_cache_modified":False}
    # Undefined _MSC_VER evaluates to zero under MinGW. The legacy fallback
    # then redefines wcscpy_s as two statements inside stralign.h expressions.
    # Restrict this exact one-line upstream backport to the reviewed header;
    # a changed dependency must be inspected instead of patched heuristically.
    require(hashlib.sha256(canonical).hexdigest()=="bb1913bed7c1d6ab7cd3fcc12e3d53fa679d6ffca7425186da5c742b1d82e433",
            "Unreviewed oneVPL MinGW compatibility header; inspect the upstream guard fix before building")
    require(canonical.count(old)==1 and not output.exists(),"oneVPL build copy must be new and unambiguous")
    patched=original.replace(old,fixed,1)
    require(hashlib.sha256(patched.replace(b"\r\n",b"\n")).hexdigest()=="f49130c9a394c16395788c5133fab49e2f2d95973460aa41d1208f1146fe475b",
            "oneVPL compatibility backport differs from Intel's reviewed source")
    shutil.copytree(source,output,ignore=shutil.ignore_patterns(".git"))
    (output/relative).write_bytes(patched)
    return output,{**identity,"status":"upstream-backport-in-build-copy",
                   "patched_header_sha256":hashlib.sha256(patched).hexdigest(),
                   "source_cache_modified":False}


def prepare_windows_gpu(plan: dict, target: str, cache: Path, prefix: Path,
                        jobs: int, msys: Path, env: dict) -> dict:
    """Compile optional device backends without requiring a GPU on the builder."""
    logs=cache/"logs"/target;root=cache/"build"/target
    nv=source_checkout("nvcodec",plan["sources"]["nvcodec"],cache)
    codec_run(["make",f"PREFIX={msys_path(prefix)}","LIBDIR=lib","install"],nv,env,msys,logs/"gpu-nvcodec-install.log")
    nv_version=codec_run(["pkg-config","--modversion","ffnvcodec"],root,env,msys,timeout=30)
    nv_package=verify_nvcodec_package(nv,prefix,nv_version)
    amf=source_checkout("amf",plan["sources"]["amf"],cache)
    headers=amf/"amf/public/include"
    require((headers/"core/Version.h").is_file(),"Official AMF source does not contain its public headers")
    destination=prefix/"include/AMF"
    require(not destination.exists(),"Private AMF header destination already exists")
    shutil.copytree(headers,destination)
    text=(destination/"core/Version.h").read_text()
    components=[re.search(r"#define\s+AMF_VERSION_"+field+r"\s+([0-9]+)",text) for field in ("MAJOR","MINOR","RELEASE")]
    require(all(components),"Cannot identify the official AMF header version")
    amf_version=".".join(m[1] for m in components)
    require(amf_version==plan["sources"]["amf"]["tag"].removeprefix("v"),"AMF headers differ from the resolved official version")
    vpl=source_checkout("libvpl",plan["sources"]["libvpl"],cache)
    vpl,vpl_compatibility=prepare_libvpl_mingw_source(vpl,root/"libvpl-source")
    build=root/"libvpl"
    configure=["cmake","-S",msys_path(vpl),"-B",msys_path(build),"-G","Ninja","-DCMAKE_BUILD_TYPE=Release",
               f"-DCMAKE_INSTALL_PREFIX={msys_path(prefix)}","-DCMAKE_INSTALL_LIBDIR=lib","-DBUILD_SHARED_LIBS=ON",
               "-DBUILD_TESTS=OFF","-DBUILD_EXAMPLES=OFF","-DINSTALL_EXAMPLES=OFF","-DBUILD_EXPERIMENTAL=OFF"]
    for stage,args in (("configure",configure),("build",["cmake","--build",msys_path(build),"--parallel",str(jobs)]),
                       ("install",["cmake","--install",msys_path(build)])):
        codec_run(args,root,env,msys,logs/f"gpu-libvpl-{stage}.log")
    version=codec_run(["pkg-config","--modversion","vpl"],root,env,msys,timeout=30)
    require(version.split(".")[:2]==plan["sources"]["libvpl"]["tag"].removeprefix("v").split(".")[:2],
            "oneVPL dispatcher API differs from the resolved official source")
    require((prefix/"lib/pkgconfig/vpl.pc").is_file() and (prefix/"bin/libvpl.dll").is_file(),
            "oneVPL must produce its private pkg-config identity and runtime dispatcher DLL")
    return {"nvcodec":{"source":plan["sources"]["nvcodec"],**nv_package,"upstream_driver_requirements":(nv/"README").read_text()[:8000]},
            "amf":{"source":plan["sources"]["amf"],"version":amf_version,"runtime":"AMD display driver supplies amfrt64.dll"},
            "libvpl":{"source":plan["sources"]["libvpl"],"api_version":version,"dispatcher":"libvpl.dll is bundled",
                      "source_compatibility":vpl_compatibility,
                      "device_implementation":"Intel graphics driver supplies the hardware implementation selected by the dispatcher"},
            "device_execution_verified":False}


def prepare_ffmpeg(plan: dict, target: str, cache: Path, jobs: int, msys: Path | None) -> tuple[Path, dict, dict]:
    source = source_checkout("ffmpeg", plan["sources"]["ffmpeg"], cache)
    build = cache / "build" / target / "ffmpeg"; build.mkdir(parents=True, exist_ok=True)
    prefix = cache / "build" / target / "ffmpeg-prefix"
    logs = cache / "logs" / target
    common = ["--enable-gpl", "--enable-version3", "--enable-shared", "--disable-static", "--disable-debug", "--disable-doc",
              "--disable-autodetect", "--enable-libx264", "--enable-libopus", "--enable-libsvtav1", "--enable-libdav1d",
              "--enable-sdl2", "--enable-ffplay"]
    if "windows" in target:
        require(msys and (msys / "usr/bin/bash.exe").is_file(), "Pass --msys2-root with an initialized UCRT64 toolchain")
        bash = msys / "usr/bin/bash.exe"
        env = {"MSYSTEM":"UCRT64","CHERE_INVOKING":"1","MSYS2_PATH_TYPE":"minimal"}
        package_text = run([bash,"--login","-c","pacman -Q"], env=env)
        required = ["mingw-w64-ucrt-x86_64-" + p for p in ("gcc","pkgconf","cmake","ninja","meson","nasm")]
        present = {line.split()[0]:line.split()[1] for line in package_text.splitlines() if len(line.split())==2}
        require(all(name in present for name in required), "MSYS2 UCRT64 FFmpeg build dependencies are missing")
        codecs, codec_info, codec_env = prepare_codecs(plan, target, cache, jobs, msys)
        env.update(codec_env)
        gpu_info=prepare_windows_gpu(plan,target,cache,codecs,jobs,msys,env)
        configure = [msys_path(source / "configure"),f"--prefix={msys_path(prefix)}",*common,"--target-os=mingw32","--arch=x86_64",
                     "--enable-ffnvcodec","--enable-nvenc","--enable-amf","--enable-libvpl","--enable-d3d11va","--enable-dxva2",
                     f"--extra-cflags=-I{msys_path(codecs/'include')}"]
        for label, args in (("configure",configure),("build",["make","-r",f"-j{jobs}"]),("install",["make","-r","install"])):
            script = "export PATH=/ucrt64/bin:/usr/bin; cd " + shlex.quote(msys_path(build)) + "; " + shlex.join(args)
            run([bash,"--login","-c",script],env=env,timeout=3600,log=logs / f"ffmpeg-{label}.log")
        dependencies={"provider":"official codec source builds; MSYS2 signed UCRT64 compiler packages","packages":{k:present[k] for k in required},
                      "source":"https://www.msys2.org/","ffmpeg_build_reference":"https://ffmpeg.org/platform.html","gpu":gpu_info}
        execution_env={"PATH":str(msys / "ucrt64/bin")+os.pathsep+str(prefix / "bin")+os.pathsep+os.environ.get("PATH","")}
    else:
        brew = run(["brew","--prefix"])
        dependencies=json.loads(run(["brew","info","--json=v2","--installed"],timeout=120))
        wanted={"cmake","ninja","meson","nasm","pkgconf"}
        formulae={x["name"]:x for x in dependencies["formulae"]}
        require(wanted.issubset(formulae), "Install brew cmake ninja meson nasm pkgconf before preparing the runtime")
        dependencies={"provider":"official codec source builds; Homebrew build tools","formulae":[formulae[n] for n in sorted(wanted)],
                      "source":"https://github.com/Homebrew/homebrew-core"}
        codecs, codec_info, env = prepare_codecs(plan, target, cache, jobs, None)
        run(["sh",source / "configure",f"--prefix={prefix}",*common,"--enable-videotoolbox","--enable-audiotoolbox"],cwd=build,env=env,timeout=900,log=logs/"ffmpeg-configure.log")
        run(["make",f"-j{jobs}"],cwd=build,env=env,timeout=3600,log=logs/"ffmpeg-build.log")
        run(["make","install"],cwd=build,env=env,timeout=300,log=logs/"ffmpeg-install.log")
        execution_env={}
    dependencies.update(codecs=codec_info,codec_prefix=str(codecs))
    return prefix,dependencies,execution_env


def prepare_datachannel(plan: dict, target: str, cache: Path, jobs: int) -> tuple[Path, dict]:
    source = source_checkout("datachannel",plan["sources"]["datachannel"],cache)
    build = cache / "build" / target; logs=cache / "logs" / target
    prefix=build / "datachannel-prefix"
    options=["-DBUILD_SHARED_LIBS=ON","-DBUILD_SHARED_DEPS_LIBS=OFF","-DNO_MEDIA=OFF","-DNO_TESTS=ON","-DNO_EXAMPLES=ON","-DNO_WEBSOCKET=ON","-DOPENSSL_USE_STATIC_LIBS=TRUE","-DCMAKE_POLICY_DEFAULT_CMP0091=NEW","-DCMAKE_POLICY_VERSION_MINIMUM=3.5"]
    crypto = source_checkout("openssl",plan["sources"]["openssl"],cache)
    # OpenSSL 4.1's mkerr.pl replaces the first /.ec/ in its config path.
    # A sibling build exposes the checkout SHA (which can contain "fec") to
    # that substitution. Keep this out-of-tree build below the verified
    # source so Configure's relative source path is always ../.. on both
    # Windows and macOS, regardless of cache or commit directory names.
    crypto_build=crypto/"_bilibilive-build"/target;crypto_build.mkdir(parents=True,exist_ok=True)
    crypto_prefix=build/"openssl-prefix"
    architecture="VC-WIN64A" if "windows" in target else "darwin64-"+TARGETS[target][1]+"-cc"
    env={"MACOSX_DEPLOYMENT_TARGET":"13.0"} if "darwin" in target else {}
    run(["perl",crypto/"Configure",architecture,"no-shared","no-tests",f"--prefix={crypto_prefix}"],cwd=crypto_build,env=env,timeout=300,log=logs/"openssl-configure.log")
    make="nmake" if "windows" in target else "make"
    run([make,*([] if make=="nmake" else [f"-j{jobs}"])],cwd=crypto_build,env=env,timeout=3600,log=logs/"openssl-build.log")
    run([make,"install_sw"],cwd=crypto_build,env=env,timeout=300,log=logs/"openssl-install.log")
    # Generated build files are untracked; the official source must remain
    # at the same complete commit with every tracked byte unchanged.
    source_checkout("openssl",plan["sources"]["openssl"],cache)
    options += [f"-DOPENSSL_ROOT_DIR={crypto_prefix}","-DCMAKE_POSITION_INDEPENDENT_CODE=ON"]
    if "windows" in target:
        options += ["-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded"]
    else:
        options += [f"-DCMAKE_OSX_ARCHITECTURES={TARGETS[target][1]}","-DCMAKE_OSX_DEPLOYMENT_TARGET=13.0"]
    cmake_build(source,build/"datachannel",prefix,options,logs,"datachannel",jobs,env=env)
    return prefix,{"source":plan["sources"]["datachannel"],"openssl":plan["sources"]["openssl"],
                   "openssl_build":{"strategy":"nested-out-of-tree","directory":str(crypto_build),
                                    "source_relative":os.path.relpath(crypto,crypto_build)},
                   "media":True,"static_dependencies":True}


def inspect_binary(path: Path, target: str) -> dict:
    data=path.read_bytes()
    expected=TARGETS[target][1]
    if data[:4]==b"\x7fELF":
        require(len(data)>=64 and data[4]==2 and data[5]==1,"Expected a little-endian ELF64 file")
        machine=struct.unpack_from("<H",data,18)[0];require(machine==62 and "linux" in target,"ELF architecture mismatch")
        return {"format":"ELF64","machine":machine,"architecture":expected,"sha256":hashlib.sha256(data).hexdigest()}
    if data[:2]==b"MZ":
        require(len(data)>=64,"Truncated PE header")
        at=struct.unpack_from("<I",data,0x3c)[0]
        require(at+26<=len(data) and data[at:at+4]==b"PE\0\0","Invalid PE header")
        machine=struct.unpack_from("<H",data,at+4)[0]
        require(machine==0x8664 and "windows" in target,"PE architecture mismatch")
        return {"format":"PE","machine":machine,"architecture":expected,"sha256":hashlib.sha256(data).hexdigest()}
    if data[:4]==b"\xcf\xfa\xed\xfe":
        require(len(data)>=32,"Truncated Mach-O header")
        cpu=struct.unpack_from("<I",data,4)[0]
        require(cpu==({"x86_64":0x01000007,"arm64":0x0100000c}.get(expected)) and "darwin" in target,"Mach-O architecture mismatch")
        return {"format":"Mach-O64","machine":cpu,"architecture":expected,"sha256":hashlib.sha256(data).hexdigest()}
    raise RuntimeError(f"Expected a native executable for {target}: {path}")


def copy_regular(source: Path, output: Path) -> None:
    source=source.resolve(strict=True)
    require(source.is_file(),f"Expected a native regular file: {source}")
    output.parent.mkdir(parents=True,exist_ok=True)
    if output.exists():
        require(sha(output)==sha(source),f"Different dependency files have the same basename: {output.name}")
        return
    shutil.copy2(source,output)


def windows_dependencies(entries: list[Path], output: Path, prefixes: list[Path], msys: Path) -> list[dict]:
    objdump=msys/"ucrt64/bin/objdump.exe"
    require(objdump.is_file(),"UCRT64 objdump is needed to verify DLL imports")
    available={}
    search=[*(p/"bin" for p in prefixes),*(p/"lib" for p in prefixes)]
    for directory in search:
        if directory.is_dir():
            for path in directory.glob("*.dll"):
                old=available.get(path.name.lower())
                require(old is None or sha(old)==sha(path),f"Ambiguous runtime DLL: {path.name}")
                available[path.name.lower()]=path
    # Private source builds have priority over optional older packages in the
    # build toolchain. Only unresolved compiler/runtime dependencies use MSYS2.
    for path in (msys/"ucrt64/bin").glob("*.dll"):
        available.setdefault(path.name.lower(),path)
    queue=list(entries);seen=set();result=[]
    system=Path(os.environ.get("SystemRoot","C:/Windows"))/"System32"
    while queue:
        binary=queue.pop(0)
        if binary in seen:continue
        seen.add(binary)
        text=run([objdump,"-p",binary]);imports=re.findall(r"DLL Name:\s*(\S+)",text)
        require(imports,f"Cannot inspect PE imports: {binary}")
        for name in imports:
            key=name.lower()
            vc_runtime = key.startswith(("vcruntime", "msvcp", "concrt"))
            if key.startswith(("api-ms-win-","ext-ms-win-")) or ((system/name).is_file() and not vc_runtime):
                result.append({"binary":binary.relative_to(output).as_posix(),"dependency":name,"source":"Windows system"});continue
            origin=available.get(key)
            require(origin is not None,f"Unresolved non-system runtime DLL: {binary.name} -> {name}")
            destination=output/"runtime/lib"/name
            copy_regular(origin,destination);queue.append(destination)
            result.append({"binary":binary.relative_to(output).as_posix(),"dependency":name,"bundled":destination.relative_to(output).as_posix()})
    return result


def mac_dependencies(entries: list[tuple[Path,Path]], output: Path, prefixes: list[Path]) -> list[dict]:
    queue=list(entries);seen=set();result=[]
    originals={destination:sha(source) for source,destination in entries}
    while queue:
        source,destination=queue.pop(0)
        if destination in seen:continue
        seen.add(destination)
        text=run(["otool","-L",source])
        dependencies=[line.strip().split(" (",1)[0] for line in text.splitlines()[1:]]
        for dependency in dependencies:
            if dependency.startswith(("/usr/lib/","/System/Library/")):
                result.append({"binary":destination.relative_to(output).as_posix(),"dependency":dependency,"source":"macOS system"});continue
            # A dylib lists its own install name as the first dependency.
            if destination.suffix==".dylib" and Path(dependency).name==destination.name:
                continue
            if dependency.startswith("@loader_path/"):
                origin=source.parent/dependency.removeprefix("@loader_path/")
            elif dependency.startswith("@rpath/"):
                suffix=dependency.removeprefix("@rpath/")
                matches=[p/suffix for p in [source.parent,*(p/"lib" for p in prefixes)] if (p/suffix).is_file()]
                require(matches,f"Unresolved Mach-O rpath: {source} -> {dependency}")
                origin=matches[0]
            else:
                origin=Path(dependency)
            require(origin.is_absolute() and origin.is_file(),f"Unresolved Mach-O dependency: {source} -> {dependency}")
            require(origin.suffix==".dylib",f"A non-system framework needs an explicit bundling implementation: {dependency}")
            bundled=output/"runtime/lib"/origin.name
            original_digest=sha(origin)
            if bundled in originals:
                require(originals[bundled]==original_digest,f"Different Mach-O dependencies share a basename: {bundled.name}")
            else:
                copy_regular(origin,bundled); originals[bundled]=original_digest; queue.append((origin.resolve(),bundled))
            relative="@loader_path/"+("../lib/" if destination.parent.name=="bin" else "")+bundled.name
            run(["install_name_tool","-change",dependency,relative,destination])
            result.append({"binary":destination.relative_to(output).as_posix(),"dependency":dependency,"bundled":bundled.relative_to(output).as_posix(),"relocated":relative})
        if destination.suffix==".dylib":run(["install_name_tool","-id","@rpath/"+destination.name,destination])
    return result


def licenses(plan: dict, cache: Path, output: Path, prefixes: list[Path], msys: Path | None) -> None:
    directory=output/"runtime/licenses";directory.mkdir(parents=True,exist_ok=True)
    for label,identity in plan["sources"].items():
        source=cache/"sources"/f"{label}-{identity['commit'][:16]}"
        if not source.is_dir():continue
        candidates=[p for p in source.iterdir() if p.is_file() and re.match(r"(?i)^(license|licence|copying|notice|third-party)",p.name)]
        if label=="nvcodec" and not candidates:
            # NVIDIA distributes the license notices in the header source
            # itself. Preserve those complete upstream files unmodified.
            candidates=list((source/"include/ffnvcodec").glob("*.h"))
        require(candidates,f"No upstream license text found for {label}")
        for path in candidates:copy_regular(path,directory/label/path.name)
        if label=="datachannel":
            for dependency in (source/"deps").iterdir():
                if not dependency.is_dir():continue
                for path in dependency.iterdir():
                    if path.is_file() and re.match(r"(?i)^(license|licence|copying|notice)",path.name):copy_regular(path,directory/label/dependency.name/path.name)
    if msys:
        source=msys/"ucrt64/share/licenses"
        require(source.is_dir(),"MSYS2 runtime package license directory is missing")
        # The signed package license set is retained, including transitive DLLs.
        for path in source.rglob("*"):
            if path.is_file():copy_regular(path,directory/"msys2"/path.relative_to(source))
    else:
        for prefix in prefixes:
            cellar=(prefix/"opt")
            if not cellar.is_dir():continue
            for formula in cellar.iterdir():
                real=formula.resolve()
                if not real.is_dir():continue
                for path in real.iterdir():
                    if path.is_file() and re.match(r"(?i)^(license|licence|copying|notice)",path.name):copy_regular(path,directory/"homebrew"/formula.name/path.name)
                for sub in (real/"share/licenses",real/"share/doc"):
                    if sub.is_dir():
                        for path in sub.rglob("*"):
                            if path.is_file() and re.match(r"(?i)^(license|licence|copying|notice)",path.name):copy_regular(path,directory/"homebrew"/formula.name/path.relative_to(real))
    write_json(directory/"sources.json",plan)


def ffplay_smoke(ffplay: Path, name: str, options: list, env: dict, log: Path) -> None:
    # FFplay 9.1 writes status-line clear escapes directly to stderr even with
    # -loglevel error. Disable that status output, not actual error reporting.
    run([ffplay,"-hide_banner","-loglevel","error","-nostats","-autoexit",*options],
        env=env,timeout=15,log=log)
    diagnostic=log.read_text(errors="replace").strip()
    require(not diagnostic,f"FFplay reported an error while previewing {name}; inspect {log}\n{diagnostic[-4000:]}")


def media_smoke(ffmpeg: Path, ffplay: Path, work: Path, env: dict) -> dict:
    """Exercise software encoding, decoding and silent SDL playback in <= 30s per child."""
    import math
    work.mkdir(parents=True, exist_ok=False)
    width=height=128;frames=8;fps=25;samples=48000*frames//fps
    video=work/"sample.nv12";audio=work/"sample.s16le"
    video.write_bytes(b"".join(bytes(32+(x+y+frame*7)%160 for y in range(height) for x in range(width))
                               +bytes([128])*(width*height//2) for frame in range(frames)))
    audio.write_bytes(b"".join(struct.pack("<hh",*(int(300*math.sin(2*math.pi*440*i/48000)),)*2)
                               for i in range(samples)))
    common=[ffmpeg,"-hide_banner","-loglevel","error","-nostdin","-y"]
    input_video=["-f","rawvideo","-pixel_format","nv12","-video_size","128x128","-framerate",str(fps),"-i",video,"-an"]
    settings={
        "h264":["-c:v","libx264","-preset","ultrafast","-tune","zerolatency","-profile:v","baseline",
                 "-level:v","3.1","-x264-params","ref=1:bframes=0:repeat-headers=1:scenecut=0:vbv-maxrate=300:vbv-bufsize=300:nal-hrd=cbr"],
        "av1":["-c:v","libsvtav1","-preset","12","-level:v","31","-svtav1-params",
                "rc=2:rtc=1:pred-struct=1:lookahead=0:irefresh-type=2:aq-mode=2:lp=2"],
    }
    checks=[]
    for codec, options in settings.items():
        encoded=work/(codec+".mkv");decoded=work/(codec+".yuv")
        run([*common,*input_video,"-vf","format=yuv420p",*options,"-b:v","300k","-bufsize","300k","-g","50","-bf","0",encoded],
            env=env,timeout=30,log=work/(codec+"-encode.log"))
        require(encoded.is_file() and encoded.stat().st_size>0,f"{codec} encoder produced no output")
        decoder="h264" if codec=="h264" else "libdav1d"
        run([*common,"-c:v",decoder,"-i",encoded,"-an","-pix_fmt","yuv420p","-f","rawvideo",decoded],
            env=env,timeout=30,log=work/(codec+"-decode.log"))
        require(decoded.stat().st_size==frames*width*height*3//2,f"{codec} CPU decoder did not produce all expected frames")
        checks.append({"codec":codec,"encoder":options[1],"decoder":decoder,"encoded_size":encoded.stat().st_size,
                       "encoded_sha256":sha(encoded),"decoded_frames":frames,"width":width,"height":height,"status":"passed"})
    encoded=work/"opus.ogg";decoded=work/"opus.s16le"
    run([*common,"-f","s16le","-ar","48000","-ac","2","-i",audio,"-vn","-c:a","libopus",
         "-application","lowdelay","-frame_duration","20","-b:a","128k",encoded],env=env,timeout=30,log=work/"opus-encode.log")
    run([*common,"-c:a","opus","-i",encoded,"-ar","48000","-ac","2","-f","s16le",decoded],
        env=env,timeout=30,log=work/"opus-decode.log")
    require(encoded.stat().st_size>0 and decoded.stat().st_size==samples*2*2,"Opus decoder did not preserve the expected sample count")
    checks.append({"codec":"opus","encoder":"libopus","decoder":"opus","encoded_sha256":sha(encoded),
                   "decoded_samples_per_channel":samples,"sample_rate":48000,"channels":2,"status":"passed"})
    silent=dict(env,SDL_VIDEODRIVER="dummy",SDL_AUDIODRIVER="dummy",SDL_RENDER_DRIVER="software")
    previews={"local-nv12":["-f","rawvideo","-pixel_format","nv12","-video_size","128x128","-framerate",str(fps),"-an",video],
              "local-pcm":["-nodisp","-f","s16le","-ar","48000","-ch_layout","stereo","-volume","0",audio],
              "h264":["-an",work/"h264.mkv"],"av1":["-an",work/"av1.mkv"]}
    for name, options in previews.items():
        log=work/("ffplay-"+name+".log")
        ffplay_smoke(ffplay,name,options,silent,log)
        checks.append({"preview":name,"video_driver":"dummy","audio_driver":"dummy","status":"passed"})
    receipt={"status":"passed","checks":checks,"scope":"synthetic local media only; no OBS/account/network/physical audio device",
             "hardware_encoders":"not validated; availability requires the compiled encoder plus a supported GPU and driver"}
    write_json(work/"verification.json",receipt)
    return receipt


def rtc_load_smoke(library: Path, env: dict) -> dict:
    symbols=["rtcInitLogger","rtcCreatePeerConnection","rtcCreateDataChannelEx","rtcDeletePeerConnection",
             "rtcDelete","rtcIsOpen","rtcGetLocalDescription","rtcGetRemoteDescription","rtcSetLocalDescription",
             "rtcSetRemoteDescription","rtcAddTrack","rtcSendMessage","rtcReceiveMessage","rtcGetAvailableAmount",
             "rtcChainRtcpReceivingSession","rtcRequestKeyframe","rtcSetUserPointer","rtcSetDataChannelCallback","rtcSetTrackCallback"]
    program=("import ctypes,json,os,pathlib,sys; p=pathlib.Path(sys.argv[1]); "
             "cookie=os.add_dll_directory(str(p.parent)) if os.name=='nt' else None; "
             "dll=ctypes.CDLL(str(p)); names=json.loads(sys.argv[2]); "
             "[getattr(dll,n) for n in names]; print(json.dumps({'loaded':True,'exports':names})); "
             "cookie.close() if cookie else None")
    value=json.loads(run([sys.executable,"-B","-c",program,library,json.dumps(symbols)],env=env,timeout=20))
    require(value=={"loaded":True,"exports":symbols},"Native RTC runtime export check failed")
    return value


def media_capabilities(encoders: str, decoders: str, target: str) -> dict:
    def names(text):
        return set(re.findall(r"^\s*[A-Z.]{6}\s+([A-Za-z0-9_-]+)\s", text, re.M))
    encoded,decoded=names(encoders),names(decoders)
    require({"libx264","libsvtav1","libopus"}<=encoded,"FFmpeg is missing a required H.264/AV1/Opus software encoder")
    require({"h264","libdav1d","opus"}<=decoded,"FFmpeg is missing a required H.264/AV1/Opus software decoder")
    hardware=("h264_nvenc","av1_nvenc","h264_qsv","av1_qsv","h264_amf","av1_amf","h264_vaapi","h264_videotoolbox")
    requested=set(hardware[:6]) if "windows" in target else ({"h264_videotoolbox"} if "darwin" in target else set())
    require(requested<=encoded,"FFmpeg is missing a requested compiled hardware encoder: "+", ".join(sorted(requested-encoded)))
    return {"software_encoders":["libx264","libsvtav1","libopus"],"software_decoders":["h264","libdav1d","opus"],
            "hardware_encoders":{name:{"compiled":name in encoded,"device_execution_verified":False} for name in hardware},
            "build_policy":{"autodetect":False,"gpu_sdk_sources":["nvcodec","amf","libvpl"] if "windows" in target else [],
                            "video_toolbox_requested":"darwin" in target,"compiled_encoder_gate":sorted(requested),
                            "driver_runtime":"NVIDIA/AMD/Intel display drivers are external device dependencies; only the source-built oneVPL dispatcher is bundled. Each GPU requires a successful device probe before use."}}


def prepare_runtime(plan: dict, target: str, cache: Path, output: Path, jobs: int, msys: Path | None) -> tuple[dict,dict]:
    if "linux" in target:
        return {"mode":"system","executables":{},"libraries":{}},{"delivery":"distribution-managed","required":["ffmpeg including ffplay,libx264,libsvtav1,libopus","libdatachannel 0.24"]}
    ffmpeg,dependency_info,env=prepare_ffmpeg(plan,target,cache,jobs,msys)
    codecs=Path(dependency_info["codec_prefix"])
    datachannel,rtc_info=prepare_datachannel(plan,target,cache,jobs)
    extension=".exe" if "windows" in target else ""
    executables={};entries=[];originals=[]
    for name in ("ffmpeg","ffplay"):
        source=ffmpeg/"bin"/(name+extension);require(source.is_file(),f"Required executable was not built: {name}")
        destination=output/"runtime/bin"/source.name;copy_regular(source,destination)
        executables[name]=destination.relative_to(output).as_posix();entries.append(destination);originals.append((source,destination))
    if "windows" in target:
        matches=list((datachannel/"bin").glob("*datachannel*.dll"))
    else:
        matches=list((datachannel/"lib").glob("libdatachannel.0.dylib"))
    require(len(matches)==1,"Exactly one libdatachannel 0.24 runtime image must be produced")
    library=output/"runtime/lib"/matches[0].name;copy_regular(matches[0],library);entries.append(library);originals.append((matches[0].resolve(),library))
    if "windows" in target:
        require(msys is not None,"MSYS2 root is required")
        dependencies=windows_dependencies(entries,output,[ffmpeg,datachannel,codecs],msys)
        env={"PATH":str(output/"runtime/lib")+os.pathsep+str(output/"runtime/bin")+os.pathsep+os.environ.get("PATH","")}
        license_prefixes=[]
    else:
        brew=Path(run(["brew","--prefix"]))
        dependencies=mac_dependencies(originals,output,[ffmpeg,datachannel,codecs,brew])
        env={};license_prefixes=[brew]
        # Sign nested Mach-O code now, before hashing the manifest. A later
        # installer must preserve these bytes while signing the outer app.
        identity=os.environ.get("BILIBILIVE_MACOS_SIGN_IDENTITY","-")
        for path in sorted((output/"runtime").rglob("*")):
            if path.is_file() and (path.suffix==".dylib" or path.parent.name=="bin"):
                run(["codesign","--force","--sign",identity,path],timeout=120)
    version=run([output/executables["ffmpeg"],"-version"],env=env)
    expected=plan["sources"]["ffmpeg"]["tag"].removeprefix("n")
    require(re.search(r"ffmpeg version n?"+re.escape(expected)+r"(?:[\s.-]|$)",version),"Built FFmpeg version differs from the resolved official release")
    encoders=run([output/executables["ffmpeg"],"-hide_banner","-encoders"],env=env)
    decoders=run([output/executables["ffmpeg"],"-hide_banner","-decoders"],env=env)
    capabilities=media_capabilities(encoders,decoders,target)
    play=run([output/executables["ffplay"],"-version"],env=env)
    smoke=media_smoke(output/executables["ffmpeg"],output/executables["ffplay"],cache/"validation"/target/"software-media",env)
    rtc=rtc_load_smoke(library,env)
    binaries={str(path.relative_to(output)):inspect_binary(path,target) for path in (output/"runtime").rglob("*") if path.is_file() and (path.suffix in (".dll",".dylib") or path.parent.name=="bin")}
    licenses(plan,cache,output,license_prefixes,msys)
    return {"mode":"bundled","executables":executables,"libraries":{"datachannel":library.relative_to(output).as_posix()}}, {
        "ffmpeg_version":version.splitlines()[0],"ffplay_version":play.splitlines()[0],"ffmpeg_encoders":encoders,"ffmpeg_decoders":decoders,
        "software_media_smoke":smoke,"rtc_export_smoke":rtc,"capabilities":capabilities,
        "build_dependencies":dependency_info,"datachannel":rtc_info,"dependency_closure":dependencies,"binaries":binaries}


def inventory(output: Path) -> dict:
    result={};total=0;folded=set()
    for path in sorted(output.rglob("*")):
        require(not path.is_symlink(),f"The final payload must contain regular files, not links: {path}")
        if path.is_dir():continue
        require(path.is_file(),f"Special file in native payload: {path}")
        name=path.relative_to(output).as_posix();size=path.stat().st_size;total+=size
        require(name.lower() not in folded,"Native payload has case-colliding paths");folded.add(name.lower())
        require(size<=512*1024**2 and total<=2*1024**3 and len(result)<4096,"Native payload exceeds application validation bounds")
        result[name]={"size":size,"sha256":sha(path),"executable":bool(path.stat().st_mode & 0o111) or (path.parent.name=="bin" and path.suffix==".exe")}
    return result


def main(argv=None):
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--target",choices=TARGETS,required=True)
    parser.add_argument("--cache",type=Path,required=True)
    parser.add_argument("--msys2-root",type=Path)
    parser.add_argument("--jobs",type=int,default=max(1,min(os.cpu_count() or 2,8)))
    parser.add_argument("--plan",action="store_true",help="Resolve public latest stable source identities; do not build")
    args=parser.parse_args(argv)
    output=args.output.absolute();cache=args.cache.absolute()
    require(output==output.resolve() and cache==cache.resolve(),"Native output/cache must not contain symlink components")
    for path in (output,cache):
        require(not path.is_relative_to(ROOT) or path.is_relative_to(ROOT/"dist"),"Native downloads/builds must be outside packaged source scope (or under dist)")
    require(not output.exists(),"Native output already exists; preserve previous evidence and use a new destination")
    require(1<=args.jobs<=64,"Native build jobs must be in 1..64")
    cache.mkdir(parents=True,exist_ok=True)
    receipt=cache/f"preparation-{args.target}-{int(time.time())}.json"
    evidence={"status":"preparing","target":args.target,"output":str(output),"commands":COMMANDS}
    temporary=None
    try:
        plan=resolve();evidence["source_plan"]=plan;write_json(receipt,evidence)
        if args.plan:
            write_json(output,plan);evidence["status"]="sources-resolved";write_json(receipt,evidence);return 0
        host=platform.machine().lower();host={"amd64":"x86_64","aarch64":"arm64"}.get(host,host)
        require((platform.system(),host)==TARGETS[args.target],"This builder requires a native runner matching the requested target")
        output.parent.mkdir(parents=True,exist_ok=True)
        temporary=Path(tempfile.mkdtemp(prefix=".bilibilive-payload-",dir=output.parent))
        plugin=prepare_plugin(plan,args.target,cache,temporary,args.jobs)
        runtime,media=prepare_runtime(plan,args.target,cache,temporary,args.jobs,args.msys2_root)
        if "darwin" in args.target:
            run(["codesign","--force","--sign",os.environ.get("BILIBILIVE_MACOS_SIGN_IDENTITY","-"),temporary/plugin["root"]],timeout=120)
            plugin["binary"].update(inspect_binary(temporary/plugin["root"]/plugin["entrypoint"],args.target))
        manifest={"schema":1,"package":"bilibilive","version":tomllib.loads((ROOT/"Cargo.toml").read_text())["package"]["version"],
                  "target":args.target,"plugin":{k:plugin[k] for k in ("root","entrypoint")},"runtime":runtime,
                  "files":inventory(temporary),"provenance":{"sources":plan,"plugin":plugin["build"],"plugin_binary":plugin["binary"],"media":media}}
        require(sum(name.startswith(plugin["root"]+"/") for name in manifest["files"])<=64,"OBS plugin payload has too many files")
        require(sum(v["size"] for k,v in manifest["files"].items() if k.startswith(plugin["root"]+"/"))<=128*1024**2,"OBS plugin payload exceeds its bound")
        write_json(temporary/"bundle-manifest.json",manifest)
        require((temporary/"bundle-manifest.json").stat().st_size<=1024**2,"Native manifest exceeds application bound")
        digest=sha(temporary/"bundle-manifest.json")
        temporary.rename(output);temporary=None
        evidence.update(status="prepared-and-verified",manifest_sha256=digest,version=manifest["version"],file_count=len(manifest["files"]),
                        source_scope_policy="Generated payload and cache are outside included source paths (or under dist)")
        write_json(receipt,evidence)
        print(json.dumps({"status":evidence["status"],"output":str(output),"target":args.target,"manifest_sha256":digest,
                          "build_environment":{"BILIBILIVE_BUNDLE_MANIFEST_SHA256":digest},"receipt":str(receipt)},indent=2))
        return 0
    except (Exception, KeyboardInterrupt) as error:
        evidence.update(status="failed",error=str(error),partial_output=str(temporary) if temporary else None)
        write_json(receipt,evidence)
        print(f"Native payload preparation failed: {error}\nReceipt: {receipt}",file=sys.stderr)
        return 1


if __name__=="__main__":
    raise SystemExit(main())
