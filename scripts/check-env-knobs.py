#!/usr/bin/env python3
"""Census gate for the environment knobs the shipped code reads.

An environment variable that changes what a daemon does is a knob nobody
configured: it is not in the config file, `lnstatus` does not report it, and a
run that was taken under one is indistinguishable afterwards from a run that
was not -- unless the code says so out loud. #347 added exactly such a knob
(`LEVICULUM_JITTER_ARM`, the three-arm A/B selector) and it is meant to be
DELETED when the experiment answers. A knob added for an experiment and then
forgotten is the shape this gate exists to prevent, in both directions:

  * a knob in the sources that no line of scripts/env-knob-census.txt
    admits fails here, naming the file and line. Adding one is a diff, and
    the diff has to say whether it is temporary and what removes it;
  * a knob the census still lists that NO source reads any more fails too.
    When the winning arm lands and the selector goes, the census line has to
    go with it -- otherwise the file slowly becomes a list of knobs that
    used to exist, which is worse than no list.

WHAT IS COUNTED: every `LEVICULUM_*` string literal in a non-test Rust source
of this workspace. Literals rather than `env::var` call sites, because the
name is routinely a `const` read somewhere else entirely
(`rnode.rs::JITTER_ARM_ENV`), and a gate that only saw call sites would miss
exactly the tidier spelling. The `LEVICULUM_` prefix is what makes a name
ours; a knob under somebody else's prefix (`RUST_LOG`, `HOME`, `I2P_*`) is
not this project's to remove.

WHAT IS NOT COUNTED: `tests/`, `benches/` and `examples/` trees, `build.rs`,
and any literal inside `env!`/`option_env!`. A test harness's own variables
(`LEVICULUM_DELIVERY_LOG`, `LEVICULUM_CITATION_FIX`) never reach a deployed
daemon; a build script's and an `env!`'s are the COMPILER's environment, read
once when the binary was built, and cannot change what that binary then does.
Holding either to a removal condition would fill the census with entries
nobody can act on. A `#[cfg(test)]` module inside a `src/` file IS counted --
finding the end of one needs a parser, and the two such knobs today are
cheaper to admit in the census, where the line says what they are, than to
exclude by a rule this script cannot enforce.

Usage:
    python3 scripts/check-env-knobs.py           # the gate
    python3 scripts/check-env-knobs.py --print   # the census, in pin format
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PIN_FILE = ROOT / "scripts" / "env-knob-census.txt"
PIN_FILE_REL = PIN_FILE.relative_to(ROOT)

KNOB = re.compile(r'(env!\s*\(\s*)?"(LEVICULUM_[A-Z0-9_]+)"')
VERDICTS = ("temporary", "permanent")
ISSUE = re.compile(r"#\d+")
SKIP_DIRS = {"target", "vendor", "reference", "tests", "benches", "examples", ".git"}


def sources() -> list[Path]:
    """Every non-test Rust source of the workspace, sorted."""
    found: list[Path] = []
    for path in ROOT.rglob("*.rs"):
        rel = path.relative_to(ROOT)
        if any(part in SKIP_DIRS for part in rel.parts) or rel.name == "build.rs":
            continue
        found.append(path)
    return sorted(found)


def census() -> dict[str, list[str]]:
    """knob -> the `<file>:<line>` sites that name it."""
    found: dict[str, list[str]] = {}
    for path in sources():
        rel = path.relative_to(ROOT)
        for number, line in enumerate(path.read_text().splitlines(), start=1):
            for macro, knob in KNOB.findall(line):
                # `env!`/`option_env!` is the COMPILER's environment, read
                # once when the binary was built: a stamped version, a git
                # sha. It cannot change what a deployed daemon does, which
                # is what a knob is.
                if macro:
                    continue
                found.setdefault(knob, []).append(f"{rel}:{number}")
    return found


def pinned() -> tuple[dict[str, tuple[str, str]], list[str]]:
    """The census file: knob -> (verdict, reason), plus its own format errors."""
    entries: dict[str, tuple[str, str]] = {}
    errors: list[str] = []
    if not PIN_FILE.exists():
        return entries, [f"{PIN_FILE_REL} is missing"]
    for number, raw in enumerate(PIN_FILE.read_text().splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 2)
        if len(parts) < 3:
            errors.append(f"{PIN_FILE_REL}:{number}: expected `<KNOB> <verdict> <reason>`")
            continue
        knob, verdict, reason = parts
        if verdict not in VERDICTS:
            errors.append(
                f"{PIN_FILE_REL}:{number}: verdict {verdict!r} is not one of {VERDICTS}"
            )
            continue
        if verdict == "temporary" and not ISSUE.search(reason):
            errors.append(
                f"{PIN_FILE_REL}:{number}: a temporary knob must name the issue it "
                f"belongs to (#NNN) and what removes it"
            )
            continue
        entries[knob] = (verdict, reason)
    return entries, errors


def main() -> int:
    found = census()
    if "--print" in sys.argv[1:]:
        for knob, sites in sorted(found.items()):
            print(f"{knob}  # {', '.join(sites)}")
        return 0

    entries, errors = pinned()
    for knob in sorted(set(found) - set(entries)):
        errors.append(
            f"{found[knob][0]}: `{knob}` is an environment knob with no verdict in "
            f"{PIN_FILE_REL} -- add a line saying whether it is temporary (and what "
            f"removes it) or permanent (and why)"
        )
    for knob in sorted(set(entries) - set(found)):
        errors.append(
            f"{PIN_FILE_REL}: `{knob}` is pinned but no source reads it any more -- "
            f"the knob is gone, so its census line goes too"
        )

    if errors:
        print("[env-knobs] FAIL")
        for error in errors:
            print(f"  {error}")
        return 1

    temporary = [k for k, (v, _) in sorted(entries.items()) if v == "temporary"]
    print(
        f"[env-knobs] OK: {len(entries)} knob(s), "
        f"{len(temporary)} temporary ({', '.join(temporary) or 'none'})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
