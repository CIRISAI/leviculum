#!/usr/bin/env python3
"""Summarise a Loquax window's map trace (1431).

    LOQUAX_MAP_TRACE=/tmp/map.trace loquax gui     # drag, turn the wheel, close
    python3 scripts/map-trace-summary.py /tmp/map.trace

The trace's lines are `<ms> event down|move|up|wheel`, `<ms> ask <zoom>
moving|rest` and `<ms> painted <worker ms> <missing tiles>`
(loquax/src/gui/trace.rs). Per drag, from its press to its release: how many
pointer events the window delivered and how many a second, how many frames it
asked the worker for, and what the worker took per frame. A drag that asks a
frame per event is the fault 1431 removed.
"""

import statistics
import sys


def main(path: str) -> int:
    drags = []
    current = None
    worker = []
    for line in open(path, encoding="utf-8"):
        parts = line.split()
        if len(parts) < 2:
            continue
        ms, what = float(parts[0]), parts[1]
        if what == "event" and parts[2] == "down":
            current = {"start": ms, "moves": 0, "asks": 0, "end": ms}
        elif what == "event" and parts[2] == "move" and current:
            current["moves"] += 1
            current["end"] = ms
        elif what == "event" and parts[2] == "up" and current:
            current["end"] = ms
            drags.append(current)
            current = None
        elif what == "ask" and current:
            current["asks"] += 1
        elif what == "painted":
            worker.append(float(parts[2]))
    for i, d in enumerate(drags):
        span = max(d["end"] - d["start"], 1e-9) / 1e3
        print(
            f"drag {i}: {d['moves']} moves in {span:.2f} s = {d['moves'] / span:.0f}/s, "
            f"{d['asks']} frames asked = {d['asks'] / span:.1f}/s"
        )
    if worker:
        print(
            f"worker: {len(worker)} frames, mean {statistics.mean(worker):.2f} ms, "
            f"max {max(worker):.2f} ms"
        )
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    sys.exit(main(sys.argv[1]))
