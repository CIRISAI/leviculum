#!/usr/bin/env python3
"""Bake Natural Earth's 1:50m data into the world tables leviculum-map
draws when no region covers the view (leviculum-map/src/outline.rs,
docs/src/concepts/loquax-map.md, `expedition-map-concept-2026-11.md`
section 2).

Input: layer geojson files of Natural Earth 5.1.2, public domain
(https://www.naturalearthdata.com/about/terms-of-use/), from
https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/
(sources and checksums in leviculum-map/fixtures/README.md). Polygons make
a table of rings, "LWO2"; line strings a table of lines, "LWL2", in the
same format, whose hole flag is always clear and whose last point is not
joined back to its first. `--max-scalerank N` keeps only the features
whose `scalerank` is at most N, which is how rivers and glaciated areas
are thinned to those a world view shows.

`--names OUT.lwn SOURCES...` bakes a table of names instead, "LWN1", the
points a world view sets: populated places from the full file (it carries
the German names), country labels, the names of marine areas, mountain
ranges and deserts, and the elevation points (peaks), each from its own
source file, told apart by its name. A name is a point at the world
coordinate above, a kind, a rank, the shallowest zoom that shows it (a
country's `MIN_LABEL` floored, a place's `MIN_ZOOM` rounded), the local
name and, where it differs, the German one (the locale the tables are
baked for, as the region fixtures are cut for German). The local name is
the name in the language of the feature's country (`LANGUAGE`), else the
source's `NAME`, which is English or international.
`--names-max-zoom N OUT.lwn SOURCES...` bakes the same table without the
names first shown deeper than zoom N, except the countries, which are
kept at every zoom (the M9's table, 1733). A country's missing
`MIN_LABEL` counts as a null one: its zoom is the deepest.

A point is the Web Mercator world coordinate of leviculum-map's
mercator.rs shifted right by 32 - BITS: with BITS = 18, 262,144 steps
around the world, 153 m at the equator and one pixel at zoom 10, the
outline's deepest, so a coast is never off by more than half a pixel there.
x is not wrapped, so a ring that reaches 180 E ends at the last step
instead of jumping back to 0; the drawer places a copy of the world each
side. Latitudes are clamped to the projection's edge, so Antarctica's
closing edge along 90 S lies on the bottom row. The geometry is Natural
Earth's own 1:50m simplification; this script only quantises it and drops
the repeated points the quantisation makes.

Output, "uv" an unsigned LEB128 varint:

    "LWO2" or "LWL2"  u8 BITS  uv ring or line count
    per ring: uv point count * 2 + (1 for a hole of the ring before)
              uv byte length of its deltas
              uv x, uv y of its first point
              uv x - west, uv y - north, uv east - x, uv south - y
              u8 k
              then per further point its dx and dy, zigzagged, k bits
              each, least significant bit first; a value of k ones is an
              escape and BITS + 1 bits with the value follow

    "LWN1"  u8 BITS  uv string count
    u32 offsets[count + 1], from the start of the bytes; offsets[0] = 0
    bytes   the strings, UTF-8, sorted by their bytes, each once
    uv point count
    per name: u8 kind  u8 rank  u8 min zoom  u8 0 (reserved)
              uv x  uv y  uv local string
              uv German string + 1 (0 where it is the local one)

The kinds are those of leviculum-map's label format (`LabelKind`): 1 city,
2 town, 4 village, 7 country, 8 sea, 9 mountain range or peak, 10 desert.
A reader skips a kind it does not know.

k is chosen per ring for the fewest bits, the smallest k on a tie, so a
ring of short steps packs tight and one long step costs only its escape.
Baking the same input again gives the same bytes.

Usage: scripts/bake-world-outline.py [--max-scalerank N] IN.geojson OUT.lwo
       scripts/bake-world-outline.py [--max-scalerank N] IN.geojson OUT.lwl
       scripts/bake-world-outline.py --names OUT.lwn IN.geojson...
"""

import json
import math
import sys

BITS = 18
MAX_LAT = 85.0511287798
# The deepest zoom the outline draws at (leviculum-map/src/outline.rs).
MAX_ZOOM = 10


def project(lon, lat):
    n = 1 << BITS
    x = min(n - 1, max(0, round((lon + 180.0) / 360.0 * n)))
    lat = max(-MAX_LAT, min(MAX_LAT, lat))
    phi = math.radians(lat)
    t = math.log(math.tan(math.pi / 4 + phi / 2))
    y = min(n - 1, max(0, round((1.0 - t / math.pi) / 2.0 * n)))
    return x, y


def line_points(coords):
    out = []
    for lon, lat in coords:
        p = project(lon, lat)
        if not out or out[-1] != p:
            out.append(p)
    return out


def ring_points(coords):
    out = line_points(coords)
    # GeoJSON repeats the first point last; the drawer closes rings itself.
    while len(out) > 1 and out[-1] == out[0]:
        out.pop()
    return out


def uv(v):
    out = bytearray()
    while True:
        b = v & 0x7F
        v >>= 7
        if v:
            out.append(b | 0x80)
        else:
            out.append(b)
            return out


def zigzag(v):
    return v << 1 if v >= 0 else ((-v) << 1) - 1


def cost(values, k):
    esc = (1 << k) - 1
    return sum(k if v < esc else k + BITS + 1 for v in values)


def pack(values, k):
    esc = (1 << k) - 1
    acc, n, out = 0, 0, bytearray()

    def put(v, width):
        nonlocal acc, n
        acc |= v << n
        n += width
        while n >= 8:
            out.append(acc & 0xFF)
            acc >>= 8
            n -= 8

    for v in values:
        if v < esc:
            put(v, k)
        else:
            put(esc, k)
            put(v, BITS + 1)
    if n:
        out.append(acc & 0xFF)
    return out


def ring_bytes(hole, pts):
    values = []
    for (ax, ay), (bx, by) in zip(pts, pts[1:]):
        values += [zigzag(bx - ax), zigzag(by - ay)]
    k = min(range(1, BITS + 2), key=lambda k: (cost(values, k), k))
    deltas = pack(values, k)
    xs = [p[0] for p in pts]
    ys = [p[1] for p in pts]
    x, y = pts[0]
    out = bytearray()
    out += uv(len(pts) * 2 + (1 if hole else 0))
    out += uv(len(deltas))
    out += uv(x) + uv(y)
    out += uv(x - min(xs)) + uv(y - min(ys)) + uv(max(xs) - x) + uv(max(ys) - y)
    out.append(k)
    out += deltas
    return out


def main():
    args = sys.argv[1:]
    max_scalerank = None
    if args[:1] == ["--max-scalerank"]:
        max_scalerank = int(args[1])
        args = args[2:]
    if args[:1] == ["--names"]:
        names(args[1], args[2:])
        return
    if args[:1] == ["--names-max-zoom"]:
        names(args[2], args[3:], int(args[1]))
        return
    src, dst = args[0], args[1]
    with open(src) as f:
        doc = json.load(f)
    rings = []
    magic = None
    for feature in doc["features"]:
        if max_scalerank is not None:
            rank = feature["properties"].get("scalerank")
            if rank is not None and rank > max_scalerank:
                continue
        geom = feature["geometry"]
        kind = geom["type"]
        lines = kind in ("LineString", "MultiLineString")
        this = b"LWL2" if lines else b"LWO2"
        if magic not in (None, this):
            sys.exit(f"{src}: lines and polygons in one input")
        magic = this
        if lines:
            parts = [geom["coordinates"]] if kind == "LineString" else geom["coordinates"]
            for coords in parts:
                pts = line_points(coords)
                if len(pts) >= 2:
                    rings.append((False, pts))
            continue
        polys = [geom["coordinates"]] if kind == "Polygon" else geom["coordinates"]
        for poly in polys:
            for i, coords in enumerate(poly):
                pts = ring_points(coords)
                if len(pts) >= 3:
                    rings.append((i > 0, pts))
    body = bytearray(magic or b"LWO2")
    body.append(BITS)
    body += uv(len(rings))
    for hole, pts in rings:
        body += ring_bytes(hole, pts)
    with open(dst, "wb") as f:
        f.write(body)
    points = sum(len(p) for _, p in rings)
    holes = sum(1 for h, _ in rings if h)
    name = "lines" if magic == b"LWL2" else "rings"
    print(f"OUTLINE {name}={len(rings)} holes={holes} points={points} bytes={len(body)}")


# The kinds of leviculum-map's label format (`LabelKind::wire`).
CITY, TOWN, VILLAGE = 1, 2, 4
COUNTRY, SEA, RANGE, DESERT = 7, 8, 9, 10


def clamp_zoom(v):
    """A source's zoom as a `u8` the world view can show, 0 past either end."""
    try:
        return max(0, min(MAX_ZOOM, int(round(float(v)))))
    except (TypeError, ValueError):
        return MAX_ZOOM


def rank_of(props, key):
    """The placement rank of a place, higher for a more important one: the
    source's label rank counted back from 7, 0 where it has none."""
    try:
        return max(0, 7 - min(7, int(props.get(key) or 7)))
    except (TypeError, ValueError):
        return 0


def anchor_of(props):
    """Where a name of a polygon or point sits: the source's own label
    point, else the middle of its largest ring's box."""
    if "LABEL_X" in props and "LABEL_Y" in props:
        return float(props["LABEL_X"]), float(props["LABEL_Y"])
    if "long_x" in props and "lat_y" in props:
        return float(props["long_x"]), float(props["lat_y"])
    if "LONGITUDE" in props and "LATITUDE" in props:
        return float(props["LONGITUDE"]), float(props["LATITUDE"])
    geom = props.get("_geometry")
    if not geom:
        return None
    biggest = None
    parts = []
    if geom["type"] == "Polygon":
        parts = [geom["coordinates"]]
    elif geom["type"] == "MultiPolygon":
        parts = geom["coordinates"]
    for poly in parts:
        if not poly:
            continue
        ring = poly[0]
        xs = [p[0] for p in ring]
        ys = [p[1] for p in ring]
        area = (max(xs) - min(xs)) * (max(ys) - min(ys))
        if biggest is None or area > biggest[0]:
            biggest = (area, (min(xs) + max(xs)) / 2, (min(ys) + max(ys)) / 2)
    if biggest is None:
        return None
    return biggest[1], biggest[2]


# The language a country's own names are written in, where Natural
# Earth carries a name field of it and the country has one language its
# places are named in (1712). Natural Earth has no endonym field: `NAME`
# is the English or the international name, and `NAMEPAR`, the one field
# that sometimes holds a local form, as often holds an old name (Bombay,
# Madras) or an English one (Copenhagen beside København). So a place's
# local name is its name in its country's language, and a feature of a
# country not listed here, or with no name in that language, keeps `NAME`.
LANGUAGE = {
    "de": ("DEU", "AUT", "LIE"),
    "fr": ("FRA", "MCO"),
    "it": ("ITA", "SMR", "VAT"),
    "es": (
        "ESP", "MEX", "ARG", "CHL", "COL", "PER", "VEN", "ECU", "URY", "CUB",
        "DOM", "GTM", "HND", "SLV", "NIC", "CRI", "PAN", "BOL", "PRY",
    ),
    "pt": ("PRT", "BRA", "AGO", "MOZ", "GNB", "STP"),
    "nl": ("NLD", "SUR"),
    "pl": ("POL",),
    "sv": ("SWE",),
    "hu": ("HUN",),
    "el": ("GRC",),
    "ru": ("RUS",),
    "uk": ("UKR",),
    "tr": ("TUR",),
    "ja": ("JPN",),
    "ko": ("KOR", "PRK"),
    "zh": ("CHN",),
    "zht": ("TWN",),
    "vi": ("VNM",),
    "id": ("IDN",),
    "he": ("ISR",),
    "fa": ("IRN",),
    "ur": ("PAK",),
    "bn": ("BGD",),
    "ar": (
        "SAU", "EGY", "IRQ", "SYR", "JOR", "LBY", "YEM", "OMN", "ARE", "QAT",
        "KWT", "BHR", "SDN", "LBN", "DZA", "TUN", "MAR", "MRT",
    ),
}
LANGUAGE_OF = {a3: lang for lang, a3s in LANGUAGE.items() for a3 in a3s}


def name_of(props):
    """(local name, German name) of a source feature, either may be None:
    the local name in the language of the feature's country ([`LANGUAGE`]),
    else the source's `NAME`; the German one where it differs from it."""
    for upper in (True, False):
        key = "NAME" if upper else "name"
        name = props.get(key)
        if not name:
            continue
        lang = LANGUAGE_OF.get(props.get("ADM0_A3"))
        local = name
        if lang:
            field = f"{key}_{lang.upper() if upper else lang}"
            local = props.get(field) or name
        de = props.get(f"{key}_DE" if upper else f"{key}_de") or name
        return local, de if de != local else None
    return None, None


def features(path, predicate=None):
    """The features of `path` as (properties, geometry), carrying the
    geometry so [`anchor_of`] can find a polygon's label point."""
    with open(path) as f:
        doc = json.load(f)
    for feature in doc["features"]:
        props = dict(feature["properties"])
        props["_geometry"] = feature["geometry"]
        if predicate is None or predicate(props):
            yield props


def places(props):
    """A populated place's kind and the zoom it appears at."""
    # A rank of 0 is the largest cities, so it must not fall to the
    # default the way `or` would take a missing one (1683: Mumbai and
    # Paris are 0 and were baked as villages).
    rank = props.get("SCALERANK")
    scalerank = 10 if rank is None else int(rank)
    kind = CITY if scalerank <= 2 else TOWN if scalerank <= 5 else VILLAGE
    return kind, clamp_zoom(props.get("MIN_ZOOM", 10)), rank_of(props, "LABELRANK")


def floor_zoom(v):
    """A source's fractional label zoom floored to the zoom that first
    shows it, a `u8` the world view can show."""
    try:
        return max(0, min(MAX_ZOOM, math.floor(float(v))))
    except (TypeError, ValueError):
        return MAX_ZOOM


def countries(props):
    # `MIN_ZOOM` is 0 for every country in the source; `MIN_LABEL` is the
    # zoom its name first belongs on (1712: Malta, Barbados and Cabo Verde
    # stood at z2 where Germany and France did not).
    # A country without a `MIN_LABEL` is treated as one whose value is
    # null: shown from the deepest zoom only (1733), never at z0.
    return COUNTRY, floor_zoom(props.get("MIN_LABEL")), rank_of(props, "LABELRANK")


def marine(props):
    # The source has no min_zoom; its label range's shallowest is the one.
    return SEA, clamp_zoom(props.get("min_label", props.get("scalerank", 0))), 0


def regions(props):
    """A region's kind: a mountain range or a desert, nothing else."""
    kind = {
        "Range/mtn": RANGE,
        "Desert": DESERT,
    }.get(props.get("FEATURECLA"))
    if kind is None:
        return None
    return kind, clamp_zoom(props.get("MIN_LABEL", 0)), 0


def peaks(props):
    """A mountain or a volcano, named as a mountain range's kind."""
    if props.get("featurecla") not in ("mountain", "volcano"):
        return None
    return RANGE, clamp_zoom(props.get("min_zoom", 0)), 0


# Each source by the name of its file, and how its features become names.
SOURCES = {
    "ne_50m_populated_places": places,
    "ne_50m_admin_0_countries": countries,
    "ne_50m_geography_marine_polys": marine,
    "ne_50m_geography_regions_polys": regions,
    "ne_50m_geography_regions_elevation_points": peaks,
}


def names(dst, sources, max_zoom=MAX_ZOOM):
    """Bake the name table "LWN1" from the source files in `sources`, of
    the names a view at most `max_zoom` deep shows, and every country
    whatever its zoom."""
    points = []
    for path in sources:
        stem = path.rsplit("/", 1)[-1].removesuffix(".geojson")
        if stem not in SOURCES:
            sys.exit(f"{path}: not a name source this script knows")
        for props in features(path):
            made = SOURCES[stem](props)
            if made is None:
                continue
            kind, min_zoom, rank = made
            # Countries are kept at every zoom (1733): the M9 draws its
            # overview down to the outline's deepest zoom, and a country
            # labelled past the cut (Malta at 4, Luxembourg at 5.7) would
            # have no name at any zoom the board shows it at.
            if min_zoom > max_zoom and kind != COUNTRY:
                continue
            at = anchor_of(props)
            if at is None:
                continue
            local, german = name_of(props)
            if not local:
                continue
            points.append((kind, rank, min_zoom, at, local, german))
    strings = sorted({s for p in points for s in (p[4], p[5]) if s})
    index = {s: i for i, s in enumerate(strings)}
    table = bytearray(b"LWN1")
    table.append(BITS)
    table += uv(len(strings))
    at = 0
    offsets = [0]
    for s in strings:
        at += len(s.encode())
        offsets.append(at)
    for off in offsets:
        table += off.to_bytes(4, "little")
    for s in strings:
        table += s.encode()
    table += uv(len(points))
    alt = 0
    for kind, rank, min_zoom, (lon, lat), local, german in points:
        x, y = project(lon, lat)
        table += bytes([kind, rank, min_zoom, 0])
        table += uv(x) + uv(y) + uv(index[local])
        if german:
            table += uv(index[german] + 1)
            alt += 1
        else:
            table += uv(0)
    with open(dst, "wb") as f:
        f.write(table)
    print(
        f"NAMES points={len(points)} alternates={alt} "
        f"strings={len(strings)} bytes={len(table)}"
    )


if __name__ == "__main__":
    main()

