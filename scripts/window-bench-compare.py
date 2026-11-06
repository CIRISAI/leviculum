#!/usr/bin/env python3
"""The window bench's A/B (1441, `just window-bench`): rounds of the bench
of BASE and of this tree, the medians of each size and gesture compared.

A WINDOW_BENCH line is `size= case= fps= frame_ms_mean= p95= ...`. Held is
`frame_ms_mean`, as `just map-bench` holds its columns: this tree's median
more than 5 % and more than 0.1 ms above BASE's is a regression, and the
exit is 1. A gesture that drew no frame (the mouse's drag on the chat at
the wide sizes, which iced does not scroll) holds nothing.

Usage: window-bench-compare.py --base FILE... --this FILE...
"""

import statistics
import sys

SHARE = 0.05
FLOOR_MS = 0.1


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


def median(rows, name):
    return statistics.median(float(r[name]) for r in rows)


def main(argv):
    if "--base" not in argv or "--this" not in argv:
        print(__doc__, file=sys.stderr)
        return 2
    b, t = argv.index("--base"), argv.index("--this")
    base_paths = argv[b + 1 : t] if b < t else argv[b + 1 :]
    this_paths = argv[t + 1 :] if b < t else argv[t + 1 : b]
    base, this = read(base_paths), read(this_paths)
    print("| size | case | fps base | fps this | ms base | ms this | p95 base | p95 this | present this | damage | change |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    regressions = []
    for key in sorted(this, key=lambda k: (int(k[0].split("x")[0]), k[1])):
        now = this[key]
        then = base.get(key)
        ms = median(now, "frame_ms_mean")
        if then is None:
            print(f"| {key[0]} | {key[1]} | - | {median(now, 'fps'):.0f} | - | {ms:.2f} | - | {median(now, 'p95'):.2f} | {median(now, 'present'):.2f} | {median(now, 'damage'):.2f} | new |")
            continue
        was = median(then, "frame_ms_mean")
        change = (ms - was) / was * 100 if was > 0 else 0.0
        held = was > 0 and ms > was * (1 + SHARE) and ms - was > FLOOR_MS
        if held:
            regressions.append(f"{key[0]} {key[1]}: {was:.2f} -> {ms:.2f} ms ({change:+.1f} %)")
        print(
            f"| {key[0]} | {key[1]} | {median(then, 'fps'):.0f} | {median(now, 'fps'):.0f} "
            f"| {was:.2f} | {ms:.2f} | {median(then, 'p95'):.2f} | {median(now, 'p95'):.2f} "
            f"| {median(now, 'present'):.2f} | {median(now, 'damage'):.2f} "
            f"| {change:+.1f} %{' REGRESSION' if held else ''} |"
        )
    if regressions:
        print("\nwindow-bench: frame time regressions (more than 5 % and 0.1 ms):")
        for r in regressions:
            print(f"  {r}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
