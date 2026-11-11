# Fixtures

`firenze.lmr`: a region file (`LMR1` version 2, layer profile 1) of
central Florence, zooms 11 to 15, six tiles (z15/17408/11943 and
z15/17409/11943, and the one tile above them at each of z14 to z11), with
the names of its places, parks and streets and the German name where it
differs (Firenze, Florenz), 197,972 bytes. Cut with

```sh
leviculum-map-tool cut -o firenze.lmr firenze.pmtiles \
    --bbox 11.2505,43.7728,11.2700,43.7785 --min-zoom 11 --max-zoom 15 \
    --locale de
```

from the Protomaps sample archive
`https://pmtiles.io/protomaps(vector)ODbL_firenze.pmtiles` (6,601,156
bytes, sha256 `7190f3d807a62f4f012b574007c96b809f6842f45a6b0c508639331fc68fd30a`;
Protomaps Basemap schema 3.7.1, planetiler 2024-08-01, OSM replication of
2024-08-07). Re-cutting gives the same bytes.

`berlin/`: two region files of central Berlin, for a pinch from z14 to
z10 that needs names at every step (1225), in a directory of their own so
the tests that read this one see Florence alone. `wide.lmr` (77,613
bytes, sha256 `f0aec6f7981750ba48fb57eb566b4f66ee0c864f6b690e1b842dc9216ba6ef5b`)
holds zooms 10 and 11, one tile each, `mitte.lmr` (181,205 bytes, sha256
`9c048b801577045b284e66618a6b3131ff9a7d094c35b8a1413dbc8f5bb1f6af`)
zooms 12 to 14, one tile each. Cut with

```sh
leviculum-map-tool cut -o wide.lmr https://build.protomaps.com/20261031.pmtiles \
    --bbox 13.37,52.50,13.45,52.54 --min-zoom 10 --max-zoom 11 --locale de
leviculum-map-tool cut -o mitte.lmr https://build.protomaps.com/20261031.pmtiles \
    --bbox 13.390,52.515,13.400,52.520 --min-zoom 12 --max-zoom 14 --locale de
```

from the Protomaps daily build of 2026-10-31, which the bucket keeps for
about a week; a later build cuts other bytes.

`buildings/firenze.lmr`: the same Florence region cut again with its
building footprints (`LMR1` version 3, 1191), 248,384 bytes, sha256
`7e474141c3bd7c907d97869751ca80049d363497e63294a114e6e625acb5ab40`: two
building sections, 2,931 footprints in 50,115 bytes. Its tile blobs are
`firenze.lmr`'s byte for byte; its names are a newer cut's (271 strings
against 265). In a directory of its own, so the tests that read this one
see the region without buildings. Cut from the same archive with

```sh
leviculum-map-tool cut -o firenze.lmr firenze.pmtiles \
    --bbox 11.2505,43.7728,11.2700,43.7785 --min-zoom 11 --max-zoom 15 \
    --locale de
```

`frankfurt/roemer.lmr`: central Frankfurt around the Römer, zoom 15 only,
six tiles with their names and building sections (`LMR1` version 3,
1193), 159,451 bytes, sha256
`e4db5f570ed9b99cc203f9927ef199e6dc57a56e4174700a3378b1f986494197`:
5,181 footprints in 78,364 bytes, at most 1,352 in a tile. The device
walk's `buildings` step puts it on the phone and opens the map on it at
z15 and z16 (`docs/src/concepts/loquax-device-walk.md`). In a directory of
its own, so the tests that read the others see none of it. Cut with

```sh
leviculum-map-tool cut -o roemer.lmr https://build.protomaps.com/20261031.pmtiles \
    --bbox 8.6765,50.1050,8.6875,50.1165 --min-zoom 15 --max-zoom 15 --locale de
```

from the Protomaps daily build of 2026-10-31; cutting it twice gave the
same bytes.

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
| `ne_50m_lakes.lwo` | `ne_50m_lakes.geojson` (876,018, `d350b759…378a52`) | none | 465 rings, 53 holes, 18,149 points | 44,502 | `04042ea3…392d0` |
| `ne_50m_rivers_lake_centerlines.lwl` | `ne_50m_rivers_lake_centerlines.geojson` (827,318, `f286e0ce…73ccc51`) | `scalerank <= 4` | 411 lines, 13,536 points | 36,236 | `efe2a2f0…beda698` |
| `ne_50m_glaciated_areas.lwo` | `ne_50m_glaciated_areas.geojson` (512,453, `c7a097b4…8e9c0a7`) | `scalerank <= 1` | 31 rings, 13 holes, 5,720 points | 16,422 | `e9191d76…6620ba` |
| `ne_50m_antarctic_ice_shelves_polys.lwo` | `ne_50m_antarctic_ice_shelves_polys.geojson` (131,055, `fedf866d…d547e790`) | none | 101 rings, 36 holes, 4,905 points | 14,344 | `a8290d6e…061bbd` |
| `ne_50m_admin_0_boundary_lines_disputed_areas.lwl` | `ne_50m_admin_0_boundary_lines_disputed_areas.geojson` (51,158, `5bf5312a…5bb64e`) | none | 34 lines, 646 points | 1,828 | `09a3f7de…b9524d` |

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
a rank, the shallowest zoom that shows it, and the local name with the
German one where it differs. The locale is German as the region fixtures
are cut for German; a frontend that would rather read the local name has
it in the same point, and a reader's language rule picks.

| table | kinds | points | alternates | strings | bytes | baked sha256 |
|---|---|---|---|---|---|---|
| `ne_50m_names.lwn` | city, town, village, country, sea, mountain range and peak, desert | 1,851 | 702 | 2,504 | 59,859 | `2df7ebaa…6d89565` |
| `ne_50m_names_m9.lwn` | the same, zooms 0 to 3 only | 337 | 205 | 534 | 12,146 | `adca2556…624b499` |

The full table is the one the desktop, the terminal and the phone read
where no region covers the view; the M9's is cut to its z0 to z3 overview
and costs it about 12 KB of flash. The sources, all of the same release:

| source | bytes | sha256 | what becomes names |
|---|---|---|---|
| `ne_50m_populated_places.geojson` | 3,350,885 | `da4662b7…f67828ee7` | every place, `SCALERANK` 0-2 a city, 3-5 a town, else a village; `MIN_ZOOM` its zoom, `LABELRANK` its rank |
| `ne_50m_admin_0_countries.geojson` | 3,083,490 | `3e458fc0…2b7fdeb` | every country, its `LABEL_X`/`LABEL_Y` the point |
| `ne_50m_geography_marine_polys.geojson` | 1,163,418 | `6fe58083…43e912f97` | every sea, gulf and bay, `min_label` its zoom |
| `ne_50m_geography_regions_polys.geojson` | 4,305,815 | `33396643…0f8b91fe76` | the regions whose `FEATURECLA` is `Range/mtn` or `Desert`, and no other |
| `ne_50m_geography_regions_elevation_points.geojson` | 104,139 | `ebe76e87…8ccd16b60` | the `mountain`s and `volcano`s, as a range's name |

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
tables — is 394,206 bytes, under the 400 KB the concept pins and a test
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
`../data/ne_50m_names_m9.lwn` (12,146 bytes, zooms 0 to 3, the layers
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
