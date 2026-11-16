# One map renderer from the M9 to a GPU desktop (#474)

Draft, 2026-10-29 (1094). Lew's brief of 2026-10-28: the map needs text,
labelled as far as possible without painting over the picture; a more
beautiful and complete map; the same drawing code from embedded devices to
GPU desktops, each drawing as well as it can without stutter; on the PC
buildings, street names set into the map, anti-aliased vectors; a GPU only
if the CPU cannot do it; and the low hanging fruit. This draft answers the
questions of the order with measurements, recommends an architecture and a
phased plan, and lists what can be made prettier now.

Everything was measured on the coder host (schneckenschreck, AMD Ryzen 9
7950X, one thread unless it says otherwise). M9 figures are estimates
scaled from the device rows of `docs/drafts/map-measurement-2026-10.md`
(the full profile drew in 176.67 ms on the device against 1.478 ms on the
host, the stripped one 90.60 against 0.647 ms: a factor of 120 to 140),
never device measurements. Nothing here changes product code.

How it was measured: a throwaway harness outside the workspace,
`/home/lew/scratch/1094/render/` on the coder host (a crate of its own
over `leviculum-map`, `leviculum-mapfill`, `leviculum-mapcut` and
`loquax::picture` by path, plus tiny-skia 0.11 and ab_glyph 0.2, both
already in the lock). Its subcommands `bench`, `bench4k`, `threads`,
`style`, `shade`, `pyramid` and `term` wrote the tables and images under
`/home/lew/scratch/1094/out/`. Three helper studies wrote notes beside it:
`notes-comaps-design.md` (CoMaps renderer design with file:line),
`notes-mwm-hillshade.md` (MWM format, Lew's server, the hillshade
algorithm) and `notes-label-data.md` (what Protomaps carries, measured
over every tile of the Frankfurt and Berlin extracts). CoMaps citations
below are paths in the snapshot at `/home/lew/scratch/comaps/` (Lew's fork,
`bd1012f5f`, Apache-2.0).

## Recommendation

1. **Keep one renderer, ours, on the CPU everywhere.** The integer scanline
   filler of `leviculum-mapfill` grows coverage anti-aliasing, casings, an
   alpha span and a glyph blitter; device profiles are parameters of it,
   not forks. Today it draws a 1080p view in 9 to 37 ms and a 4K view at
   pixel ratio 2 in 38 to 112 ms on one thread, and four threads bring the
   4K one to 19 to 52 ms (section 5).
2. **No GPU renderer.** Pan holds 60 fps on the CPU at 4K with a cached
   raster: shifting the frame costs 0.6 to 1.1 ms and drawing the exposed
   strip 0.4 to 2.3 ms. The one measured shortfall is a continuous zoom
   animation at 4K, where scaling the cached raster on the CPU takes 37 ms
   (nearest) to 74 ms (bilinear). The cheapest GPU step, if Lew wants
   animated zoom on 4K, is iced's wgpu backend compositing our raster as a
   texture, not a GPU map renderer (section 6).
3. **Labels need data first.** The cut drops every name
   (`leviculum-mapcut/src/mvt.rs:41`, `class_of` keeps no attribute but
   `kind`). A format step LMR2 adds a per-tile label section (place,
   water and park points, a name reference per named line) and one
   region-wide string table; places and water alone cost 0.6 to 2.0 % of
   a region, street names a few per cent more (section 1).
4. **Labels are placed by one greedy rule** for every frontend: pins
   first and never covered (the 1084 rule), then places by kind and rank,
   water and park names, street names on their straightest run, repeated
   every 300 px at the earliest (CoMaps' spacing). Measured placement in
   the terminal: 5 to 22 microseconds for a view.
5. **The M9 first needs its stroke made cheap.** Stroke is 55 ms of the
   90 ms stripped frame for 21,422 pixels, about 627 cycles a pixel; the
   suspect, unmeasured, is 64-bit division in the edge setup and the
   stroker, which the Xtensa LX7 does in software. Casings double the
   stroke and AA costs more again, so they wait for that fix and its
   device row (section 2).
6. **Buildings only on the desktop**, as a section a board skips: they
   double a region (+119 % Frankfurt, +99 % Berlin) and doubled the M9
   frame (177 against 90 ms).
7. **Hillshade: port Lew's CoMaps generator to Rust as an optional
   sibling layer**, cut from SRTM or Copernicus GLO-30. Measured from one
   real SRTM tile, the Frankfurt box's shade pyramid z9 to z12 is 274 to
   417 KB (2.8 to 4.2 % of its region); the blend costs 0.13 ms at
   320x240 and 3.6 ms at 1080p on the host, an estimated 20 ms on the M9
   unoptimised (section 6b).
8. **Do not read MWM files.** Vectors stay Protomaps PMTiles cut into
   LMR; from Lew's CoMaps world only the hillshade idea and parameters
   come over (section 6d, option c). Lew's server can later serve LMR
   regions and shade layers beside the MWMs, which makes the autoload
   independent of a third party.

## 1. Labels end to end

### What the source carries

Basemap 4.x, measured over all 1,363 tiles of the Frankfurt extract and
5,584 of Berlin's (`notes-label-data.md`):

- `name` is on every `places` feature, on `pois`, on `roads` from z11, on
  water points and on `earth`. Landuse and water polygons and buildings
  never carry one: area names arrive as points (water points of kind
  `water`, 466 in Frankfurt and 3,613 in Berlin; parks as `pois` of kind
  `park`).
- `places` keys: `kind`, `kind_detail`, `min_zoom`, `population`,
  `population_rank`, `sort_key`, `wikidata`, `name:de`, `name:en` and the
  other languages. Examples: Frankfurt am Main is `locality`/`city`, rank
  12, `min_zoom` 4; Offenbach rank 9, `min_zoom` 8; villages rank 4,
  `min_zoom` 11; suburbs rank 1, `min_zoom` 13.
- `roads` keys: `kind`, `kind_detail`, `min_zoom`, `sort_rank`, `ref`,
  `shield_text`, `network`, `oneway`, `is_bridge`, `is_tunnel`, `is_link`.
- Named features, Frankfurt, z11-12 / z13 / z14 / z15: places 3,214 /
  2,665 / 2,641 / 2,429; pois 1,221 / 1,640 / 6,239 / 34,540 (of 168,433
  at z15); roads 975 / 1,743 / 18,632 / 22,357.
- The trap: 1,774 of Frankfurt's 1,933 places at z13 are `kind_detail`
  `locality`, OSM field and area names. Unfiltered a 256 px view at z13
  holds 26.8 places on average (p90 94); without `locality`, `hamlet`
  and `isolated_dwelling` it holds 2.2 (p90 5). The z12 tiles already
  carry the `min_zoom` 13 places, so the filter is `min_zoom <= view
  zoom` as well.

### Language

`name:de` differs from `name` almost only at z0 to z10 (countries:
España, Spanien). From z11 on in Frankfurt it differs for no place and
no road; for pois in 1 to 25 cases a band. CoMaps chooses at run time
from up to 64 names a feature (`libs/i18n/string_utf8_multilang.hpp:34`)
by map language, system languages, then `int_name`, `en`, the local name
(`libs/i18n/localisation.cpp:163`), and shows the local name as a second
line where it differs (`libs/i18n/localisation_translation.cpp:69`).

Proposal: resolve at the cut. The region stores `name`, and a second
string only where `name:<locale>` differs, the locale the cut was asked
for (loquax passes its own; the autoload knows it). Fallback chain
`name:<locale>`, then `name:en` where `name` is in a script the locale
does not read, then `name`. A board's region is cut for its user, so the
board never chooses.

### The format step: LMR2

Measured cost of a label table per tile (`notes-label-data.md`, section 2;
records of 10 bytes a point label, 8 a line label, strings per tile):

| variant | Frankfurt | Berlin |
|---|---|---|
| points: places, water and park points, pois | 1,212,487 (12.3 %) | 3,573,920 (10.2 %) |
| points without pois | 193,247 (2.0 %) | 218,967 (0.6 %) |
| points and named lines, geometry copied | 3,067,635 (31.1 %) | 8,309,864 (23.6 %) |
| points and named lines, geometry referenced | 2,183,355 (22.1 %) | 6,172,608 (17.5 %) |
| points and one anchor plus angle per line name per tile | 2,035,709 (20.6 %) | 5,861,773 (16.7 %) |

Per-tile string tables are most of it (Frankfurt 1.22 MB, Berlin 3.52 MB
in the line variants). One region-wide table instead is 573 KB for
Frankfurt (31,315 strings) and 1.68 MB for Berlin (90,821, beyond a u16
index), all pois included; places and water alone are 41 and 43 KB.

So LMR2:

- **One string table per region**, UTF-8, u32 offsets, sorted so a name
  is found by binary search too (search by place name comes free).
- **A label section per tile**, beside the geometry blob: point labels
  (kind u8, rank u8, min zoom u8, flags u8, string u32, anchor 2 x i16,
  12 bytes) and line labels as a reference (feature index u16 into the
  tile's geometry blob, string u32), so street names reuse the geometry
  the tile already holds. The cut then must not pack two named lines of
  different names into one feature (it packs parts into as few features
  as fit, `loquax-map.md` section 4, "Layer profile 1"); a named line
  keeps its own feature, at a header's cost.
- **Pois thinned at the cut** as CoMaps does: sort by (class priority,
  rank) and push a poi's minimum zoom up while a stronger one lies within
  80 px at that zoom (`libs/indexer/displacement_manager.hpp:24`, `:107`).
  Profile 1's pois: none until the poi slice; places, water and parks
  first.
- **Old readers refuse cleanly**: `Header::parse` rejects a version it
  does not know with `RegionError::Version`
  (`leviculum-map/src/region.rs:137`), so a v2 file in a v1 reader is a
  clean error, never a misread. A board that wants no labels reads the
  geometry index and never opens the label section; the sections have
  their own index entries, so skipping costs nothing.
- Estimated size, places, water, parks and street names, region-wide
  strings: Frankfurt about 0.4 to 0.8 MB (4 to 8 %), Berlin 0.8 to 1.5 MB
  (2 to 4 %). Derived from the counts above, not measured as one table.

### Placement

One rule for every frontend, in `leviculum-map` beside
`pins::labels` (`leviculum-map/src/pins.rs:210`), pure and in the unit of
its caller (cells, pixels):

1. Pins and their labels first; nothing covers them (the 1066/1084 rule).
2. Places by class, then rank: city, town, district (`macrohood`),
   village, suburb, neighbourhood; filter `min_zoom <= z` and drop
   `locality`, `hamlet`, `isolated_dwelling`, `farm`.
3. Water and park names, in their area's colour family (italic on the
   PC).
4. Street names, major roads first, from z14 (major) and z15 (all).
5. Greedy: a label is placed when its boxes are free, else dropped.
   Order is deterministic (priority, then feature id), so a pan does not
   reshuffle, CoMaps' main anti-flicker tool
   (`libs/drape/overlay_tree.cpp:36`). CoMaps has no hysteresis and no
   fade for map labels; it re-lays out every 5 to 15 frames while moving
   (`overlay_tree.cpp:18`) and once at rest. We re-lay out at rest only:
   while moving, labels ride along in the cached raster.

Street names on the PC: CoMaps bends text glyph by glyph along the road,
rejects a label where two glyphs differ by more than 16 degrees, reads it
left to right by walking the road from the other end, and repeats it every
`max(300 px, text) + text` (`libs/drape_frontend/text_layout.cpp:503` to
`:537`). The harness's simpler first step, the same rules on a straight
label: find the longest run whose segments turn less than 12 degrees, set
the text on its middle at the run's angle, flipped to read left to right,
collision by squares along the baseline. `07-road-names-*.png` shows it
works on Frankfurt z15 and Berlin; bending along curves is a later step.

Terminal (`term-labels-*.ans`, committed under
`docs/drafts/map-renderer-samples/`): text cannot rotate, so street names
only along horizontal or vertical runs (1090, section 2), and places:

- **Density rule:** at most one place label per 20 x 6 cell block, one free
  cell around each label, names never cut (a name that does not fit is
  left out; cut names like `ORANIENBURGER VORS` read worse than none).
  That gives 1 to 3 labels at 80 x 24 and 20 at 155 x 65 (Berlin z12).
- **Halo:** the label's own cell background (dark on white, light on near
  black); one cell of space around it.

### Fonts

- **M9:** pre-rendered bitmap glyphs with 4-bit coverage in flash, Latin-1
  and Latin Extended-A (umlauts, Polish, Czech; about 320 glyphs),
  Cyrillic optional; two sizes (12 and 15 px) regular and bold. About 60
  bytes a glyph and size, so roughly 80 to 120 KB of flash; the halo is
  a one-pixel dilation of the coverage, drawn first in the halo colour.
  No shaping: European scripts need none. Roboto (Apache-2.0) is what
  CoMaps ships for those blocks (`data/fonts/`, about 165 KB).
- **PC:** cosmic-text and swash are in the lock already (iced uses them).
  The harness set text as outlines through tiny-skia with a stroked
  halo: 0.28 ms a label (70 ms for 250 at 1080p), fine for a raster drawn
  once at rest, too slow per frame. A glyph cache (rasterised glyph and
  halo coverage per size, blitted with alpha) costs a few microseconds a
  glyph; rotated street names rasterise per label once and stay in the
  cached raster.

## 2. Detail tiers from one code path

| parameter | M9 (320x240) | TUI | GUI 1x | GUI 2x / 4K |
|---|---|---|---|---|
| layers | no buildings, no pois | no buildings | buildings z14+ | buildings z14+ |
| anti-aliasing | off (text: 4-bit glyphs) | n/a (cells) | areas and lines | areas and lines |
| casings | major roads, after the stroke fix | no | all roads z13+ | all roads z13+ |
| line ends and joins | square (today) | n/a | round | round |
| labels | places, water; streets z15 major | places, water | all, streets along runs | all, scaled |
| label density | about 10 a view | 1 per 20 x 6 cells | collision only | collision only |
| hillshade | optional, z<=12, if the blend fits | background tone | yes | yes |
| overzoom | 2 | as today | 3 | 3 |

**One format with optional sections, no cut per profile.** The geometry
blob stays profile 1 for everyone; buildings, labels and shade are
sections (or a sibling file for shade) a device does not read. Measured
cost of buildings (`notes-label-data.md`, section 3, through the real
`cut_tile`): Frankfurt +0.56 MB at z13, +2.61 MB at z14, +8.56 MB at z15
(+119 % in all); Berlin +1.81, +7.97, +25.10 MB (+99 %). 23 % of
Frankfurt's and 30 % of Berlin's z15 buildings carry a `height`, so a
2.5D look is possible later on the PC. Draw time: on the host with
tiny-skia, buildings take Frankfurt z15 at 1080p from 175 to 280 ms
(+60 %); on the M9 the full profile drew in 177 ms against 90 ms
without them (`map-measurement-2026-10.md`, device rows).

**Simplification** stays at the cut: profile 1 already simplifies at half
a pixel from z13 and one pixel below (`leviculum-map/src/profile.rs`).
CoMaps keeps four geometry levels per feature at 1.3 px of a 256 px tile
(0.4 px at the deepest) and drops areas under 7 px
(`libs/indexer/scales.cpp:49` to `:84`); our per-zoom tiles are finer
already.

**The M9's stroke.** Stroke takes 13.4 M cycles for 21,422 pixels and
5,919 edges in both profiles (`map-measurement-2026-10.md`, device rows):
about 2,270 cycles an edge. Every edge does two `ceil_div` and two
`div_euclid` on `i64` (`leviculum-mapfill/src/fill.rs:231`, `:232`, `:248`,
`:249`), and every stroke segment an integer square root by Newton steps
of `u64` division and two `i64` divisions (`leviculum-mapfill/src/stroke.rs:56`
to `:62`). The ESP32-S3 has a 32-bit divider; 64-bit division is a
library call. Hypothesis, to be tested by a CCOUNT microbenchmark of
`Filler::edge` and `stroke` on the device before any change: the
divisions are most of fill's and stroke's per-edge cost. If it holds, an
`i32` path (sub-pixel coordinates fit in 23 bits) is the cheapest M9
speed-up there is, and casings become affordable.

## 3. Anti-aliasing

Measured with tiny-skia 0.11 on the same geometry, AA off against on
(`bench-tinyskia.md`; PMTiles geometry, unsimplified, so tiny-skia draws
more vertices than mapfill does from a region):

| view | size | no AA | AA | casings, round, no AA | casings, round, AA |
|---|---|---|---|---|---|
| Frankfurt z15 | 320x240 | 2.80 ms | 5.48 | 9.15 | 13.56 |
| Frankfurt z15 | 1920x1080 | 27.94 | 83.30 | 79.30 | 175.49 |
| Frankfurt z13 | 1920x1080 | 52.06 | 125.88 | 135.41 | 303.32 |
| Berlin z13 | 1920x1080 | 83.78 | 204.70 | 226.99 | 464.45 |
| Berlin z13 | 3840x2160 (2x) | 153.10 | 442.19 | 360.58 | 815.93 |

AA costs tiny-skia 2.0 to 3.0 times. tiny-skia is a general float
rasteriser; for the same Frankfurt z15 1080p view mapfill drew from the
region in 10.7 ms where tiny-skia took 27.9 ms without AA. So our own
coverage AA is the way, and its cost estimate is 2 to 2.5 times
mapfill's fill and stroke: for each row, accumulate exact area coverage of
the edges crossing it (the cell-accumulation scheme of font rasterisers),
four sub-scanlines are enough at map scale.

- **Desktop:** AA on, areas and lines, gamma: blend in linear light for
  dark lines on light ground only if a golden test shows a difference;
  sRGB blending is what tiny-skia does and looks right in the images.
- **M9:** off for geometry. CoMaps itself draws lines and areas without
  AA on phones: the line shader's fade is commented out and the
  post-process AA is off "by energy-saving issues"
  (`libs/shaders/GL/line.fsh.glsl:33`, `libs/drape/support_manager.hpp:51`);
  only text is smooth. That is the right split for us too: AA text on
  the M9, AA geometry on the PC.
- **AA on lines only** saves the area edges, which are most edges (13,747
  fill edges against 5,919 stroke edges in the M9 view); on the desktop
  full AA is affordable, on the M9 neither until the stroke fix.

Images: `03-casings-roundjoins-*` against `04-aa-*`.

## 4. Style

- **Palette** (proposed table `LIGHT` in the harness, CoMaps colours as
  the reference: land `#F5E8D6`, water `#89CDDC`, park `#D5DB8F`, woods
  `#A2BA4E`, motorway `#F57A00`, `data/styles/default/light/colors.mapcss`):
  warmer land, a forest tone apart from parks (the cut maps wood and
  forest to `Park` today, `leviculum-mapcut/src/mvt.rs:46`), water a step
  greener, roads in three warm steps (highway, major, medium) instead of
  one, minor white, paths brown and dashed. `02-palette-*.png`.
- **Casings** under all fills, darker for major roads; tunnels faded,
  bridges drawn last with their own casing, by `is_bridge` and
  `is_tunnel`, which the cut would keep as a class flag. CoMaps orders
  buildings < bridge under-lines < casings < fills, each `layer` step
  1,000 deeper (`data/styles/default/include/priorities_3_FG.prio.txt`,
  `libs/drape_frontend/apply_feature_functors.cpp:515`), and starts a
  road's casing a few zooms after its fill.
- **Draw order of areas:** CoMaps draws smaller areas over larger by the
  log of their bounding box (`libs/indexer/drawing_rule_def.cpp:63`),
  water above all landcover. Ours draws by class; water is already after
  parks. Size order is a cut-time sort, free at draw.
- **Buildings:** fill `#D5D6D2`-like, a one-pixel edge from z15, fading
  in from z14 as CoMaps does (`Basemap.mapcss:714`).
- **Dark theme:** one table for one table (`09-dark-*.png`), the 1042
  palette tokens deciding the label colours; the terminal's `-dark`
  remap of 1090 is the same idea.
- **Rounded joins:** round caps and joins on the PC; the M9 keeps square
  caps (round joins are a circle per vertex through the filler).
- **Hillshade** is section 6b; isolines (CoMaps carries them as ordinary
  features plus a step, `ISOLINES.md`) are a later outdoor layer.

## 5. Speed and smoothness

**Budgets.** Desktop: 16.7 ms a frame at 60 fps. M9: the flush alone is
30.8 ms (153,600 bytes at 40 MHz, measured), so a full frame cannot go
faster than 32 fps; "no stutter" there means a pan step of at most
66 ms (15 fps) and a full redraw, on zoom, of at most 150 ms, and input
is never held by a draw.

**Today's renderer on the host** (`bench-today.md`, `bench-today-4k.md`,
`bench-threads.md`; loquax's `Painter` over LMR1, mapfill, no AA):

| view | 320x240 | 1920x1080 | 3840x2160 at pixel ratio 2 (z+1) | same, 4 threads | 8 threads |
|---|---|---|---|---|---|
| Frankfurt z13 | 2.06 ms | 23.01 | 53.29 | 19.1 | 32.2 |
| Frankfurt z15 | 0.62 | 10.68 | | | |
| Berlin z12 | | 17.96 | 112.25 | | |
| Berlin z13 | 2.25 | 36.85 | 96.77 (106.3 in the band run) | 52.0 | 41.7 |
| Berlin z14 | | 25.87 | 57.14 (70.4) | 20.2 | 25.5 |

The threaded run splits the frame into horizontal bands, one `Painter`
each; every band reads and projects whole tiles again, so it scales to
about 3 to 3.5 times at four threads and no further. Splitting by tile
with the projection shared would scale better; not measured.

**Pan:** shifting a 4K RGBA frame 16 to 256 px costs 0.59 to 1.06 ms; the
exposed strip, drawn as a viewport of its own, 0.38 ms (16 px), 0.72 ms
(64 px), 2.34 ms (256 px). So **a pan step at 4K costs under 4 ms** with
today's code, and at 60 fps the CPU has room for AA and casings in the
strip.

**Zoom gesture:** scaling the cached 4K raster by 1.1 costs 36.8 ms
nearest and 74.2 ms bilinear (tiny-skia `draw_pixmap`, one thread). That
does not fit 16.7 ms; at 1080p it is a quarter (est. 9 to 19 ms).

**4K with everything, extrapolated:** AA 2 to 2.5x and casings about 2x
on strokes take a 4K redraw from 57 to 112 ms to roughly 150 to 300 ms on
one thread, 50 to 100 ms on four, labels from cache included. That is a
redraw at rest after a gesture, not a frame: fine.

**Techniques, in order of value:** (1) a raster cache of the view with a
margin, blit on pan, draw only the exposed strip; (2) the redraw at rest
on a worker thread, the old raster shown scaled until it lands; (3)
level of detail while moving: no AA, no labels, then the full redraw;
(4) tiles of the view drawn on several threads. The M9 gets (1) too: its
frame lives in SRAM, a shift is a `memmove` of 153,600 bytes, and the
flush dominates.

## 6. GPU: needed or not

**Verdict: not needed for what is specified today.** Numbers: pan at 4K
costs under 4 ms on the CPU (blit plus strip); a full redraw at rest at
4K is 19 to 52 ms on four threads today and an estimated 50 to 100 ms
with AA, casings and labels, once per gesture; the M9 has no GPU and
must run the same code anyway.

**Decision rule.** Take the GPU when a required interaction needs a
per-frame transform of more than about 2 megapixels at 60 fps that the
CPU cannot do in 16.7 ms, measured on the target machine: animated zoom
or rotation at 4K (CPU scaling: 37 to 74 ms), map tilt, or a pan whose
p99 frame stays above 16.7 ms after the raster cache and threads.

**The cheapest GPU step:** iced's `wgpu` backend, with our raster uploaded
as a texture (iced's image widget) and pan and zoom as texture
transforms; our renderer stays on the CPU and draws the redraw at rest.
loquax builds iced with tiny-skia alone today (`loquax/Cargo.toml:159`);
with both backends enabled iced picks tiny-skia when no GPU adapter is
found (from iced's documentation as remembered, to be checked). Cost:
a larger binary and a GPU driver path in the GUI, nothing in the map
core.

**Full GPU renderers** (from knowledge, not checked online in this
pass): Vello (linebender, Apache-2.0/MIT, compute-shader 2D, pre-1.0),
lyon (tessellation to triangles, mature, MIT/Apache-2.0), maplibre-rs
(experimental, little activity), maplibre-native (C++, BSD-2, mature, a
style-spec engine of its own). Each gives up the one code path: the
style, the label rule and the golden images would exist twice, and the
M9 could not follow. CoMaps is the counter-example worth noting: a mature
GPU map that still draws geometry without AA and re-lays labels only
every few frames, so the GPU buys it throughput, not beauty.

## 6b. Hillshading for off-road use

**Superseded in scope (2026-11-11):** the elevation decisions, two tiers,
the sources and the pyramid served in pieces, are section 3 of [the
expedition map](../src/concepts/loquax-expedition-map.md); the algorithm
and the port below stay its basis.

**The algorithm** (Lew's, `generator/hillshade_generator.cpp`, summary in
`notes-mwm-hillshade.md` A.1): four lights at 247.5, 292.5, 337.5 and
22.5 degrees, 45 degrees up, weighted by aspect after Mark (1992); three
scales (Gaussian sigma 0.7, 2.5, 7 px, mixed 0.45, 0.35, 0.20, vertical
exaggeration 1.3 times 1, 1.5, 2.5); flat-land damping
`0.35 + 0.65 smoothstep(0.015, 0.045, amplitude)`; a local contrast step
(unsharp, sigma 5, 0.5 and 2.4) that HILLSHADE.md does not mention;
aerial perspective and relief boost from a 12 km window; quantisation per
tile; 256 px tiles with a 1 px border, rows delta coded, zlib. The blend
draws black (alpha up to 0.7) or white (up to 0.14) with alpha
`strength * |1 - shade/180|^1.2`, above landcover and below water, lines
and labels (`libs/drape_frontend/hillshade_renderer.cpp`).

**Port:** a stage in `leviculum-mapcut` that reads SRTM `.hgt` and
GLO-30, renders the pyramid with the same parameters (Apache-2.0 notice
kept, though Lew is the author), and writes a sibling `<region>.lms`
with LMR's header and index conventions; golden tests against tiles of
CoMaps' generator from the same `.hgt`. The renderer blends through a
256-entry table after the areas and before water. The terminal modulates
each cell's background toward black or white by the same alpha.

**Measured** from one real SRTM 1" tile (N50E008, 25,934,402 bytes, from
the public `elevation-tiles-prod` bucket) with a simplified version of the
algorithm in the harness (lights, scales and blend as above; no damping,
unsharp step or aerial perspective):

| zoom | m/px at 50 N | tiles (Frankfurt box) | step 4 | step 8 | step 16 |
|---|---|---|---|---|---|
| z9 | 196 | 1 | 6,350 B | 3,563 | 2,320 |
| z10 | 98 | 4 | 26,479 | 15,806 | 10,151 |
| z11 | 49 | 9 | 104,607 | 66,640 | 43,145 |
| z12 | 24.5 | 20 | 279,446 | 187,920 | 122,761 |
| z13 | 12.3 | 72 | 981,226 | 674,282 | 435,612 |

z9 to z12 is 416,882 bytes at step 4 and 273,929 at step 8: 4.2 and 2.8 %
of the 9.88 MB Frankfurt region. SRTM's 30 m does not justify z13 (CoMaps
caps the native zoom at 27 m a pixel, slippy z11 in Germany; z12 at 24.5 m
is the honest limit); deeper zooms overzoom z12 bilinearly.
`notes-mwm-hillshade.md` estimates Germany at 17 to 75 MB for z7 to z11
and 68 to 300 MB to z12, about 1 to 4 % of its 3.9 GB region.

**Blend cost** (`bench-shade.md`, precomputed shade, one thread, an
unoptimised per-channel divide by 255):

| size | RGBA | Rgb565 |
|---|---|---|
| 320x240 | 0.133 ms | 0.167 ms |
| 1920x1080 | 3.648 | 4.742 |
| 3840x2160 | 14.582 | 17.207 |

M9 estimate: 0.167 ms times 120 to 140 is 20 to 23 ms, too much on a 90
ms frame. A table of 16 alpha levels with shifts instead of division
should bring it to about 5 ms (estimate). Generating the raster is a cut
job (566 ms for a 1080p view on the host), never a device job.
Images: `10-noshade-taunus-*` against `11-hillshade-taunus-*`.

**Elevation sources:** SRTM 1" is public domain (NASA/USGS), 25.9 MB a
degree cell, ends at 60 N. Copernicus GLO-30 covers the whole earth and
allows derived products with its attribution; the attribution text in
`notes-mwm-hillshade.md` is from memory and must be checked against the
licence before shipping. CoMaps builds from both (Lew's server: OSM plus
GLO-30).

**Autoload (1089):** the shade layer is fetched like a region: a box,
zooms 9 to 12, from a shade pyramid on Lew's server (or cut locally from
DEM cells, 26 MB a degree, too much for an autoload), a few hundred KB a
city. It belongs in the same cache with the same eviction.

## 6c. CoMaps as inspiration

What a mature app does, in short (details and citations in
`notes-comaps-design.md`):

- **Priority** is one 64-bit number: style priority, then a one-byte
  population rank `log1.1(population)`, then text length, longer first
  (`libs/drape/overlay_handle.cpp:235`, `libs/indexer/feature_impl.cpp:9`).
  City 7350, town 7150, village 6950, motorway name 6800, primary 6250,
  river 3200, residential 3020
  (`data/styles/default/include/priorities_4_overlays.prio.txt`).
- **Collision:** greedy over a sorted list with an R-tree, one rectangle
  for an icon or straight caption, one per glyph for path text
  (`libs/drape/overlay_tree.cpp:225`, `libs/drape_frontend/path_text_handle.cpp:311`);
  an icon and its caption live and die together; labels 75 px off-screen
  still compete (`libs/drape/constants.hpp:12`).
- **Fonts:** FreeType distance fields at 22 px into one 1024x1024 atlas,
  HarfBuzz shaping, 11 MB of fonts (mostly CJK fallbacks).
- **Data per zoom:** four geometry levels, 1.3 px tolerance, 7 px area
  drop, pois thinned offline at 80 px.
- **Smoothness:** two reading threads, tiles 256 to 1024 px, no redraw
  after two idle frames (`libs/drape_frontend/frontend_renderer.hpp:484`),
  labels re-laid every 5 to 15 frames in motion.

**Copy:** the offline poi thinning, the one-byte rank, the label spacing
and reading-direction rules, size-ordered landcover, casings and bridge
under-lines as passes, per-zoom paler landcover, population-gated city
labels (`Basemap_label.mapcss:137`).

**Do not copy:** GPU batching and buffers; the four-rank overlay tree and
R-tree (a sorted list does for 20 to 300 labels); distance-field fonts,
HarfBuzz and 11 MB of fonts; per-glyph path text with spline smoothing
(first step: straight labels); run-time language selection with ICU
transliteration (resolve at the cut); the MapCSS toolchain (a small table
per class: zoom range, draw order, label priority).

## 6d. Lew's CoMaps maps directly?

**The server answers** (HEAD only, 2026-10-29): `https://maps.lew-palm.de/`
200 (nginx, 9,485 bytes); `/maps/2026.06.28/260830/countries.txt` 200,
363,935 bytes, the snapshot's length; and
`/maps/2026.06.28/260830/Germany_Hesse_Regierungsbezirk%20Darmstadt.mwm`
200, 162,855,018 bytes, exactly the `countries.txt` value,
`accept-ranges: bytes`.

**The format** (`notes-mwm-hillshade.md` B): a section table at the end,
named sections; four geometry scales per feature (country z10, 12, 14,
17; world z3, 5, 7, 9), points as zigzag deltas against a predicted point
in varints, areas pre-triangulated as strips; types as classificator
indices (about 1,700); names in up to 64 languages; a spatial index and
an Elias-Fano offset table; hillshade in its own section (version 2,
21-byte index entries, zlib tiles); format version 12 (June 2026), the
features section with a sub-version changed in 2025-06. Addressing:
`<server>/maps/<series>/<data version>/<file>.mwm`, `countries.txt` a
signed JSON tree with size and SHA-1. Germany is 48 files and 4,678 MB;
Frankfurt lies in Regierungsbezirk Darmstadt (162.9 MB), Berlin is one
file (110.5 MB); low zooms need World.mwm (97.8 MB) and WorldCoasts
(8.5 MB).

| option | effort | for | against |
|---|---|---|---|
| (a) MWM reader in loquax and on the board | 25 to 35 days | one source, hillshade inside | geometry per feature, not per tile: index, offsets, feature, geometry are 2 to 4 random reads a feature; Darmstadt's offsets alone about 2.8 MB of PSRAM; classificator indices change per series |
| (b) MWM to LMR converter, on Lew's server | 5 to 8 days on CoMaps' C++ libraries, 15 to 20 with our own reader | autoload from our own server, no third party | a run per map series, inherits CoMaps' four-level simplification |
| (c) Protomaps vectors, hillshade our own (or from the MWM section) | 6 to 9 days for our SRTM/GLO-30 stage; 2 to 3 to read the MWM section by range requests as a demo | the only thing MWM has that we lack; small; devices skip it | two sources |
| (d) Protomaps as today | none | | no relief |

**Recommendation: (c), our own shade stage**, with a range read of one
MWM hillshade section as the byte-exact reference for golden tests (not
attempted here: whether the served files carry the section is
unverified). Relief is what the MWMs offer that loquax lacks, and it is
small; LMR's per-tile blobs suit the ESP32 far better than MWM's
feature-indexed layout. Then, independent of the format: **serve LMR
regions and shade layers from Lew's server** (cut there from a Protomaps
planet build or later from an OSM pipeline), set as the default
`map_source`, which ends the hotlinking of `build.protomaps.com` that
`loquax-map.md` section 4 names and keeps the areas a user looks at off a
third party. Licences: OSM data stays ODbL in every option, the shade
carries the DEM's attribution, any CoMaps algorithm followed closely keeps
its Apache-2.0 notice.

## 7. Telemetry on the map

The store keeps every fix (`loquax/src/store.rs:152`, table `positions`:
time, lat/lon, altitude, speed, bearing, accuracy), so all of it is data
we have:

- **Tracks:** a peer's fixes of the last hours as a polyline, newest
  opaque, older fading; a few hundred vertices are one stroke, well under
  a millisecond on the host and a few ms on the M9 after the stroke fix.
- **Accuracy circle:** a filled circle at 20 % alpha with a one-pixel rim;
  needs an alpha span in the filler (also needed for the hillshade and
  for fading). Cost proportional to its pixels; drawn under the pin.
- **Heading and speed:** a pin with a wedge in the bearing direction when
  speed is above walking pace; speed as a second label line on the PC.
- **Own position:** a blue dot with an accuracy circle and a heading
  cone.
- **Stale styling:** a fix older than the announce cadence's silent
  interval (one hour, `docs/src/concepts/announce-cadence.md`) drawn
  hollow and grey, its label with the age.

All of it goes through the pins rule: pins and their labels first.

## 8. Testing beauty without slowing down

- **Golden images per profile** from a small committed region (the size of
  `duomo.lmr`, 188 KB) at fixed views: M9 320x240, TUI cells per
  technique, GUI 1x and 2x, light and dark. The integer path (no AA) is
  bit-exact, so its threshold is zero differing pixels; the AA and text
  path gets a tolerance (at most 0.1 % of pixels differing by more than 2
  per channel).
- **Performance gates by counts, not clocks:** the forge runner is shared
  and a wall-clock budget lost a landing there (672, 689). The filler
  already counts edges, spans and pixels and
  `leviculum-mapfill/tests/fixture.rs` pins them; a gate per profile pins those counts plus labels placed, so a
  change that draws more shows as a number. Wall-clock and device CCOUNT
  budgets run on the rig by the reviewer when a profile's counts move.
- A "prettier" change states its count deltas and its device estimate in
  its commit, as this draft does.

## 9. Low hanging fruit

Ordered by beauty for the effort. Images are under
`/home/lew/scratch/1094/out/` on the coder host, at 1280x800 for the PC
and as `-m9` at 320x240 shown twice the size; `00-today-*` is the
renderer as it is, `01-tinyskia-today-style-*` the same style through the
harness, each later number one step on top of the one before.

| # | change | effort | runtime cost | where | images |
|---|---|---|---|---|---|
| 1 | Palette: warmer land, forest apart from park, three road steps, paths brown | 2 h (style table, cut maps forest to a class of its own) | none | all | `02-palette-*` |
| 2 | Dark theme table | 1 to 2 h | none | GUI, M9 | `09-dark-*` |
| 3 | Road casings and round joins (PC) | 3 h | strokes about 2x; Frankfurt z15 1080p 27.9 to 79.3 ms in tiny-skia, mapfill's share small on the PC | GUI, TUI octant | `03-casings-roundjoins-*` |
| 4 | M9: `i32` edge setup and stroke, after a CCOUNT microbenchmark confirms the 64-bit divisions | 3 h plus a device run | hypothesis: a large share of the 55 ms stroke and of fill | M9 | none |
| 5 | Anti-aliasing in the filler (GUI) | 1 to 2 days | 2 to 2.5x fill and stroke (tiny-skia: 2 to 3x) | GUI | `04-aa-*` |
| 6 | Bridges and tunnels: keep `is_bridge`/`is_tunnel` as class flags, draw in layers | 4 h with a re-cut | none | all | in `03-*` and later |
| 7 | Place names (needs LMR2's point labels: a slice, not an hour) | 2 to 3 days with the format | placement microseconds, text a glyph blit | all | `05-places-*`, `term-labels-*.ans` |
| 8 | Water and park names | with 7 | as 7 | all | `06-water-park-names-*` |
| 9 | Street names along straight runs | 1 to 2 days after 7 | 0.28 ms a label as outlines; cache at rest | GUI | `07-road-names-*` |
| 10 | Buildings on the PC (a section) | 1 day with the format | +60 % draw at z15 (tiny-skia), +87 to 119 % bytes | GUI | `08-buildings-*` |
| 11 | Hillshade | 6 to 9 days | 0.13 ms (320x240) to 3.6 ms (1080p) host blend | all | `11-hillshade-*` |

The first three and the dark theme change no format; items 1 to 3 are a
day together and take the map from `00-today-frankfurt-z15-1280.png` to
`04-aa-frankfurt-z15-1280.png` on the PC.

## Phased plan

1. **Style, no format change:** palette, dark table, casings and round
   joins on the PC, forest class and bridge/tunnel flags in the cut,
   golden images per profile with count gates. M9: the division
   microbenchmark, then the `i32` path if it holds.
2. **LMR2 with labels:** region-wide strings, per-tile label sections,
   poi thinning at the cut, the shared placement rule; places and water
   in all three frontends, street names on the PC.
3. **AA and glyph cache on the PC**, the raster cache with blit on pan,
   the redraw at rest on a worker.
4. **Buildings section and the M9 font**, M9 labels.
5. **Hillshade:** the Rust port, `.lms`, the blend, the terminal tone,
   the autoload of shade.
6. **Lew's server as `map_source`:** LMR regions and shade layers served
   by our own.
7. Only if section 6's rule fires: iced's wgpu backend for animated zoom.

## Open questions for Lew

1. Animated, continuous zoom at 4K: wanted? It is the one thing that
   needs the GPU step (section 6).
2. The M9's targets: is 15 fps panning and a 150 ms redraw "no stutter"
   for you, or do you want more (then the flush at 40 MHz SPI is the wall:
   is a faster SPI clock on the M9's panel an option to try)?
3. Language at the cut: `name` plus the user's locale where it differs,
   the local name not shown twice. Or both lines as CoMaps does?
4. Hillshade source: our own stage from SRTM/GLO-30 on your server, or
   should loquax read the hillshade sections of your MWMs by range
   requests (needs the sections served as they are)?
5. Serve LMR regions from maps.lew-palm.de as the default `map_source`,
   cut there from a Protomaps planet build?
6. Pois: which classes at all (the measurement shows 168,433 pois in
   Frankfurt's z15 alone)? **Decided 2026-11-11 (Lew): all of them go into
   the data; only the display thins by zoom and rank.** The short list
   proposed here (station, hospital, pharmacy, shelter, peak, water point)
   is rejected; [the expedition map](../src/concepts/loquax-expedition-map.md),
   section 4.
   6b. Shading (section 6b, and question 4's source): see the expedition
   map, section 3.
7. Street names bent along curves (CoMaps' per-glyph layout), or straight
   labels on straight runs as the samples show?

## See also

- `docs/src/concepts/loquax-map.md`: the decisions, the region format
  (section 4) and the terminal ladder (section 6).
- `docs/drafts/map-measurement-2026-10.md`: the device rows.
- `docs/drafts/terminal-map-rendering-2026-10.md`: the cell techniques
  (1090).
- `docs/drafts/map-renderer-samples/`: the terminal samples with labels.
