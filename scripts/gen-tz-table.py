#!/usr/bin/env python3
"""Write the board's clock rules: for every zone of the time zone outline
(leviculum-map/data/timezones.lwz, scripts/bake-timezones.py) the POSIX
TZ string of the footer of its TZif file in the build host's tzdata
(RFC 8536, section 3.3), into leviculum-map/data/tz-rules.txt, which
`leviculum_map::timezones::board_offset` reads (order 1684).

Linux and Android read the system's own tzdata at run time; a board has no
file system and evaluates these strings instead. A footer only describes
the rules after a zone's last explicit transition, so a zone whose rules
are not a POSIX rule (Africa/Casablanca's Ramadan changes) is right on the
board only outside the transitions the TZif file lists explicitly.

Output:

    # leviculum-map clock rules, scripts/gen-tz-table.py
    # tzdata VERSION
    NAME POSIX-TZ-STRING      (one line a zone, in the outline's order)

The version is the first line of /usr/share/zoneinfo/tzdata.zi
("# version 2026b"), or +VERSION where that is missing.

The table in the tree was generated from tzdata 2026f, the outline's
release, compiled with zic 2026f `-b fat` from IANA's tzdb-2026f.tar.lz,
since the build host's Debian tzdata was 2026b (commands and checksums in
leviculum-map/fixtures/README.md). The host's own /usr/share/zoneinfo is
the default; pass a newer zoneinfo directory where the host lags.

Usage: scripts/gen-tz-table.py [ZONEINFO] [OUT]
       (defaults /usr/share/zoneinfo and leviculum-map/data/tz-rules.txt)
"""

import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def uv(data, at):
    v, shift = 0, 0
    while True:
        b = data[at]
        at += 1
        v |= (b & 0x7F) << shift
        shift += 7
        if not b & 0x80:
            return v, at


def zone_names(path):
    with open(path, "rb") as f:
        data = f.read()
    if data[:4] != b"LWZ1":
        sys.exit(f"{path}: not a zone table")
    count, at = uv(data, 5)
    names = []
    for _ in range(count):
        n, at = uv(data, at)
        names.append(data[at : at + n].decode())
        at += n
    return names


def version(zoneinfo):
    zi = os.path.join(zoneinfo, "tzdata.zi")
    if os.path.exists(zi):
        with open(zi) as f:
            first = f.readline().split()
        if first[:2] == ["#", "version"] and len(first) == 3:
            return first[2]
    with open(os.path.join(zoneinfo, "+VERSION")) as f:
        return f.read().strip()


def footer(path):
    with open(path, "rb") as f:
        data = f.read()
    if data[:4] != b"TZif" or data[4:5] == b"\0":
        sys.exit(f"{path}: not a TZif file of version 2 or later")
    if not data.endswith(b"\n"):
        sys.exit(f"{path}: no footer")
    start = data.rindex(b"\n", 0, len(data) - 1)
    tz = data[start + 1 : -1].decode("ascii")
    if not tz:
        sys.exit(f"{path}: empty footer")
    return tz


def main():
    zoneinfo = sys.argv[1] if len(sys.argv) > 1 else "/usr/share/zoneinfo"
    out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(ROOT, "leviculum-map/data/tz-rules.txt")
    names = zone_names(os.path.join(ROOT, "leviculum-map/data/timezones.lwz"))
    lines = [
        "# leviculum-map clock rules, scripts/gen-tz-table.py",
        f"# tzdata {version(zoneinfo)}",
    ]
    for n in names:
        lines.append(f"{n} {footer(os.path.join(zoneinfo, n))}")
    with open(out, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"TZRULES zones={len(names)} bytes={sum(len(l) + 1 for l in lines)}")


if __name__ == "__main__":
    main()
