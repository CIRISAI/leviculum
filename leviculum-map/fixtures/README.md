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

**Natural Earth** is in the public domain
(<https://www.naturalearthdata.com/about/terms-of-use/>); no permission is
needed and none is claimed. The map names it on the view anyway, as the
attribution line "Natural Earth", the way it names OpenStreetMap over a
region. Chosen over the world file ratatui's `Map` shape draws, whose
licence is unsettled (`docs/src/concepts/loquax-map.md`, section 7).
