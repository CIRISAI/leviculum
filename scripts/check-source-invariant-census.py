#!/usr/bin/env python3
"""Census gate for the source-invariant `tests/` targets.

Every `test`-kind target in the workspace must carry a verdict in
scripts/source-invariant-targets.txt: either `run` (the push gate executes it)
or `skip` (it does not, and the line says why). A target in neither fails here.

Why a census and not a hand-picked run list. Codeberg #220 put a compile check
of every target on the push path; nothing on that path executes any of them, so
a `tests/` target can be broken for as long as it takes somebody to reach
`just standard`. Naming a few files in the Justfile would fix that for the files
named on the day it was written and for no file added afterwards. The census
shape -- the one scripts/ignored-counts.txt already uses -- is what makes
forgetting the thing that goes red.

Enumeration comes from `cargo metadata`, i.e. from cargo's own view of the
workspace, so a target declared by an explicit `[[test]]` or living in
`tests/<name>/main.rs` is covered exactly like a bare `tests/<name>.rs`.

WHAT THIS GATE DOES NOT CHECK: that a target in the `run` list actually meets
the class criterion the file states. Nothing here can see a spawned daemon
behind a helper module. The criterion is enforced by the one-line reason next
to the entry, in a diff, read by a person.

Usage:
    python3 scripts/check-source-invariant-census.py           # the gate
    python3 scripts/check-source-invariant-census.py --print   # the census
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LIST_FILE = ROOT / "scripts" / "source-invariant-targets.txt"
LIST_FILE_REL = LIST_FILE.relative_to(ROOT)
VERDICTS = ("run", "skip")


def census() -> dict[tuple[str, str], str]:
    """Every workspace test target -> its source path, from cargo metadata."""
    proc = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        sys.exit("[source-invariant-census] `cargo metadata` failed")
    meta = json.loads(proc.stdout)
    found: dict[tuple[str, str], str] = {}
    for pkg in meta["packages"]:
        for target in pkg["targets"]:
            if "test" in target["kind"]:
                src = Path(target["src_path"])
                try:
                    src_rel = str(src.relative_to(ROOT))
                except ValueError:
                    src_rel = str(src)
                found[(pkg["name"], target["name"])] = src_rel
    return found


def read_verdicts() -> dict[tuple[str, str], tuple[str, str, int]]:
    """(package, target) -> (verdict, reason, line number)."""
    verdicts: dict[tuple[str, str], tuple[str, str, int]] = {}
    for lineno, raw in enumerate(LIST_FILE.read_text().splitlines(), 1):
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        body, _, reason = raw.partition("#")
        fields = body.split()
        if len(fields) != 3:
            sys.exit(
                f"[source-invariant-census] {LIST_FILE_REL}:{lineno}: expected "
                f"`<run|skip>  <package>  <target>  # <reason>`, got: {raw!r}"
            )
        verdict, pkg, target = fields
        if verdict not in VERDICTS:
            sys.exit(
                f"[source-invariant-census] {LIST_FILE_REL}:{lineno}: verdict must "
                f"be one of {VERDICTS}, got {verdict!r}"
            )
        if not reason.strip():
            sys.exit(
                f"[source-invariant-census] {LIST_FILE_REL}:{lineno}: {pkg} "
                f"{target} states no reason. The reason IS the check -- nothing "
                f"else reads whether this verdict is right."
            )
        if (pkg, target) in verdicts:
            first = verdicts[(pkg, target)][2]
            sys.exit(
                f"[source-invariant-census] {LIST_FILE_REL}:{lineno}: {pkg} "
                f"{target} already has a verdict at line {first}"
            )
        verdicts[(pkg, target)] = (verdict, reason.strip(), lineno)
    return verdicts


def run_list() -> list[tuple[str, str]]:
    """The admitted targets, for scripts/run-source-invariant-tests.py."""
    return [k for k, v in sorted(read_verdicts().items()) if v[0] == "run"]


def missing_verdict(pkg: str, target: str, src: str) -> str:
    return f"""
  {pkg} / {target}
      {src}
      has no verdict in {LIST_FILE_REL}.

A new `tests/` target is compiled by `check-all-targets` on every push and
executed by nothing before `just standard`. That is the gap this file closes,
so a target that appears without a verdict fails here rather than riding the
push path unrun. Add ONE line, with the reason:

    run   {pkg}   {target}   # <what file in the tree it asserts a property of>
  or
    skip  {pkg}   {target}   # <why it is not admissible>

`run` iff all four hold: the thing under test is a file in this tree and the
target reads it and asserts on what it read; it starts no process; it opens no
socket and touches no device; it finishes in milliseconds. A target whose
subject is the CODE -- a golden render, a vector corpus, an in-process protocol
suite -- is `skip` even when it is fast and pure, because admitting those is
`cargo test --workspace` on every push under another name.

Close to the line is `skip`, with the reason saying so. The skip list is not
the failure half, it is the honest half.
"""


def stale_verdict(pkg: str, target: str, lineno: int) -> str:
    return f"""
  {pkg} / {target}
      has a verdict at {LIST_FILE_REL}:{lineno}, but cargo knows no such test
      target. It was renamed or deleted; delete or rename the line.

A verdict left behind for a target nobody has any more is how a run list stops
matching what it claims to run: the manifest wrapper then reports a gate that
executed zero tests, at the far end, instead of this gate naming the file.
"""


def main() -> int:
    found = census()
    verdicts = read_verdicts()

    if "--print" in sys.argv[1:]:
        for (pkg, target), src in sorted(found.items()):
            verdict = verdicts.get((pkg, target), ("?", "", 0))[0]
            print(f"{verdict:<5} {pkg:<20} {target:<40} {src}")
        return 0

    failures = [
        missing_verdict(pkg, target, src)
        for (pkg, target), src in sorted(found.items())
        if (pkg, target) not in verdicts
    ]
    failures += [
        stale_verdict(pkg, target, lineno)
        for (pkg, target), (_, _, lineno) in sorted(verdicts.items())
        if (pkg, target) not in found
    ]

    if failures:
        print(
            "[source-invariant-census] FAIL: the verdicts in "
            f"{LIST_FILE_REL} no longer cover the workspace's test targets."
        )
        for failure in failures:
            print(failure)
        return 1

    runs = sum(1 for v in verdicts.values() if v[0] == "run")
    print(
        f"[source-invariant-census] OK: {len(found)} test targets, "
        f"{runs} run by `just fast`, {len(found) - runs} skipped with a reason, "
        f"all matching {LIST_FILE_REL}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
