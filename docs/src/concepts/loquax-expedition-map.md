# Loquax: the expedition map

Part of the [Loquax design record](loquax.md). Lew, 2026-11-11: the map
in Loquax grows into a full offline travel map for any kind of journey,
and stays fused with the chat. This page holds the decisions of that
conversation and the three studies run beside it. [Loquax: the
map](loquax-map.md) keeps the renderer, the region format and positions
as they are today; this page is where the map goes next.

Status words: **decided** is Lew's word of 2026-11-11, **proposed** is
the reviewer's and not yet confirmed, **open** needs Lew or a study.
Later orders cite this page by section number and by the K-numbers of
the last section, so neither is renumbered. The draft it was made from
(`expedition-map-concept-2026-11.md`, order 1682) was checked against
the tree on 2026-11-11; a statement the tree does not support carries
"(not verified in the tree)".

## 1. What it is

- **One app, not two (decided).** The fusion of chat, positions and map
  is the product; no other map app has a mesh under it. A second
  launcher entry that opens the same app in the map, without chat
  onboarding, is optional (Android `activity-alias`, a second `.desktop`
  entry). The map crates stay free of chat so a map-only build remains
  possible later.
- **Not a clone (decided).** CoMaps and Organic Maps are the reference
  for scope, not a blueprint. What this map does differently (Lew,
  2026-11-11):
  - **Married to Reticulum.** Map, positions and messages are one
    thing; that is the vision no existing map app can host.
  - **Lean.** The core entirely in Rust, efficient, with as few
    external libraries as possible. Keep it simple; the existing apps
    have grown heavy.
  - **No monetisation.** Nothing in the app is built to earn, since
    that is where an app starts to work against its users.
  - **Open to every contribution,** from many hands, whatever tools the
    contributor used.
- **The model for look and touch is MapOut (decided),** the iOS app: its
  map picture, and how a finger lays a route and measures a distance.
  A study of MapOut precedes the route editor.
- **For every kind of travel (decided).** No selection of content by
  travel style, see points of interest below.
- **Order (decided in outline):** the cheap things first, offline
  routing last.
- **The same with perfect Internet (decided).** The expedition is the
  hard case, not the only one. A family walking through an amusement
  park, every phone on a good mobile network, uses the same features
  and sees the same thing. So every feature here has one face for the
  user and picks its path by itself: through the home and the Internet
  while they are in reach, directly between devices when they are not.
  No feature may need the mesh, and none may need the Internet.
- **Every decision the user has to take is a cost (decided).** The user
  need not understand the system. Whatever can be decided by a rule is
  decided by a rule, in daily life as on an expedition. The one step
  that cannot be automated is setting up an external mailbox. So every
  choice, switch and question in this concept has to justify itself;
  the ones left are listed in section 11.
- **A desktop and a phone can each stand alone, and each can work with
  an external mailbox (decided).** The phone can be a full node; the
  desktop gets the unnoticed local mailbox the phone has.

## 2. The world view

Today the far zooms show Natural Earth land and country borders only
(`leviculum-map/data/ne_50m_land.lwo`, 159,357 bytes, and the boundary
lines `leviculum-map/data/ne_50m_admin_0_boundary_lines_land.lwl`,
49,512 bytes), up to z10 (`MAX_ZOOM` (`leviculum-map/src/outline.rs:58`)).

**Decided:** more Natural Earth on z0 to z9, public domain, version
5.1.2, baked by the existing script (`scripts/bake-world-outline.py`).

| layer | filter | bytes | |
|---|---|---|---|
| lakes, 1:50m | none | 44,502 | measured |
| rivers and lake centerlines | scalerank <= 4 | 36,236 | measured |
| glaciated areas | scalerank <= 1 | 16,422 | measured |
| Antarctic ice shelves | none | 14,344 | measured |
| disputed area lines | none | 1,828 | measured |
| populated places, 1,251 points | | about 30 KB | estimate |
| country names | | about 8 KB | estimate |
| marine, region and elevation names as points | | about 15 KB | estimate |

Measured with a copy of `scripts/bake-world-outline.py` that reproduces
the committed land file byte for byte (the copy and the baked files are
not in the tree). Total about 166 KB beside today's 209 KB.

- **Disputed boundaries are drawn dashed (decided).**
- **Places come from the full populated places file (decided),** which
  carries German names; the simple file does not. Language rule as for
  regions: the local name plus the user's language where it differs.
- **A full world relief is a later step of its own (decided).**
- Names reuse the one placement rule. Missing are label kinds for
  country, sea, mountain range and desert; readers skip unknown kinds,
  so new kinds are compatible. Names without a region need a labels-only
  source that does not count as a filled region.
- **The M9 draws a different world,** a Protomaps z0 to z3 cut of
  755,245 bytes (`leviculum-map/data/world-overview.lmr`, `WORLD` in
  `loquax-m9/src/map.rs`) with land, lakes and 183 place names, without
  borders and without sea or range names
  (`leviculum-map/fixtures/README.md`, "The world overview"). Proposed
  there: the border table plus about 15 KB of names over the overview.
  Flash is ample by the last documented headroom (`free=6605664` of the
  8 MiB application, [the handheld messenger](handheld-messenger.md));
  the draw scratch in RAM is not measured.
- Desktop users never see Protomaps' own Natural Earth zooms, since the
  autoload starts at z7 at the earliest: nothing is asked for below
  `MIN_ZOOM` (`loquax/src/ui/viewload.rs:53`), z10, and a view loads
  `VIEW_BELOW` (`loquax/src/ui/map.rs:126`), three zooms above its own.
  The world view is ours alone.

### The world in Equal Earth, flowing into Mercator

Built by order 1710 (Lew, 2026-11-11): the world view's projection.

The world view is drawn in the Equal Earth projection — areas true to
size, so Greenland is a third of Australia and not seven times
Africa's share — and from some zoom on it goes over into Mercator,
what every tile in the world is cut in. There is no hard break: one
mapping for the view, a blend of two screen positions
(`leviculum-map/src/blend.rs`),

```text
P(point) = (1 - t) · W_c(point) + t · M_c(point)
```

- `M_c` is the view's Mercator as every deeper zoom draws it, the
  centre on the middle pixel.
- `W_c` is the world projection with its **central meridian at the view
  centre's longitude** (north is up at the centre in both, nothing
  tilts while blending), **scaled and shifted so the view centre maps
  to the same pixel at the same local scale** as Mercator's. The world
  projection is a parameter: Equal Earth (Šavrič, Patterson, Jenny
  2018; the published polynomial, its inverse by Newton) ships as the
  default; the orthographic globe is implemented behind the same
  parameter and the `globe-world` build switch, until Lew has chosen
  on pictures.
- `t` is a smooth step of the view zoom, `6u⁵−15u⁴+10u³` — C² at both
  ends, no kink: 0 up to z2 (or the width's world floor on a screen
  wider than 1024 px), 1 from z5 on. Regions are cut from z6 down
  today, and the blend additionally holds **only where no region
  reaches into the view**, so no raster or tile is ever reprojected:
  the moment region data appears, the view is Mercator-linear exactly.

Because both mappings pin the centre at the same local scale, what is
in the middle of the screen does not move during a zoom beyond what
the zoom itself explains; only the far parts of a world view bend.
Panning turns the central meridian with the view, so the world wraps
and nothing jumps at the antimeridian; a land ring crossing the
projection's cut (the meridian opposite the centre) is unwrapped and,
where it overshoots one edge, drawn again a turn over, so both edges
show their part.

#### The scale match, and what it costs at high latitudes

Equal Earth is not conformal: at the centre its east–west and
north–south scales differ by k², where k is the east–west scale per
unit of arc. The match takes their geometric mean — exactly one, Equal
Earth being equal-area — so `t = 0` is true Equal Earth and area
ratios hold; as `t` grows, each axis' scale slides towards Mercator's
(`k^∓t`), so near the handover the world term agrees with Mercator
locally. The anisotropy k² per latitude:

| centre latitude | 0° | 31° | 52.5° | 65° |
|---|---|---|---|---|
| k² (EW/NS) | 0.74 | 0.95 | 1.31 | 2.12 |

Measured on the z1→z7 ramp at 0/0, Berlin, Merzouga, Singapore and
65 N (every tenth of a zoom, fixed ground points in the middle third
of a 480×320 view; `leviculum-map/tests/blend.rs`, asserted):

- Beyond the zoom and a per-axis scale about the centre, the middle
  third moves at most **0.47 px (Equal Earth)** and **0.59 px
  (globe)** per tenth of a zoom — the irreducible shape morph, under
  the one-pixel bound.
- Raw (beyond the uniform zoom alone) the globe stays at **0.59 px**;
  Equal Earth pays its anisotropy with up to **2.82 px** per tenth of
  a zoom at the 65 N centre (1.1 px at Berlin-like latitudes), spread
  evenly across the thirty steps of the band. The one-pixel bound
  does not hold raw for Equal Earth at high latitudes and cannot:
  the shapes themselves differ by k² between the ends of the ramp.
- Where it further does not hold: with the scale matched at a 65 N
  centre, the drawn world is sec 65° ≈ 2.4 times wider than
  Mercator's square, so the whole world does not fit one screen
  there; it shrinks back as the centre pans south. The globe shows
  half the world by its nature.

Area check at `t = 0`: Greenland against Africa in drawn pixels is
the true spherical ratio to well within 2 % (the same table's rings,
Lambert shoelace against screen shoelace, asserted). Angle check at
`t = 1`: the blend equals today's Mercator mapping to float precision,
and every golden from z5 up is untouched — the linear drawers' code
path did not change.

#### Everything that reads the projection

The blend holds in every reader of the view's mapping when (shape set,
`t < 1`, no region in view) — one condition, asked of the painter:

- the world picture itself (`Painter::draw_with`,
  `outline::draw_world_view`); in the window and on the phone the tile
  cache draws such a frame whole each time (`TileCache::compose_into`)
  instead of composing step tiles, and `mapframe` stops pinning a
  zooming frame to a step, since a step change would jump `t`;
- pins (`screenmap::place_exact`), tracks (`screenmap::draw_tracks`),
  the time zone view's borders and clocks (1684;
  `outline::bounds_view`, `timezones::labels_view`), the world's own
  names (`outline::names_view`) — on the globe the far side places
  none of them;
- hit testing: screen to world is Newton on the blend
  (`Blend::to_world`), started at the Mercator inverse, answering
  only within 1/32 px; a point off the world (past Equal Earth's
  edge, off the globe) has no answer. Wired in the core; the
  frontends' click paths still use the Mercator inverse and meet the
  blend only on world views, where a click's error is the blend's
  bend at that point — the remaining wiring is listed in the 1710
  report;
- the fit-all rule and fly-to run in world coordinates and need no
  change: the centre they aim at lands on the centre pixel under any
  `t`;
- the scale bar (1701, not yet landed): under `t < 1` there is no
  single scale; the decision recorded here is **show none at world
  zoom**, from `t = 1` down the bar as specified;
- names while the view moves ride along linearly between placements
  (sprites), so at `0 < t < 1` they can drift a few pixels from their
  blended ground until the frame comes to rest, which replaces them
  exactly.

The M9 keeps Mercator: its world view is not the outline but its own
z0–z3 overview region — tiles, which the blend never reprojects by
design — and its FPU has no doubles for the per-vertex trigonometry.
A later fixed-point or table build can revisit this; nothing in its
budget moved.

#### Pictures and cost

Pinned: `leviculum-map/tests/golden/world-blend-{ee,globe}-480x320-{z2,z35}.png`.
The zoom strips (five centres, both shapes, z1→z7) and the two frame
series at Berlin and Merzouga are the 1710 report's, rendered by
`cargo run --release -p leviculum-map --example world_blend_pictures`.
A 1024×768 world view on the reference host draws in 11.3 ms linear,
21.1 ms Equal Earth, 24.1 ms globe (release, median of nine): a
blended frame redrawn whole every zoom frame holds 40 fps on the
desktop; the phone runs the same code through the facade.

## 3. Elevation

**Two tiers (decided).**

- **Base tier, everywhere:** shading in the quality CoMaps ships today,
  about 30 m, to z12. Part of every region.
- **Fine tier, where one hikes:** the best source obtainable for that
  area, on the device only for the area looked at or saved.

**The server holds the most detailed data obtainable (decided),** and
the app may thin it later for size.

**Sources (study of 2026-11-11, proposed):**

- Base: Copernicus GLO-30. Its licence allows distribution and
  adaptation; attribution and disclaimer bind subsequent users. It is a
  surface model: forest and buildings are in it. GLO-90 where a tile is
  withheld.
- Fine: built from Mapterhorn's catalogue of 151 open national sources
  (`download.mapterhorn.com/attribution.json`), all of them bare earth
  models from laser scans, none under a non-commercial or share-alike
  licence. Germany's sixteen states at 1 m, Switzerland 0.5 m, Austria
  1 m, and so on; the full table is in the study report (not verified
  in the tree: the report was handed over beside the draft and is not
  committed). Japan needs a question to the survey office first.
- Nothing better than GLO-30 exists for the Himalaya, Central Asia, the
  Caucasus, the Andes, Patagonia, the Sahara and East Africa. There the
  base tier is the fine tier.
- **As fine as the disk allows (decided, Lew 2026-11-11).** The
  fineness is a parameter of the cut, set by the disk Lew buys, not a
  fixed cap. The pyramid makes that painless: a deeper level is added
  beside the existing pieces, which never change, so the store starts
  coarse and is refined area by area, the Alps first. A 5 m cell is
  used up to about z14 at 45 degrees north, 2 m to about z16, 1 m
  beyond. Estimated for Europe: about 160 GB at 5 m, 1 TB at 2 m, 4 TB
  at 1 m (Switzerland 1.1, 7 and 28 GB); beside the store the cut needs
  room for one country's raw data at a time. The raw sources together
  are about 19.8 TB and are not kept.
- **Non-commercial sources are not in the default data (decided).** Our
  own use is non-commercial, but the clause travels with the tiles to
  everyone who uses free software commercially, and FABDEM adds
  share-alike. FABDEM stays noted as a possible separate, marked layer;
  GEDTM30 (CC BY 4.0, still labelled a test release) is the free
  counterpart to watch.
- **Attribution** is generated per pack from the sources inside it.

**Shading itself:** phase 5 of `docs/drafts/map-renderer-roadmap-2026-10.md`,
section 6b: Lew's CoMaps generator ported to Rust, golden tests against
its tiles. The single sibling file of that draft becomes a pyramid
served in pieces (section 5).

**Contour lines (decided):** low priority, later, behind a switch, off
by default.

## 4. Points of interest, search, articles

- **All points of interest go into the data (decided).** The short list
  of the renderer roadmap (open question 6 there) is rejected. Only the
  display thins by zoom and rank.
- Proposed: a section of their own, as the buildings are, so a board can
  skip it. First a measurement of what all of them cost a region; the
  roadmap counts 168,433 in Frankfurt's z15.
- **Offline Wikipedia text for a place (decided in direction):** the
  whole article without images, as CoMaps ships it, if clever
  compression lets us keep all of it; the measurement in CoMaps' own
  map files decides. **Language (decided):** the user's; English where
  the article is missing in it; failing both, whichever language the
  article exists in. The server holds all languages, the device fetches
  the one. The text keeps its CC BY-SA licence and names its source.

  Measured 2026-11-11 in 19 CoMaps maps of version 260830 (decoded
  block by block, 91,185 articles): CoMaps ships whole articles as
  HTML, all sections, with no image, table or link and without the
  reference sections, no length limit, each article once per map in up
  to 17 languages. An article is 2,979 bytes raw and 1,036 compressed
  on average (BWT, move-to-front, Huffman, ratio 2.87). The section is
  6 to 12 % of a dense map (Hamburg 6.2 of 50.6 MB, Lake Geneva 14.7 of
  121 MB) and about 1 % of a sparse one. On German maps German is 60 to
  70 % of the text, English 6 to 10 %. Extrapolated: all of Germany 220
  to 280 MB in 17 languages, about 165 MB German only; Europe 2.5 to
  6 GB in all languages. (The decoder and its statistics are not in the
  tree.)

  Compression, tried on one map's text: zstd per article with a 1 MB
  trained dictionary is about 12 % smaller than CoMaps and keeps single
  articles fetchable, which loading by looking needs; a large window
  over the whole text saves 32 to 40 % and loses that, so it suits a
  saved area. Plain text instead of HTML saves 2 %.

  So the whole article can ship (proposed): fetched per article when a
  place is opened, and kept whole with a saved area. Attribution better
  than CoMaps does it: a link to the article and the correct licence
  name beside the text.
- **Wikivoyage (decided: note it, study it, do not forget it):** travel
  guide articles under the same licence; how they fit in best is a
  study of its own, at a convenient time.
- **Place search** is wanted (Lew's opening list); scope is open.

### Other data for travel (decided: all of it, the light things first)

Lew, 2026-11-11: note the ranking, build the low-hanging fruit first
(small data, easy to get), and do all of it over time. The list is the
reviewer's from memory; a study verifies licence, size and coverage of
every source before one is used.

1. **Keep what OpenStreetMap already carries.** The cut drops nearly
   everything but geometry. Supply (drinking water, springs, wells,
   fuel with its kind, huts, shelters, camp sites, opening hours), the
   state of a way (surface, track grade, hiking difficulty, trail
   visibility, fords, access), marked hiking and cycling routes as
   whole named lines, border crossings, protected and military areas.
   No new source.
2. **Tiny reference data.** Magnetic declination (the world magnetic
   model), the geoid for GNSS height, per country the emergency
   numbers, driving side and currency. **Time zones:** the clock rules
   come from the system's own database on Linux and Android (Lew); that
   database holds no boundaries, so the outlines come as data, and a
   board needs a small table of its own.
   - **A time zone view (decided):** one control, and the world shows
     its zones with the current time written into each.
3. **Radio rules by country.** Which LoRa frequencies, power and duty
   cycle are legal where, so a board crossing a border gets legal
   settings by itself. Fits the configurable radio parameters and
   [regulatory airtime](regulatory-airtime.md).
4. **Weather and warnings over the mesh.** The mailbox at home has the
   Internet and the group in the field has not. A forecast for a point
   is a few hundred bytes and fits LoRa. The same for avalanche
   bulletins, fires, earthquakes nearby.
5. **Land cover where OpenStreetMap is empty** (desert, Central Asia,
   high mountains): forest, rock, sand, snow, wetland as areas from
   satellite data; seasonal water and river networks.
6. **Satellite imagery as an extra pack** for saved areas; in the
   desert the image often is the map. Large.
7. Travel content beyond Wikipedia: Wikivoyage with its phrasebooks,
   place names in many languages and scripts for the search, mobile
   coverage, tides and sea marks.

**Rain radar (decided, if an open source exists):** live rain moving
over the map. Online by nature and data-hungry, so outside the offline
theme, but wanted.

**The study of 2026-11-11 (sources read that day unless marked):**

- **Rain radar is possible.** Germany's weather service (CC BY 4.0, a
  composite every 5 minutes, a 2 hour nowcast), the European radar
  composite OPERA (CC BY 4.0, open since November 2025, 1 km, 5
  minutes), Switzerland, France, the Netherlands, the United States
  (public domain), and satellite rain worldwide as the fallback (0.1
  degrees, 30 minutes, 4 hours late). All may be re-tiled on our
  server. RainViewer may not: personal and educational use only. A
  frame is an estimated 20 to 150 KB for Germany and 100 to 600 KB for
  Europe; over LoRa only a summary of about 100 bytes.
- **Weather by message:** ECMWF open data and DWD ICON, both CC BY 4.0.
- **Time zone outlines:** timezone-boundary-builder (ODbL), the variant
  with oceans, simplified to an estimated 1 to 3 MB for the world view.
  Natural Earth's time zone layer has fixed offsets from 2012 and no
  zone names the system database knows: not usable.
- **Declination:** the world magnetic model is public domain and about
  4.5 KB. **Geoid:** a 15 minute grid of about 2 MB (from memory).
- **Radio rules:** no open source states the law. The Things Network's
  frequency plans (Apache-2.0) are the best machine-readable seed;
  proposed is our own small table, each row citing its regulation.
- **Land cover:** ESA WorldCover, CC BY 4.0, 10 m, worldwide.
- **Imagery:** our own cloud-free mosaic from raw Sentinel-2; an
  estimated 20 to 80 MB for 100 by 100 km. The ready-made cloudless
  mosaics after 2016 forbid redistribution.
- **Avoid:** iOverlander, the world database of protected areas,
  openAIP, RainViewer, the Australian and Japanese radar, commercial
  imagery. Protected and military areas come from OpenStreetMap.
- **Not verified:** the licence of the avalanche bulletins, of the
  geoid page, of the river network, and several public-domain claims
  made from memory. The condensed report with every figure's origin was
  handed over with the draft (not verified in the tree: it is not
  committed).

## 5. Loading

**Loading by looking (decided).** The user never thinks about data. What
is looked at is loaded at the depth it is looked at, the fine elevation
included, only for the visible area; it stays on the device and works
offline. What arrives is drawn at once, so the user sees the map build
up while it loads.

- **Cache limit (decided):** configurable, 500 MB by default. What was
  only looked at may be evicted, least recently viewed first.
- **Saving an area (decided):** one control on the map, "save this view
  for offline use". The area is what the screen shows. What the save
  takes and when it asks is section 11. **What the user saves is never
  evicted.** Country lists and route corridors are not part of the
  first version.
- **Consequence (proposed):** every kind of data (vectors, shading,
  points of interest) is a pyramid the device fetches pieces from, cut
  on the server, not on the device. The region format already orders
  the index first and tiles coarse to fine (version 4, [Loquax: the
  map](loquax-map.md), section 4).

## 6. Sharing map data over Reticulum

These rules replace the older ones of [map data over
Reticulum](loquax-map-over-reticulum.md) (decision of 2026-11-07), which
was rewritten to them.

- **One protocol, two roles (decided).** The map server and every
  Loquax speak the same request protocol; they differ in what they hold.
  **Every Loquax is a map server, always, on by default.**
- **Only signed data is shared (decided).** The server cuts the pieces
  and signs their index of hashes; a device verifies whatever arrives
  against the pinned server key, whoever carried it. That is what makes
  automatic use safe, and it retires the earlier rule that a foreign
  map source is offered and never used by itself. Pieces a device cut
  itself from Protomaps are not shared.
- **Finding neighbours (decided):** a broadcast to a plain destination,
  which Reticulum never carries past one hop
  (`reference/Reticulum/docs/source/understanding.rst`, "Plain
  destinations are only reachable directly"). The question names the
  area coarsely, since it is unencrypted. Whoever holds data answers,
  with a random delay; then an ordinary encrypted link carries the same
  protocol as to the server.
- **A measured fast neighbour beats the Internet server (decided).** As
  a race, so the user never waits: ask the server at once, ask the
  neighbours beside it, switch to a neighbour once it is measured fast,
  remember it while it stays reachable. Without Internet: neighbours,
  then the server over Reticulum.
- **One hop is not fast.** A LoRa neighbour is one hop too. Speed is
  always measured, in steps: the link's own round trip first, a tiny
  request next, more only after that was fast. Today's 4 KiB probe is
  too dear for LoRa.
- **Coarse before fine, inside a time budget (decided in outline).** The
  device estimates what arrives in a fixed time and stops there.
- **LoRa: nothing automatic (decided).** About 400 bytes a second
  before duty cycle; 100 KB is over four minutes of airtime on a shared
  channel. A power user may allow it in the settings, off by default,
  with a fixed ceiling per fetch.
- **Messages come first (proposed).** One transfer at a time, paused
  while own messages wait; a giver on low battery serves nothing.
- **Open, for a study against the reference:** the answer format that
  lets a link come up without a prior announce; whether the question
  can stay off LoRa without the app knowing the medium; the rate limit
  of the question.

## 7. Map and chat

**Sharing (decided):** tap a place, share, done. Points, meeting points
and routes travel as ordinary chat messages with a small map preview.

- Small enough for LoRa: a named point is a few dozen bytes; a route is
  simplified to what the link carries.
- Readable in other clients as text with coordinates; GPX in and out.
- **Laying routes in Loquax (decided):** low priority, later, after the
  MapOut study.

**For groups on the move (decided: all of it into the concept, built in
steps, the reviewer sets the order).**

Step 1, cheap and useful at once:

- share a point or a route;
- direction and distance to every member, as an arrow that needs no map
  data (works on the M9 and in empty desert);
- "where is everyone": fit the group, with the age of each fix;
- the way back: the own track is always recorded, "back to camp" shows
  bearing, distance and the line walked;
- measuring a distance, the height of a point, coordinates in degrees,
  UTM and MGRS, sunrise and sunset at a place.

Step 2:

- shared markers (water, camp, fuel, soft sand, closed track) that every
  member sees and that reconcile when devices meet again;
- the journey as a package: before departure one member hands saved
  areas, routes and points to the group over the local network;
- a shared photo appears on the map where it was taken;
- the elevation profile of a route or a track.

Step 3, each with a careful design of its own:

- the rally call: one tap, everyone gets "come here";
- the convoy warning: someone falls further back than a set distance,
  or a fix grows too old;
- the emergency button, repeated until someone confirms, and the sign
  of life; a false alarm and a missing alarm both hurt here.

Only we can do these, having radio and elevation on the device:

- the radio trail: the map remembers where the mesh was heard;
- line of sight: from which hill the camp or a relay should be in LoRa
  reach.

Several of these are on the experiment list of 2026-11-04 already (edge
arrows, radio trail, line of sight, twilight, group hull) (not verified
in the tree: the list is Lew's decision of that day and has no page).

## 8. Groups

Columba's "Group Tracker" is Sideband's telemetry collector under
another name (read in Columba's checkout of 2026-08-14; not verified in
the tree, which holds no Columba checkout): no group object, no
invitation, every member picks the host by hand, all positions pass
through one phone that reads them in the clear. Not a model.

**Decided: one group concept, the existing one** (study 1299,
`docs/drafts/group-chats-study-2026-11.md`, Lew's decisions of
2026-11-02): invitation, join in two taps, the member list arrives by
itself. Its host is the founder's home. That is right at home and wrong
in the field, where nobody reaches the home.

**The group lives in the founder's mailbox, and that is accepted
(decided, Lew 2026-11-11).** If the device that runs the mailbox is off,
the group is out of reach. A group is best founded by someone whose
mailbox always runs. Whoever founds one gets a short notice, at least
the first time: you can found this group, but while the device your
mailbox runs on is off, the group cannot be reached. Proposed: the
notice appears when the mailbox runs on the founding device itself (a
phone, a laptop), since that is the case it warns of; wording as for
every user-facing text, no term a WhatsApp user would not know.

The additions below soften that rule where they can; they do not
replace it.

Additions (decided):

1. **Positions in a group, with one switch and two paths.** One switch
   in the group's chat: "share my location with this group". The path
   is chosen by the app, never by the user.
   - *Home in reach* (the park, the city, any good Internet): the
     position goes to the group as a group message does, once, and the
     home fans it out. This is the path a phone in mailbox mode needs,
     since by the decision of 2026-11-10 it does not listen to the mesh
     itself, and it is the path Columba and Sideband members share
     through 1307g. The copy keeps the writer: the group's copy carries
     every field of the original but the custom ones verbatim, the
     telemetry field included (`copy` (`loquax/src/home/group.rs:1397`),
     the field loop at lines 1407 to 1426), and its LXMF source is the
     group, so `read` (`loquax/src/telemetry.rs:169`) books a plain
     telemetry field under the writer `lev.group_copy` names, not under
     the group the copy came from (1687).
   - *Home out of reach* (the field): the position announce sealed
     under the group key. Announces travel several hops with no host,
     it is one transmission for all, and only members can read it. The
     position announce exists; the group key is planned in
     [N homes under one identity](loquax-homes.md), decision (a).
   - Sealed announces are not sent through the public Internet hub by
     default (proposed): there every client of the hub would carry
     every group's announces.
2. **Field operation, through the founder's second home (proposed,
   replaces the reviewer's first idea).** The first idea here was that
   a member sends directly to the others on its list. The group study
   had already rejected member-side fan-out, since members never hold
   the group's key (`docs/drafts/group-chats-study-2026-11.md`,
   section 2.1). The designed answer is N homes under one identity
   (Lew, 2026-11-01/02, [N homes under one identity](loquax-homes.md);
   nothing of it is built): the founder's phone is a home beside the
   always-on one, whichever home accepted a message fans it out, and
   the homes reconcile when they see each other. In the field the
   founder's phone hosts the group for the mesh. The limit is plain:
   the founder must travel with the group. Meeting points, markers and
   the rally call go the same way. Still to study: what holds when
   markers contradict, what ten people cost in LoRa airtime, a
   latest-value object beside the message history.
3. **Founding on the road.** The invitation also as a QR code from
   device to device. Whether a group can be founded with no home in
   reach is part of the same study.

**Compatibility with Columba and Sideband** comes through order 1307g,
written and not built (not verified in the tree: no page or draft
names 1307g): a Loquax group is also a telemetry collector, so their
users enter the group's address as their host. It works only while the
home is reachable; their scheme can do no more.

### Solo users, with no always-on mailbox

Lew, 2026-11-11: all of this must work as well as it can for a user who
runs Loquax alone. Read in the tree on 2026-11-11:

- **Android:** built. "Mailbox on this phone" runs the home inside the
  app (`loquax-android/src/mailbox.rs`), so a solo phone can found and
  host a group, and gets the notice above.
- **Desktop:** no such mode. A Loquax without a mailbox is a plain
  messenger; it can join a group and cannot found one
  (`loquax/src/shell.rs`, "groups live at a mailbox, and this Loquax
  has none"). A home on the same computer is an explicit setup today.
  Proposed: the desktop gets the same unnoticed local home the phone
  has. Not part of the map work.
- **M9:** a client of a home only.

What works solo, independent of any mailbox: the world view, elevation,
loading by looking, saving an area, sharing map data between
neighbours, sharing a point or a route with one peer, the arrow to a
peer, the way back, the measuring tools.

What depends on the group's host being up: everything sent to a group.
With the host on a phone that is off or in flight mode the group stands
still for everyone, however good the members' Internet is.

Two things found on the way, **open**, not verified on a device; order
1687 verifies both:

- **F1. Group mail may not be collected.** The host seems to collect
  propagated mail only for the home's own identity, not for the group
  addresses. The collection sites in `loquax/src/home/daemon.rs` as of
  2026-11-11: the `Collector` is built at line 449, a round starts it
  at line 958, and `Collector::collect` (line 2462) runs one fetch for
  the home's instance. If so, lines written to a group while its host
  was off may never arrive.
- **F2. A phone that hosts its own mailbox hears no mesh positions.** It
  has no AutoInterface and no BLE yet, and seems not to pass mesh
  positions on to its screens. The phone as a full node ([the phone as
  a full node](loquax-phone-node.md)) is a concept with nothing built.
  **The field scenario on phones depends on it,** so its priority rises
  with this concept.

## 9. The map server

It becomes a precondition, not a follow-up: loading by looking, signed
pieces, shading and points of interest all need pieces cut and served
by our own server. Today there is none in operation: the default source
is the Protomaps daily build (`PROTOMAPS_BUILDS`
(`leviculum-mapcut/src/http.rs:17`)), which that project discourages as
hotlinking ([Loquax: the map](loquax-map.md), "The autoload"); `lnmapd`
is designed ([map data over Reticulum](loquax-map-over-reticulum.md))
and not built, and `leviculum.network/maps/` answered 404 on
2026-11-11.

**The server is a setting (decided).** By default it is ours; a user
may name another. The setting is the server's Reticulum address, and
that one value is also the trust anchor: the address is a hash of the
server's public key, so a key that arrives with an announce or with a
signed index either hashes to the configured address or is rejected.
No key has to be entered beside it. Both exist as config keys already
(`map_destination` in [map data over
Reticulum](loquax-map-over-reticulum.md), `map_source` in [Loquax: the
map](loquax-map.md)). Of the two, `map_source` is read by Loquax's
config today (`loquax/src/config.rs`); `map_destination` is named by the
design only (not verified in the tree: no code reads it yet). Proposed: the Internet address is optional; the
server's signed index names its own HTTP address, so one value
configures both paths, and `map_source` stays as an override.

**Where it runs (decided, Lew 2026-11-11): a store at home, a front
door on the project's server.** The project earns nothing, a rented
server with more than about 100 GB is too dear, and 100 GB would not
hold the fine elevation of Europe alone (an estimated 160 GB). Lew's
home line has a fixed address and room for a large disk.

- **At home: the complete store.** A large disk with every piece, the
  fine elevation included. Cutting happens there or on the desktop
  machine, where the raw sources fit.
- **`leviculum.network`: the public address with a cache.** Devices
  always ask it. What it holds it serves at once; what it lacks it
  fetches from home once and keeps while there is room. Read on
  2026-11-11: 2 cores, 1.9 GiB of memory, 27 GB free of 40 GB. It can
  serve and cache, and it cannot cut.
- This works because the pieces are signed and never change once cut,
  so any cache or mirror can serve them without being trusted.
- What it gives: each piece crosses the home line's upload at most
  once; the home address stays hidden; an outage at home leaves
  everything already asked for in service; no larger rented server is
  needed; others can mirror later.
- What it costs: the first fetch of an area not yet cached runs at the
  home line's upload speed, so areas in demand are placed on the front
  door ahead of time; one disk is one point of failure, and the cut
  data, being reproducible and large, stays out of the backup.
- Serving and cutting are separate from the start. This replaces the
  earlier thought of a VM of its own for the map server.

The home line's upload: Lew measured 17 Mbit/s on 2026-11-11 (30 at an
earlier time); 17 is the figure to plan with, about 2 MB a second. A
city region of 10 MB crosses in about 5 s, a saved area of 500 MB in
about 4 minutes, Switzerland's fine elevation (an estimated 1.1 GB) in
about 9. Enough for a first fetch by one user; a rate limit keeps the
household's share, and areas in demand are still placed ahead.

**The store gets a small server of its own at home (decided, Lew
2026-11-11).** Lew chooses and provides the machine; it may host other
things of his beside the map store, which is his to settle. Until it
stands, the project's server alone carries the first steps. Proposed:
the store opens no port at home; it keeps a tunnel to the front door,
which fetches missing pieces through it.

## 10. Open

1. Place search: what can be searched and how results look.
2. How a device decides "home in reach" for the group paths, and what
   happens at the change between the two paths (a member half in the
   field, half on the Internet).
3. What a phone costs in battery as a full node, measured on a real
   phone.
4. The sizes of everything but elevation in the store, known after the
   first cuts.

## 11. What is left for the user to decide

Each line is a choice this concept still puts to the user, and why.

| choice | why it stays | rule that replaces the rest |
|---|---|---|
| share my location with this group | privacy: no position leaves by default | the path (home or field) is chosen by the app |
| save this view for offline use | only the user knows the journey | what is saved, see below |
| found a group, invite, join | the user's intent | the member list, the key and the host follow by themselves |
| set up an external mailbox | cannot be automated | everything else about homes |
| contour lines on | taste, off by default | |

**Settings are where the rest lives (decided).** By default everything
runs for nine users in ten with no decision. A power user may change a
rule in the settings menu, and only there: no setting appears as a
question in the flow, and no default depends on one being touched.

Decided, removing choices the draft still had:

- **Saving an area asks nothing.** It saves the view to full depth with
  the fine elevation. Only when that exceeds the free space or a fixed
  size does the app say so and offer the smaller save.
- **No mode for the user to pick between solo and mailbox.** Every
  Loquax is always a home of its own. An external mailbox, once set up,
  is a second home of the same identity (the N homes of [N homes under
  one identity](loquax-homes.md)). While the mailbox is in reach the
  device rests as its client; when it is not, the device is the node.
  This retires the first-start question on Android, and turns the two
  modes of the mailbox-first decision (2026-11-10) into two behaviours
  the app switches between ([the propagation node](loquax-mailbox.md),
  section 5; [what a Loquax Mailbox is](loquax-mailbox-roles.md), M6).
  Open: what it costs a phone's battery, which nobody has measured on a
  real phone; a bad number means the rule needs a brake.
- Cache limit, sharing map data, the source of a piece, the transport,
  the depth loaded on a slow link: all rules, with settings only for
  those who look for them.

## See also

- [Loquax: the map](loquax-map.md), [map data over
  Reticulum](loquax-map-over-reticulum.md), [tracks on the
  map](loquax-tracks.md), [N homes under one identity](loquax-homes.md),
  [telemetry](telemetry.md).
- `docs/drafts/map-renderer-roadmap-2026-10.md`, section 6b.
- `docs/drafts/group-chats-study-2026-11.md`.

## Order of work

**Its place (decided, Lew 2026-11-11):** the guiding goal gets a second
half. The first stays "a board is a full mesh node". The second is
"Loquax as a travel companion: map and group, in daily life as in the
field". The phone as a full node belongs to both, since the board hangs
on the phone by Bluetooth. Reds and protocol keep their precedence on
the main lane; map work counts as work on the guiding goal.

The order is the reviewer's, with the light data pulled forward as Lew
asked. Each step carries its K-number; later orders cite them.

**At once, no server needed**

- **K0** This page: the draft landed, the renderer roadmap's question 6
  marked decided, [map data over Reticulum](loquax-map-over-reticulum.md)
  rewritten to section 6, the mailbox pages to section 11 (order 1682).
- **K1** World view with more Natural Earth (section 2): lakes, rivers
  (scalerank <= 4), glaciers, ice shelves, disputed borders dashed,
  places from the full populated places file, country, sea and
  landscape names; the new label kinds; all four targets, or a reason
  per target; on the M9 the borders and names over the overview.
- **K2** The light data: declination (WMM2025), geoid, time zone
  outlines with the time zone view (one control, the time per zone,
  rules from the system's tzdata, the M9 a table of its own), country
  facts.
- **K3** What OpenStreetMap already carries kept in the cut, and the
  measurement of what all points of interest cost a region (a section
  of their own, a proposal for the thinning rule).

**The map server, the bottleneck for nearly everything after**

- **K4** Study and design of the server format: a pyramid per kind of
  data, a signed index, pieces that never change, the index naming its
  own HTTP address, the Reticulum address as the single trust anchor;
  `lnmapd` and HTTP from one directory; the store at home, the
  project's server as cache through an outgoing tunnel; cutting
  separate from serving. Beside it, on its own lane: the phone as a
  full node, the desktop's unnoticed local mailbox, the N homes.
- **K5** The server in operation on the project's server with the first
  test areas.

**After that, in this order**

- **K6** Shading, base tier (roadmap 6b, the port of Lew's generator);
  loading by looking with visible build-up.
- **K7** Saving an area (asks nothing, never evicted); the cache limit
  as a setting.
- **K8** Sharing points and routes, GPX, group step 1 (arrow, where is
  everyone, the way back, measuring, coordinate formats, sun times).
- **K9** The group key and group positions (two paths, one switch);
  1307g (a group is also a Sideband collector) for Columba
  compatibility.
- **K10** Map data between neighbours (a study against the reference
  first: the answer format without a prior announce, the question kept
  off LoRa).
- **K11** Fine elevation from Mapterhorn's sources, the Alps first, as
  fine as Lew's disk allows.
- **K12** Points of interest on the map, place search, Wikipedia (whole
  articles, zstd per article with a dictionary), rain radar (DWD,
  OPERA, USA, IMERG), weather and warnings by message, radio rules by
  country.
- **K13** Studies: field operation of groups over N homes, MapOut,
  Wikivoyage; then the route editor.
- **K14** Group steps 2 and 3, contour lines (a switch, off), world
  relief, land cover, imagery packs.
- **K15** Offline routing, last.
