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
