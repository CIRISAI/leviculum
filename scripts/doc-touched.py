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
Justfile. Nothing touched inside the workspace is a pass, not an error: there
is no documentation to break.

leviculum-nrf and leviculum-esp are their own cargo workspaces, so
`cargo metadata` here does not list them and `cargo doc -p` from this root
cannot reach them. This recipe therefore cannot document them, and until
2026-09-28 the paragraph above said it did not have to, because `lint-nrf`
and `build-esp-image` carried their own rustdoc gate. Half of that was
false: `build-esp32` (the recipe's real name) has always run
`RUSTDOCFLAGS="-D warnings" cargo doc`, `lint-nrf` never did. `just
doc-touched` was green on a batch it could not see, and when 362 measured
the gap it was 18 broken intra-doc links across the nrf host members and 50
across 11 modules of the firmware crate (Codeberg #367). `lint-nrf` now
carries the line for both halves of its workspace, per bundle feature set.

So the skip stands -- and is now announced. A branch that touched either
foreign workspace gets a line naming the recipe that docs it, because the
cost of the hole was not the skip but its silence.

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


# The foreign cargo workspaces this recipe cannot document, and the recipe that
# does. `cargo doc -p <crate>` from this root fails for both: they are not
# members here, so `cargo metadata --no-deps` never names them.
FOREIGN_WORKSPACES = {
    "leviculum-nrf/": "just lint-nrf",
    "leviculum-esp/": "just build-esp32",
}


def foreign_notes(paths):
    """[(workspace prefix, recipe)] for the foreign workspaces this branch touched."""
    return sorted(
        (prefix, recipe)
        for prefix, recipe in FOREIGN_WORKSPACES.items()
        if any(path.startswith(prefix) for path in paths)
    )


def main():
    paths = changed_paths()
    for prefix, recipe in foreign_notes(paths):
        print(
            f"doc-touched: {prefix.rstrip('/')} is its own workspace and is not "
            f"documented here; `{recipe}` carries its rustdoc gate.",
            flush=True,
        )
    crates = touched_crates(members(), paths)
    if not crates:
        print("doc-touched: no workspace crate touched; nothing to document.")
        return 0

    print(f"doc-touched: {' '.join(crates)}", flush=True)
    args = []
    for crate in crates:
        args += ["-p", crate]
    env = dict(os.environ, RUSTDOCFLAGS="-D warnings")
    return subprocess.run(
        ["cargo", "doc", "--no-deps", *args], cwd=REPO_DIR, env=env
    ).returncode


if __name__ == "__main__":
    sys.exit(main())
