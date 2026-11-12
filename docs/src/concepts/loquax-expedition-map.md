# Loquax: the expedition map

The expedition-map half of the Leitstern (Lew, 2026-11-11): one app
whose map serves daily life and the field alike. This page collects its
concept sections as they land; the first is the world view's
projection (order 1710).

## The world in Equal Earth, flowing into Mercator

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

### The scale match, and what it costs at high latitudes

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

### Everything that reads the projection

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

### Pictures and cost

Pinned: `leviculum-map/tests/golden/world-blend-{ee,globe}-480x320-{z2,z35}.png`.
The zoom strips (five centres, both shapes, z1→z7) and the two frame
series at Berlin and Merzouga are the 1710 report's, rendered by
`cargo run --release -p leviculum-map --example world_blend_pictures`.
A 1024×768 world view on the reference host draws in 11.3 ms linear,
21.1 ms Equal Earth, 24.1 ms globe (release, median of nine): a
blended frame redrawn whole every zoom frame holds 40 fps on the
desktop; the phone runs the same code through the facade.
