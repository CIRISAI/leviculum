#!/usr/bin/env python3
"""The window bench's A/B (1441, 1470, `just window-bench`): rounds of the
bench of BASE and of this tree, the medians of each size and gesture
compared, and a verdict only where the host's noise allows one.

A WINDOW_BENCH line is `size= case= fps= frame_ms_mean= p95= ...`. Held is
`frame_ms_mean`, as `just map-bench` holds its columns: this tree's median
more than 5 % and more than 0.1 ms above BASE's is a regression, and the
exit is 1. Three things refuse that verdict (1470, measured 2026-11-07 on
schneckenschreck: one binary against itself, three rounds at load 0.4 to
4.9, held rows from -25.8 % to +11.8 %, seven false regressions):

- the 1-minute load at the start of any side of any round above the bound
  (`--max-load`): the whole run is SKIP, exit 3, the table still printed;
- fewer than three rounds on a side: SKIP, exit 3, a median of two is a
  mean and one round is an anecdote;
- a row whose rounds on either side spread (max - min over the median)
  more than the 5 % threshold: that row is "noise", no verdict, because a
  rule that cannot tell one binary from itself at that row cannot tell two
  apart there either.

A gesture that drew fewer than 30 frames on either side holds nothing. The
chat's mouse drag rows (`chat-drag`, `real-drag`, with or without `+fps`)
are left out of the table: iced 0.14 does not scroll on a drag and they
draw 0 frames at every size (1444 turns the drag into a selection).

Usage:
  window-bench-compare.py --base FILE... --this FILE...
      [--load FILE --max-load X] [--mode ab|aa]
  window-bench-compare.py --check-exes ab|aa BASE_EXE THIS_EXE
  window-bench-compare.py --selftest

`--load FILE` holds one `SIDE ROUND LOAD1` line per side and round, as
scripts/window-bench.sh writes it. `--mode aa` is the A/A selftest of one
binary against itself (`just window-bench HEAD`): a regression there is a
defect of this rule, and says so. `--check-exes` prints both exes' sha256
and, in an A/B, fails when they are the same file content: the A/B of
2026-11-07 compared one binary with itself because cargo took BASE's
artefacts as fresh for HEAD.

Exit: 0 green, 1 regression (or exes equal in an A/B), 2 usage, 3 SKIP.
"""

import hashlib
import io
import os
import statistics
import sys
import tempfile

SHARE = 0.05
FLOOR_MS = 0.1
MIN_FRAMES = 30
MIN_ROUNDS = 3
DROPPED = ("chat-drag", "real-drag")
SKIP = 3


def read(paths):
    rows = {}
    for path in paths:
        with open(path, encoding="utf-8") as f:
            for line in f:
                if not line.startswith("WINDOW_BENCH "):
                    continue
                fields = dict(w.split("=", 1) for w in line.split()[1:])
                key = (fields["size"], fields["case"])
                rows.setdefault(key, []).append(fields)
    return rows


def read_loads(path):
    loads = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            parts = line.split()
            if len(parts) == 3:
                loads.append((parts[0], int(parts[1]), float(parts[2])))
    return loads


def median(rows, name):
    return statistics.median(float(r[name]) for r in rows)


def spread(rows, name):
    values = [float(r[name]) for r in rows]
    mid = statistics.median(values)
    return (max(values) - min(values)) / mid if mid > 0 else 0.0


def dropped(case):
    return case.split("+", 1)[0] in DROPPED


def compare(base, this, loads, max_load, mode, out):
    """The table and the verdict to `out`; the exit status."""
    print("| size | case | fps base | fps this | ms base | ms this | p95 base | p95 this | present this | damage | spread base | spread this | change |", file=out)
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|", file=out)
    regressions, noise, gone = [], [], set()
    for key in sorted(this, key=lambda k: (int(k[0].split("x")[0]), k[1])):
        if dropped(key[1]):
            gone.add(key[1])
            continue
        now = this[key]
        then = base.get(key)
        ms = median(now, "frame_ms_mean")
        if then is None:
            print(f"| {key[0]} | {key[1]} | - | {median(now, 'fps'):.0f} | - | {ms:.2f} | - | {median(now, 'p95'):.2f} | {median(now, 'present'):.2f} | {median(now, 'damage'):.2f} | - | {spread(now, 'frame_ms_mean') * 100:.1f} % | new |", file=out)
            continue
        was = median(then, "frame_ms_mean")
        change = (ms - was) / was * 100 if was > 0 else 0.0
        enough = min(median(now, "frames"), median(then, "frames")) >= MIN_FRAMES
        sb, st = spread(then, "frame_ms_mean"), spread(now, "frame_ms_mean")
        noisy = enough and max(sb, st) > SHARE
        held = enough and not noisy and ms > was * (1 + SHARE) and ms - was > FLOOR_MS
        if held:
            regressions.append(f"{key[0]} {key[1]}: {was:.2f} -> {ms:.2f} ms ({change:+.1f} %)")
        if noisy:
            noise.append(key)
        verdict = " REGRESSION" if held else " noise" if noisy else "" if enough else " (not held)"
        print(
            f"| {key[0]} | {key[1]} | {median(then, 'fps'):.0f} | {median(now, 'fps'):.0f} "
            f"| {was:.2f} | {ms:.2f} | {median(then, 'p95'):.2f} | {median(now, 'p95'):.2f} "
            f"| {median(now, 'present'):.2f} | {median(now, 'damage'):.2f} "
            f"| {sb * 100:.1f} % | {st * 100:.1f} % | {change:+.1f} %{verdict} |",
            file=out,
        )
    if gone:
        print(f"\nLeft out: {', '.join(sorted(gone))} (iced 0.14 does not scroll on a mouse drag, 0 frames; 1444).", file=out)
    if noise:
        print(f"\n{len(noise)} rows noise: their rounds spread more than {SHARE * 100:.0f} % on a side, no verdict.", file=out)
    if loads:
        shown = ", ".join(f"{side} {i}: {load:.2f}" for side, i, load in loads)
        print(f"\nLoad (1 min) at each start, bound {max_load:.2f}: {shown}", file=out)
    rounds = min((len(r) for r in list(this.values()) + list(base.values())), default=0)
    skip = []
    if rounds < MIN_ROUNDS:
        skip.append(f"{rounds} rounds on a side, a verdict needs {MIN_ROUNDS}")
    over = [(side, i, load) for side, i, load in loads if max_load is not None and load > max_load]
    if over:
        side, i, load = max(over, key=lambda o: o[2])
        skip.append(f"load {load:.2f} at the start of {side} round {i} is above the bound {max_load:.2f}")
    if skip:
        print(f"\nwindow-bench: SKIP, no verdict: {'; '.join(skip)}.", file=out)
        return SKIP
    if regressions:
        if mode == "aa":
            print("\nwindow-bench: A/A, one binary against itself, and the rule still called regressions; the noise rule is wrong:", file=out)
        else:
            print("\nwindow-bench: frame time regressions (more than 5 % and 0.1 ms):", file=out)
        for r in regressions:
            print(f"  {r}", file=out)
        return 1
    print(f"\nwindow-bench: {'A/A ' if mode == 'aa' else ''}green, no row regressed.", file=out)
    return 0


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def check_exes(mode, base_exe, this_exe, out):
    sb, st = sha256(base_exe), sha256(this_exe)
    print(f"base exe {sb} {base_exe}", file=out)
    print(f"this exe {st} {this_exe}", file=out)
    if mode == "ab" and sb == st:
        print("window-bench: BASE and this tree differ but their exes are the same bytes; the A/B would compare one binary with itself.", file=out)
        return 1
    return 0


def _line(size, case, ms, frames=200):
    return f"WINDOW_BENCH size={size} case={case} fps=60 frame_ms_mean={ms} p95={ms} max={ms} update=0 draw=0 present=0 damage=0 frames={frames}\n"


def selftest():
    """Every verdict, from synthetic rounds; the positive controls first."""
    failures = []
    with tempfile.TemporaryDirectory() as tmp:

        def rounds(side, values, case="chat-wheel", extra=""):
            paths = []
            for i, ms in enumerate(values, 1):
                p = os.path.join(tmp, f"{side}-{case}-{i}.txt")
                with open(p, "w", encoding="utf-8") as f:
                    f.write(_line("900x560", case, ms) + extra)
                paths.append(p)
            return read(paths)

        def run(name, want, base, this, loads=(), max_load=2.0, mode="ab", says=None):
            buf = io.StringIO()
            got = compare(base, this, list(loads), max_load, mode, buf)
            if got != want or (says and says not in buf.getvalue()):
                failures.append(f"{name}: exit {got}, want {want}\n{buf.getvalue()}")

        # Positive control: tight rounds, 20 % slower, is a regression.
        run("regression", 1, rounds("b", [10.0, 10.1, 10.0]), rounds("t", [12.0, 12.1, 12.0]), says="REGRESSION")
        # The same in an A/A says the rule is wrong.
        run("aa regression", 1, rounds("b", [10.0, 10.1, 10.0]), rounds("t", [12.0, 12.1, 12.0]), mode="aa", says="noise rule is wrong")
        # One binary, tight rounds: green.
        run("aa green", 0, rounds("b", [10.0, 10.2, 10.1]), rounds("t", [10.1, 10.0, 10.2]), mode="aa", says="A/A green")
        # 2026-11-07's shape: a side whose rounds spread 25 % is noise, not a regression.
        run("noise", 0, rounds("b", [10.0, 12.6, 10.2]), rounds("t", [11.5, 10.1, 12.0]), says=" noise")
        # Load above the bound at one start: SKIP even with a real regression.
        run("load", SKIP, rounds("b", [10.0, 10.0, 10.0]), rounds("t", [12.0, 12.0, 12.0]),
            loads=[("base", 1, 0.4), ("this", 1, 0.5), ("base", 2, 4.9)], says="above the bound")
        # Under the bound: the verdict stands.
        run("load ok", 1, rounds("b", [10.0, 10.0, 10.0]), rounds("t", [12.0, 12.0, 12.0]),
            loads=[("base", 1, 0.4), ("this", 1, 1.9)])
        # Two rounds: SKIP.
        run("rounds", SKIP, rounds("b", [10.0, 10.0]), rounds("t", [12.0, 12.0]), says="a verdict needs 3")
        # Fewer than 30 frames holds nothing.
        b = {("900x560", "chat-wheel"): [dict(w.split("=", 1) for w in _line("900x560", "chat-wheel", 10.0, 3).split()[1:])] * 3}
        t = {("900x560", "chat-wheel"): [dict(w.split("=", 1) for w in _line("900x560", "chat-wheel", 20.0, 3).split()[1:])] * 3}
        run("thin", 0, b, t, says="(not held)")
        # The chat's drag rows leave the table; the map's drag stays.
        drag_b = rounds("b", [10.0, 10.0, 10.0], case="real-drag+fps", extra=_line("900x560", "map-drag", 5.0))
        drag_t = rounds("t", [99.0, 99.0, 99.0], case="real-drag+fps", extra=_line("900x560", "map-drag", 5.0))
        buf = io.StringIO()
        got = compare(drag_b, drag_t, [], 2.0, "ab", buf)
        text = buf.getvalue()
        if got != 0 or "| real-drag" in text or "| map-drag" not in text or "Left out: real-drag+fps" not in text:
            failures.append(f"drag rows: exit {got}\n{text}")

        # The exes: equal bytes in an A/B is red (positive control), fine in an A/A.
        a, b2, c = (os.path.join(tmp, n) for n in ("a", "b", "c"))
        for p, body in ((a, b"one"), (b2, b"one"), (c, b"two")):
            with open(p, "wb") as f:
                f.write(body)
        with open(os.devnull, "w") as null:
            for name, want, args in (
                ("exes equal ab", 1, ("ab", a, b2)),
                ("exes equal aa", 0, ("aa", a, b2)),
                ("exes differ ab", 0, ("ab", a, c)),
            ):
                got = check_exes(*args, null)
                if got != want:
                    failures.append(f"{name}: exit {got}, want {want}")
    for f in failures:
        print(f"window-bench-compare selftest: FAIL {f}", file=sys.stderr)
    if failures:
        return 1
    print("window-bench-compare selftest: 11 verdicts as expected")
    return 0


def main(argv):
    if argv == ["--selftest"]:
        return selftest()
    if argv and argv[0] == "--check-exes":
        if len(argv) != 4 or argv[1] not in ("ab", "aa"):
            print(__doc__, file=sys.stderr)
            return 2
        return check_exes(argv[1], argv[2], argv[3], sys.stdout)
    flags = ("--base", "--this", "--load", "--max-load", "--mode")
    if "--base" not in argv or "--this" not in argv:
        print(__doc__, file=sys.stderr)
        return 2
    args, current = {}, None
    for word in argv:
        if word in flags:
            current = word
            args[current] = []
        elif current is None:
            print(__doc__, file=sys.stderr)
            return 2
        else:
            args[current].append(word)
    loads = read_loads(args["--load"][0]) if args.get("--load") else []
    max_load = float(args["--max-load"][0]) if args.get("--max-load") else None
    mode = args["--mode"][0] if args.get("--mode") else "ab"
    return compare(read(args["--base"]), read(args["--this"]), loads, max_load, mode, sys.stdout)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
