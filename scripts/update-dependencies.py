#!/usr/bin/env python3
"""Refresh current Project Z default branches and stable external dependencies."""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[1]


def run(*args):
    result = subprocess.run(args, cwd=ROOT, text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError(f"{' '.join(args[:3])}: {result.stderr.strip()}")
    return result.stdout


@contextmanager
def build_lock():
    path = Path("/var/tmp/project-z-cargo-target/.project-z-cargo-owner.lock")
    if os.name != "posix" or not path.parent.exists():
        yield
        return
    import fcntl
    with path.open("a") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        yield


def latest(package):
    request = urllib.request.Request(f"https://crates.io/api/v1/crates/{package}", headers={"User-Agent": "bilibilive-dependencies/0.1 (+https://github.com/LIghtJUNction/bilibilive)"})
    with urllib.request.urlopen(request, timeout=30) as response:
        document = json.load(response)
    stable = document["crate"].get("max_stable_version")
    if not stable:
        raise RuntimeError(f"No stable registry version found for {package}")
    return stable


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apply", action="store_true", help="Rewrite external lower bounds, refresh Cargo.lock and run local validation")
    args = parser.parse_args()
    # The projection helper is source tooling; do not leave binary bytecode in
    # a directory that is included in the source package.
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("bilibilive_release", ROOT / "scripts/prepare-release.py")
    projection = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(projection)
    document = tomllib.loads((ROOT / "Cargo.toml").read_text())
    changes = []
    for dependencies in projection.tables(document):
        for alias, declaration in list(dependencies.items()):
            if isinstance(declaration, dict) and "git" in declaration:
                if any(key in declaration for key in ("version", "rev", "tag", "branch")):
                    raise RuntimeError(f"{alias}: Project Z dependencies must use the default branch without a version selector")
                head = run("git", "ls-remote", declaration["git"], "HEAD").split()[0]
                changes.append({"dependency": alias, "git": declaration["git"], "default_head": head})
                continue
            if isinstance(declaration, dict) and any(key in declaration for key in ("path", "workspace")):
                continue
            name = declaration.get("package", alias) if isinstance(declaration, dict) else alias
            version = latest(name)
            current = declaration.get("version") if isinstance(declaration, dict) else declaration
            changes.append({"dependency": alias, "current": current, "latest": version})
            if isinstance(declaration, dict):
                declaration["version"] = version
            else:
                dependencies[alias] = version
    print(json.dumps({"apply": args.apply, "dependencies": changes}, indent=2))
    if not args.apply:
        return 0
    with build_lock():
        (ROOT / "Cargo.toml").write_text(projection.render(document))
        subprocess.run(["cargo", "update"], cwd=ROOT, check=True)
        lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
        heads = {}
        for package in lock["package"]:
            source = package.get("source", "")
            if not source.startswith("git+"):
                continue
            endpoint, revision = source[4:].rsplit("#", 1)
            if "?" in endpoint:
                raise RuntimeError(f"Locked selector is not a default branch: {endpoint}")
            if endpoint not in heads:
                heads[endpoint] = run("git", "ls-remote", endpoint, "HEAD").split()[0]
            if revision != heads[endpoint]:
                raise RuntimeError(f"Default branch moved during refresh: {endpoint}; run the update again")
        for command in (["cargo", "check", "--locked", "--all-targets", "--all-features"], ["cargo", "test", "--locked", "--all-features"]):
            subprocess.run(command, cwd=ROOT, check=True)
    print(json.dumps({"status": "updated-and-locally-validated", "heads": heads}, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Dependency update failed: {error}", file=sys.stderr)
        raise SystemExit(1)
