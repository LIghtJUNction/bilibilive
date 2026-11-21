#!/usr/bin/env python3
"""Local Git/Cargo fixtures; no remote repository or registry request is needed."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location(
    "cargo_worktrees", Path(__file__).with_name("prepare-cargo-worktrees.py"))
HELPER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HELPER)


class WorktreeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="bilibilive-cargo-worktree-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.base = self.root / "cargo-base"
        (self.base / "registry").mkdir(parents=True)
        self.environment = dict(os.environ, CARGO_HOME=str(self.base))
        self.environment.pop("CARGO_NET_OFFLINE", None)
        dependency = self.root / "dependency"
        (dependency / "src").mkdir(parents=True)
        (dependency / "src/lib.rs").write_text("pub fn marker() -> bool { true }\n")
        (dependency / "README.md").write_text("This is a real committed release resource.\n")
        (dependency / "Cargo.toml").write_text(
            '[package]\nname="worktree-probe"\nversion="0.1.0"\nedition="2021"\n'
            'include=["src/**", "README*", "Cargo.toml"]\n[workspace]\n')
        self.commit(dependency)
        self.source = self.root / "source"
        (self.source / "src").mkdir(parents=True)
        (self.source / "src/lib.rs").write_text("")
        (self.source / "Cargo.toml").write_text(
            '[package]\nname="worktree-consumer"\nversion="0.1.0"\nedition="2021"\n'
            '[dependencies]\nworktree-probe={git=' + json.dumps(dependency.as_uri()) + '}\n[workspace]\n')
        metadata = json.loads(self.command("cargo", "metadata", "--format-version", "1", cwd=self.source))
        self.commit(self.source)
        self.original = Path(next(p["manifest_path"] for p in metadata["packages"]
                                  if p["name"] == "worktree-probe")).parent
        self.output = self.root / "cargo-prepared"

    def command(self, *args, cwd):
        return subprocess.check_output(args, cwd=cwd, env=self.environment, text=True,
                                       stderr=subprocess.PIPE, timeout=30)

    def commit(self, path):
        self.command("git", "init", "-q", cwd=path)
        self.command("git", "add", ".", cwd=path)
        self.command("git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                     "commit", "-qm", "Committed fixture source", cwd=path)

    def cache_snapshot(self):
        return {str(p.relative_to(self.base / "git")): HELPER.sha256(p)
                for p in (self.base / "git").rglob("*") if p.is_file()}

    def test_exact_worktree_preserves_real_resources_and_the_original_cache(self):
        hook = self.original / ".git/hooks/README.sample"
        self.assertTrue(hook.is_file())
        original_hook = hook.read_bytes()
        before = self.cache_snapshot()
        receipt = HELPER.prepare(self.source, self.base, self.output)
        self.assertEqual(before, self.cache_snapshot())
        HELPER.verify_prepared(self.source, self.base, self.output, receipt)
        package = receipt["packages"][0]
        worktree = Path(package["worktree"])
        self.assertTrue((worktree / ".git").is_file())
        self.assertEqual(receipt["metadata_before_sha256"], receipt["metadata_after_sha256"])
        paths = self.command("cargo", "package", "--list", "--allow-dirty", "--manifest-path",
                             str(worktree / "Cargo.toml"), cwd=worktree).splitlines()
        self.assertIn("README.md", paths)
        self.assertIn("src/lib.rs", paths)
        self.assertFalse(any(path.startswith(".git/") for path in paths))
        self.assertEqual(hook.read_bytes(), original_hook)
        self.assertEqual((Path(package["git_database"]) / "hooks/README.sample").read_bytes(), original_hook)
        (worktree / "README.md").write_text("A changed real resource must be rejected.\n")
        with self.assertRaisesRegex(RuntimeError, "changed tracked files|source file bytes changed"):
            HELPER.verify_prepared(self.source, self.base, self.output, receipt)

    def test_modified_frozen_source_stops_before_preparing_any_profile(self):
        (self.source / "src/lib.rs").write_text("pub fn changed() {}\n")
        before = self.cache_snapshot()
        with self.assertRaisesRegex(RuntimeError, "Frozen source contains modified"):
            HELPER.prepare(self.source, self.base, self.output)
        self.assertFalse(self.output.exists())
        self.assertEqual(before, self.cache_snapshot())


if __name__ == "__main__":
    unittest.main()
