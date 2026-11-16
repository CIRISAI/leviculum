#!/usr/bin/env python3
"""Bake Natural Earth's 1:50m countries into the table leviculum-map looks a
point's country up in (leviculum-map/src/countries.rs, provenance in
leviculum-map/fixtures/README.md).

Input: ne_50m_admin_0_countries.geojson of Natural Earth 5.1.2, public
domain (https://www.naturalearthdata.com/about/terms-of-use/), from
https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/.

The rings are those of scripts/bake-world-outline.py, quantised and packed
by its own helpers, at BITS = 16: a step is 611 m at the equator, four of
the outline's, less than the 1:50m data's own generalisation. 18 bits
would cost 251,377 bytes against 200,354 for no better answer, and
Douglas-Peucker at one step on top would save only another 9 %. Each ring
belongs to a country by its ISO 3166-1 alpha-2 code: Natural Earth's
ISO_A2, or where that is not two letters its ISO_A2_EH (France, Norway,
Kosovo XK, Taiwan TW, the Australian territories) and for
the rest the state the territory lies in by ISO's own list (Somaliland SO,
Northern Cyprus CY). The Siachen Glacier, which ISO places in no country,
is left out. Natural Earth's Indian Ocean Territories are two ISO countries,
Christmas Island CX east of 100 E and the Cocos (Keeling) Islands CC west of
it. A country's rings stay together, in Natural Earth's order, and the
countries go by code.

Output, "uv" an unsigned LEB128 varint:

    "LCO1"  uv country count
    per country: 2 bytes its code, uv its ring count
    then the rings, a whole LWO2 table of bake-world-outline.py

Baking the same input again gives the same bytes.

Usage: scripts/bake-country-outline.py IN.geojson OUT.lco
"""

import importlib.util
import json
import os
import sys

BITS = 16

_spec = importlib.util.spec_from_file_location(
    "bake_world_outline", os.path.join(os.path.dirname(__file__), "bake-world-outline.py")
)
outline = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(outline)
outline.BITS = BITS

# Where ISO_A2 and ISO_A2_EH are both -99: the ISO country it lies in, or
# None to leave the feature out.
BY_ADM0 = {"SOL": "SO", "CYN": "CY", "KAS": None}


def code_of(props):
    for key in ("ISO_A2", "ISO_A2_EH"):
        c = props.get(key, "")
        if len(c) == 2 and c.isascii() and c.isalpha() and c.isupper():
            return c
    adm = props.get("ADM0_A3", "")
    if adm not in BY_ADM0:
        sys.exit(f"no code for {props.get('ADMIN')} ({adm})")
    return BY_ADM0[adm]


def split(code, props, poly):
    """The code a polygon of a feature goes under."""
    if props.get("ADM0_A3") == "IOA":
        return "CX" if poly[0][0][0] > 100.0 else "CC"
    return code


def main():
    src, dst = sys.argv[1], sys.argv[2]
    with open(src) as f:
        doc = json.load(f)
    countries = {}
    for feature in doc["features"]:
        props = feature["properties"]
        code = code_of(props)
        if code is None:
            continue
        geom = feature["geometry"]
        polys = [geom["coordinates"]] if geom["type"] == "Polygon" else geom["coordinates"]
        for poly in polys:
            rings = countries.setdefault(split(code, props, poly), [])
            for i, coords in enumerate(poly):
                pts = outline.ring_points(coords)
                if len(pts) < 3:
                    # An outer ring too small to keep takes its holes along.
                    if i == 0:
                        break
                    continue
                rings.append((i > 0, pts))
    head = bytearray(b"LCO1")
    head += outline.uv(len([c for c in countries if countries[c]]))
    table = bytearray(b"LWO2")
    table.append(outline.BITS)
    total = sum(len(r) for r in countries.values())
    table += outline.uv(total)
    for code in sorted(countries):
        rings = countries[code]
        if not rings:
            continue
        head += code.encode("ascii") + outline.uv(len(rings))
        for hole, pts in rings:
            table += outline.ring_bytes(hole, pts)
    body = head + table
    with open(dst, "wb") as f:
        f.write(body)
    points = sum(len(p) for r in countries.values() for _, p in r)
    holes = sum(1 for r in countries.values() for h, _ in r if h)
    print(
        f"COUNTRIES countries={len([c for c in countries if countries[c]])} rings={total} "
        f"holes={holes} points={points} bytes={len(body)}"
    )


if __name__ == "__main__":
    main()
