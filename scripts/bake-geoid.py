#!/usr/bin/env python3
"""Bake the EGM2008 geoid undulation grid leviculum-map turns a GNSS
ellipsoid height into a height above sea level with
(leviculum-map/src/geoid.rs).

Source: EGM2008, the Earth Gravitational Model 2008 of the US National
Geospatial-Intelligence Agency (NGA), in the 5 arc-minute grid GeographicLib
computed from NGA's own gridded files and publishes for its Geoid class:

    https://sourceforge.net/projects/geographiclib/files/geoids-distrib/egm2008-5.tar.bz2
    tarball sha256 9a57c14330ac609132d324906822a9da9de265ad9b9087779793eb7080852970
    geoids/egm2008-5.pgm sha256 96d55e88db186ddae892b00c5f8cc42a37cdac8ddb69b6de00af8c75dffb34a3

Licence: public domain. EGM2008 is a work of the US Government (NGA,
17 U.S.C. 105). PROJ-data, which redistributes the same model as
us_nga_egm08_25.tif (also produced by GeographicLib), states for it
"*License*: Public Domain"
(https://raw.githubusercontent.com/OSGeo/PROJ-data/master/us_nga/us_nga_README.txt).
GeographicLib's geoid page (https://geographiclib.sourceforge.io/C++/doc/geoid.html)
places no licence of its own on the .pgm data files; its code is MIT.

The PGM is 4320 x 2161 posts, 16-bit big-endian, row 0 at 90 N, column 0
at 0 E, a post every 5'; a post's height in metres is
Offset + Scale * pixel, with Offset and Scale read from the PGM's comment
lines (-108 and 0.003 for this file). This script keeps every third post
of every third row, 721 rows from 90 N to 90 S and 1440 columns from 0 E
to 359.75 E, a post every 15'. The posts are kept exactly, not smoothed,
and as the PGM's own pixel values, so a post here is bit for bit a post
there and the 3 mm step of the source survives.

Output, all little-endian:

    "LGD1"
    u16 rows (721)        u16 cols (1440)
    u16 step in arc-seconds (900)   u16 reserved (0)
    i32 offset in micrometres (-108000000)
    u32 scale in micrometres per pixel step (3000)
    rows * cols u16 pixel values, row-major, north to south, east from 0 E

height in metres = (offset + scale * pixel) / 1e6. 20 + 721 * 1440 * 2 =
2,076,500 bytes. Baking the same input again gives the same bytes.

Usage:

    scripts/bake-geoid.py geoids/egm2008-5.pgm leviculum-map/data/egm2008-15.lgd

It prints the output's sha256 and a sampled set of (row, col, pixel) read
straight from the 5' source, which leviculum-map/tests/geoid.rs pins.
"""

import hashlib
import struct
import sys

STEP_IN = 3  # 5' source posts per 15' output post
SAMPLES = 64


def read_pgm(path):
    with open(path, "rb") as f:
        data = f.read()
    pos = 0
    tokens = []
    meta = {}
    # Header: magic, width, height, maxval, with '#' comment lines between.
    while len(tokens) < 4:
        while data[pos : pos + 1].isspace():
            pos += 1
        if data[pos : pos + 1] == b"#":
            end = data.index(b"\n", pos)
            words = data[pos + 1 : end].decode("ascii").split(None, 1)
            if len(words) == 2:
                meta[words[0]] = words[1].strip()
            pos = end + 1
            continue
        start = pos
        while not data[pos : pos + 1].isspace():
            pos += 1
        tokens.append(data[start:pos].decode("ascii"))
    pos += 1  # the single whitespace byte after maxval
    if tokens[0] != "P5":
        sys.exit("not a binary PGM (P5)")
    width, height, maxval = int(tokens[1]), int(tokens[2]), int(tokens[3])
    if maxval != 65535:
        sys.exit("expected a 16-bit PGM")
    if "Offset" not in meta or "Scale" not in meta:
        sys.exit("PGM lacks its Offset or Scale comment")
    body = data[pos:]
    if len(body) != width * height * 2:
        sys.exit("PGM body length does not match its header")
    return width, height, meta, body


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    width, height, meta, body = read_pgm(sys.argv[1])
    if (width, height) != (4320, 2161):
        sys.exit("expected the 5' grid, 4320 x 2161")
    offset_um = round(float(meta["Offset"]) * 1e6)
    scale_um = round(float(meta["Scale"]) * 1e6)

    def src(row, col):
        i = (row * width + col) * 2
        return (body[i] << 8) | body[i + 1]

    rows = (height - 1) // STEP_IN + 1
    cols = width // STEP_IN
    out = bytearray(b"LGD1")
    out += struct.pack("<HHHHiI", rows, cols, 900, 0, offset_um, scale_um)
    for r in range(rows):
        for c in range(cols):
            out += struct.pack("<H", src(r * STEP_IN, c * STEP_IN))
    with open(sys.argv[2], "wb") as f:
        f.write(out)

    print(f"{len(out)} bytes, sha256 {hashlib.sha256(out).hexdigest()}")
    print(f"offset_um {offset_um} scale_um {scale_um}")
    # A fixed, spread set of posts, corners and both poles included, read
    # from the 5' source, not from the output.
    picks = [(0, 0), (0, cols - 1), (rows - 1, 0), (rows - 1, cols - 1)]
    x = 12345
    while len(picks) < SAMPLES:
        x = (x * 1103515245 + 12345) % (1 << 31)
        r = x % rows
        x = (x * 1103515245 + 12345) % (1 << 31)
        c = x % cols
        picks.append((r, c))
    for r, c in picks:
        print(f"    ({r}, {c}, {src(r * STEP_IN, c * STEP_IN)}),")


if __name__ == "__main__":
    main()
