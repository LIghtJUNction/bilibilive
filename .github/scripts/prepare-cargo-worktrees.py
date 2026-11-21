#!/usr/bin/env python3
"""Prepare an isolated Cargo Git cache with exact, detached Git worktrees.

Cargo/libgit2 creates .git/hooks/README.sample inside dependency checkouts.
An explicit package.include = ["README*"] can match that generated metadata.
Keep the complete publication verifier unchanged: use standard Git worktrees,
whose .git file points to metadata outside the package's source directory.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib


SHARED_PATHS = (
    "registry", ".package-cache", ".package-cache-mutate",
    "config", "config.toml", "credentials", "credentials.toml",
)
OFFICIAL_LAUNCHER_SHA256 = "4aed1514057bc2ec63252183c3cc0eb2a8f7a52cd2be86a5cffa7cc19069b5b4"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def run(*args: str, cwd: Path, env: dict | None = None) -> str:
    environment = dict(os.environ if env is None else env, GIT_OPTIONAL_LOCKS="0")
    timeout = 300 if Path(args[0]).name == "cargo" else 30
    result = subprocess.run(args, cwd=cwd, env=environment, capture_output=True, text=True, timeout=timeout)
    require(result.returncode == 0, f"{' '.join(args[:4])} failed: {result.stderr.strip()}")
    return result.stdout


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for block in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def digest_json(value) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def canonical(url: str) -> str:
    return url.removeprefix("git+").split("#", 1)[0].rstrip("/").removesuffix(".git").lower()


def graph(metadata: dict) -> dict:
    return {
        "packages": sorted(
            (p["id"], p["name"], p["version"], p.get("source") or "")
            for p in metadata["packages"]
        ),
        "resolve": metadata["resolve"],
        "workspace_members": metadata["workspace_members"],
        "workspace_default_members": metadata["workspace_default_members"],
    }


def tracked_snapshot(root: Path) -> dict:
    output = subprocess.check_output(["git", "ls-tree", "-rz", "HEAD"], cwd=root, timeout=30,
                                     env=dict(os.environ, GIT_OPTIONAL_LOCKS="0"))
    entries = []
    for entry in output.split(b"\0"):
        if not entry:
            continue
        identity, raw_path = entry.split(b"\t", 1)
        mode, kind, oid = identity.decode("ascii").split()
        name = os.fsdecode(raw_path)
        path = root / name
        require(kind == "blob", "Dependency submodules require a separate recursive source verifier")
        require(not Path(name).is_absolute() and ".." not in Path(name).parts, "Invalid tracked path")
        if mode == "120000":
            require(path.is_symlink(), f"Tracked symlink is missing: {name}")
            content = hashlib.sha256(os.fsencode(os.readlink(path))).hexdigest()
        else:
            require(mode in ("100644", "100755") and path.is_file() and not path.is_symlink(),
                    f"Tracked source file is missing: {name}")
            content = sha256(path)
        entries.append([name, mode, oid, content])
    return {"files": len(entries), "sha256": digest_json(entries)}


def source_identity(source: Path) -> dict:
    require(not run("git", "status", "--porcelain", "--untracked-files=no", cwd=source).strip(),
            "Frozen source contains modified tracked files")
    return {
        "path": str(source),
        "commit": run("git", "rev-parse", "HEAD", cwd=source).strip(),
        "tree": run("git", "rev-parse", "HEAD^{tree}", cwd=source).strip(),
        "lock_sha256": sha256(source / "Cargo.lock"),
        "manifest_sha256": sha256(source / "Cargo.toml"),
    }


def git_packages(metadata: dict) -> list[dict]:
    return [p for p in metadata["packages"] if (p.get("source") or "").startswith("git+")]


def official_profile(base_home: Path) -> dict | None:
    path = base_home / "profile.json"
    if not path.exists():
        return None
    profile = json.loads(path.read_text())
    require(set(profile) == {"cargo_home", "neutral_cwd", "cargo", "rustc", "rustdoc", "toolchain_bin",
                             "launcher_bin", "original_config_files"}, "Unexpected official Cargo profile")
    require(profile["cargo_home"] == str(base_home) and profile["launcher_bin"] == str(base_home / "bin"),
            "Official base profile identifies another Cargo home")
    require(sha256(base_home / "bin/cargo") == OFFICIAL_LAUNCHER_SHA256,
            "Official Cargo launcher differs from the reviewed source")
    require(os.access(base_home / "bin/cargo", os.X_OK), "Official Cargo launcher is not executable")
    return profile


def verify_prepared(source: Path, base_home: Path, output: Path, receipt: dict) -> None:
    """Recheck a prepared profile using local files and Git only; never run Cargo."""
    source, base_home, output = source.resolve(), base_home.resolve(), output.resolve()
    require(receipt.get("status") == "prepared-verified", "Cargo worktree preparation did not finish")
    require(receipt.get("schema_version") == 1, "Unsupported Cargo worktree receipt")
    require(receipt.get("helper_sha256") == sha256(Path(__file__)), "Cargo worktree helper changed")
    require(receipt.get("source") == source_identity(source), "Frozen source identity changed")
    require(receipt.get("base_cargo_home") == str(base_home) and receipt.get("cargo_home") == str(output),
            "Cargo home differs from the verified preparation")
    require((output / "git").is_dir() and not (output / "git").is_symlink(),
            "Prepared Git cache must be an independent directory")
    require(receipt.get("graph_equal") is True and receipt.get("metadata_before_sha256") ==
            receipt.get("metadata_after_sha256"), "Cargo dependency graph or features changed")
    for item in receipt["shared_paths"]:
        path = output / item["name"]
        require(path.is_symlink() and os.readlink(path) == item["link_target"],
                f"Shared Cargo path changed: {item['name']}")
        require(Path(item["link_target"]) == base_home / item["name"], "Shared path is outside base profile")
        if "sha256" in item:
            require(sha256(path) == item["sha256"], f"Cargo configuration changed: {item['name']}")
    official = receipt.get("official_profile")
    if official is not None:
        base_profile = official_profile(base_home)
        require(base_profile is not None, "Official base profile is missing")
        expected = dict(base_profile, cargo_home=str(output), launcher_bin=str(output / "bin"))
        require(json.loads((output / "profile.json").read_text()) == expected,
                "Prepared official profile changed settings beyond its home and launcher path")
        require(sha256(base_home / "profile.json") == official["base_profile_sha256"] and
                sha256(output / "profile.json") == official["profile_sha256"], "Official Cargo profile changed")
        require(not (output / "bin/cargo").is_symlink() and
                sha256(output / "bin/cargo") == OFFICIAL_LAUNCHER_SHA256 and
                os.access(output / "bin/cargo", os.X_OK), "Prepared official launcher changed")
        for item in official["configuration_files"]:
            require(sha256(base_home / item["name"]) == item["sha256"] and
                    sha256(output / item["name"]) == item["sha256"] and
                    not (output / item["name"]).is_symlink(), "Official Cargo configuration changed")
        require(sha256(base_home / "initialization-receipt.json") == official["base_initialization_sha256"],
                "Base official profile initialization receipt changed")
    lock = tomllib.loads((source / "Cargo.lock").read_text())
    locked = sorted((p["name"], p["version"], p["source"]) for p in lock["package"]
                    if p.get("source", "").startswith("git+"))
    recorded = sorted((p["name"], p["version"], p["source"]) for p in receipt["packages"])
    require(recorded == locked, "Worktree receipt does not cover every locked Git package")
    for item in receipt["packages"]:
        root, database = Path(item["worktree"]), Path(item["git_database"])
        require(root.is_relative_to(output / "git/checkouts") and
                database.is_relative_to(output / "git/db"), "Worktree or database leaves isolated profile")
        require(not root.is_symlink() and not database.is_symlink() and
                root.resolve() == root and database.resolve() == database,
                "Worktree or database is not independent")
        require((root / ".git").is_file(), "Prepared worktree metadata is not a .git file")
        require(run("git", "rev-parse", "HEAD", cwd=root).strip() == item["locked_commit"],
                "Worktree HEAD differs from locked commit")
        require(item["source"].rsplit("#", 1)[1] == item["locked_commit"] and
                canonical(item["source"]) == item["canonical_url"], "Worktree source identity differs")
        require(run("git", "rev-parse", "HEAD^{tree}", cwd=root).strip() == item["tree"],
                "Worktree tree differs from verified source")
        require(Path(run("git", "rev-parse", "--path-format=absolute", "--git-common-dir", cwd=root).strip())
                == database, "Worktree references another Git database")
        require(run("git", "--git-dir", str(database), "rev-parse", "--is-bare-repository", cwd=output).strip()
                == "true", "Prepared Git database is not bare")
        require(run("git", "--git-dir", str(database), "rev-parse", item["locked_commit"] + "^{tree}",
                    cwd=output).strip() == item["tree"], "Bare database lacks the verified commit")
        require(not run("git", "status", "--porcelain", "--untracked-files=no", cwd=root).strip(),
                "Prepared worktree contains changed tracked files")
        require(tracked_snapshot(root) == item["tracked_source"], "Prepared source file bytes changed")
        manifest = Path(item["manifest_path"])
        require(manifest.is_relative_to(root) and sha256(manifest) == item["manifest_sha256"],
                "Dependency manifest changed")


def prepare(source: Path, base_home: Path, output: Path) -> dict:
    source, base_home, output = source.resolve(), base_home.resolve(), output.resolve()
    require(not output.exists(), "Output must be a new Cargo home")
    require(not output.is_relative_to(source) and not output.is_relative_to(base_home),
            "Isolated Cargo home must be outside source and the base Cargo home")
    require((base_home / "registry").is_dir(), "Base Cargo registry cache is missing")
    identity = source_identity(source)
    official = official_profile(base_home)
    environment = dict(os.environ, CARGO_HOME=str(base_home), CARGO_NET_OFFLINE="true",
                       CARGO_CACHE_AUTO_CLEAN_FREQUENCY="never")
    cargo = str(base_home / "bin/cargo") if official is not None else "cargo"
    before = json.loads(run(cargo, "metadata", "--locked", "--all-features", "--format-version", "1",
                            "--offline", cwd=source, env=environment))
    packages = git_packages(before)
    require(packages, "No locked Git dependencies were found")
    require((base_home / ".package-cache").is_file(), "Base Cargo cache lock is missing")
    output.mkdir(parents=True)
    shared = []
    configuration_files = []
    for name in SHARED_PATHS:
        original = base_home / name
        if original.exists() or original.is_symlink():
            if official is not None and name in ("config", "config.toml"):
                shutil.copy2(original, output / name)
                configuration_files.append({"name": name, "sha256": sha256(original)})
                continue
            (output / name).symlink_to(original, target_is_directory=original.is_dir())
            item = {"name": name, "link_target": str(original)}
            # Configuration identity is public release policy; credentials are
            # linked for Cargo itself without reading or hashing their content.
            if name in ("config", "config.toml"):
                item["sha256"] = sha256(original)
            shared.append(item)
    official_receipt = None
    if official is not None:
        (output / "bin").mkdir()
        shutil.copy2(base_home / "bin/cargo", output / "bin/cargo")
        projected = dict(official, cargo_home=str(output), launcher_bin=str(output / "bin"))
        (output / "profile.json").write_text(json.dumps(projected, indent=2) + "\n")
        official_receipt = {
            "base_profile_sha256": sha256(base_home / "profile.json"),
            "profile_sha256": sha256(output / "profile.json"),
            "launcher_sha256": OFFICIAL_LAUNCHER_SHA256,
            "configuration_files": configuration_files,
            "base_initialization_sha256": sha256(base_home / "initialization-receipt.json"),
        }
    old_checkouts = (base_home / "git/checkouts").resolve()
    prepared = []
    for package in packages:
        endpoint, revision = package["source"].removeprefix("git+").rsplit("#", 1)
        require("?" not in endpoint and bool(re.fullmatch(r"[0-9a-f]{40,64}", revision)),
                "Expected default-branch Git dependencies with complete locked commits")
        manifest = Path(package["manifest_path"]).resolve()
        root = Path(run("git", "rev-parse", "--show-toplevel", cwd=manifest.parent).strip()).resolve()
        relative = root.relative_to(old_checkouts)
        require(len(relative.parts) == 2, "Unexpected Cargo checkout layout")
        require(run("git", "rev-parse", "HEAD", cwd=root).strip() == revision,
                "Original Cargo checkout does not identify the locked commit")
        require(not run("git", "status", "--porcelain", "--untracked-files=no", cwd=root).strip(),
                "Original Cargo checkout contains changed tracked files")
        tree = run("git", "rev-parse", "HEAD^{tree}", cwd=root).strip()
        snapshot = tracked_snapshot(root)
        original_database = (base_home / "git/db" / relative.parts[0]).resolve()
        require(original_database.is_dir(), "Original Cargo Git database is missing")
        alternate = original_database / "objects/info/alternates"
        require(not alternate.exists() or not alternate.read_bytes().strip(),
                "Git object alternates need an independent database export")
        database = output / "git/db" / relative.parts[0]
        if not database.exists():
            # A byte copy, rather than hard links, preserves the old cache and
            # keeps later Cargo Git operations local to this temporary profile.
            shutil.copytree(original_database, database)
        worktree = output / "git/checkouts" / relative
        if not worktree.exists():
            worktree.parent.mkdir(parents=True, exist_ok=True)
            run("git", "--git-dir", str(database), "worktree", "add", "--detach", str(worktree), revision,
                cwd=source)
            shutil.copy2(root / ".cargo-ok", worktree / ".cargo-ok")
        require(tracked_snapshot(worktree) == snapshot, "Exact worktree changed tracked source bytes")
        prepared.append({
            "name": package["name"], "version": package["version"], "source": package["source"],
            "canonical_url": canonical(package["source"]), "locked_commit": revision, "tree": tree,
            "original_checkout": str(root), "original_git_database": str(original_database),
            "worktree": str(worktree), "git_database": str(database),
            "manifest_path": str(worktree / manifest.relative_to(root)),
            "manifest_sha256": sha256(manifest), "tracked_source": snapshot,
        })
    environment["CARGO_HOME"] = str(output)
    if official is not None:
        cargo = str(output / "bin/cargo")
        environment["PATH"] = str(output / "bin") + os.pathsep + environment.get("PATH", "")
    after = json.loads(run(cargo, "metadata", "--locked", "--all-features", "--format-version", "1",
                           "--offline", cwd=source, env=environment))
    require(graph(before) == graph(after), "Isolated cache changed the full dependency graph or features")
    for package in git_packages(after):
        item = next(p for p in prepared if p["source"] == package["source"] and p["name"] == package["name"])
        require(package["manifest_path"] == item["manifest_path"], "Cargo did not use the prepared worktree")
    receipt = {
        "schema_version": 1, "status": "prepared-verified", "helper_sha256": sha256(Path(__file__)),
        "source": identity, "base_cargo_home": str(base_home), "cargo_home": str(output),
        "shared_paths": shared, "packages": prepared,
        "official_profile": official_receipt,
        "metadata_before_sha256": digest_json(graph(before)),
        "metadata_after_sha256": digest_json(graph(after)), "graph_equal": True,
        "source_bytes_preserved": True, "original_git_cache_modified": False,
        "publication_guard_modified": False, "publication_scope_filtered": False,
    }
    verify_prepared(source, base_home, output, receipt)
    return receipt


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--base-cargo-home", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--github-env", type=Path)
    args = parser.parse_args()
    try:
        require(not args.receipt.exists(), "Receipt path already exists")
        receipt = prepare(args.source, args.base_cargo_home, args.output)
        args.receipt.parent.mkdir(parents=True, exist_ok=True)
        args.receipt.write_text(json.dumps(receipt, indent=2) + "\n")
        if args.github_env:
            home = str(args.output.resolve())
            require("\n" not in home and "\r" not in home, "Invalid Cargo home path")
            with args.github_env.open("a", encoding="utf-8") as output:
                output.write(f"CARGO_HOME={home}\nCARGO_CACHE_AUTO_CLEAN_FREQUENCY=never\n")
        print(f"Prepared {len(receipt['packages'])} exact Git worktrees; full dependency graph and features unchanged")
        return 0
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Cargo worktree preparation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
