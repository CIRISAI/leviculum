# Fixtures

All regions here were re-cut on 2026-11-12 under layer profile 2 (1707:
the deepest zoom keeps the source's geometry, joined line ends and the
tile-edge strip are held at the simplified zooms, building footprints
keep their corners), as `LMR1` version 4 files. The old cuts' sizes are
noted per file; `old/` keeps one old-format file for the tests that read
it.

`firenze.lmr`: a region file of central Florence, zooms 11 to 15, six
tiles (z15/17408/11943 and z15/17409/11943, and the one tile above them
at each of z14 to z11), with the names of its places, parks, rivers and
streets and the German name where it differs (Firenze, Florenz), 210,647
bytes, sha256
`a6b26302ad896a33d0e5bd5e5806995eda027613c70eaa95ee407062a30d6ccd`
(profile 1 cut it to 198,229 bytes as version 4, 197,972 as version 2; its z15 blobs grew 24 %
and z14/z13 about 6 % under profile 2). Cut with

```sh
leviculum-map-tool cut -o firenze.lmr firenze.pmtiles \
    --bbox 11.2505,43.7728,11.2700,43.7785 --min-zoom 11 --max-zoom 15 \
    --locale de --buildings false --pois false
```

from the Protomaps sample archive
`https://pmtiles.io/protomaps(vector)ODbL_firenze.pmtiles` (6,601,156
bytes, sha256 `7190f3d807a62f4f012b574007c96b809f6842f45a6b0c508639331fc68fd30a`;
Protomaps Basemap schema 3.7.1, planetiler 2024-08-01, OSM replication of
2024-08-07). Re-cutting gives the same bytes.

`berlin/`: two region files of central Berlin, for a pinch from z14 to
z10 that needs names at every step (1225), in a directory of their own so
the tests that read this one see Florence alone. `wide.lmr` (77,507
bytes, sha256 `3f5e84dae91f4743b194d10081ded4b87001fa56a0ed1fd70286cf60682ba875`)
holds zooms 10 and 11, one tile each — both region zooms, so profile 2
moved only its profile byte against the same day's profile 1 cut —
`mitte.lmr` (201,547 bytes, sha256
`85c436e9cdef05cd9e6b1fd449493f979d0517834f2083e29860083cedec3753`)
zooms 12 to 14, one tile each (181,359 under profile 1 from the same
build: +11 %, its deepest z14 unsimplified). Cut with

```sh
leviculum-map-tool cut -o wide.lmr https://build.protomaps.com/20261111.pmtiles \
    --bbox 13.37,52.50,13.45,52.54 --min-zoom 10 --max-zoom 11 \
    --locale de --pois false
leviculum-map-tool cut -o mitte.lmr https://build.protomaps.com/20261111.pmtiles \
    --bbox 13.390,52.515,13.400,52.520 --min-zoom 12 --max-zoom 14 \
    --locale de --pois false
```

from the Protomaps daily build of 2026-11-11, which the bucket keeps for
about a week; a later build cuts other bytes. (The earlier cuts came from
the build of 2026-10-31, so the data moved a little too: that build's
profile 1 cuts were 77,613 and 181,205 bytes.)

`buildings/firenze.lmr`: the same Florence region cut again with its
building footprints, 270,359 bytes, sha256
`fe2c558efbac464cdc3deb6cfeff7fdb7dd9727fc6b9d6c9d1a361f4431c01c8`: two
building sections, 2,934 footprints in 59,672 bytes — at corner-exact
tolerance 0 (1707) the three footprints the old half-pixel tolerance
collapsed return, and the sections grew from 50,115 bytes (the whole file from
248,384 under profile 1). Its tile
blobs are `firenze.lmr`'s byte for byte. In a directory of its own, so
the tests that read the region without buildings see none of it. Cut
from the same archive with

```sh
leviculum-map-tool cut -o firenze.lmr firenze.pmtiles \
    --bbox 11.2505,43.7728,11.2700,43.7785 --min-zoom 11 --max-zoom 15 \
    --locale de --pois false
```

`frankfurt/roemer.lmr`: central Frankfurt around the Römer, zoom 15 only,
six tiles with their names and building sections, 208,392 bytes, sha256
`021f297c2a39f789fae4a3d168b466da667e3d2443bdf5f0f1973acde8b27e22`:
5,247 footprints in 106,569 bytes. Its one zoom is the deepest, so
profile 2 falls entirely on it: the same build's profile 1 cut is
159,455 bytes (+31 %, road and area geometry at source detail and the
66 sub-pixel footprints kept). The device walk's `buildings` step puts
it on the phone and opens the map on it at z15 and z16
(`docs/src/concepts/loquax-device-walk.md`). In a directory of its own,
so the tests that read the others see none of it. Cut with

```sh
leviculum-map-tool cut -o roemer.lmr https://build.protomaps.com/20261111.pmtiles \
    --bbox 8.6765,50.1050,8.6875,50.1165 --min-zoom 15 --max-zoom 15 \
    --locale de --pois false
```

from the Protomaps daily build of 2026-11-11; cutting it twice gave the
same bytes.

`old/firenze-v2.lmr`: the Florence region as the cut wrote it before
1291 and 1707 — `LMR1` version 2, layer profile 1, 197,972 bytes, sha256
`4445b1fc701b8b098a56ac2a9271611c6b034569017093f013ae4c7c591e85db` —
frozen, never re-cut. The old-version reading tests hold to it: a
version 2 file refuses every truncation where a version 4 file is read
while it is written, and it is the base the version 1 and version 3
header tests build on (`tests/lmr2.rs`, `tests/buildings.rs`,
`tests/region.rs`).

**© OpenStreetMap contributors**, <https://www.openstreetmap.org/copyright>.
The data is available under the Open Database License (ODbL) 1.0,
<https://opendatacommons.org/licenses/odbl/1-0/>. These files are derived
databases of it and are under the same licence, not under the AGPL of the
code around them; each carries this notice inside itself as well. Pictures
drawn from them are produced works and carry the same attribution.

## The world outline

`../data/ne_50m_land.lwo` is not a test fixture but the table the crate
draws where no region covers the view (`src/outline.rs`): Natural Earth's
1:50m land polygons, version 5.1.2, 1,422 rings and 59,239 points in
159,357 bytes. Baked with

```sh
scripts/bake-world-outline.py ne_50m_land.geojson leviculum-map/data/ne_50m_land.lwo
```

from
`https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_50m_land.geojson`
(1,636,166 bytes, sha256
`e874b27a51d146452be360cafb3cc50c86001074a67d534113e6534682f9826b`). Baking
again gives the same bytes (sha256
`a154e60ef141e605a707441bb11215efcb97b5a013da08acc8ddfca90c0e2579`).

The borders drawn over it, `../data/ne_50m_admin_0_boundary_lines_land.lwl`
(`LWL2`, lines rather than rings, the same quantisation), are Natural
Earth's 1:50m land boundary lines of the same version, 393 lines and 19,760
points in 49,512 bytes. Maritime boundaries are left out: over water the
coasts already part the countries. The 1:110m lines would be 12,679 bytes,
but 1:50m fits the budget of about 100 KB and matches the land it lies on.
Baked with

```sh
scripts/bake-world-outline.py ne_50m_admin_0_boundary_lines_land.geojson \
    leviculum-map/data/ne_50m_admin_0_boundary_lines_land.lwl
```

from
`https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_50m_admin_0_boundary_lines_land.geojson`
(760,189 bytes, sha256
`2faac4f6b34386f3d21b6e018cf151f241f00e5c936d44dd17d7d9bfb147fa48`). Baking
again gives the same bytes (sha256
`bf43e6ed683d1aca77f9c737e5e8d702fb24185c1580a7a3c65494a3273eeb6e`).

### The layers over the land (1683)

Five more tables go over the land and under the borders, and the disputed
boundary lines dashed over all of them. Each is baked from its own file of
the same Natural Earth 5.1.2 release, at
`https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/`,
and each reproduces byte for byte:

| table | source (bytes, sha256) | filter | rings/lines, points | bytes | baked sha256 |
|---|---|---|---|---|---|
| `ne_50m_lakes.lwo` | `ne_50m_lakes.geojson` (876,018, `d350b75978b26fe839b797c2c529b2fb8f47fb3983c03f4964e36d5df9378a52`) | none | 465 rings, 53 holes, 18,149 points | 44,502 | `04042ea312b5a091f588da5ecc21acf458405ea320bfa04d416c10ff0aa392d0` |
| `ne_50m_rivers_lake_centerlines.lwl` | `ne_50m_rivers_lake_centerlines.geojson` (827,318, `f286e0ce978fde999ca2d7a78c764be08542e19b63cded52b05c12d5173ccc51`) | `scalerank <= 4` | 411 lines, 13,536 points | 36,236 | `efe2a2f09f8a597599ed42443b37fbb43d443ffd41d23fd3597872ce9beda698` |
| `ne_50m_glaciated_areas.lwo` | `ne_50m_glaciated_areas.geojson` (512,453, `c7a097b4b8e0d360dc82c90ab20c34b78aa5939a5ec8ab157c18945938e9c0a7`) | `scalerank <= 1` | 31 rings, 13 holes, 5,720 points | 16,422 | `e9191d768dee5c23533d4d5f60de350fda64515099cdc26998b6da58df6620ba` |
| `ne_50m_antarctic_ice_shelves_polys.lwo` | `ne_50m_antarctic_ice_shelves_polys.geojson` (131,055, `fedf866d20e20e37d037fa19968b6d3022fbe5323fd8728527b0f0ddb547e790`) | none | 101 rings, 36 holes, 4,905 points | 14,344 | `a8290d6e697aa5b5e8805a83d8bf8f5c2be4626d9aa2711432d915490f061bbd` |
| `ne_50m_admin_0_boundary_lines_disputed_areas.lwl` | `ne_50m_admin_0_boundary_lines_disputed_areas.geojson` (51,158, `5bf5312a293cf038a367c2f150d34f295a3ccd2704affca26a1bbb1bb95bb64e`) | none | 34 lines, 646 points | 1,828 | `09a3f7ded2568502a75483777994e08046b39e1a68a1da3f5536f70965b9524d` |

The filters are the script's `--max-scalerank`: the whole river file is
69,984 bytes and the whole glaciated file 40,892, and the world view only
shows the features a `scalerank` of 4 or 1 reaches. The lakes and the
shelves are small enough whole. The disputed lines are the boundaries
Natural Earth marks as disputed; the drawer strokes them dashed (4 pixels
on, 3 off, `outline::DASH_ON`/`DASH_OFF`), so they read apart from the
borders without a legend.

```sh
scripts/bake-world-outline.py ne_50m_lakes.geojson \
    leviculum-map/data/ne_50m_lakes.lwo
scripts/bake-world-outline.py --max-scalerank 4 ne_50m_rivers_lake_centerlines.geojson \
    leviculum-map/data/ne_50m_rivers_lake_centerlines.lwl
scripts/bake-world-outline.py --max-scalerank 1 ne_50m_glaciated_areas.geojson \
    leviculum-map/data/ne_50m_glaciated_areas.lwo
scripts/bake-world-outline.py ne_50m_antarctic_ice_shelves_polys.geojson \
    leviculum-map/data/ne_50m_antarctic_ice_shelves_polys.lwo
scripts/bake-world-outline.py ne_50m_admin_0_boundary_lines_disputed_areas.geojson \
    leviculum-map/data/ne_50m_admin_0_boundary_lines_disputed_areas.lwl
```

### The names (1683)

A table of names, `LWN1` (`scripts/bake-world-outline.py`'s docstring, and
`src/outline.rs` for its reader): a point at the world coordinate, a kind,
a rank, the shallowest zoom that shows it, the local name, and the German
one where it differs. The local name is the name in the language of the
place's country, for the countries whose one language Natural Earth
carries a name field of (the script's `LANGUAGE`: Italia, Москва, Milano,
España); Natural Earth has no endonym field, and its `NAME` is English or
international, so every other feature keeps `NAME` as its local name
(Prague, the seas, the ranges). `NAMEPAR` is not used: it holds a local
form for Lisboa and Wien but an old name for Mumbai (Bombay) and an
English one beside København. The German name is `NAME_DE`, else `NAME`.

Every frontend shows the German name, whatever the app's own language:
the window and the phone (`loquax::mapnames`), the terminal
(`loquax::ui::map`) and the M9 (`loquax_m9::map`) all set a label's
`text`; the local name is the label's `local`, which no frontend shows
yet. A country's shallowest zoom is its `MIN_LABEL`, floored (1712; its
`MIN_ZOOM` is 0 for every country, so 1683 set Malta, Barbados and Cabo
Verde at z2), a place's its `MIN_ZOOM`, rounded.

| table | kinds | points | alternates | strings | bytes | baked sha256 |
|---|---|---|---|---|---|---|
| `ne_50m_names.lwn` | city, town, village, country, sea, mountain range and peak, desert | 1,851 | 902 | 2,704 | 63,899 | `f69f4a81be95be54c382da1ccfa1101282dca26e3605af012d2e2284da420829` |
| `ne_50m_names_m9.lwn` | the same, zooms 0 to 3 only | 206 | 138 | 340 | 7,421 | `36300f0b08c9944c82dc30f1688a2fd2455ee08a86e9afe3c22148c0951403e2` |

The full table is the one the desktop, the terminal and the phone read
where no region covers the view; the M9's is cut to its z0 to z3 overview
and costs it about 7 KB of flash. The sources, all of the same release:

| source | bytes | sha256 | what becomes names |
|---|---|---|---|
| `ne_50m_populated_places.geojson` | 3,350,885 | `da4662b7bbfeb897d02f228c5839131dce27acff5717630f91ccff4f67828ee7` | every place, `SCALERANK` 0-2 a city, 3-5 a town, else a village; `MIN_ZOOM` its zoom, `LABELRANK` its rank |
| `ne_50m_admin_0_countries.geojson` | 3,083,490 | `3e458fc036ad0a66411f2c1e6cac49c5d7bfb81cb1123bc513b22511a2b7fdeb` | every country, its `LABEL_X`/`LABEL_Y` the point, its `MIN_LABEL` floored its zoom (1712) |
| `ne_50m_geography_marine_polys.geojson` | 1,163,418 | `6fe58083e0cc5c7fad9e396970e28a8580bbd8770cfa4d1d7b5a34423e912f97` | every sea, gulf and bay, `min_label` its zoom |
| `ne_50m_geography_regions_polys.geojson` | 4,305,815 | `33396643d8f0eed408e80bf5ca43dcdb7993f790fdc4eadae311660f8b91fe76` | the regions whose `FEATURECLA` is `Range/mtn` or `Desert`, and no other |
| `ne_50m_geography_regions_elevation_points.geojson` | 104,139 | `ebe76e874f1f873875cdacec5955f2280fdcd8be9faddf71f035dac8ccd16b60` | the `mountain`s and `volcano`s, as a range's name |

```sh
scripts/bake-world-outline.py --names leviculum-map/data/ne_50m_names.lwn \
    ne_50m_populated_places.geojson ne_50m_admin_0_countries.geojson \
    ne_50m_geography_marine_polys.geojson ne_50m_geography_regions_polys.geojson \
    ne_50m_geography_regions_elevation_points.geojson
scripts/bake-world-outline.py --names-max-zoom 3 \
    leviculum-map/data/ne_50m_names_m9.lwn \
    ne_50m_populated_places.geojson ne_50m_admin_0_countries.geojson \
    ne_50m_geography_marine_polys.geojson ne_50m_geography_regions_polys.geojson \
    ne_50m_geography_regions_elevation_points.geojson
```

A kind a reader does not know is skipped, so a later release may add one.
The whole world view — land, borders, these five layers and both name
tables — is 393,521 bytes, under the 400 KB the concept pins and a test
holds it to (`leviculum-map/tests/golden.rs`).

**Natural Earth** is in the public domain
(<https://www.naturalearthdata.com/about/terms-of-use/>); no permission is
needed and none is claimed. The map names it on the view anyway, as the
attribution line "Natural Earth", the way it names OpenStreetMap over a
region. Chosen over the world file ratatui's `Map` shape draws, whose
licence is unsettled (`docs/src/concepts/loquax-map.md`, section 7).

## The world overview

`../data/world-overview.lmr` is not a test fixture either but the region
the ThinkNode M9 carries in its image so that any view at any place shows
a map (1477, `loquax-m9/src/map.rs`, `WORLD`): the whole world at zooms 0
to 3 under layer profile 1, coasts, land cover, the major roads and 183
place names at z3 (no country borders: profile 1 has no boundary class) (`LMR1` version 4), 755,245 bytes, sha256
`b4e5d8626a4e52537a23dde58a97ddb5ab2568ef5fab19605e3e5b9798b00b3c`. The
board draws it down to z11 (the map core's overzoom of 8 past z3); a
detailed region wins wherever one lies under the view. Cut with

```sh
leviculum-map-tool cut -o world-overview.lmr https://build.protomaps.com/20261108.pmtiles \
    --bbox -180,-85,180,85 --min-zoom 0 --max-zoom 3 --locale de
```

from the Protomaps daily build of 2026-11-08 (Protomaps Basemap 4.15.2,
OpenStreetMap data of 2026-11-08T04:00:00Z), which the bucket keeps for
about a week; cutting it twice gave the same bytes. Zooms 0 to 4 would be
1,950,000 bytes and 0 to 5 5,238,084 bytes, over the 1 MiB the image
grants the world.

Since 1683 the board draws the country borders and the disputed lines of
`../data/ne_50m_admin_0_boundary_lines_land.lwl` and
`../data/ne_50m_admin_0_boundary_lines_disputed_areas.lwl` over that
region — 51,340 bytes — with the world view's own names over them from
`../data/ne_50m_names_m9.lwn` (7,421 bytes, zooms 0 to 3, the layers
above). The region's own 183 places stay; a name both tables carry is
placed once, by its text.

The same ODbL notice as above applies; the file carries it inside itself.

## The time zones (1684)

`../data/timezones.lwo`, `../data/timezones.lwl` and `../data/timezones.lwz`
are the world's time zone outlines the crate's `timezones` module reads
(`src/timezones/mod.rs`): timezone-boundary-builder release 2026f, the
"same since now" variant with the oceans, 63 zones.

| table | what | count | bytes | baked sha256 |
|---|---|---|---|---|
| `timezones.lwo` (`LWO2`) | every polygon of every zone, zones in name order | 3,004 rings, 1,334 holes, 598,780 points | 1,007,112 | `e7e1dd47991a3dbd0f58bc926f9f5f41c86f074fa294a6247dd26d527897ea1c` |
| `timezones.lwl` (`LWL2`) | the borders between zones, each once | 2,909 lines, 302,268 points | 503,524 | `c31cc9e1af9028e327e8f1632bb51b79959abfbc7bab7d7e2c401aa7af2d3c19` |
| `timezones.lwz` (`LWZ1`) | zone names; per ring its zone and label anchor | 63 zones, 436 labels | 9,515 | `5e19dbc13105006d974b04480b5c651a9f07b013a6fdac4e0cc0addd09782eb9` |

Source:
`https://github.com/evansiroky/timezone-boundary-builder/releases/download/2026f/timezones-with-oceans-now.geojson.zip`
(26,768,042 bytes, sha256
`5983306d5b8646ee834e9ab60a83fcf3330119260c9cd2a1f16c9690d008b4f0`),
holding `combined-with-oceans-now.json` (86,399,837 bytes, sha256
`016b2a6f78d53f079a400401a8d8d43fc75e3f40f62cdce34238c0f77c4bf69e`).
Baked with python3-shapely 2.1.0 on GEOS 3.13.1, coverage simplification
at a tolerance of 2 grid units (the world outline's 18-bit grid, 153 m at
the equator), twice to the same bytes:

```sh
scripts/bake-timezones.py combined-with-oceans-now.json leviculum-map/data
```

The source is not a clean coverage (23 small gaps, 45 small overlaps,
borders whose two sides differ vertex for vertex); the script rebuilds it
as one before simplifying, so every border is simplified once and stays
shared (its docstring has the steps). At tolerance 6 the three tables are
793,101 bytes, at 3 1,175,882, at 2 1,520,151.

**Licence.** The README of the release, section "Licenses": "The outputted
data is licensed under the [Open Data Commons Open Database License
(ODbL)](http://opendatacommons.org/licenses/odbl/)." The data is almost
wholly OpenStreetMap's. These tables are derived databases under the ODbL
1.0, <https://opendatacommons.org/licenses/odbl/1-0/>, not under the AGPL
of the code; a view that draws them carries the attribution
`timezones::ATTRIBUTION`, "Time zones: © OpenStreetMap contributors,
timezone-boundary-builder (ODbL)".

### The board's clock rules

`../data/tz-rules.txt` (2,120 bytes, sha256
`8a80139004ee15e30a1d26a8be9b90ae77b1a86037a2fd90aa0cd532a1731bce`) holds
the POSIX TZ string of every zone above, from the footers of tzdata 2026f,
which the board evaluates (`timezones::board_offset`). The build host had
Debian's 2026b, which lacks 2026c to 2026f (Alberta, Morocco, the
Northwest Territories, Manitoba, Cuba), so the zone files were compiled
from IANA's release: `https://data.iana.org/time-zones/releases/tzdb-2026f.tar.lz`
(569,858 bytes, sha256
`357829e3a16a78224d23f1ae2327704c1da1fd35f6769a7aac0682755fb9027a`),
public domain.

```sh
tar --lzip -xf tzdb-2026f.tar.lz && cd tzdb-2026f && make zic
./zic -b fat -d ../zoneinfo-2026f tzdata.zi && cp tzdata.zi ../zoneinfo-2026f/
scripts/gen-tz-table.py ../zoneinfo-2026f
```

`zoneinfo-2026f/` here is that output for the 63 zones and
`Africa/Casablanca`, 64 files, 87,563 bytes: the test holds the board's
strings to the TZif files they came from, every hour of 2026, and finds
the one known disagreement, Havana from 2026-11-01T05:00Z (Cuba stays on
-04 through 2027-11-07, an exception its lasting rule cannot express).

## Countries

`../data/ne_50m_admin_0_countries.lco` is the table `src/countries.rs`
looks a point's country up in: Natural Earth's 1:50m admin-0 countries,
version 5.1.2, 239 countries, 1,631 rings (12 of them holes, Lesotho's in
South Africa among them) and 97,910 points in 200,354 bytes, the rings in
the `LWO2` format of the world outline at 16 bits behind a header of codes
(`LCO1`). The code is Natural Earth's `ISO_A2`, else its `ISO_A2_EH`
(France, Norway, Kosovo XK, Taiwan TW, the Australian territories); the
Indian Ocean Territories split into Christmas Island CX and the Cocos
(Keeling) Islands CC; Somaliland counts as SO and Northern Cyprus as CY,
ISO's view; the Siachen Glacier, in no ISO country, is left out. Natural
Earth keeps overseas departments inside their state (French Guiana is FR).
Baked with

```sh
scripts/bake-country-outline.py ne_50m_admin_0_countries.geojson \
    leviculum-map/data/ne_50m_admin_0_countries.lco
```

from
`https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_50m_admin_0_countries.geojson`
(3,083,490 bytes, sha256
`3e458fc036ad0a66411f2c1e6cac49c5d7bfb81cb1123bc513b22511a2b7fdeb`). Baking
again gives the same bytes (sha256
`3b27ee3172c3dbf6de3fb31ca4923e76df9508ef2abb8e040773649ecb69374d`). Public
domain, as the outline above.

`../data/country-facts.lcf` holds per country its English name, emergency
numbers, driving side and currencies (`LCF1`, UTF-8 text, 255 rows, 8,421
bytes, sha256
`9c21b8bbd6894f64b3dcf91c8bc6a47477544705e252fad584c1748615acad9e`), from
Wikidata: emergency phone number P2852 with its use P366, driving side
P1622, currency P38 with its ISO 4217 code P498, over every item with an
ISO 3166-1 alpha-2 code P297 that is not dissolved. Baked with

```sh
scripts/bake-country-facts.py --fetch wikidata-country-facts.json \
    leviculum-map/data/country-facts.lcf
```

whose query ran against `https://query.wikidata.org/sparql` on 2026-11-11
at 19:14:58 UTC; the response (1,762,694 bytes, sha256
`828ba77b85e02bf8dc427d2422d978e225bd771f458eac8b4b158aefde62f946`) bakes
again without `--fetch` into the same bytes. A later query gives other
bytes as Wikidata changes. Where Wikidata has no value the field is empty:
no emergency number for AC AQ BL BQ BV CC CQ CX DG EH FK GF HM IO KP MF MP
MS NF PN PS SJ SX TA TF TK UM WF, no driving side for AQ BV CP HM, no
currency with an ISO code for AC AQ BV CP CQ DG FO HM.

**Wikidata**: "All structured data from the main, Property, Lexeme, and
EntitySchema namespaces is available under the Creative Commons CC0
License" (<https://www.wikidata.org/wiki/Wikidata:Licensing>, read
2026-11-11), CC0 1.0, <https://creativecommons.org/publicdomain/zero/1.0/>.
No attribution is owed; this note names the source anyway.
