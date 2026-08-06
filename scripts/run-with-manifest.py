#!/usr/bin/env python3
"""Run a test command and record what it actually EXECUTED (Guarantee B, step 1).

docs/src/concepts/checks-and-citations.md §Guarantee B: half of it exists --
scripts/check-ignored-counts.py pins how many tests are #[ignore]d. Nothing
says whether the rest are executed by anything. Two suites had been running
nowhere for a month (Codeberg #189) while every gate was green.

This script is the first half of the other half: a gate wrapper that emits a
manifest of the test names its command ran. Step 2 (not here) takes the union
of those manifests and reports every test that exists in no manifest.

    python3 scripts/run-with-manifest.py --gate mvr -- \
        cargo test -p leviculum-std --test mvr -- --test-threads=1

The command's stdout and stderr are merged and passed straight through, and
its exit status is this script's exit status, so a wrapped gate behaves like
the bare one.

THE FULL OUTPUT IS ALSO KEPT ON DISK
------------------------------------
Passing output through is not the same as preserving it. Whatever runs a gate
decides how much of its stdout survives, and a caller that summarised a red run
with `tail -6` threw away the only assertion diff it produced (Codeberg #195);
re-running turned the run green and the evidence was gone for good. So every
invocation also streams the merged output to <gate>.log next to the manifest,
line by line (a killed run keeps what it printed), and a run that exits non-zero
copies it to <gate>.failed.log, which nothing but the NEXT failure overwrites.
Green runs cannot clobber the last red one, which is the case that mattered.

WHY RUN OUTPUT AND NOT `cargo test --list`
------------------------------------------
A list records intent. `cargo test -- --exact <typo>` matches nothing, runs
zero tests and exits 0, so a by-name gate reads green whether or not it
measured anything -- scripts/run-status-parity.sh closes exactly that hole by
hand for itself (EXPECTED=3, parsed back out of the summary line). Parsing the
run generalises that fix: the manifest cannot contain a test that did not run,
and the reconciliation below cannot pass if it does.

WHERE THE MANIFESTS GO
----------------------
$LEVICULUM_MANIFEST_DIR, else
$XDG_STATE_HOME/leviculum-ci/test-manifests/<repo-slug>/<gate>.json
(default $HOME/.local/state). Not in the repo: this is run state, not source,
and a checked-in artefact gets diffed, blessed and eventually committed
wrong. Not in target/: `cargo clean` deletes it, and the tiers run with
different CARGO_TARGET_DIR values, which would silently split one union into
several. ~/.local/state/leviculum-ci/ is where this repo already keeps CI run
state (last-results.txt, the tier logs, scripts/check-tier2-staleness.sh).

<repo-slug> is <dirname>-<hash of the real repo path>: two checkouts on one
host (the CI tree and the rig worktree) must not overwrite each other's
manifests, or step 2 reads a union assembled from two different trees.

The file carries gate, command, commit, dirtiness, host, repo and both
timestamps, which is what step 2 needs to age a manifest out the way
check-tier2-staleness.sh ages out a tier-2 result.

STANDING CANARY
---------------
canary() runs before anything real is parsed, on every invocation. It feeds
the parser a fixture holding tests that must appear in a manifest and tests
that must never, plus a deliberately miscounted unit that the reconciler must
reject. A manifest writer that silently stops writing -- a libtest format
change, a regex that stops matching -- is green forever, which is the defect
the concept page exists to remove; a one-time demonstration at implementation
time decays.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shlex
import socket
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

SCHEMA = 1

# libtest, one result per line: `test <name> ... <status>`. The name may hold
# spaces (doc-tests: `leviculum-micron/src/lib.rs - (line 14)`), so the name is
# non-greedy up to the first ` ... `.
RESULT_LINE = re.compile(r"^test (?P<name>.+?) \.\.\. ?(?P<rest>.*)$")
# libtest decorates a #[should_panic] test as `test <name> - should panic ...`,
# while `--list` (what step 2 enumerates with) prints the bare name. Recording
# the decorated form would leave three real leviculum-core tests looking unrun
# forever, and no count would disagree -- the manifest would be wrong in a way
# only a name-level join can see. Found exactly that way, on this script's
# first union.
DECORATION = re.compile(r" - should panic$")
# `running 12 tests` / `running 1 test`
RUNNING_LINE = re.compile(r"^running (?P<count>\d+) tests?$")
# `test result: ok. 12 passed; 0 failed; 3 ignored; 0 measured; 5 filtered out; ...`
SUMMARY_LINE = re.compile(
    r"^test result: \w+\.\s*(?P<passed>\d+) passed;\s*(?P<failed>\d+) failed;"
    r"\s*(?P<ignored>\d+) ignored;\s*(?P<measured>\d+) measured;"
    r"\s*(?P<filtered>\d+) filtered out"
)
# cargo, on stderr: `     Running unittests src/lib.rs (target/.../deps/x-hash)`
CARGO_RUNNING = re.compile(r"^\s*Running (?P<desc>.+?) \((?P<exe>[^()]+)\)$")
# cargo, on stderr: `   Doc-tests leviculum_micron`
CARGO_DOCTESTS = re.compile(r"^\s*Doc-tests (?P<crate>\S+)$")
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")

# `ignored` may carry a reason (`ignored, needs hardware`); `ok` may carry a
# time with --report-time. Prefix match, so both are covered.
STATUSES = (("ok", "ok"), ("FAILED", "failed"), ("ignored", "ignored"), ("bench", "ok"))


def classify(rest: str, glued: bool = False) -> str | None:
    """Map the tail of a libtest result line to ok/failed/ignored, or None.

    None means the status has not been printed yet: with
    `--test-threads=1 --nocapture` libtest writes `test <name> ... `, lets the
    test print, and writes `ok` afterwards, so the status arrives on a later
    line -- or glued to the end of the test's own output, when what the test
    printed did not end in a newline. `glued` allows that second reading, and
    is only set where the line already began with a `test <name> ... ` prefix.
    """
    rest = rest.strip()
    if not rest:
        return None
    for token, status in STATUSES:
        if rest.startswith(token):
            return status
    if glued:
        for token, status in STATUSES:
            if rest.endswith(token):
                return status
    return None


class Unit:
    """One test executable's run: `-p leviculum-std --test mvr` and its names."""

    def __init__(self, selector: str, descriptor: str) -> None:
        self.selector = selector
        self.descriptor = descriptor
        self.ok: list[str] = []
        self.failed: list[str] = []
        self.ignored: list[str] = []
        self.unresolved: list[str] = []
        self.summary: dict[str, int] | None = None

    def record(self, name: str, status: str) -> None:
        getattr(self, status).append(name)

    def as_json(self) -> dict:
        return {
            "selector": self.selector,
            "descriptor": self.descriptor,
            # Executed = ok + failed. A failing test ran; an ignored one did
            # not. Kept as three lists rather than one, so step 2 can join on
            # the union without losing why a name is in it.
            "ok": sorted(self.ok),
            "failed": sorted(self.failed),
            "ignored": sorted(self.ignored),
            "unresolved": sorted(self.unresolved),
            "summary": self.summary,
        }

    def reconcile(self) -> str | None:
        """Compare the parsed names against libtest's own summary line."""
        if self.summary is None:
            return (
                f"{self.selector}: libtest printed no `test result:` summary. "
                f"The unit did not finish, or the output format changed."
            )
        got = (len(self.ok), len(self.failed), len(self.ignored))
        want = (
            self.summary["passed"] + self.summary["measured"],
            self.summary["failed"],
            self.summary["ignored"],
        )
        if got != want:
            return (
                f"{self.selector}: parsed {got[0]} passed / {got[1]} failed / "
                f"{got[2]} ignored, libtest reported {want[0]} / {want[1]} / "
                f"{want[2]}."
            )
        if self.unresolved:
            return (
                f"{self.selector}: {len(self.unresolved)} test(s) whose result "
                f"was never printed: {', '.join(sorted(self.unresolved)[:5])}"
            )
        return None


def selector_for(pkg: str, kinds: list[str], name: str) -> str:
    """The cargo selector that runs one test unit.

    Byte-identical in behaviour to selector_for() in
    scripts/check-ignored-counts.py: step 2 joins the two on this string, so
    the two must not drift. Change one, change the other.
    """
    for kind, flag in (("lib", "--lib"), ("proc-macro", "--lib")):
        if kind in kinds:
            return f"-p {pkg} {flag}"
    for kind in ("test", "bin", "example", "bench"):
        if kind in kinds:
            return f"-p {pkg} --{kind} {name}"
    return f"-p {pkg} --{kinds[0]} {name}"


def target_index() -> tuple[dict[tuple[str, str], str], dict[str, str]]:
    """Two lookups built from `cargo metadata`, no build required.

    * (source path relative to the package, underscored target name) -> selector.
      Both halves are needed: `src/lib.rs` repeats in every package, and
      leviculum-proxy's lib and bin targets share the executable stem
      `lora_proxy`. The pair is unique across this workspace for every
      testable target (only build scripts collide, and they produce no tests).
    * underscored lib target name -> package, for `Doc-tests <crate>`, whose
      header names the crate and not the package (`lora_proxy`, not
      `leviculum-proxy`).
    """
    proc = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        return {}, {}
    meta = json.loads(proc.stdout)
    by_src: dict[tuple[str, str], str] = {}
    by_crate: dict[str, str] = {}
    for pkg in meta["packages"]:
        pkg_dir = Path(pkg["manifest_path"]).parent
        for target in pkg["targets"]:
            kinds = target["kind"]
            if "custom-build" in kinds:
                continue
            try:
                rel = str(Path(target["src_path"]).relative_to(pkg_dir))
            except ValueError:
                rel = target["src_path"]
            stem = target["name"].replace("-", "_")
            by_src[(rel, stem)] = selector_for(pkg["name"], kinds, target["name"])
            # Any library kind can carry doc-tests, not just `lib`:
            # leviculum-ffi's is [cdylib, staticlib, rlib] and its header
            # reads `Doc-tests leviculum`.
            if {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"} & set(kinds):
                by_crate[stem] = pkg["name"]
    return by_src, by_crate


class Parser:
    """Streaming libtest/cargo output parser. One instance per run."""

    def __init__(self, by_src: dict[tuple[str, str], str], by_crate: dict[str, str]):
        self.by_src = by_src
        self.by_crate = by_crate
        self.units: list[Unit] = []
        self.current: Unit | None = None
        self.pending: str | None = None
        self.warnings: list[str] = []

    def _unit(self, selector: str, descriptor: str) -> None:
        self.current = Unit(selector, descriptor)
        self.units.append(self.current)
        self.pending = None

    def _from_running(self, desc: str, exe: str) -> None:
        # `unittests src/lib.rs` / `tests/mvr/main.rs`
        src = desc.split(" ", 1)[1] if desc.startswith("unittests ") else desc
        stem = re.sub(r"-[0-9a-f]{6,}$", "", Path(exe).name)
        selector = self.by_src.get((src, stem))
        if selector is None:
            selector = f"unresolved {src} ({stem})"
            self.warnings.append(
                f"could not map `{desc}` ({stem}) to a cargo selector; recorded "
                f"as `{selector}`"
            )
        self._unit(selector, desc)

    def feed(self, raw: str) -> None:
        line = ANSI.sub("", raw).rstrip("\n")

        m = CARGO_RUNNING.match(line)
        if m:
            self._from_running(m.group("desc"), m.group("exe"))
            return
        m = CARGO_DOCTESTS.match(line)
        if m:
            crate = m.group("crate")
            pkg = self.by_crate.get(crate)
            selector = f"-p {pkg} --doc" if pkg else f"unresolved doc {crate}"
            if pkg is None:
                self.warnings.append(f"unknown doc-test crate `{crate}`")
            self._unit(selector, f"Doc-tests {crate}")
            return

        if self.current is None:
            if not RESULT_LINE.match(line):
                return
            # A test binary invoked directly, with no cargo header above it.
            self.warnings.append(
                "test results appeared before any `Running`/`Doc-tests` header; "
                "recorded under `unattributed`"
            )
            self._unit("unattributed", "no cargo header")

        if RUNNING_LINE.match(line):  # `running 12 tests`, the unit's header
            return

        m = SUMMARY_LINE.match(line)
        if m:
            self._resolve_pending(line)
            self.current.summary = {k: int(v) for k, v in m.groupdict().items()}
            return

        m = RESULT_LINE.match(line)
        if m:
            name = DECORATION.sub("", m.group("name"))
            rest = m.group("rest")
            if self.pending is not None:
                # The previous test's status never arrived on its own line.
                self.current.unresolved.append(self.pending)
                self.pending = None
            status = classify(rest, glued=True)
            if status is None:
                self.pending = name
            else:
                self.current.record(name, status)
            return

        self._resolve_pending(line)

    def _resolve_pending(self, line: str) -> None:
        """A held-back `test <name> ... ` whose status arrives on a later line.

        Only reachable under `--test-threads=1 --nocapture`, where the test's
        own output lands between the two halves of its result line.
        """
        if self.pending is None or self.current is None:
            return
        status = classify(line)
        if status is not None:
            self.current.record(self.pending, status)
            self.pending = None

    def finish(self) -> None:
        if self.pending is not None and self.current is not None:
            self.current.unresolved.append(self.pending)
            self.pending = None


# --- standing canary --------------------------------------------------------
#
# Runs before the real command, every invocation. Two halves, as
# docs/src/concepts/checks-and-citations.md §Standing canaries requires: a test
# that must always show up in a manifest, and tests that must never.

CANARY_LINES = [
    "     Running tests/canary.rs (target/debug/deps/canary-0123456789abcdef)",
    "",
    "running 5 tests",
    "test canary::must_appear_ok ... ok",
    "test canary::must_appear_should_panic - should panic ... ok",
    "test canary::must_appear_failed ... FAILED",
    "test canary::must_never_appear_ignored ... ignored, no hardware on this bench",
    "    test canary::must_never_appear_indented ... ok",
    "note: test canary::must_never_appear_prose ... ok",
    "test canary::must_appear_split ... ",
    "some output the test printed in --nocapture mode",
    "ok",
    "",
    # Deliberately miscounted: three tests passed above, this claims five. The
    # reconciler must reject it, and must accept the corrected counts below.
    "test result: FAILED. 5 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; "
    "finished in 0.01s",
]

CANARY_MUST_APPEAR = {
    "canary::must_appear_ok",
    "canary::must_appear_split",
    # Under its `--list` name, not libtest's decorated one.
    "canary::must_appear_should_panic",
}
CANARY_MUST_NOT_APPEAR_DECORATED = "canary::must_appear_should_panic - should panic"
CANARY_MUST_NOT_APPEAR = {
    "canary::must_never_appear_ignored",
    "canary::must_never_appear_indented",
    "canary::must_never_appear_prose",
}
CANARY_TRUE_SUMMARY = {
    "passed": 3,
    "failed": 1,
    "ignored": 1,
    "measured": 0,
    "filtered": 0,
}


def canary() -> bool:
    """True if the parser still sees what it exists to see."""
    parser = Parser({("tests/canary.rs", "canary"): "-p canary --test canary"}, {})
    for line in CANARY_LINES:
        parser.feed(line)
    parser.finish()

    def fail(msg: str) -> bool:
        sys.stderr.write(f"[manifest] CANARY FAILED -- {msg}\n")
        sys.stderr.write(
            "[manifest]   The manifest writer cannot see what it exists to record.\n"
            "[manifest]   libtest's output format changed under the parser, or the\n"
            "[manifest]   parser was edited. Fix it; do not skip this check: a\n"
            "[manifest]   writer that silently stops writing is green forever.\n"
        )
        return False

    if len(parser.units) != 1:
        return fail(f"expected 1 unit from the fixture, parsed {len(parser.units)}")
    unit = parser.units[0]
    if unit.selector != "-p canary --test canary":
        return fail(f"the `Running` line no longer resolves: {unit.selector!r}")

    executed = set(unit.ok) | set(unit.failed)
    missing = CANARY_MUST_APPEAR - executed
    if missing:
        return fail(f"tests that must always be recorded were not: {sorted(missing)}")
    if "canary::must_appear_failed" not in unit.failed:
        return fail("a FAILED test was not recorded as executed-and-failed")
    if CANARY_MUST_NOT_APPEAR_DECORATED in executed:
        return fail(
            "a #[should_panic] test was recorded under libtest's decorated name, "
            "which no `--list` enumeration will ever match"
        )
    present = CANARY_MUST_NOT_APPEAR & executed
    if present:
        return fail(f"tests that must never be recorded were: {sorted(present)}")
    if "canary::must_never_appear_ignored" not in unit.ignored:
        return fail("an ignored test was not recorded as ignored")

    # The reconciler is the other half of the guarantee: it is what makes "the
    # manifest matches what actually ran" mechanical rather than asserted. The
    # fixture's summary claims four passing tests where two are in it, so a
    # working reconciler must reject it -- and must accept the true counts.
    if unit.reconcile() is None:
        return fail("the reconciler accepted a unit whose summary disagrees with it")
    unit.summary = dict(CANARY_TRUE_SUMMARY)
    if (problem := unit.reconcile()) is not None:
        return fail(f"the reconciler rejected a consistent unit: {problem}")
    return True


# --- manifest ---------------------------------------------------------------


def manifest_dir() -> Path:
    override = os.environ.get("LEVICULUM_MANIFEST_DIR")
    if override:
        return Path(override)
    state = os.environ.get("XDG_STATE_HOME") or str(Path.home() / ".local" / "state")
    slug = f"{ROOT.name}-{hashlib.sha1(str(ROOT).encode()).hexdigest()[:8]}"
    return Path(state) / "leviculum-ci" / "test-manifests" / slug


def git(*args: str) -> str:
    proc = subprocess.run(
        ["git", "-C", str(ROOT), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    return proc.stdout.strip() if proc.returncode == 0 else ""


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Run a test command and record the tests it executed."
    )
    ap.add_argument("--gate", required=True, help="manifest name, e.g. mvr")
    ap.add_argument("command", nargs=argparse.REMAINDER)
    args = ap.parse_args()

    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        ap.error("no command given (use `--gate <name> -- <command>`)")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", args.gate):
        ap.error(f"gate name {args.gate!r} must be lowercase kebab-case")

    if not canary():
        return 1

    by_src, by_crate = target_index()
    parser = Parser(by_src, by_crate)

    env = dict(os.environ)
    if sys.stdout.isatty():
        # The command's output goes through a pipe now, so cargo would drop
        # colour that the unwrapped gate had. ANSI is stripped before parsing.
        env.setdefault("CARGO_TERM_COLOR", "always")

    out_dir = manifest_dir()
    out_dir.mkdir(parents=True, exist_ok=True)
    log_path = out_dir / f"{args.gate}.log"
    failed_log_path = out_dir / f"{args.gate}.failed.log"

    started = time.time()
    proc = subprocess.Popen(
        command,
        cwd=os.getcwd(),
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        errors="replace",
    )
    assert proc.stdout is not None
    # Line-buffered and flushed per line: a run that is killed, times out or
    # panics its way out still leaves everything it had printed on disk.
    with open(log_path, "w", errors="replace") as log:
        log.write(f"$ {shlex.join(command)}\n")
        log.flush()
        for raw in proc.stdout:
            sys.stdout.write(raw)
            sys.stdout.flush()
            log.write(raw)
            log.flush()
            parser.feed(raw)
        rc = proc.wait()
    parser.finish()
    finished = time.time()

    if rc != 0:
        # Only a failure overwrites the failure log, so the green runs that
        # follow a red one cannot erase what the red one printed.
        failed_log_path.write_text(log_path.read_text(errors="replace"), errors="replace")

    executed = sum(len(u.ok) + len(u.failed) for u in parser.units)
    for warning in parser.warnings:
        sys.stderr.write(f"[manifest] WARNING: {warning}\n")

    payload = {
        "schema": SCHEMA,
        "gate": args.gate,
        "command": command,
        "repo": str(ROOT),
        "commit": git("rev-parse", "HEAD"),
        "dirty": bool(git("status", "--porcelain")),
        "host": socket.gethostname(),
        "started": datetime.fromtimestamp(started, timezone.utc).isoformat(),
        "finished": datetime.fromtimestamp(finished, timezone.utc).isoformat(),
        "duration_s": round(finished - started, 3),
        "exit_code": rc,
        "executed": executed,
        "log": str(log_path),
        "failed_log": str(failed_log_path) if rc != 0 else None,
        "units": [u.as_json() for u in parser.units],
    }

    out = out_dir / f"{args.gate}.json"
    tmp = out.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(payload, indent=2, sort_keys=False) + "\n")
    os.replace(tmp, out)

    print(
        f"[manifest] {args.gate}: {executed} tests executed across "
        f"{len(parser.units)} unit(s) -> {out}"
    )
    print(f"[manifest] full output: {log_path}")

    if rc != 0:
        # The gate failed on its own terms. The manifest of a failed run is
        # still written -- a failing test did execute -- but nothing below is
        # asserted about a run that did not finish.
        failed = sorted(n for u in parser.units for n in u.failed)
        sys.stderr.write(
            f"[manifest] {args.gate}: exit {rc}"
            + (f", failing: {', '.join(failed)}" if failed else "")
            + "\n"
        )
        sys.stderr.write(
            f"[manifest] FULL OUTPUT OF THIS FAILURE: {failed_log_path}\n"
            "[manifest] It is kept until the next failure of this gate, whatever\n"
            "[manifest] the caller did with the stdout above (Codeberg #195).\n"
        )
        return rc

    problems = [p for p in (u.reconcile() for u in parser.units) if p]
    if problems:
        sys.stdout.flush()
        sys.stderr.write(
            "[manifest] FAILED: the manifest does not match what libtest said ran.\n"
        )
        for problem in problems:
            sys.stderr.write(f"[manifest]   {problem}\n")
        sys.stderr.write(
            "[manifest] The command passed; the record of it is wrong, which is\n"
            "[manifest] worse than no record. Fix the parser in "
            "scripts/run-with-manifest.py.\n"
        )
        return 1

    if executed == 0:
        sys.stdout.flush()
        sys.stderr.write(
            f"[manifest] FAILED: `{' '.join(command)}` exited 0 and executed no "
            f"tests.\n"
            "[manifest] That is the hazard this whole mechanism exists for: a\n"
            "[manifest] by-name selector that matches nothing runs nothing and\n"
            "[manifest] exits 0, so the gate reads green having measured nothing.\n"
            "[manifest] Fix the selector, or do not wrap a command that runs no\n"
            "[manifest] tests.\n"
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
