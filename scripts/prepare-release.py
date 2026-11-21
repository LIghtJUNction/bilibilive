#!/usr/bin/env python3
"""Verify Project Z registry sources and stage a reproducible crates.io package.

The development checkout is never rewritten. This command does not publish.
"""
from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
DEP_TABLES = ("dependencies", "build-dependencies", "dev-dependencies")


def run(*args: str, cwd: Path = ROOT) -> str:
    result = subprocess.run(args, cwd=cwd, text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError(f"{' '.join(args[:3])} failed: {result.stderr.strip()}")
    return result.stdout


def canonical(url: str) -> str:
    return url.removeprefix("git+").split("#", 1)[0].split("?", 1)[0].rstrip("/").removesuffix(".git").lower()


def tables(document: dict):
    for kind in DEP_TABLES:
        yield document.get(kind, {})
    for target in document.get("target", {}).values():
        for kind in DEP_TABLES:
            yield target.get(kind, {})
    yield document.get("workspace", {}).get("dependencies", {})


def project_manifest(document: dict, versions: dict) -> dict:
    document = copy.deepcopy(document)
    for dependencies in tables(document):
        for alias, declaration in dependencies.items():
            if not isinstance(declaration, dict) or "git" not in declaration:
                continue
            name = declaration.get("package", alias)
            key = (name, canonical(declaration["git"]))
            if key not in versions:
                raise RuntimeError(f"No unique locked identity for Git dependency {alias}")
            forbidden = {"version", "branch", "tag", "rev", "path", "registry", "registry-index"} & declaration.keys()
            if forbidden:
                raise RuntimeError(f"{alias} mixes Git default-branch source with selectors: {sorted(forbidden)}")
            declaration.pop("git")
            declaration["version"] = "=" + versions[key]
    return document


def toml_value(value):
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False)
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return repr(value)
    if isinstance(value, list):
        return "[" + ", ".join(toml_value(item) for item in value) + "]"
    if isinstance(value, dict):
        return "{ " + ", ".join(f"{json.dumps(key)} = {toml_value(item)}" for key, item in value.items()) + " }"
    raise RuntimeError(f"Unsupported TOML value: {type(value).__name__}")


def render(document: dict) -> str:
    lines = []

    def visit(path, table):
        if path:
            lines.extend(("", "[" + ".".join(json.dumps(part) for part in path) + "]"))
        for key, value in table.items():
            if not isinstance(value, dict):
                lines.append(f"{json.dumps(key)} = {toml_value(value)}")
        for key, value in table.items():
            if isinstance(value, dict):
                visit((*path, key), value)

    visit((), document)
    result = "\n".join(lines).lstrip() + "\n"
    if tomllib.loads(result) != document:
        raise RuntimeError("TOML projection did not round-trip")
    return result


def crate_archive(name: str, version: str, cache: Path) -> Path:
    filename = f"{name}-{version}.crate"
    cargo_home = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")))
    cached = list((cargo_home / "registry/cache").glob(f"*/{filename}"))
    if cached:
        return cached[0]
    path = cache / filename
    if path.is_file():
        return path
    cache.mkdir(parents=True, exist_ok=True)
    url = f"https://static.crates.io/crates/{name}/{filename}"
    request = urllib.request.Request(url, headers={"User-Agent": "bilibilive-release/0.1 (+https://github.com/LIghtJUNction/bilibilive)"})
    try:
        with urllib.request.urlopen(request, timeout=45) as response:
            payload = response.read(64 * 1024 * 1024 + 1)
    except Exception as error:
        raise RuntimeError(f"{name} {version} is not available from crates.io: {error}") from error
    if len(payload) > 64 * 1024 * 1024:
        raise RuntimeError(f"{filename} exceeds the release archive limit")
    temp = path.with_suffix(".download")
    temp.write_bytes(payload)
    temp.replace(path)
    return path


def publication_files(name: str, output: str) -> set[str]:
    listed = set(output.splitlines())
    if not listed or any(
        not Path(path).parts or Path(path).is_absolute() or ".." in Path(path).parts
        for path in listed
    ):
        raise RuntimeError(f"{name}: Cargo returned an invalid publication file list")
    # Cargo can list checkout administration files; only the root .git is metadata.
    listed = {path for path in listed if Path(path).parts[:1] != (".git",)}
    if not listed:
        raise RuntimeError(f"{name}: Cargo returned an invalid publication file list")
    return listed


def verify_dependency(package: dict, versions: dict, cache: Path) -> dict:
    name, version = package["name"], package["version"]
    source = package["source"]
    endpoint, revision = source.removeprefix("git+").rsplit("#", 1)
    if "?" in endpoint or not re.fullmatch(r"[0-9a-f]{40,64}", revision):
        raise RuntimeError(f"{name}: expected a default-branch Git lock with a complete revision")
    source_dir = Path(package["manifest_path"]).parent
    if run("git", "rev-parse", "HEAD", cwd=source_dir).strip() != revision:
        raise RuntimeError(f"{name}: Cargo's dependency checkout does not identify the locked Git commit")
    if run("git", "status", "--porcelain", "--untracked-files=no", cwd=source_dir).strip():
        raise RuntimeError(f"{name}: locked dependency checkout contains modified tracked files")
    listed = publication_files(name, run("cargo", "package", "--list", "--allow-dirty", "--manifest-path", str(source_dir / "Cargo.toml"), cwd=source_dir))
    archive = crate_archive(name, version, cache)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    prefix = f"{name}-{version}/"
    members = {}
    expanded_size = 0
    with tarfile.open(archive, "r:gz") as crate:
        for item in crate:
            if item.isdir():
                continue
            if not item.isfile():
                raise RuntimeError(f"{name}: crate contains a non-regular archive member")
            if not item.name.startswith(prefix):
                raise RuntimeError(f"{name}: archive member outside the package prefix")
            path = item.name[len(prefix):]
            if path in members or Path(path).is_absolute() or ".." in Path(path).parts:
                raise RuntimeError(f"{name}: invalid or duplicate archive path")
            if item.size > 32 * 1024 * 1024:
                raise RuntimeError(f"{name}: oversized package member")
            expanded_size += item.size
            if expanded_size > 128 * 1024 * 1024 or len(members) >= 20000:
                raise RuntimeError(f"{name}: expanded package exceeds the release archive limit")
            members[path] = crate.extractfile(item).read()
    published = tomllib.loads(members.get("Cargo.toml", b"").decode())
    identity = published.get("package", {})
    if (identity.get("name"), identity.get("version"), canonical(identity.get("repository", ""))) != (name, version, canonical(endpoint)):
        raise RuntimeError(f"{name}: registry package identity differs from the locked Git package")
    if "Cargo.toml.orig" not in members:
        raise RuntimeError(f"{name}: archive has no original manifest for source comparison")
    actual_manifest = tomllib.loads(members["Cargo.toml.orig"].decode())
    expected_manifest = project_manifest(tomllib.loads((source_dir / "Cargo.toml").read_text()), versions)
    # Workspace membership affects the repository, not the published crate.
    expected_manifest.pop("workspace", None)
    actual_manifest.pop("workspace", None)
    if expected_manifest != actual_manifest:
        raise RuntimeError(f"{name} {version}: published manifest differs from the locked source projection; publish the matching Project Z version first")
    skipped = {"Cargo.toml", "Cargo.toml.orig", "Cargo.lock", ".cargo_vcs_info.json"}
    checked = []
    mismatches = []
    for path, payload in members.items():
        if path in skipped:
            continue
        original = source_dir / path
        if path not in listed:
            mismatches.append(path + " (not in the locked publication scope)")
        elif not original.is_file() or original.read_bytes() != payload:
            mismatches.append(path)
        else:
            checked.append(path)
    # Cargo, rather than a guessed extension list, defines the complete release
    # scope. This catches newly added resources, tests and native build inputs.
    for path in sorted(listed - skipped - members.keys()):
        mismatches.append(path + " (missing from registry)")
    if mismatches:
        raise RuntimeError(f"{name} {version}: registry content differs at {', '.join(mismatches[:12])}; publish the matching Project Z version first")
    vcs = json.loads(members.get(".cargo_vcs_info.json", b"{}"))
    return {
        "package": name, "version": version, "repository": canonical(endpoint),
        "locked_git_sha": revision, "published_git_sha": vcs.get("git", {}).get("sha1"),
        "archive_sha256": digest, "files_compared": len(checked),
        "publication_filelist_sha256": hashlib.sha256("\n".join(sorted(listed)).encode()).hexdigest(),
        "verification": "complete archive content and projected manifest match",
    }


def prepare(output: Path, cache: Path, skip_package: bool) -> dict:
    output = output.resolve()
    if output.exists() or output.is_relative_to(ROOT):
        raise RuntimeError("--output must be a new directory outside the source checkout")
    if run("git", "status", "--porcelain", "--untracked-files=no").strip():
        raise RuntimeError("Commit tracked source and Cargo.lock changes before preparing a release")
    commit = run("git", "rev-parse", "HEAD").strip()
    if any(entry.startswith("160000 ") for entry in run("git", "ls-tree", "-r", commit).splitlines()):
        raise RuntimeError("Source contains a Git submodule; git archive would omit its contents")
    if not (ROOT / "Cargo.lock").is_file():
        raise RuntimeError("Cargo.lock is required")
    lock_bytes = (ROOT / "Cargo.lock").read_bytes()
    if run("git", "show", f"{commit}:Cargo.lock").encode() != lock_bytes:
        raise RuntimeError("Cargo.lock must match the committed source")
    metadata = json.loads(run("cargo", "metadata", "--format-version", "1", "--locked", "--all-features"))
    internal = [package for package in metadata["packages"] if (package.get("source") or "").startswith("git+")]
    versions = {}
    for package in internal:
        endpoint = canonical(package["source"])
        if not endpoint.startswith("https://github.com/lightjunction/"):
            raise RuntimeError(f"Unexpected Git dependency source: {endpoint}")
        key = package["name"], endpoint
        if key in versions and versions[key] != package["version"]:
            raise RuntimeError(f"Ambiguous Git package identity: {key}")
        versions[key] = package["version"]
    receipts = [verify_dependency(package, versions, cache) for package in internal]
    lock = tomllib.loads(lock_bytes.decode())
    replacements = {}
    by_name = {(receipt["package"], receipt["version"]): receipt for receipt in receipts}
    for package in lock["package"]:
        source = package.get("source", "")
        if source.startswith("git+"):
            receipt = by_name[(package["name"], package["version"])]
            replacements[source] = REGISTRY
            package["source"] = REGISTRY
            package["checksum"] = receipt["archive_sha256"]
    for package in lock["package"]:
        for index, dependency in enumerate(package.get("dependencies", [])):
            for source, replacement in replacements.items():
                dependency = dependency.replace(f"({source})", f"({replacement})")
            package["dependencies"][index] = dependency
    output.parent.mkdir(parents=True, exist_ok=True)
    output.mkdir()
    try:
        with tempfile.TemporaryFile() as archive:
            subprocess.run(["git", "archive", "--format=tar", commit], cwd=ROOT, stdout=archive, check=True)
            archive.seek(0)
            with tarfile.open(fileobj=archive, mode="r:") as tar:
                if any(member.type == b"g" for member in tar.getmembers()):
                    raise RuntimeError("Unsupported archive metadata")
                tar.extractall(output, filter="data")
        document = project_manifest(tomllib.loads((output / "Cargo.toml").read_text()), versions)
        (output / "Cargo.toml").write_text(render(document))
        (output / "Cargo.lock").write_text(render(lock))
        # Cargo must accept the projected graph without resolving new versions.
        projected = json.loads(run("cargo", "metadata", "--format-version", "1", "--locked", "--all-features", cwd=output))
        if any((package.get("source") or "").startswith("git+") for package in projected["packages"]):
            raise RuntimeError("Registry projection still contains Git dependencies")
        expected = {(p["name"], p["version"]) for p in metadata["packages"]}
        actual = {(p["name"], p["version"]) for p in projected["packages"]}
        if expected != actual:
            raise RuntimeError("Registry projection changed the resolved package graph")
        if not skip_package:
            run("cargo", "package", "--locked", cwd=output)
        receipt = {"schema_version": 1, "source_commit": commit, "source_lock_sha256": hashlib.sha256(lock_bytes).hexdigest(), "registry_dependencies": receipts, "status": "verified-package" if not skip_package else "verified-manifest", "published": False}
        (output / "release-provenance.json").write_text(json.dumps(receipt, indent=2) + "\n")
        return receipt
    except BaseException:
        # Keep the reviewable staging files and failure evidence; source is intact.
        (output / "RELEASE-BLOCKED.txt").write_text("Release staging did not complete. Do not publish this directory.\n")
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cache", type=Path, default=Path.home() / ".cache/bilibilive/release-archives")
    parser.add_argument("--skip-package", action="store_true", help="Only verify the projected manifest/lock graph; do not compile the .crate")
    args = parser.parse_args()
    try:
        print(json.dumps(prepare(args.output, args.cache, args.skip_package), indent=2))
    except (RuntimeError, OSError, ValueError, tarfile.TarError, subprocess.CalledProcessError) as error:
        print(f"Release preparation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
