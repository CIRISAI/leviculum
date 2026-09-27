#!/usr/bin/env python3
"""Rustdoc over the crates this branch touched -- the cheap half of `doc-gate`.

`doc-gate` runs `cargo doc --workspace --no-deps` under `-D warnings`, so a
broken or private intra-doc link is a land gate failure. It costs ~21 s over
the whole workspace and is therefore excused from `guards`, and that excusal
is the hole Codeberg #359 fell into: pass 344 wrote `[`Column`]` and
`[`json_string`]` into leviculum-lxmf-node/src/telemetry.rs, gated on
`cargo test`, fmt, clippy and `just guards`, and not one of those four runs
rustdoc. The red surfaced a day later on the land gate with sixty-three
commits queued behind it.

Docing only what changed is the part of that gate a coder pass can afford. A
pass touches one or two crates; `cargo doc --no-deps -p <crate>` for those is
seconds rather than twenty.

WHAT COUNTS AS TOUCHED -- three sources, unioned:

  * `git diff --name-only <merge-base with origin/master> HEAD`: every commit
    this branch has that the remote does not, so a pass sees its own landed
    work and that of every earlier pass still unpushed on the same lane.
  * `git diff --name-only HEAD`: staged and unstaged work. A pass gates
    BEFORE it commits, so without this the recipe would be blind to the very
    change it is being run about.
  * `git ls-files --others --exclude-standard`: a brand-new file in a crate
    is a change to that crate.

With no `origin/master` (a plain clone, a detached CI checkout) the first
source is skipped rather than fatal; the other two still apply.

WHAT IT SKIPS. Paths outside every workspace member -- docs/, scripts/, the
Justfile -- and leviculum-nrf and leviculum-esp, which are their own
workspaces and carry their own rustdoc gate inside `lint-nrf` and
`build-esp-image`. Nothing touched inside the workspace is a pass, not an
error: there is no documentation to break.

Usage:
  python3 scripts/doc-touched.py
"""

import json
import os
import subprocess
import sys

REPO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def git(*args, check=True):
    """Stripped stdout lines of a git command, or [] when it is allowed to fail."""
    proc = subprocess.run(
        ["git", *args], cwd=REPO_DIR, capture_output=True, text=True
    )
    if proc.returncode != 0:
        if check:
            raise RuntimeError(f"git {' '.join(args)}: {proc.stderr.strip()}")
        return []
    return [line for line in proc.stdout.splitlines() if line]


def members():
    """[(package name, directory relative to the workspace root)]."""
    proc = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=REPO_DIR,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"cargo metadata: {proc.stderr.strip()}")
    meta = json.loads(proc.stdout)
    root = meta["workspace_root"]
    return [
        (pkg["name"], os.path.relpath(os.path.dirname(pkg["manifest_path"]), root))
        for pkg in meta["packages"]
    ]


def changed_paths():
    """Every path this branch has moved, committed or not."""
    paths = set()
    if git("rev-parse", "--verify", "--quiet", "origin/master", check=False):
        base = git("merge-base", "origin/master", "HEAD")[0]
        paths.update(git("diff", "--name-only", base, "HEAD"))
    paths.update(git("diff", "--name-only", "HEAD"))
    paths.update(git("ls-files", "--others", "--exclude-standard"))
    return paths


def touched_crates(member_list, paths):
    """The workspace packages those paths belong to.

    Longest matching directory wins, so a nested member (vendor/nmea0183) is
    attributed to itself rather than to whatever sits above it.
    """
    hit = set()
    for path in paths:
        best = None
        for name, directory in member_list:
            prefix = "" if directory == "." else directory + "/"
            if path.startswith(prefix) and (best is None or len(prefix) > len(best[1])):
                best = (name, prefix)
        if best:
            hit.add(best[0])
    return sorted(hit)


def main():
    crates = touched_crates(members(), changed_paths())
    if not crates:
        print("doc-touched: no workspace crate touched; nothing to document.")
        return 0

    print(f"doc-touched: {' '.join(crates)}")
    args = []
    for crate in crates:
        args += ["-p", crate]
    env = dict(os.environ, RUSTDOCFLAGS="-D warnings")
    return subprocess.run(
        ["cargo", "doc", "--no-deps", *args], cwd=REPO_DIR, env=env
    ).returncode


if __name__ == "__main__":
    sys.exit(main())
