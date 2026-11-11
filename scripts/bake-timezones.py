#!/usr/bin/env python3
"""Bake the world's time zone outlines into the three tables
leviculum-map's `timezones` module reads (order 1684,
leviculum-map/src/timezones/mod.rs, provenance in
leviculum-map/fixtures/README.md).

Source: timezone-boundary-builder release 2026f, the "same since now"
variant with territorial waters and the open ocean,
https://github.com/evansiroky/timezone-boundary-builder/releases/download/2026f/timezones-with-oceans-now.geojson.zip
(26,768,042 bytes, sha256
5983306d5b8646ee834e9ab60a83fcf3330119260c9cd2a1f16c9690d008b4f0), which
holds combined-with-oceans-now.json (86,399,837 bytes, sha256
016b2a6f78d53f079a400401a8d8d43fc75e3f40f62cdce34238c0f77c4bf69e): 63
zones, each the zones that keep the same time since the release merged
under the IANA name of the most populous. Licence, from the README's
"Licenses" section: "The outputted data is licensed under the Open Data
Commons Open Database License (ODbL)", and the data is almost wholly
OpenStreetMap's; the baked tables are derived databases under the same
licence.

Needs python3-shapely (2.x on GEOS 3.12 or newer, for
`coverage_simplify`; `sudo apt install python3-shapely`) and numpy.

Steps:

1. Every zone is clipped to the Web Mercator latitudes and projected to
   the world coordinate of leviculum-map's mercator.rs shifted right by
   32 - BITS (BITS = 18, the world outline's grid, 153 m at the equator),
   as floats, then snapped to that grid.
2. The source is not a clean coverage: 23 small gaps and 45 small
   overlaps between neighbours, and edges that do not match vertex for
   vertex. All boundaries are noded together and polygonized; every face
   goes to the zone that holds its interior point, the smallest such zone
   where two overlap, and a face no zone holds goes to the neighbour it
   shares the longest edge with. The faces of a zone are dissolved again.
   Every border is now the same vertices on both sides.
3. GEOS's coverage simplification (Visvalingam-Whyatt over the shared
   edges, each edge once, the coverage stays gapless) at TOLERANCE grid
   units, then the repeated points dropped.

Output, three files beside the world outline's (formats in
scripts/bake-world-outline.py's docstring):

    timezones.lwo  "LWO2", every polygon of every zone, the zones in name
                   order, each zone's polygons largest first, each outer
                   ring followed by its holes.
    timezones.lwl  "LWL2", the borders between zones, each once: the edges
                   two zones share, chained into lines, and the stretches
                   of the antimeridian where the zone east of it is not
                   the zone west of it.
    timezones.lwz  "LWZ1", the zones and what each ring belongs to:

        "LWZ1"  u8 BITS  uv zone count
        per zone, in index order (sorted by name): uv byte length, the
                  IANA name, UTF-8
        uv ring count (that of timezones.lwo)
        per ring: uv zone index
                  u8 min zoom of its label, 255 for none (every hole, and
                  a polygon too small to hold one at the zooms the
                  outline draws)
                  if not 255: uv x, uv y of the label anchor

A ring's label anchor is its polygon's pole of inaccessibility (shapely's
polylabel, to 2 units), and its min zoom the shallowest zoom at which
the circle around the anchor that stays inside the polygon is
LABEL_RADIUS pixels wide; a zone's largest polygon always has a label,
at the outline's deepest zoom (10) at the latest.

Baking the same input with the same GEOS gives the same bytes.

Usage: scripts/bake-timezones.py [--tolerance T] combined-with-oceans-now.json OUTDIR
       (default T = 2; writes OUTDIR/timezones.lwo, .lwl and .lwz)
"""

import importlib.util
import json
import math
import os
import sys

import numpy as np
import shapely
from shapely.geometry import LineString, MultiLineString, Polygon, box, shape
from shapely.ops import polylabel
from shapely.strtree import STRtree

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "bake_world_outline", os.path.join(HERE, "bake-world-outline.py")
)
_outline = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_outline)
uv = _outline.uv
ring_bytes = _outline.ring_bytes
BITS = _outline.BITS
MAX_LAT = _outline.MAX_LAT
N = 1 << BITS

TOLERANCE = 2.0
# The radius, in pixels, a label's free circle needs: a time like 23:45
# is about 40 pixels wide in the frontends' label font.
LABEL_RADIUS = 20.0
MAX_ZOOM = 10
NO_LABEL = 255


def project(c):
    lon = c[:, 0]
    lat = np.clip(c[:, 1], -MAX_LAT, MAX_LAT)
    x = (lon + 180.0) / 360.0 * N
    y = (1.0 - np.log(np.tan(np.pi / 4 + np.radians(lat) / 2)) / np.pi) / 2.0 * N
    return np.column_stack([x, y])


def polygons(g):
    if g.is_empty:
        return []
    if g.geom_type == "Polygon":
        return [g]
    if g.geom_type in ("MultiPolygon", "GeometryCollection"):
        return [p for part in g.geoms for p in polygons(part)]
    return []


def load(src):
    with open(src) as f:
        doc = json.load(f)
    clip = box(-180, -MAX_LAT, 180, MAX_LAT)
    zones = []
    for feature in doc["features"]:
        g = shape(feature["geometry"]).intersection(clip)
        g = shapely.transform(g, project)
        g = shapely.set_precision(shapely.make_valid(g), 1.0)
        zones.append((feature["properties"]["tzid"], g))
    zones.sort(key=lambda z: z[0])
    return [z[0] for z in zones], [z[1] for z in zones]


def clean(geoms):
    """Rebuild the zones as a coverage: node, polygonize, assign faces."""
    lines = shapely.union_all([g.boundary for g in geoms], grid_size=1.0)
    faces = list(shapely.polygonize([lines]).geoms)
    tree = STRtree(geoms)
    area = [g.area for g in geoms]
    owner = []
    gaps = overlaps = 0
    for f in faces:
        hits = sorted(
            (int(j) for j in tree.query(f.point_on_surface(), predicate="intersects")),
            key=lambda j: (area[j], j),
        )
        overlaps += len(hits) > 1
        owner.append(hits[0] if hits else None)
    # A gap goes to the neighbour it shares the longest edge with; repeat,
    # since a gap may only touch other gaps at first.
    while any(o is None for o in owner):
        face_tree = STRtree(faces)
        changed = False
        for i, f in enumerate(faces):
            if owner[i] is not None:
                continue
            best = None
            for j in face_tree.query(f, predicate="touches"):
                j = int(j)
                if owner[j] is None:
                    continue
                shared = f.boundary.intersection(faces[j].boundary).length
                if best is None or shared > best[0]:
                    best = (shared, owner[j])
            if best is not None:
                owner[i] = best[1]
                gaps += 1
                changed = True
        if not changed:
            sys.exit("a gap touches no zone")
    out = []
    for z in range(len(geoms)):
        mine = [f for f, o in zip(faces, owner) if o == z]
        out.append(shapely.union_all(mine, grid_size=1.0))
    print(f"CLEAN faces={len(faces)} gaps={gaps} overlaps={overlaps}", file=sys.stderr)
    return out


def ring_points(coords):
    out = []
    for x, y in coords:
        p = (min(N - 1, max(0, int(round(x)))), min(N - 1, max(0, int(round(y)))))
        if not out or out[-1] != p:
            out.append(p)
    while len(out) > 1 and out[-1] == out[0]:
        out.pop()
    return out


def label(poly):
    """The anchor and min zoom of a polygon's label."""
    p = polylabel(poly, tolerance=2.0)
    r = poly.boundary.distance(p)
    if r <= 0:
        return None, NO_LABEL
    # A grid unit is 2^(z - 10) pixels at zoom z.
    zoom = max(0, math.ceil(MAX_ZOOM + math.log2(LABEL_RADIUS / r) - 1e-9))
    anchor = (min(N - 1, max(0, int(round(p.x)))), min(N - 1, max(0, int(round(p.y)))))
    return anchor, zoom if zoom <= MAX_ZOOM else NO_LABEL


def borders(rings, zone_of):
    """The lines two zones share, each once, plus the antimeridian where
    its two sides differ."""
    count = {}
    for pts in rings:
        for a, b in zip(pts, pts[1:] + pts[:1]):
            key = (a, b) if a < b else (b, a)
            count[key] = count.get(key, 0) + 1
    edges = {k for k, c in count.items() if c >= 2}
    # The antimeridian: per zone, its runs on x = 0 and on x = N - 1.
    west, east = {}, {}
    for pts, z in zip(rings, zone_of):
        for a, b in zip(pts, pts[1:] + pts[:1]):
            if a[0] == b[0] == 0:
                west.setdefault(z, []).append(LineString([(N - 1, a[1]), (N - 1, b[1])]))
            elif a[0] == b[0] == N - 1:
                east.setdefault(z, []).append(LineString([a, b]))
    for ze, le in east.items():
        e = shapely.union_all(le)
        for zw, lw in west.items():
            if zw == ze:
                continue
            seg = e.intersection(shapely.union_all(lw))
            for part in getattr(seg, "geoms", [seg]):
                if part.geom_type != "LineString" or part.length == 0:
                    continue
                ys = sorted(int(round(c[1])) for c in part.coords)
                if ys[0] != ys[-1]:
                    a, b = (N - 1, ys[0]), (N - 1, ys[-1])
                    edges.add((a, b))
    # Chain the edges into lines, breaking at every node of degree not 2.
    adj = {}
    for a, b in edges:
        adj.setdefault(a, []).append(b)
        adj.setdefault(b, []).append(a)
    for v in adj.values():
        v.sort()
    used = set()
    lines = []

    def walk(start, nxt):
        line = [start]
        prev, cur = start, nxt
        used.add((prev, cur) if prev < cur else (cur, prev))
        while True:
            line.append(cur)
            if len(adj[cur]) != 2:
                break
            n = [p for p in adj[cur] if (min(cur, p), max(cur, p)) not in used]
            if not n:
                break
            prev, cur = cur, n[0]
            used.add((prev, cur) if prev < cur else (cur, prev))
        return line

    for v in sorted(adj):
        if len(adj[v]) == 2:
            continue
        for n in adj[v]:
            if (min(v, n), max(v, n)) not in used:
                lines.append(walk(v, n))
    for v in sorted(adj):
        for n in adj[v]:
            if (min(v, n), max(v, n)) not in used:
                lines.append(walk(v, n))
    return lines


def main():
    args = sys.argv[1:]
    tolerance = TOLERANCE
    if args[:1] == ["--tolerance"]:
        tolerance = float(args[1])
        args = args[2:]
    src, outdir = args
    names, geoms = load(src)
    geoms = clean(geoms)
    if not shapely.coverage_is_valid(geoms):
        sys.exit("the cleaned zones are not a coverage")
    simple = shapely.coverage_simplify(geoms, tolerance)
    rings, zone_of, labels = [], [], []
    for z, g in enumerate(simple):
        polys = sorted(polygons(g), key=lambda p: -p.area)
        first = True
        for poly in polys:
            outer = ring_points(poly.exterior.coords)
            if len(outer) < 3:
                continue
            anchor, zoom = label(poly)
            if first and anchor is not None and zoom == NO_LABEL:
                zoom = MAX_ZOOM
            first = first and anchor is None
            rings.append((False, outer))
            zone_of.append(z)
            labels.append((anchor, zoom))
            for hole in poly.interiors:
                pts = ring_points(hole.coords)
                if len(pts) >= 3:
                    rings.append((True, pts))
                    zone_of.append(z)
                    labels.append((None, NO_LABEL))
    lines = borders([pts for _, pts in rings], zone_of)

    lwo = bytearray(b"LWO2")
    lwo.append(BITS)
    lwo += uv(len(rings))
    for hole, pts in rings:
        lwo += ring_bytes(hole, pts)
    lwl = bytearray(b"LWL2")
    lwl.append(BITS)
    lwl += uv(len(lines))
    for pts in lines:
        lwl += ring_bytes(False, pts)
    lwz = bytearray(b"LWZ1")
    lwz.append(BITS)
    lwz += uv(len(names))
    for n in names:
        b = n.encode()
        lwz += uv(len(b)) + b
    lwz += uv(len(rings))
    for z, (anchor, zoom) in zip(zone_of, labels):
        lwz += uv(z)
        lwz.append(zoom)
        if zoom != NO_LABEL:
            lwz += uv(anchor[0]) + uv(anchor[1])
    for name, data in (("timezones.lwo", lwo), ("timezones.lwl", lwl), ("timezones.lwz", lwz)):
        with open(os.path.join(outdir, name), "wb") as f:
            f.write(data)
    points = sum(len(p) for _, p in rings)
    holes = sum(1 for h, _ in rings if h)
    lpoints = sum(len(p) for p in lines)
    labelled = sum(1 for _, z in labels if z != NO_LABEL)
    print(
        f"TIMEZONES tolerance={tolerance} zones={len(names)} rings={len(rings)} holes={holes} "
        f"points={points} lines={len(lines)} line_points={lpoints} labels={labelled} "
        f"lwo={len(lwo)} lwl={len(lwl)} lwz={len(lwz)} total={len(lwo) + len(lwl) + len(lwz)}"
    )


if __name__ == "__main__":
    main()
