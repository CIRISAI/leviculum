#!/usr/bin/env python3
"""Run the `tests/` targets admitted to the source-invariant class.

The list is scripts/source-invariant-targets.txt -- the same file the census
gate reads, so there is one list and not two. This script does not decide what
is admissible; it runs what the file says, grouped into one `cargo test` per
package.

It is meant to be called THROUGH scripts/run-with-manifest.py, which the
Justfile recipe does, so the run records WHICH tests it executed (Guarantee B,
docs/src/concepts/checks-and-citations.md) and a run list that has stopped
selecting anything -- the failure the census cannot see -- executes zero tests
and is reported as a failure rather than as a pass. One wrapper around the
whole step and not one per package: the wrapper costs ~0.95 s of interpreter
and manifest I/O against ~0.9 s for all the cargo runs together, so per-package
manifests would triple the price of the step to record the same names.

Exit status is the first non-zero child's, after every package has run, and
each `cargo test` runs `--no-fail-fast`: one red target must not hide what the
others would have said, within a package or across them.

Usage:
    python3 scripts/run-source-invariant-tests.py [--list]
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

_spec = importlib.util.spec_from_file_location(
    "source_invariant_census", ROOT / "scripts" / "check-source-invariant-census.py"
)
_census = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_census)


def main() -> int:
    admitted = _census.run_list()
    if not admitted:
        print(
            "[source-invariant] FAIL: no target is admitted in "
            "scripts/source-invariant-targets.txt -- an empty run list would "
            "make this step green by running nothing."
        )
        return 1

    by_package: dict[str, list[str]] = {}
    for pkg, target in admitted:
        by_package.setdefault(pkg, []).append(target)

    if "--list" in sys.argv[1:]:
        for pkg, targets in sorted(by_package.items()):
            for target in targets:
                print(f"{pkg} {target}")
        return 0

    status = 0
    for pkg, targets in sorted(by_package.items()):
        # --no-fail-fast: one red target must not swallow the verdicts of the
        # targets behind it in the same package. Without it cargo stops at the
        # first failing binary and the manifest records a short run, which is
        # the evidence-throwing-away shape Codeberg #195 is about.
        cmd = ["cargo", "test", "--no-fail-fast", "-p", pkg]
        for target in targets:
            cmd += ["--test", target]
        print(f"[source-invariant] {pkg}: {len(targets)} target(s)", flush=True)
        rc = subprocess.run(cmd, cwd=ROOT).returncode
        if rc != 0 and status == 0:
            status = rc

    if status == 0:
        print(
            f"[source-invariant] OK: {len(admitted)} target(s) across "
            f"{len(by_package)} package(s)"
        )
    return status


if __name__ == "__main__":
    sys.exit(main())
