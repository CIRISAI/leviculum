# The map server

Order 1686 (K4), the expedition-map half of the Leitstern: the server
is the precondition for loading by looking, shading, points of
interest and sharing. This page decides the store on disk, the signed
index, what a device pins, the request protocol shared by Reticulum
and HTTP, the front-door cache, and the attribution per pack. The
minimal implementation lives in `lnmapd/` — `lnmapcut` writes the
store, `lnmapd` serves it, cutting and serving separate programs —
and every claim of this page is held by that crate's tests.

Decided by Lew (2026-11-11): every kind of data — vectors, shading,
points of interest, later the elevation fine tier — is a pyramid cut
on the server into immutable pieces; the server signs an index of
their hashes; a device verifies against the pinned server key,
whoever carried the piece; the setting is the server's Reticulum
address, the single trust anchor (`map_destination`); the store lives
at Lew's home, workorse is the public front door with a cache.
**Proposed by the reviewer, not Lew's word, in two places below:**
that the signed index names the server's own HTTP address, so one
value configures both paths (§ "The index"), and that the home store
opens no port and builds the outgoing tunnel to the front door itself
(§ "The tunnel from home").

[Loquax: map data over Reticulum](loquax-map-over-reticulum.md) fixed
the destination (`lev.maps`, its own daemon `lnmapd`, never a mode of
`lnpnd`), the client ladder, the speed probe and the fairness rules;
all of that stands. Its request API (`/regions` with whole-file
hashes, `/region` by id) is superseded by the piece protocol here:
the index replaces `/regions`, pieces replace `/region`, and the
catalogue thinking — pre-cut packs, the client takes the smallest
covering its view — is unchanged.

## The store on disk

```text
<store>/index                  the signed index, replaced atomically
<store>/pieces/<aa>/<64 hex>   one piece, immutable, named by the
                               SHA-256 of its bytes (<aa> its first
                               two characters, fanning the directory)
```

One directory, two transports: `lnmapd` serves it over HTTP and over
Reticulum, nginx may serve the same files statically. Pieces are
immutable — their name is their content — so every cache layer from
the front door to a browser may hold them forever; only `index`
changes, and it is replaced atomically (temp file and rename), so a
reader never sees a half-written index.

A pack is one logical cut of one data kind: `vectors` (a region file
per catalogue entry, the format of [Loquax: the map](loquax-map.md)
section "Region file") and, since order 1740, `elevation` (a height
file, § "Height packs" below); shading and pois-as-their-own-tier get
their own `kind` strings in the same index without a format change.

## The pieces: a prefix split of the region file

The region format did the hard work already: from version 4 the file
is written index first, then the tiles coarse to fine, the names and
buildings last, exactly so that a reader of a file still arriving
uses every tile that is there (`leviculum-map/src/region.rs`, the
1291 layout). So a pack's pieces are nothing but the byte ranges of
the region file, split at zoom boundaries
(`lnmapd/src/store.rs`, `split_region`):

- piece 0: the header, the texts, the tile index, and the shallowest
  zoom's tiles;
- each further piece: one zoom deeper (a zoom with no bytes of its
  own folds into the piece before it);
- the last piece: the deepest zoom plus the label, building and poi
  sections at the file's end.

The pieces concatenate back to the byte-identical file, and **every
prefix of pieces is a readable region**: fetch them in order, stop
when the time budget is spent, and the map draws what arrived —
coarse before fine is the file layout, not a protocol feature. A
resumed fetch continues at the next piece; nothing is re-sent. Each
piece records the deepest zoom that is complete once it is there, so
a client can say "z13 landed, z15 is still coming" without opening
the assembly.

Within one pack the fine tier is not split by area: area granularity
comes from the catalogue (city and Bundesland packs), as decided in
[Loquax: map data over Reticulum](loquax-map-over-reticulum.md). A
country pack's fine pieces are large; the catalogue's answer is that
a country is Internet material, and the index's per-piece sizes let
the client ladder say so in numbers.

## The index

One file, binary, little-endian, signed; the exact byte layout is
the module doc of `lnmapd/src/store.rs` and is held by the format
tests. What it carries:

- **Header:** format version, **generation** (strictly grows with
  every write; the rollback check below), created time, and the
  server's own HTTP base URL. The URL in the index is the reviewer's
  proposal: with it, one pinned Reticulum address configures both
  paths — the device learns the HTTP side from the signed index and
  `map_source` stays an override, nothing more.
- **Per pack:** id, kind, bounds, zoom range, total bytes, the
  SHA-256 of the whole assembled file, the attribution piece's hash
  and length, and the pieces in assembly order — each with SHA-256,
  offset, length and the zoom it completes.
- **Trailer:** the server identity's public keys (x25519 and
  ed25519, the announce's 64 bytes) and an ed25519 signature over
  everything before them.

Versioning: the format version is for readers (a reader refuses a
version it does not know); the generation is for freshness. A device
remembers the highest generation it has verified and refuses an
index behind it — a replay of an old, validly signed index cannot
roll a device back to deleted packs or withdrawn pieces.

## What a device pins

One setting: `map_destination`, the server's `lev.maps` destination
hash. The verification chain
(`lnmapd/src/store.rs`, `Index::verify`; client side
`lnmapd/src/client.rs`):

1. The index's embedded public keys are hashed to an identity, and
   the `lev.maps` destination hash is recomputed from them. It must
   equal the pin — so the key that signed the index IS the key
   behind the destination the device would dial on the mesh, and an
   HTTP-only client needs no announce to check it.
2. The ed25519 signature over the index body must verify.
3. The generation must not be behind the highest already seen.
4. Every piece must hash to the name the index gave it — the whole
   piece check, whoever carried the piece: the front door, a
   neighbour, a USB stick. There is no trusted transport, only the
   pin.

A tampered piece is discarded before a byte of it reaches the
assembly; the good prefix stays usable. The shipped default pin is
Lew's server, as the zero-config page already lists its TCP entry; a
heard `lev.maps` announce that is not the pinned one is offered,
never auto-used ([Loquax: map data over
Reticulum](loquax-map-over-reticulum.md), "How it is found").

## The request protocol

The same two asks on both transports, the same piece ids (64 hex
characters of the SHA-256):

| ask        | HTTP                    | Reticulum (`lev.maps` link)            |
|------------|-------------------------|----------------------------------------|
| the index  | `GET <base>/index`      | request `/index`, no data              |
| one piece  | `GET <base>/piece/<id>` | request `/piece`, data `{"piece": id}` |

Answers are the raw bytes (HTTP) and the usual msgpack-bin response
(Reticulum), a single RESPONSE packet when it fits the link MDU, a
response Resource otherwise — the Resource's automatic bz2 earns on
region bytes what it earns on pages (the ~35 % measured in
[Loquax: map data over Reticulum](loquax-map-over-reticulum.md)).
Unknown ids: 404 on HTTP; on the mesh a silent drop, because the
protocol has no 404 and a clean timeout is the correct shape. The
speed probe (`/probe`) and the ask-before-LoRa rules stay as the
ladder page fixed them and ride on this protocol unchanged.

There is no "cut me this box" ask anywhere: the server does owe no
stranger a planet-archive cut; it serves what `lnmapcut` prepared.

## The front door

workorse is the public face; the store lives at home. `lnmapd
--upstream <home>` turns the same server into the front-door cache
(`lnmapd/src/serve.rs`):

- serves what it holds;
- a piece it lacks it fetches from the upstream **once**, checks
  that the bytes hash to the asked name (content addressing is the
  whole integrity check; a lying upstream is refused and nothing is
  cached), stores it, serves it — and never asks again, pieces being
  immutable;
- the piece cache is bounded (`--cache-cap-mb`): least recently
  served pieces leave when the cap is passed, the piece just written
  never;
- the index it refreshes on a TTL (`--index-ttl-secs`); a refreshed
  index must be well-formed, signed by its own embedded key and not
  behind the held generation, else the held one keeps serving — the
  front door cannot pin (it is not a device), but it never steps
  backwards and never serves garbage;
- upstream fetches are serialised: a cold cache costs the home link
  one transfer per piece, not one per requester.

The front door never signs anything and holds no key. A device
behind it verifies exactly as if it had asked the home store.

## The tunnel from home

**Proposed by the reviewer, not Lew's word.** The home store opens
no inbound port: it builds a persistent outgoing tunnel to the front
door itself (an autossh reverse tunnel or a WireGuard peer — K5
picks the mechanism on workorse), and the front door's `--upstream`
points at its local end of that tunnel. The failure modes are then:
home down → the front door serves everything it holds (measured in
the tests: index within TTL and all cached pieces); front door down
→ the mesh path to the home store still answers, since `lev.maps`
needs no inbound HTTP. Until the small home server exists, workorse
alone is both store and front (no `--upstream`), which is the same
binary and the same directory.

## Attribution

Every pack names an attribution piece: a small UTF-8 text piece —
the attribution line and the licence notice, taken from the region
file's own header texts — content-addressed and fetched like any
piece. A client that shows a pack's data can show its attribution
without fetching region bytes, and the licence text travels with the
data on every carrier. The region file itself carries the same texts
in its header (the map view's attribution line keeps working from
the region alone); the index-level piece is for catalogue UIs and
for data kinds whose own format has no text header.

## Worked example

The committed grunewald fixture (z12–15,
`leviculum-mapcut/fixtures/README.md`), cut by `leviculum-map-tool
cut` and added by `lnmapcut --store store --identity key --id
grunewald --http-url https://leviculum.network/maps grunewald.lmr`:

```text
grunewald: 4 pieces, 53302 bytes, zooms 12..=15, generation 1
  piece 0: completes z12, 22603 bytes, 40519689a831b747…
  piece 1: completes z13, 21974 bytes, cb7a33e0c21356ab…
  piece 2: completes z14,  4618 bytes, fb2aead0ac34dbbf…
  piece 3: completes z15,  4107 bytes, fa83751d2510cf05…
```

The signed index is 518 bytes. A client with a two-second budget on
a link that moves ~22 kB/s fetches piece 0, draws the forest at z12,
and picks up piece 1 in the next window; a fast link takes all four
and has the byte-identical region file. The tests
(`lnmapd/tests/store_format.rs`, `fetch_budget.rs`, `http_serve.rs`,
`node_integ.rs`) hold exactly these claims: every piece boundary is
a readable region with its zooms complete, the budget stops the
fetch cleanly, a tampered piece or index or a rollback is refused on
every transport, and the front door survives its upstream.

An id names one pack of one kind. `lnmapcut` refuses to put a height
file under the id of a vectors pack (or the reverse) and leaves the
store as it was, since a device that pinned the id would otherwise get
data of another kind; `--replace` says the swap is meant. The
prefix planner (`lnmapd::client::fetch_prefix`) takes only vectors
packs: a height pack's blocks stand alone and are fetched by block,
so its piece zoom byte is no "deepest zoom complete"
(`lnmapd/tests/store_elevation.rs`).

## Height packs

Order 1740 (K6a, Lew 2026-11-12: desert relief first, heights shipped
to the device instead of finished shade pictures). The data half only:
shading and drawing are order 1741. The byte layout is the module doc
of `leviculum-map/src/elevation.rs` and is held by
`leviculum-map/tests/elevation.rs` and `elevation_alloc.rs`; the cut is
`leviculum-map-tool heights` (`leviculum-map-tool/src/heights.rs`), the
store side `lnmapd/src/store.rs` (`KIND_ELEVATION`).

**Tile layout.** The heights live at z12 only, 256 x 256 per tile, one
height per Web Mercator pixel, sampled bilinearly from the source at the
pixel's centre, north row first. Tiles are grouped in **blocks**: the
8 x 8 z12 tiles under one z9 tile. A block is one piece. The pack's
file is a **directory piece** (the zooms, the box the cut was asked
for, the attribution and the notice, and the block list in piece
order), then the block pieces. z11, z10 and z9 are not shipped: the
device derives them from the block, each height the mean of the 2 x 2,
4 x 4 or 8 x 8 stored heights under it, rounded half up
(`Block::heights`). The pilot measured why: shipping z12 alone and
deriving the rest is 0.77 times the bytes of CoMaps' shade tiles for
the same box, the full z9 to z12 pyramid 1.16 times.

Unlike a region file, a height pack is not a prefix ladder: every
block piece stands alone and is complete from z9 to z12 over its own
area, so a device fetches the directory and then the blocks of its
view in any order. In the index, the directory's zoom byte is 0 (it
completes no zoom), every block's is 12; the pack's zoom range is
9..=12 and its bounds are the cut box.

**Encoding.** Whole metres as `i16`. Per tile: the residual of the 2D
predictor (left + up − up-left; the first row from the left, the first
column from above), in wrapping 16-bit arithmetic so every `i16` grid
round-trips; zigzag to `u16`; two byte planes (all high bytes, then all
low bytes); one zstd frame per tile, level 19, with its content
checksum. The writer is the C zstd library in the cut tool; the reader
is `ruzstd`, pure Rust and `no_std`, the same code on the boards, the
Android facade and the desktop. The committed Erg Chebbi fixture was
written by the C library and is read by the Rust decoder in the tests,
so the interop is tested, not assumed.

**Why metres suffice.** The relief of order 1741 normalises the shade
by local relief, so what matters is the height error against the
smallest relief the desert shows: Chegaga's linear dunes stand 20 to
80 m, Erg Chebbi's z12 tiles span 692 to 904 m. The vertical error
budget, per height:

| source of error | size |
|---|---|
| GLO-30 itself, absolute, against the reference (from the cell's own XML, `absolutePositionalAccuracyLE90`, cell N31 W005) | 1.76 m LE90, about 1.07 m as a standard deviation |
| rounding to whole metres | uniform within ±0.5 m, 0.29 m standard deviation |
| both together | about 1.11 m standard deviation: the rounding adds 4 % |
| the derived zooms | a mean of 4 to 64 stored heights, rounded once more: ±0.5 m |

So the rounding error is a quarter of the source's own, and 2.5 % of the
lowest dune. Its one visible trace: on ground flatter than one metre
per z12 pixel (about 33 m at 30° N, so slopes under 1.7°) the stored
surface steps in one-metre terraces; the source's own noise on such
ground is the same size, and the shading's smallest scale has to blur
over a few pixels in either case (1741 decides how). Decimetres would
cost 2.43 times the shade tiles' bytes instead of 1.16 (pilot) for an
accuracy the source does not have.

**Versioning and refusals.** Both piece kinds carry a magic (`LMHD`,
`LMHB`), a `u16` version (1) and their header length. A reader refuses
a version it does not know, rather than guess; a later version that
changes the encoding gets a new number, and a server that ships it
keeps the old pack until devices read the new one. Before allocating
anything a length field sizes, the reader checks that the lengths add
up to exactly the piece's own length, that the zooms and the tile edge
are ones it reads, that the tile count is the zooms' square and that
the block lies on the block zoom's grid; a hostile length is refused
with nothing allocated (`elevation_alloc.rs` counts the bytes). A tile
frame is refused when it does not decode to exactly one tile, carries
no content checksum or a wrong one; the zstd window is capped at one
tile and the decoder drains one tile's worth of blocks at a time, so a
frame of endless RLE blocks is refused within a megabyte (measured: the
uncapped decoder reached 200 MB on the test's frame).

**The cut.** `leviculum-map-tool heights --bbox w,s,e,n -o pack.lmh`
reads Copernicus GLO-30 from the public bucket (or `--source <dir>`).
It walks the blocks a row at a time, north to south; for each row it
fetches the 1° source cells the row needs (with the bilinear's reach
one source pixel past the row's south and east edges) into memory,
decodes them, drops the cells the row no longer needs, and writes each
block as it is done. Nothing touches the disk but the output: the peak
disk is the pack, the peak memory the cells of one block row (ten
cells, 631 MB resident, on the pilot box). A missing cell is sea level
(the bucket has no cells over open sea). The output depends only on
the source bytes, the box and the zooms; the tests cut twice and with
one and three threads and compare bytes.

**Attribution.** Taken from the licence that ships beside every cell
(`INFO/eula_F.pdf`): the attribution is Article 6 (b)'s notice for
adapted data, "produced using Copernicus WorldDEM-30 © DLR e.V.
2010-2014 and © Airbus Defence and Space GmbH 2014-2018 provided under
COPERNICUS by the European Union and ESA; all rights reserved"; the
notice names the licence and carries Article 6 (c)'s disclaimer word
for word. Both are in the directory piece and in the pack's
attribution piece.

**Measured on the pilot box** (29.3 to 31.5 N, 7.6 to 3.7 W, 2026-11-12,
this host, release build):

| | |
|---|---|
| blocks, tiles | 30 z9 blocks, 1,920 z12 tiles (they reach past the box) |
| pack | 37,063,251 bytes over 136,583 km² of blocks: 271 bytes per km² |
| the 1,350 z12 tiles that meet the box | 26,596,337 bytes (the pilot: 26.0 MB) |
| source | 20 GLO-30 cells fetched into memory (the largest 42.9 MB), none absent |
| cut | 118 s wall, 49 s user; zstd-19 encode 22 ms per tile (43 s summed) |
| host decode, z12 | 1.74 ms per tile |
| host derive, z11 / z10 / z9 | 7.1 / 28 / 111 ms per tile (4, 16, 64 tiles decoded) |
| working set to decode | 643,016 bytes peak on the heap: three tile buffers (384 KiB) plus the zstd decoder (about 244 KiB), pinned under 684 KiB by `elevation_alloc.rs` |

Not measured yet: the decode in the Android facade and on the M9 (no
device on the cutting host). What the M9 must hold is the working set
above, so it reads heights from PSRAM, not internal RAM; the flash cost
is `ruzstd`'s decoder.

## What this page does not decide

Deployment (K5 on workorse: nginx in front, the tunnel mechanism,
systemd units), the Loquax client ladder's move from whole-region
fetches to pieces (it keeps its probe, thresholds and questions),
the shading itself (order 1741 draws from the height packs above), and periculum cells for the served
store (the S1 cells of [Loquax: map data over
Reticulum](loquax-map-over-reticulum.md) apply to this protocol and
come with the ladder work).
