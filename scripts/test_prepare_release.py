#!/usr/bin/env python3
"""Offline regression checks for dependency publication scope verification."""
from __future__ import annotations

import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location(
    "prepare_release", Path(__file__).with_name("prepare-release.py")
)
assert SPEC and SPEC.loader
RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RELEASE)


class PublicationFilesTests(unittest.TestCase):
    def test_only_root_git_administration_is_removed(self):
        retained = {
            "src/lib.rs",
            ".gitignore",
            ".gitattributes",
            ".github/workflows/ci.yml",
            "tests/fixtures/.git/config",
        }
        listing = retained | {".git", ".git/hooks/README.sample"}
        self.assertEqual(RELEASE.publication_files("fixture", "\n".join(listing)), retained)

    def test_invalid_paths_are_rejected_before_filtering(self):
        for path in ("/src/lib.rs", "../lib.rs", "src/../../lib.rs", ".git/../src/lib.rs"):
            with self.subTest(path=path):
                with self.assertRaisesRegex(RuntimeError, "invalid publication file list"):
                    RELEASE.publication_files("fixture", "src/lib.rs\n" + path)

    def test_a_list_must_contain_source_files(self):
        for listing in ("", ".", ".git/hooks/README.sample", "src/lib.rs\n\nREADME.md"):
            with self.subTest(listing=listing):
                with self.assertRaisesRegex(RuntimeError, "invalid publication file list"):
                    RELEASE.publication_files("fixture", listing)


class VerifyDependencyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        self.revision = "a" * 40
        self.manifest = (
            '[package]\nname = "fixture"\nversion = "1.0.0"\n'
            'repository = "https://github.com/lightjunction/fixture"\n'
        ).encode()
        self.files = {
            "Cargo.toml": self.manifest,
            "src/lib.rs": b"pub fn value() -> u8 { 1 }\n",
            ".gitignore": b"target/\n",
            ".gitattributes": b"*.rs text\n",
            ".github/workflows/ci.yml": b"name: CI\n",
            "tests/fixtures/.git/config": b"[core]\nbare = false\n",
        }
        for name, content in self.files.items():
            path = self.source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        self.listed = set(self.files) | {"Cargo.toml.orig", ".git/hooks/README.sample"}
        self.archive = self.root / "fixture-1.0.0.crate"
        self.package = {
            "name": "fixture",
            "version": "1.0.0",
            "source": "git+https://github.com/lightjunction/fixture.git#" + self.revision,
            "manifest_path": str(self.source / "Cargo.toml"),
        }

    def write_archive(self, extra=None):
        files = self.files | {"Cargo.toml.orig": self.manifest} | (extra or {})
        with tarfile.open(self.archive, "w:gz") as archive:
            for name, content in files.items():
                member = tarfile.TarInfo("fixture-1.0.0/" + name)
                member.size = len(content)
                archive.addfile(member, io.BytesIO(content))

    def fake_run(self, *args, cwd):
        self.assertEqual(cwd, self.source)
        if args == ("git", "rev-parse", "HEAD"):
            return self.revision + "\n"
        if args == ("git", "status", "--porcelain", "--untracked-files=no"):
            return ""
        if args[:3] == ("cargo", "package", "--list"):
            return "\n".join(sorted(self.listed)) + "\n"
        self.fail(f"Unexpected command: {args}")

    def verify(self):
        with mock.patch.object(RELEASE, "run", side_effect=self.fake_run):
            with mock.patch.object(RELEASE, "crate_archive", return_value=self.archive):
                return RELEASE.verify_dependency(self.package, {}, self.root / "cache")

    def test_matching_source_passes_with_git_administration_noise(self):
        self.write_archive()
        receipt = self.verify()
        expected = self.listed - {".git/hooks/README.sample"}
        self.assertEqual(receipt["files_compared"], len(self.files) - 1)
        self.assertEqual(
            receipt["publication_filelist_sha256"],
            hashlib.sha256("\n".join(sorted(expected)).encode()).hexdigest(),
        )

    def test_source_omitted_from_registry_still_fails(self):
        self.write_archive()
        (self.source / "src/new.rs").write_text("pub fn new() {}\n")
        self.listed.add("src/new.rs")
        with self.assertRaisesRegex(RuntimeError, r"src/new.rs \(missing from registry\)"):
            self.verify()

    def test_changed_dotfile_and_nested_fixture_still_fail(self):
        self.write_archive()
        for name in (".gitignore", ".github/workflows/ci.yml", "tests/fixtures/.git/config"):
            with self.subTest(name=name):
                path = self.source / name
                original = path.read_bytes()
                path.write_bytes(original + b"changed\n")
                try:
                    with self.assertRaises(RuntimeError) as error:
                        self.verify()
                    self.assertIn(name, str(error.exception))
                finally:
                    path.write_bytes(original)

    def test_registry_git_administration_is_not_ignored(self):
        self.write_archive({".git/hooks/README.sample": b"private checkout metadata\n"})
        with self.assertRaisesRegex(RuntimeError, "not in the locked publication scope"):
            self.verify()


if __name__ == "__main__":
    unittest.main()
