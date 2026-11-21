#!/usr/bin/env python3
"""Local fixtures only. These tests never build, install, spawn or apply an update."""

import contextlib
import hashlib
import io
import json
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import installer_update_probe as probe


class ProbeFixtures(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="velopack-probe-fixture-")
        self.root = Path(self.temporary.name).resolve()

    def tearDown(self):
        self.temporary.cleanup()

    def archive(self, files):
        data = io.BytesIO()
        with zipfile.ZipFile(data, "w") as archive:
            for name, body in files:
                archive.writestr(name, body)
        data.seek(0)
        return zipfile.ZipFile(data)

    def release(self):
        package_id, version, channel = "private-probe", "0.0.2", "win-x64"
        executable = b"fixture executable; never run"
        name = f"{package_id}-{version}-{channel}-full.nupkg"
        nuspec = (f"<package><metadata><id>{package_id}</id><version>{version}</version>"
                  f"<channel>{channel}</channel><os>win</os><machineArchitecture>x64</machineArchitecture>"
                  f"<mainExe>{probe.PROGRAM}.exe</mainExe></metadata></package>")
        package = self.root / name
        with zipfile.ZipFile(package, "w") as archive:
            archive.writestr(f"{package_id}.nuspec", nuspec)
            archive.writestr(f"lib/app/{probe.PROGRAM}.exe", executable)
        with zipfile.ZipFile(self.root / f"{package_id}-{channel}-Portable.zip", "w") as archive:
            archive.writestr(f"current/{probe.PROGRAM}.exe", executable)
            archive.writestr("current/sq.version", nuspec)
            archive.writestr("Update.exe", "fixture helper; never run")
            archive.writestr(".portable", "")
        feed = {"Assets": [{"PackageId": package_id, "Version": version, "Type": "Full", "FileName": name,
                            "SHA256": probe.digest(package), "Size": package.stat().st_size}]}
        probe.write_json(self.root / f"releases.{channel}.json", feed)
        return package_id, version, feed

    def confirmation(self):
        root = self.root / "install"
        root.mkdir()
        executable = root / "probe.exe"
        executable.write_bytes(b"new version fixture; never run")
        packages = self.root / "packages"
        packages.mkdir()
        context = {"nonce": "a" * 32, "package_id": "private-probe", "install_root": str(root),
                   "executable": str(executable), "packages": str(packages)}
        receipt = {**context, "phase": "confirmed", "version": "0.0.2", "manifest_version": "0.0.2",
                   "pid": 123, "installation_portable": True}
        return receipt, context, probe.digest(executable)

    def test_work_must_be_new_and_inside_runner_temp(self):
        self.assertEqual(probe.private_work(self.root / "new", str(self.root)), self.root / "new")
        for path in (self.root, self.root.parent / "outside", Path("relative")):
            with self.subTest(path=path), self.assertRaises(RuntimeError):
                probe.private_work(path, str(self.root))

    def test_work_rejects_symlink_to_another_directory(self):
        outside = self.root / "outside"
        outside.mkdir()
        runner = self.root / "runner"
        runner.mkdir()
        try:
            (runner / "link").symlink_to(outside, target_is_directory=True)
        except OSError:
            self.skipTest("creating fixture symlinks is unavailable")
        with self.assertRaises(RuntimeError):
            probe.private_work(runner / "link/new", str(runner))

    def test_archive_rejects_traversal_and_windows_drive(self):
        for name in ("../outside", "/absolute", "C:/outside", "current\\..\\outside"):
            with self.subTest(name=name), self.archive([(name, b"x")]) as archive:
                with self.assertRaises(RuntimeError):
                    probe.archive_entries(archive)

    def test_archive_accepts_internal_mac_link_and_rejects_escape(self):
        for target, allowed in (("../Resources/sq.version", True), ("../../../outside", False)):
            info = zipfile.ZipInfo("Test.app/Contents/MacOS/sq.version")
            info.create_system = 3
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            with self.archive([(info, target)]) as archive:
                if allowed:
                    self.assertEqual(len(probe.archive_entries(archive, True)), 1)
                else:
                    with self.assertRaises(RuntimeError):
                        probe.archive_entries(archive, True)

    def test_release_verifies_full_and_portable_executable(self):
        package_id, version, _ = self.release()
        result = probe.inspect_release(self.root, package_id, version, "x86_64-pc-windows-msvc")
        self.assertEqual(result["executable_sha256"], hashlib.sha256(b"fixture executable; never run").hexdigest())

    def test_release_rejects_wrong_identity_or_digest(self):
        package_id, version, feed = self.release()
        for key, bad in (("Version", "0.0.1"), ("PackageId", "another-app"), ("SHA256", "0" * 64)):
            modified = json.loads(json.dumps(feed))
            modified["Assets"][0][key] = bad
            probe.write_json(self.root / "releases.win-x64.json", modified)
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                probe.inspect_release(self.root, package_id, version, "x86_64-pc-windows-msvc")

    def test_confirmation_requires_new_process_version_and_digest(self):
        receipt, context, expected = self.confirmation()
        self.assertEqual(probe.validate_confirmation(receipt, context, expected, 122)["pid"], 123)
        for key, bad in (("version", "0.0.1"), ("manifest_version", "0.0.1"),
                         ("pid", 122), ("nonce", "different"), ("installation_portable", False)):
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                probe.validate_confirmation({**receipt, key: bad}, context, expected, 122)
        with self.assertRaises(RuntimeError):
            probe.validate_confirmation(receipt, context, "0" * 64, 122)

    def test_confirmation_rejects_an_executable_outside_the_installation(self):
        receipt, context, expected = self.confirmation()
        outside = self.root / "another.exe"
        outside.write_bytes(Path(context["executable"]).read_bytes())
        with self.assertRaises(RuntimeError):
            probe.validate_confirmation({**receipt, "executable": str(outside)}, context, expected, 122)

    def test_cleanup_failure_is_nonzero_and_receipt_failed(self):
        fixture_root = self.root

        class PrivateFake:
            def __init__(self, args):
                self.result = {"status": "preparing"}
                self.output = fixture_root / "result.json"

            def run(self):
                self.result["status"] = "passed"

            def cleanup(self):
                raise RuntimeError("fixture cleanup failure")

        args = ["probe", "--work", str(self.root / "new"), "--target", "x86_64-pc-windows-msvc",
                "--output", str(self.root / "new/result.json")]
        with patch.object(sys, "argv", args), patch.object(probe, "Probe", PrivateFake), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(probe.main(), 1)
        self.assertEqual(probe.read_json(self.root / "result.json")["status"], "failed")


if __name__ == "__main__":
    unittest.main()
