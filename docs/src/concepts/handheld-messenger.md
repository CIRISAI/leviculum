# A messenger whose core runs anywhere

A real messenger runs on the handheld. It sends and receives LXMF
messages, keeps contacts and history, and shows on a map where other
nodes are, from their telemetry, the way Columba does on Android today.
Lew decided on 2026-10-21 that it is written once, as a core, and
reused: on the ThinkNode M9 ([The ThinkNode M9](thinknode-m9.md)), on
the desktop, and later on Android. The map uses OpenStreetMap data and
existing libraries, not a renderer of our own. This page records that
decision, what exists to build it from, the core/frontend cut and the
map as decided on 2026-10-25, and what is open.

## Decided

- **The features:** send and receive LXMF messages, contacts, history,
  and the positions of other nodes on a map, taken from their telemetry.
- **One core, several frontends:** the M9, the desktop, later Android.
- **The map** is built from OpenStreetMap material with existing
  libraries. We do not write our own renderer. (Narrowed 2026-10-25,
  Lew: one piece of our own, a polygon filler; see "The map".)
- **Columba is the reference for scope and protocol behaviour**: what a
  messenger does and how it behaves on the wire. It is not a reference
  for how the code is designed. (Superseded 2026-10-25, Lew: Columba is
  the wire reference for BLE and LXMF behaviour, not the scope ceiling;
  the scope is the full-featured messenger the product goals below
  describe.)
- **2026-10-25, Lew: a full-featured messenger.** It is of the class a
  modern phone app sets, with mail-like features where LXMF carries them
  (threads, replies, long-form, attachments). Columba remains the
  reference for wire behaviour, not the ceiling for scope. One core, four
  frontends: the terminal (`loquax`), a desktop GUI, the M9's display and
  keyboard, later Android; the TUI and the GUI ship side by side on the
  desktop.

## What exists today

- **`loquax`, the terminal messenger.** It sends (direct, propagated, or
  `auto` with the fallback to a propagation node) and drains the
  propagation node, and since 2026-10-25 (994) plain `loquax` opens its terminal
  frontend (`loquax tui` under its old name), slice one ([Loquax: the user interface](loquax-ui.md), "Built
  2026-10-25"): the first frontend on the shared widgets, over a screen
  model that lives in the `no_std` core. Its frontend already
  talks to the backend only through `Outbox` (`loquax/src/outbox.rs:376`),
  but the crate is `std`. It depends on `leviculum-std` and tokio
  (`loquax/Cargo.toml:183`, `loquax/Cargo.toml:187`), since 2026-10-25 behind its `std` feature,
  and `just loquax-core-no-std` holds the core to `no_std` (green since
  993; the pass order is in [Loquax: architecture](loquax-architecture.md),
  "Plan: the Loquax core without std").
- **#232's rule:** the messenger's core must be `std`-free, not merely
  terminal-free, because the next frontend after the TUI is firmware. The
  shape #232 asks for is a `no_std + alloc` core crate, a store behind a
  trait the core owns, effects as data, and a cross-compile gate. That
  core crate does not exist yet. [Loquax:
  architecture](loquax-architecture.md), section 4, has the terminal-free
  requirement it extends.
- **`lnomad`'s core/frontend boundary (#219).** It is the same
  requirement for the browser, cut on 2026-10-25: model, `update` and the
  effects live in `lnomad/src/app/` and import no terminal crate, the
  ratatui frontend in `lnomad/src/tui.rs` behind the `tui` feature, and
  `just lnomad-core-no-tui` holds the line. That is enough for a desktop
  GUI as a second frontend. It is not yet enough for the M9: the core is
  a `std` module of the `lnomad` crate, and the crate without the feature
  still carries the fetch layer (`leviculum-std`, tokio), so a firmware
  frontend still needs the core as a `no_std` crate of its own.
- **`leviculum-lxmf`, `no_std + alloc`** (`leviculum-lxmf/src/lib.rs:38`):
  - delivery (`LxmfRouter`, `leviculum-lxmf/src/router.rs:462`);
  - propagation nodes, both the client and the node
    (`PropagationNode`, `leviculum-lxmf/src/propagation_node.rs:889`);
  - stamps (`CooperativeStamper`, `leviculum-lxmf/src/stamp.rs:269`).
- **Telemetry.** [Telemetry](telemetry.md) settles positions as LXMF
  fields (`FIELD_TELEMETRY`, `FIELD_TELEMETRY_STREAM`) and the
  collector exchange. The codec is `Telemetry`
  (`leviculum-lxmf/src/telemetry.rs:176`) with its `Location`
  (`leviculum-lxmf/src/telemetry.rs:56`).
- **The C surface, `leviculum-ffi`**, is the path to Android
  (`leviculum-ffi/src/lib.rs:1-12`, [C API](../c-api/overview.md)). Today
  it covers identities, destinations, links, requests and resources, and
  no LXMF.
- **`embedded-graphics` on the T114's ST7789.** The T114 paints its
  status screen through `embedded-graphics`
  (`leviculum-nrf/screen/Cargo.toml:10`) into a monochrome framebuffer
  (`MonoFb`, `leviculum-nrf/screen/src/lib.rs:377`), which the ST7789
  backend scales onto a 240x135 panel (`leviculum-nrf/src/st7789.rs:3`).
  That is a status screen, not a colour UI. It shows that the drawing
  library works on our firmware.

## The core/frontend cut

*Decided 2026-10-25 (Lew).* The frontends: iced on the desktop, ratatui
in the terminal and, through mousefood, on the M9 ([A GUI toolkit for
the desktop and the M9](gui-toolkit.md), "Decision").

**The core owns:**

- conversations, contacts and delivery state;
- telemetry positions;
- persistence, behind a trait;
- the LXMF and propagation node machinery.

It is `no_std + alloc` and tested on the host.

**Frontends own only rendering and input:**

- `loquax`'s terminal;
- a desktop GUI in iced (added 2026-10-25, see "Decided");
- the M9's display and keyboard, ratatui through mousefood;
- an Android app over the C surface.

Nothing a frontend draws is decided inside it.

## The map

2026-10-25, Lew: the map is a product of its own as well as a view
inside the messenger: OpenStreetMap vector data, on the desktop and on
the M9, showing received telemetry positions. The device measurement
below decides whether the vector path holds or the PNG fallback is
taken, not whether the map exists. (Decided 2026-10-27, Lew: the vector
path holds, the PNG fallback is dropped; [Loquax: the map](loquax-map.md),
section 7.)

2026-10-27, Lew: the map is part of Loquax, on the desktop (the terminal
and the iced GUI) as well as on the board, because the positions arrive
over the same LXMF channel as the messages. The plan, the renderer split
and the slices are on [Loquax: the map](loquax-map.md), decided by Lew on
2026-10-27.

2026-10-25, Lew: the embedded map is vector, drawn with a minimal
polygon filler of our own on `embedded-graphics` and measured on the
device before any product code, with PNG raster tiles as the documented
fallback only (the filler is `leviculum-mapfill`, the measurement
`docs/drafts/map-measurement-2026-10.md`, "Device measurement, prepared
2026-10-25").

What the research found, in three sentences. No vector map renderer for
microcontrollers exists off the shelf, though the tile formats (PMTiles,
Mapbox Vector Tiles), a prost-based MVT decoder and `embedded-graphics`
do. The gap is polygon filling: `embedded-graphics` 0.8 draws outlines
but cannot fill an arbitrary polygon, so a scanline filler is the one
renderer piece we write. Decoding, not drawing, is the clock: on the
host the z15 view spent 4.93 ms of its 5.87 ms in decoding and clipping
(`docs/drafts/map-measurement-2026-10.md`, "Table 3").

The shape: a desktop tool cuts an OSM extract, thins layers and
simplifies geometry for each zoom level, and writes a compact tile set
to the device's flash. The device draws filled areas, polylines, its own
GNSS position, and the telemetry positions of other nodes.

*Superseded 2026-10-27 (Lew):* the fallback was raster, the measured
precedent: Meshtastic's device UI on the M9 reads PNG raster tiles from
the microSD card at `/maps/<style>/{z}/{x}/{y}.png` ([The ThinkNode
M9](thinknode-m9.md), "Storage"). That layout is not our plan; the one
format for the desktop and the board is the region file of [Loquax: the
map](loquax-map.md), section 4.

**The first step is the device measurement, not product code.** For one
city at three zoom levels: the flash the tiles need, the RAM used while
rendering one 240x320 view, and the time it takes to draw that view.
The host half is done (the draft above); the device half is prepared
there under "Device measurement, prepared 2026-10-25".

## Order of work

*Decided 2026-10-25 (Lew), with stage one inserted as step 3; until then
the reviewer's proposal of 2026-10-21, steps 1, 2 and 4 to 7 below.*

1. The community port is visible and reviewed.
2. Bring-up and radio at the rig ([The ThinkNode M9](thinknode-m9.md),
   "Bring-up plan").
3. Stage one: the M9 is a board like the others, with the same status
   screen, one-button operation on one key, Mesh mode and a propagation
   node ([The ThinkNode M9](thinknode-m9.md), "Decided"). The keyboard UI
   follows only when that is stable. Concept work for the GUI, on the
   desktop and on a 240x320 display, may run in parallel.
4. The messenger core is taken out of `loquax` and tested on the host.
5. The M9 frontend, with messages.
6. Positions on a map.
7. The map itself.

## The browser

2026-10-25, Lew: `lnomad` is the terminal frontend of a browser whose
core (fetch, micron model, layout) also drives a desktop GUI and the
M9's display. The `lxmf@` handoff (browser to messenger, [Loquax:
architecture](loquax-architecture.md)) becomes an in-process call once
both share a shell. #219's core/frontend boundary ("What exists today")
is the refactor this needs; its first step, the module boundary, landed
on 2026-10-25.

## One interaction model

2026-10-25, Lew: one interaction concept across the terminal, the
desktop GUI and the M9: the same commands, the same names in the
palette, the same delivery glyph table ([Loquax: the user
interface](loquax-ui.md), section 2); each frontend maps them to its
input. Code is shared where it is the same problem (model, update,
layout), not for its own sake (rendering, input).
lnflash is the third user of the shared widgets, after `loquax` and
`lnomad` (decided 2026-10-26, Lew, Codeberg #476), and its rule "start,
it flashes, done" is the zero config form of this model ([Flashing an
LNode](lnode-flashing.md), "Product goal").

2026-10-25, Lew: `lnomad` and `loquax` (then lnmsg) stay two programs with shared
widgets. Since 984 the shared widgets are two crates.
`leviculum-ui` is the terminal-free half (styled text, palette, the key
and mouse events with the text-editing table, the footer, help and toast
models, the spinner frames, and the delivery glyph table), so a core can
take it and stay terminal-free; `leviculum-ui-ratatui` is the ratatui half
(the widgets, the terminal lifecycle, the event mapping, inline pictures).
`lnomad` draws through them already; `loquax`'s TUI is built on them
(994). The palette's names landed with it, in `leviculum_ui::commands`,
and `leviculum-ui` builds `no_std` without its `std` feature, so the
messenger core holds the same types a board will.

## Open

- **The licence text for the map data.** OSM data is ODbL: which notice
  ships with the tiles, and where the device shows it ([Licensing and
  third-party notices](licensing-and-notices.md)).
- **Whether the M9 frontend and the terminal share a text layout
  engine.** Since 2026-10-25 the rule for answering it is "One
  interaction model" above: shared where it is the same problem.

## Not yet built

All of it: the `no_std` core crate, a firmware frontend, LXMF on the C
surface, the tile tool, the tile format, and the map view. Since
2026-10-25 also the desktop GUIs of the messenger and the browser.

## See also

- [The ThinkNode M9](thinknode-m9.md), the first device this is for.
- [Loquax: a terminal LXMF messenger](loquax.md) and [Loquax:
  architecture](loquax-architecture.md).
- [Telemetry](telemetry.md), for the positions on the map.
- [A GUI toolkit for the desktop and the M9](gui-toolkit.md), the
  toolkit candidates measured and the decision for iced and ratatui.
