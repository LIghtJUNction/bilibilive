"""Private fixtures for the native installer publication boundaries."""
import hashlib
import io
import json
from pathlib import Path
import struct
import tempfile
import unittest
import zipfile

import installer
import installer_icons


def pe():
    data = bytearray(512); data[:2] = b"MZ"; struct.pack_into("<I", data, 60, 128)
    data[128:132] = b"PE\0\0"; struct.pack_into("<H", data, 132, 0x8664)
    struct.pack_into("<H", data, 152, 0x20b)
    return bytes(data)


def macho(arm):
    return struct.pack("<IIIIIIII", 0xfeedfacf, 0x0100000c if arm else 0x01000007, 0, 2, 0, 0, 0, 0)


def archive(path, files):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as stream:
        for name, data in files.items(): stream.writestr(name, data)


def fixture(root, target):
    version = "1.2.3"
    platform, arch, channel = installer.TARGETS[target]
    executable = pe() if platform == "win" else macho(arch == "arm64")
    payload_files = {"resources/obs-plugin/obs-bilibilive-pk/entrypoint": b"private plugin fixture",
                     "runtime/bin/ffmpeg": b"private ffmpeg fixture", "runtime/bin/ffplay": b"private preview fixture",
                     "runtime/lib/datachannel": b"private RTC fixture"}
    manifest = {"schema": 1, "package": "bilibilive", "version": version, "target": target,
                "plugin": {"root": "resources/obs-plugin/obs-bilibilive-pk", "entrypoint": "entrypoint"},
                "runtime": {"mode": "bundled", "executables": {"ffmpeg": "runtime/bin/ffmpeg", "ffplay": "runtime/bin/ffplay"},
                            "libraries": {"datachannel": "runtime/lib/datachannel"}},
                "files": {name: {"size": len(data), "sha256": hashlib.sha256(data).hexdigest(), "executable": False}
                          for name, data in payload_files.items()}, "provenance": {"fixture": True}}
    manifest_bytes = json.dumps(manifest).encode()
    entry = "bilibilive.exe" if platform == "win" else "Contents/MacOS/bilibilive"
    prefix = "lib/app/" if platform == "win" else "lib/app/Contents/Resources/bilibilive/"
    spec = f"<package><metadata><id>bilibilive</id><version>{version}</version><channel>{channel}</channel><mainExe>{entry}</mainExe><os>{platform}</os><machineArchitecture>{arch}</machineArchitecture></metadata></package>"
    files = {"bilibilive.nuspec": spec.encode(), "lib/app/" + entry: executable,
             ("lib/app/Squirrel.exe" if platform == "win" else "lib/app/Contents/MacOS/UpdateMac"): executable}
    files.update({prefix + name: data for name, data in payload_files.items()})
    files[prefix + "bundle-manifest.json"] = manifest_bytes
    package = root / f"bilibilive-{version}-{channel}-full.nupkg"
    archive(package, files)
    asset = {"PackageId": "bilibilive", "Version": version, "Type": "Full", "FileName": package.name,
             "Size": package.stat().st_size, "SHA256": installer.digest(package)}
    feed = root / f"releases.{channel}.json"
    feed.write_text(json.dumps({"Assets": [asset]}))
    portable_prefix = "current/" if platform == "win" else "bilibilive.app/Contents/Resources/bilibilive/"
    portable_entry = "current/bilibilive.exe" if platform == "win" else "bilibilive.app/Contents/MacOS/bilibilive"
    portable_files = {portable_entry: executable,
                      ("Update.exe" if platform == "win" else "bilibilive.app/Contents/MacOS/UpdateMac"): executable}
    portable_files.update({portable_prefix + name: data for name, data in payload_files.items()})
    portable_files[portable_prefix + "bundle-manifest.json"] = manifest_bytes
    if platform == "win": portable_files[".portable"] = b""
    archive(root / f"bilibilive-{channel}-Portable.zip", portable_files)
    setup = b"xar!private fixture"
    if platform == "win":
        setup = bytearray(executable)
        struct.pack_into("<qq", setup, 256, len(executable), package.stat().st_size)
        setup[272:304] = installer.SETUP_BUNDLE_SIGNATURE
        setup += package.read_bytes()
    (root / f"bilibilive-{channel}-Setup.{'exe' if platform == 'win' else 'pkg'}").write_bytes(setup)
    return version, manifest, manifest_bytes, files, package, feed


def repackage(package, files, feed):
    archive(package, files)
    value = json.loads(feed.read_text()); value["Assets"][0]["Size"] = package.stat().st_size
    value["Assets"][0]["SHA256"] = installer.digest(package); feed.write_text(json.dumps(value))


class InstallerTests(unittest.TestCase):
    def test_three_channels_have_exact_payload_and_native_headers(self):
        for target in list(installer.TARGETS)[1:]:
            with self.subTest(target=target), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                version, manifest, contents, *_ = fixture(root, target)
                result = installer.validate_velopack(root, version, target, manifest, contents)
                self.assertEqual(result["payload_files_verified"], 4)
                self.assertEqual(result["channel"], installer.TARGETS[target][2])

    def test_rehashed_outer_archive_cannot_hide_payload_modification(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); target = "aarch64-apple-darwin"
            version, manifest, contents, files, package, feed = fixture(root, target)
            files["lib/app/Contents/Resources/bilibilive/runtime/bin/ffmpeg"] = b"changed after manifest/signing"
            repackage(package, files, feed)
            with self.assertRaisesRegex(ValueError, "changed native payload"):
                installer.validate_velopack(root, version, target, manifest, contents)

    def test_missing_and_unregistered_runtime_files_are_both_rejected(self):
        for mutation in ("missing", "extra"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); target = "x86_64-pc-windows-msvc"
                version, manifest, contents, files, package, feed = fixture(root, target)
                if mutation == "missing": del files["lib/app/runtime/bin/ffplay"]
                else: files["lib/app/runtime/bin/unregistered.exe"] = b"not registered"
                repackage(package, files, feed)
                with self.assertRaisesRegex(ValueError, "missing or unregistered"):
                    installer.validate_velopack(root, version, target, manifest, contents)

    def test_update_manifest_is_bound_to_entrypoint_and_platform(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); target = "x86_64-apple-darwin"
            version, manifest, contents, files, package, feed = fixture(root, target)
            files["bilibilive.nuspec"] = files["bilibilive.nuspec"].replace(b"osx-x64", b"osx-arm64")
            repackage(package, files, feed)
            with self.assertRaisesRegex(ValueError, "NuSpec identifies"):
                installer.validate_velopack(root, version, target, manifest, contents)

    def test_package_paths_and_case_collisions_are_rejected(self):
        for extra in ("../escape", "/absolute", "C:/escape", "lib/app/RUNTIME/bin/ffmpeg"):
            with self.subTest(path=extra), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); target = "x86_64-pc-windows-msvc"
                version, manifest, contents, files, package, feed = fixture(root, target)
                files[extra] = b"private adversarial fixture"; repackage(package, files, feed)
                with self.assertRaises(ValueError): installer.validate_velopack(root, version, target, manifest, contents)

    def test_feed_digest_and_size_are_checked_before_content(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); target = "x86_64-pc-windows-msvc"
            version, manifest, contents, _, _, feed = fixture(root, target)
            value = json.loads(feed.read_text()); value["Assets"][0]["SHA256"] = "0" * 64
            feed.write_text(json.dumps(value))
            with self.assertRaisesRegex(ValueError, "feed digest or size"):
                installer.validate_velopack(root, version, target, manifest, contents)

    def test_payload_rejects_any_unregistered_or_symbolic_link_input(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); target = "x86_64-pc-windows-msvc"
            version, manifest, contents, files, *_ = fixture(root, target)
            native = root / "payload"; native.mkdir()
            for name in manifest["files"]:
                path = native / name; path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(files["lib/app/" + name])
            (native / "bundle-manifest.json").write_bytes(contents)
            self.assertEqual(installer.payload(native, version, target), manifest)
            (native / "unregistered").write_bytes(b"extra")
            with self.assertRaisesRegex(ValueError, "unregistered"):
                installer.payload(native, version, target)
            (native / "unregistered").unlink()
            if sys_platform_has_symlinks():
                (native / "link").symlink_to("bundle-manifest.json")
                with self.assertRaisesRegex(ValueError, "symbolic link"):
                    installer.payload(native, version, target)

    def test_wrong_native_architecture_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "wrong architecture"):
            installer.native_header(macho(True), "x86_64-apple-darwin")
        with self.assertRaises(ValueError): installer.native_header(b"fake executable", "x86_64-pc-windows-msvc")

    def test_windows_installer_must_embed_the_verified_full_package(self):
        for change in ("payload", "extent"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); target = "x86_64-pc-windows-msvc"
                version, manifest, contents, _, package, _ = fixture(root, target)
                setup = root / "bilibilive-win-x64-Setup.exe"
                data = bytearray(setup.read_bytes())
                if change == "payload": data[-1] ^= 1
                else: struct.pack_into("<q", data, 264, package.stat().st_size + 1)
                setup.write_bytes(data)
                with self.assertRaisesRegex(ValueError, "installer embed"):
                    installer.validate_velopack(root, version, target, manifest, contents)

    def test_icon_export_keeps_project_svg_and_has_real_format_tables(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = installer_icons.create_icons(Path(__file__).parent / "bilibilive.svg", Path(temporary) / "icons")
            ico = (output / "bilibilive.ico").read_bytes()
            self.assertEqual(struct.unpack_from("<HHH", ico), (0, 1, 4))
            for i in range(4):
                _, _, _, _, planes, bits, size, offset = struct.unpack_from("<BBBBHHII", ico, 6 + i*16)
                self.assertEqual((planes, bits), (1, 32)); self.assertLessEqual(offset + size, len(ico))
                self.assertEqual(ico[offset:offset + 8], b"\x89PNG\r\n\x1a\n")
            icns = (output / "bilibilive.icns").read_bytes()
            self.assertEqual(icns[:4], b"icns"); self.assertEqual(struct.unpack_from(">I", icns, 4)[0], len(icns))
            self.assertEqual(installer_icons.path_polygon("M0 0h10v10h-10z")[0], (0.0, 0.0))


class ReleaseSourceTests(unittest.TestCase):
    def test_publication_rejects_stale_rerun_artifacts_and_old_diagnostics(self):
        import os
        import subprocess
        import sys
        import textwrap
        workflow = (Path(__file__).resolve().parent.parent / ".github/workflows/release.yml").read_text()
        selected = workflow.split("      - name: Verify the four complete asset sets before publication\n", 1)[1]
        script = textwrap.dedent(selected.split("        run: |\n", 1)[1].split("      - uses:", 1)[0])
        commit = "a" * 40
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); dist = root / "dist"; dist.mkdir()
            def write(name, data):
                (dist / name).write_bytes(data)
                checksum = hashlib.sha256(data).hexdigest()
                (dist / (name + ".sha256")).write_text(checksum + "  " + name + "\n", encoding="ascii")
                return {"name": name, "size": len(data), "sha256": checksum}
            indexes = []
            for target in installer.TARGETS:
                name = f"bilibilive-v1.2.3-{target}-assets.json"
                record = write(f"private-{target}.fixture", b"private release artifact, never executed")
                value = {"schema": 1, "version": "1.2.3", "source_commit": commit, "target": target,
                         "publish": True, "assets": [record]}
                write(name, json.dumps(value).encode()); indexes.append(name)
            env = dict(os.environ, RELEASE_TAG="v1.2.3", RELEASE_COMMIT=commit)
            def validate():
                return subprocess.run([sys.executable, "-c", script], cwd=root, env=env, capture_output=True)
            result = validate(); self.assertEqual(result.returncode, 0, result.stderr)
            (dist / "old-failed-attempt.log").write_text("private diagnostic")
            result = validate(); self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"Unexpected asset", result.stderr); (dist / "old-failed-attempt.log").unlink()
            original = (dist / indexes[0]).read_bytes()
            for changes in ({"source_commit": "b" * 40}, {"publish": False}, {"version": "1.2.2"}):
                with self.subTest(changes=changes):
                    value = json.loads(original); value.update(changes); write(indexes[0], json.dumps(value).encode())
                    result = validate(); self.assertNotEqual(result.returncode, 0)
                    self.assertIn(b"different source or validation run", result.stderr)
            write(indexes[0], original)
            data = json.loads(original)["assets"][0]
            write(data["name"], b"tampered artifact with a fresh sidecar")
            result = validate(); self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"differs from the successful native build", result.stderr)

    def test_preview_freezes_a_commit_and_formal_publication_requires_an_existing_matching_tag(self):
        import os
        import subprocess
        import sys
        import textwrap
        workflow = (Path(__file__).resolve().parent.parent / ".github/workflows/release.yml").read_text()
        selected = workflow.split("      - name: Freeze the exact source and validate its release identity\n", 1)[1]
        block = selected.split("        run: |\n", 1)[1].split("\n  native:", 1)[0]
        script = textwrap.dedent(block)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            def git(*args):
                return subprocess.run(["git", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", *args],
                                      cwd=root, check=True, capture_output=True, text=True).stdout.strip()
            git("init", "-q"); git("config", "user.name", "Private fixture"); git("config", "user.email", "fixture@example.invalid")
            (root / "Cargo.toml").write_text('[package]\nname="fixture"\nversion="1.2.3"\n')
            git("add", "Cargo.toml"); git("commit", "-qm", "private release identity fixture")
            commit = git("rev-parse", "HEAD")
            receipt = root / "outputs"
            env = dict(os.environ, GITHUB_OUTPUT=str(receipt), PUBLISH="false", REQUESTED_TAG="nonexistent-tag")
            completed = subprocess.run([sys.executable, "-c", script], cwd=root, env=env, capture_output=True)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            values = dict(line.split("=", 1) for line in receipt.read_text().splitlines())
            self.assertEqual(values, {"release_tag": "v1.2.3", "source_sha": commit, "tag_object": "", "publish": "false"})
            self.assertEqual(git("tag", "--list"), "")
            receipt.unlink(); env.update(PUBLISH="true", REQUESTED_TAG="v1.2.3")
            failed = subprocess.run([sys.executable, "-c", script], cwd=root, env=env, capture_output=True)
            self.assertNotEqual(failed.returncode, 0); self.assertFalse(receipt.exists())
            git("tag", "-a", "v1.2.3", "-m", "private immutable release fixture")
            tag_object = git("rev-parse", "refs/tags/v1.2.3")
            passed = subprocess.run([sys.executable, "-c", script], cwd=root, env=env, capture_output=True)
            self.assertEqual(passed.returncode, 0, passed.stderr)
            values = dict(line.split("=", 1) for line in receipt.read_text().splitlines())
            self.assertEqual(values["publish"], "true"); self.assertEqual(values["tag_object"], tag_object)
            self.assertEqual(git("rev-parse", "HEAD"), commit)
            self.assertEqual(git("rev-parse", "refs/tags/v1.2.3"), tag_object)


def sys_platform_has_symlinks():
    import os
    return os.name != "nt"


if __name__ == "__main__": unittest.main()
