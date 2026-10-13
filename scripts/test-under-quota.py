#!/usr/bin/env python3
"""Run the workspace's tests under a CPU quota and name the ones that wait on the host.

The forge runner is a shared machine. A test whose pass depends on the host
being fast (a wall-clock budget on CPU-bound work, a sleep that assumes the
other thread got scheduled) is a red waiting for a slow day: the first landing
in three days was lost to one (672, lnpnd's stamp worker against a ten second
wall budget). This script is the survey that finds the rest before the forge
does: every test binary of `cargo test --workspace`, run under
`systemd-run --user --scope -p CPUQuota=<q>`, with libtest's own per-test
duration (`--report-time`), and a table of what got slow and what went red.

    just test-under-quota                                # all of the below
    python3 scripts/test-under-quota.py run              # under 25 % of a CPU
    python3 scripts/test-under-quota.py run --quota none # the same, unconstrained
    python3 scripts/test-under-quota.py report           # table: slow + red

WHY SHARDS, AND WHY IT RESUMES
------------------------------
Under a quarter of a CPU the suite takes about an hour and a half on the
coder host, far longer than one command of a coder pass may (600 s). So the
unit of work is a shard: up to --shard-size tests of one binary, run as one
process with `--exact` filters, so each binary's tests still run in parallel
against each other the way `cargo test` runs them. Each finished shard leaves
a JSON record and `run` skips every shard that has one. With --budget it
stops launching shards that could end past that many seconds and exits 3
("not finished, run me again"); a coder pass repeats

    python3 scripts/test-under-quota.py run --budget 590 --shard-timeout 240

until it exits 0 (all green) or 1 (all ran, something red). A shard killed
at --shard-timeout reports its unfinished tests as `timeout`: before calling
one a hang, run that shard alone with a longer bound, since a shard of
serialised interop tests can simply be longer than the bound.

The test binaries are built UNCONSTRAINED first (`cargo test --workspace
--no-run`): the question is what the tests do on a slow host, not how long
rustc takes on one. The release binaries the mvr tests spawn are not built
here; `just test-under-quota` builds them first (`build-integ-bins`).

Doctests are not run: rustdoc compiles each one at test time, which under the
quota measures the compiler again, and the 33 there are one-line examples.

WHERE THE RESULTS GO
--------------------
$XDG_STATE_HOME/leviculum-ci/under-quota/<commit>[-dirty]/<quota>/, one JSON
per shard and one .log with the shard's full output (a red keeps its
`---- name stdout ----` block there). A new commit starts a new survey; the
`report` table compares the quota run against the unconstrained one of the
same commit when there is one.
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# `test foo::bar ... ok <1.234s>`; libtest prints the time only under
# `-Zunstable-options --report-time`, which the prebuilt test binary accepts
# with RUSTC_BOOTSTRAP=1 in its environment (no rustc involved). The opener and
# the status are two writes: a spawned daemon's stderr (rnsd's warnings, a
# helper's tracing) lands between them often enough under a quota that a
# one-line pattern loses a fifth of the interop suite. So the status is the
# first one after the opener, wherever it lands.
OPENER = re.compile(r"^test (?P<name>\S+)(?: - should panic)? \.\.\. ", re.M)
STATUS = re.compile(
    r"(?:(?P<st>ok|FAILED) <(?P<secs>\d+(?:\.\d+)?)s>|(?P<ign>ignored)(?:, [^\n]*)?)[ \t]*$",
    re.M,
)


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=REPO, capture_output=True, text=True, check=True
    ).stdout.strip()


def survey_dir(quota: str) -> Path:
    commit = git("rev-parse", "--short=12", "HEAD")
    if git("status", "--porcelain", "--untracked-files=no"):
        commit += "-dirty"
    state = os.environ.get("XDG_STATE_HOME") or str(Path.home() / ".local" / "state")
    tag = "none" if quota == "none" else quota.rstrip("%") + "pct"
    return Path(state) / "leviculum-ci" / "under-quota" / commit / tag


def build() -> list[dict]:
    """Build every test binary, return one entry per executable."""
    out = subprocess.run(
        ["cargo", "test", "--workspace", "--no-run", "--message-format=json"],
        cwd=REPO,
        stdout=subprocess.PIPE,
        text=True,
    )
    if out.returncode != 0:
        sys.exit(f"test-under-quota: cargo test --no-run exited {out.returncode}")
    bins = []
    for line in out.stdout.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") != "compiler-artifact" or not msg.get("executable"):
            continue
        if not msg["profile"]["test"]:
            continue
        pkg_dir = str(Path(msg["manifest_path"]).parent)
        bins.append(
            {
                "key": f"{Path(pkg_dir).name}/{msg['target']['kind'][0]}/{msg['target']['name']}",
                "exe": msg["executable"],
                "pkg_dir": pkg_dir,
                "pkg_name": msg["package_id"].split("#")[-1].split("@")[0].split("/")[-1],
            }
        )
    return bins


def list_tests(b: dict) -> list[str]:
    out = subprocess.run(
        [b["exe"], "--list", "--format", "terse"],
        cwd=b["pkg_dir"],
        capture_output=True,
        text=True,
        env=test_env(b),
    )
    return [
        line[: -len(": test")]
        for line in out.stdout.splitlines()
        if line.endswith(": test")
    ]


def test_env(b: dict) -> dict:
    env = dict(os.environ)
    # What cargo sets for a test process at run time; a test that reads them
    # through std::env rather than env!() would otherwise see nothing.
    env["CARGO_MANIFEST_DIR"] = b["pkg_dir"]
    env["CARGO_PKG_NAME"] = b["pkg_name"]
    env["RUSTC_BOOTSTRAP"] = "1"
    return env


def run_shard(b: dict, names: list[str], quota: str, timeout: float, log: Path) -> dict:
    unit = f"lev-quota-{os.getpid()}-{hashlib.sha1(log.name.encode()).hexdigest()[:10]}"
    cmd = [b["exe"], "--exact", "-Zunstable-options", "--report-time", *names]
    if quota != "none":
        cmd = [
            "systemd-run", "--user", "--scope", "--quiet", f"--unit={unit}",
            "-p", f"CPUQuota={quota}", "--", *cmd,
        ]
    start = time.monotonic()
    timed_out = False
    with open(log, "w") as fh:
        proc = subprocess.Popen(
            cmd, cwd=b["pkg_dir"], env=test_env(b), stdout=fh, stderr=subprocess.STDOUT
        )
        try:
            rc = proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            if quota != "none":
                # The scope's cgroup holds the daemons the tests spawned too.
                subprocess.run(["systemctl", "--user", "stop", f"{unit}.scope"],
                               capture_output=True)
            proc.kill()
            rc = proc.wait()
    wall = time.monotonic() - start
    results = parse_log(log, names, timed_out)
    return {"key": b["key"], "rc": rc, "timed_out": timed_out, "wall": wall,
            "tests": results, "log": str(log)}


def parse_log(log: Path, names: list[str], timed_out: bool) -> dict:
    text = log.read_text(errors="replace")
    results = {}
    openers = list(OPENER.finditer(text))
    for i, m in enumerate(openers):
        # Never past the next opener: that status is the next test's.
        end = openers[i + 1].start() if i + 1 < len(openers) else len(text)
        st = STATUS.search(text, m.end(), end)
        if st is None:
            continue
        status = (st["st"] or st["ign"]).lower()
        results[m["name"]] = {"status": status,
                              "secs": float(st["secs"]) if st["secs"] else None}
    for n in names:
        if n not in results:
            results[n] = {"status": "timeout" if timed_out else "missing", "secs": None}
    return results


def cmd_run(args) -> int:
    started = time.monotonic()
    out = survey_dir(args.quota)
    out.mkdir(parents=True, exist_ok=True)
    bins = build()
    pending = 0
    reds = 0
    for b in sorted(bins, key=lambda b: b["key"]):
        if args.only and not any(o in b["key"] for o in args.only):
            continue
        if not Path(b["exe"]).exists():
            # The host's hourly disk hygiene (`cargo sweep --maxsize`) can
            # take binaries out from under a survey that runs for hours.
            b = {x["key"]: x for x in build()}[b["key"]]
        names = list_tests(b)
        for i in range(0, len(names), args.shard_size):
            chunk = names[i : i + args.shard_size]
            sid = hashlib.sha1("\0".join(chunk).encode()).hexdigest()[:10]
            stem = f"{b['key'].replace('/', '__')}.{i // args.shard_size}.{sid}"
            rec_path = out / f"{stem}.json"
            if rec_path.exists():
                rec = json.loads(rec_path.read_text())
            else:
                if args.budget and time.monotonic() - started + args.shard_timeout > args.budget:
                    pending += 1
                    continue
                rec = run_shard(b, chunk, args.quota, args.shard_timeout, out / f"{stem}.log")
                rec_path.write_text(json.dumps(rec, indent=1))
                print(f"{b['key']} [{i // args.shard_size}] {len(chunk)} tests "
                      f"{rec['wall']:.1f}s rc={rec['rc']}", flush=True)
            reds += sum(t["status"] not in ("ok", "ignored") for t in rec["tests"].values())
    print(f"test-under-quota: {out}")
    if pending:
        print(f"test-under-quota: {pending} shards not run yet (budget); run again")
        return 3
    print(f"test-under-quota: complete, {reds} red")
    return 1 if reds else 0


def load(out: Path) -> dict:
    tests = {}
    for p in sorted(out.glob("*.json")):
        rec = json.loads(p.read_text())
        # Re-read the log: the record is what the parser of the day made of it.
        log = Path(rec["log"])
        if log.exists():
            rec["tests"] = parse_log(log, list(rec["tests"]), rec["timed_out"])
        for name, t in rec["tests"].items():
            tests[(rec["key"], name)] = dict(t, log=rec["log"])
    return tests


def cmd_report(args) -> int:
    quota = load(survey_dir(args.quota))
    base = load(survey_dir("none"))
    if not quota:
        sys.exit(f"test-under-quota: no survey under {survey_dir(args.quota)}")
    rows = [
        (k, t) for k, t in quota.items()
        if t["status"] not in ("ok", "ignored") or (t["secs"] or 0) >= args.threshold
    ]
    rows.sort(key=lambda kt: -(kt[1]["secs"] or 1e9))
    print(f"| test | target | under {args.quota} | normal | status |")
    print("|---|---|---|---|---|")
    for (key, name), t in rows:
        b = base.get((key, name), {})
        q = f"{t['secs']:.1f} s" if t["secs"] is not None else "-"
        n = f"{b['secs']:.1f} s" if b.get("secs") is not None else "-"
        print(f"| `{name}` | {key} | {q} | {n} | {t['status']} |")
    ran = sum(t["status"] != "ignored" for t in quota.values())
    red = sum(t["status"] not in ("ok", "ignored") for t in quota.values())
    print(f"\n{ran} tests ran under {args.quota}, {red} red, "
          f"{len(rows) - red} green at or above {args.threshold:.0f} s")
    return 1 if red else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="run (or resume) the survey")
    r.add_argument("--quota", default="25%", help="systemd CPUQuota, or `none`")
    r.add_argument("--budget", type=float, default=0.0,
                   help="stop launching shards once one could end past this many "
                        "seconds, and exit 3 (0: run to the end)")
    # Eight because the interop suite serialises its tests: a shard of
    # sixteen took 250 s under 25 %, which is most of a coder command.
    r.add_argument("--shard-size", type=int, default=8)
    r.add_argument("--shard-timeout", type=float, default=600.0,
                   help="a shard still running after this is killed and its "
                        "unfinished tests reported as `timeout`")
    r.add_argument("--only", action="append", help="substring of a target key")
    p = sub.add_parser("report", help="table of slow and red tests")
    p.add_argument("--quota", default="25%")
    p.add_argument("--threshold", type=float, default=10.0)
    args = ap.parse_args()
    return cmd_run(args) if args.cmd == "run" else cmd_report(args)


if __name__ == "__main__":
    sys.exit(main())
