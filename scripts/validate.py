#!/usr/bin/env python3
"""Run the local acceptance gates without accessing a live Bilibili account."""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]


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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true", help="Also build the optimized native application")
    parser.add_argument("--obs-plugin", action="store_true", help="Also build the OBS plugin with the installed OBS/Qt SDK")
    args = parser.parse_args()
    commands = [
        ["cargo", "fmt", "--all", "--", "--check"],
        ["cargo", "check", "--locked", "--all-targets", "--all-features"],
        ["cargo", "test", "--locked", "--all-features"],
        ["cargo", "check", "--locked", "--no-default-features"],
    ]
    if args.release:
        commands.append(["cargo", "build", "--locked", "--release"])
    receipts = []
    with build_lock():
        for command in commands:
            started = time.monotonic()
            subprocess.run(command, cwd=ROOT, check=True)
            receipts.append({"command": command, "seconds": round(time.monotonic() - started, 3), "exit_code": 0})
    if args.obs_plugin:
        for command in (
            ["cmake", "-S", "plugins/obs-bilibilive-pk", "-B", "build/obs-plugin", "-DCMAKE_BUILD_TYPE=Release"],
            ["cmake", "--build", "build/obs-plugin", "--parallel", "2"],
        ):
            subprocess.run(command, cwd=ROOT, check=True)
            receipts.append({"command": command, "exit_code": 0})
    print(json.dumps({"status": "passed", "checks": receipts, "live_account_used": False}, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"Validation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
