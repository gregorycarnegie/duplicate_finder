#!/usr/bin/env python3
"""Check that the Verus crates Cargo builds the app with match the Verus release
that verifies the proofs.

Cargo pins vstd and its verus_* dependencies from crates.io; CI verifies with a
separately downloaded Verus release. Crates are published from a version-bump
commit, so they never share the release's exact commit. This script fetches both
commits and requires each crate's source directory to be identical apart from
its Cargo.toml (version numbers). A mismatch means the app could be compiled
with macros or library code that the pinned Verus release never checked.

Usage: check_verus_crates.py path/to/verus-x86-linux/version.json
"""

import json
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = "https://github.com/verus-lang/verus"
ROOT = Path(__file__).resolve().parent.parent


def run(*args, cwd=None):
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True).stdout


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    release_commit = json.loads(Path(sys.argv[1]).read_text())["verus"]["commit"]

    metadata = json.loads(
        run(
            "cargo", "metadata", "--locked", "--format-version", "1",
            "--manifest-path", str(ROOT / "src-tauri" / "Cargo.toml"),
        )
    )
    crates = []
    for package in metadata["packages"]:
        if package["name"] != "vstd" and not package["name"].startswith("verus_"):
            continue
        vcs = json.loads((Path(package["manifest_path"]).parent / ".cargo_vcs_info.json").read_text())
        if vcs.get("dirty"):
            sys.exit(f"{package['name']} {package['version']} was published from a dirty tree")
        crates.append((package["name"], package["version"], vcs["git"]["sha1"], vcs["path_in_vcs"]))
    if not any(name == "vstd" for name, *_ in crates):
        sys.exit("vstd is not in the dependency graph")

    failed = False
    with tempfile.TemporaryDirectory() as clone:
        run("git", "init", "--quiet", clone)
        commits = {release_commit} | {sha for _, _, sha, _ in crates}
        run("git", "fetch", "--quiet", "--depth=1", REPO, *commits, cwd=clone)
        for name, version, sha, path in crates:
            diff = run(
                "git", "diff", "--stat", sha, release_commit, "--",
                path, f":(exclude){path}/Cargo.toml", cwd=clone,
            )
            if diff:
                failed = True
                print(f"{name} {version} ({sha[:7]}) differs from Verus {release_commit[:7]}:\n{diff}")
            else:
                print(f"{name} {version} ({sha[:7]}) matches Verus {release_commit[:7]} in {path}")
    if failed:
        sys.exit("Pin the vstd crates and the Verus release to matching sources.")


if __name__ == "__main__":
    main()
