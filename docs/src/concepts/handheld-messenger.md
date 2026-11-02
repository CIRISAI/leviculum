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
  talks to the backend only through `Outbox` (`loquax/src/outbox.rs:386`),
  but the crate is `std`. It depends on `leviculum-std` and tokio
  (`loquax/Cargo.toml:241`, `loquax/Cargo.toml:245`), since 2026-10-25 behind its `std` feature,
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
  - delivery (`LxmfRouter`, `leviculum-lxmf/src/router.rs:495`);
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

## The M9 frontend: the plan, measured (1343)

Lew, 2026-11-02: Loquax is the M9's UI. The board's facts are in one
place on [The ThinkNode M9](thinknode-m9.md), "The board as the UI sees
it"; the frontend renders the scene the other three targets already
render (`Scene`, `loquax/src/ui/scene.rs:960`), and this section records
what was measured for the plan, the budgets, and the slices. The host
numbers below are 64-bit figures from a counting allocator, upper bounds
for the 32-bit board (the 1337 pattern); the spike code ran under
`/home/lew/scratch/1343/` on the rig host and is not in the tree — what
is in the tree is the census test that pins the scene numbers
(`loquax-android/tests/m9_scene_census.rs`).

### The renderer, measured

Two ways to draw the chat list screen at 320x240 were built against a
Rgb565 framebuffer on the host, both fed from the scene the ugly store's
model builds: a scene-driven pixel renderer over `embedded-graphics`
(the drawing crate the firmware already uses for the T114's panel,
`leviculum-nrf/screen/Cargo.toml:10`), and ratatui through mousefood
0.5.3, the 2026-10-25 decision ([A GUI toolkit for the desktop and the
M9](gui-toolkit.md), "Decision"). Code sizes are the deltas of
`xtensa-esp32s3-none-elf` binaries at `opt-level = "s"` with fat LTO
over an esp-hal-plus-alloc baseline (`.text` 2,147 B); heap and time are
the host harness's.

| | pixel over embedded-graphics | ratatui through mousefood |
|---|---|---|
| flash: `.text` delta | +10,704 B | +57,508 B |
| flash: `.rodata` delta | +6,664 B (three ISO 8859-1 fonts) | +48,772 B (the Unicode 6x10 atlas) |
| heap to construct | none: it draws into the caller's frame | 281,536 B (mousefood's default `framebuffer` holds a second 153,600 B frame; the terminal's cell buffers and glyph state are the rest) |
| heap per draw, peak | 48 B | 7,946 B on the first draw, then none |
| one draw on the host, median of 50 | 0.089 ms | 0.090 ms (unchanged frame), 0.095 ms (selection moves) |
| rows on screen | 6 at 36 px each | 11 at two cells each, on the 53x24 grid |

Both drew the list correctly, umlauts included. Neither host time is a
board time; the bound on the board is the measured 31 ms full-frame
flush, and the map firmware's host-to-device factor of roughly twenty
puts both renderers' CPU cost in the low single-digit milliseconds —
slice 1 measures it on the board before anything is claimed.

**The decision of 2026-10-25 stands: ratatui through mousefood.** The
spike adds the price tag: about 90 KB more flash of 16 MB and about
280 KB of heap, which goes to PSRAM. What it buys is the terminal's
widget model and the scene renderer shared rather than written a second
time per node — a pixel renderer per scene node beside the cell one is
exactly the duplication the no-duplication rule forbids where one
renderer can serve. Two findings the spike settles beyond the choice:

- **The delivery glyphs need the ASCII fallback.** The mousefood `fonts`
  atlas (embedded-graphics-unicodefonts 0.2.0, the 6x10 mapping) covers
  Latin, Greek, Cyrillic and the box/geometric runs, but not U+2713,
  U+2714 or U+25F7 — the tick ladder does not render. The model already
  carries the switch (`ascii`, `loquax/src/ui.rs:688`); the M9 frontend
  sets it, which closes gui-toolkit's open glyph question without a
  custom font.
- **mousefood's own frame is redundant on the M9.** Its `framebuffer`
  feature holds a full Rgb565 frame on the heap beside the panel's; with
  the feature off, cells draw straight into the `DrawTarget`. Which of
  the two the SPI driver wants (DMA out of one PSRAM frame against
  per-cell writes) is a slice 1 measurement, not a decision here.

### Memory budgets, measured

The census test (`loquax-android/tests/m9_scene_census.rs`) builds the
ugly store's model — 20 conversations, 800 messages, the store shaped
like a year of use — and each screen's scene at the M9's Compact class,
counting every allocation. Held is what the scene keeps until the next
one replaces it; peak is the build's high-water mark above its start.
The asserted ceilings are about double these figures, so growth is
caught while it is still a review question.

| what | held | peak |
|---|---:|---:|
| the restored model (the state everything draws from) | 447,639 B | |
| chats list scene | 26,532 B | 26,816 B |
| open chat scene (40 messages, the `LOAD_WINDOW` cap is 200, `loquax/src/ui/history.rs:41`) | 111,184 B | 145,638 B |
| fresh group's chat scene | 654 B | 28,748 B |
| group member sheet | 2,233 B | 30,168 B |
| scene build time, host median | 0.034 ms (list), 0.065 ms (chat) | |
| renderer (mousefood, constructed once) | 281,536 B | |
| the panel's frame | 153,600 B | |

The map screen is not a scene: its pixels come from the map renderer,
measured on this board already — 153,600 B frame in SRAM or PSRAM,
edge slots and vertices in `.bss`, 90 to 177 ms per full draw plus the
31 ms flush (`docs/drafts/map-measurement-2026-10.md`, "Device rows,
measured 2026-10-26").

**The rule that follows.** Model plus one scene plus renderer plus two
frames is about 1.1 MB on the 64-bit host, so at most that on the
board: the UI lives in PSRAM. The 64 KiB placeholder heap
(`HEAP_SIZE`, `leviculum-esp/src/lib.rs:92`) becomes a measured
two-region allocation — SRAM for the node, the radio and the flush
path, PSRAM for the UI heap — sized in slice 1 by exactly this census
run on the board, with the firmware asserting its budget the way the
nRF heap does. What stays stage 2, by the same numbers: the browser's
full cell page (1,286,124 B for the README as cells, 1337's census)
waits for the runs layout or lives in PSRAM
([lnomad everywhere](lnomad-everywhere.md), section 2.2), and the map
stays the separate renderer it is.

### The input model

One table. The M9's keyboard feeds the same resolver the terminal uses:
a key becomes a `keys::Action` (`resolve`, `loquax/src/keys.rs:885`)
through the shared rows (`ROWS`, `loquax/src/keys.rs:216`, over
`leviculum_ui::keymap`'s scroll rows), and the action becomes a scene
intent (`Intent::from_action`, `loquax/src/ui/intent.rs:484`). Nothing
M9-specific enters the model; what the platform layer owns is the I2C
controller's scancodes to `KeyEvent`, including layers and repeat,
which the first keyboard slice measures on the device. The one-button
grammar keeps its designated key in stage one ([One
button](one-button.md), the gesture table: short status, double
announce, 2 s mode, 8 s sleep); once the messenger UI runs, those
gestures stay on that key while the other 36 drive the scene, so the
board remains operable like every other board.

### Networking: a client of a home, over LoRa

The M9 is a full node in Mesh mode (the propagation-node decision on
[The ThinkNode M9](thinknode-m9.md), "Decided") and a Loquax client of
a home. What that needs, measured against the code:

- **The wire is ready.** The five requests and their answers are
  `no_std` (`wire`, `loquax/src/home.rs:37`), as are the admission
  code's codec (`code`), the archive relation and the wake rules.
- **The client's ask loop is not.** `home::client` is gated `std`
  (`loquax/src/home.rs:39`): it waits on tokio time and drives
  `leviculum_std::ReticulumNode`'s link API
  (`loquax/src/home/client.rs:10`). The board port is the same request
  flow over `leviculum-core`'s links and the firmware's clock — a seam
  cut like the store's (`StoreWrite`, `loquax/src/ui/history.rs:112`)
  and (`StoreRead`, `loquax/src/ui/history.rs:167`): the
  retry ladder and budgets move down to `no_std`, the waiting stays
  with the platform. Rendezvous over TCP does not exist here and is not
  needed: over LoRa the client reaches the home destination through the
  mesh by path request, the way every destination is reached.
- **The home's address gets onto the board** as the admission code: 140
  typeable characters (`lqxh1…`, `loquax/src/home/code.rs`), entered on
  the 37 keys for the first pairing, or written over the USB control
  link by lnflash as a control fact beside the radio settings — the
  envelope exists, the fact does not yet. The reverse direction is the
  display: the M9 shows its own code as a QR for a phone to scan, since
  it has a screen and no camera.
- **The RAM of a client session** is bounded by the pull batch — a
  `/sync` answer carries the messages themselves — and is not measured
  yet; the pairing slice runs the census on the board with a real pull
  before the batch size is fixed.

### The test plan

Everything below runs without a human at the bench
(`feedback_reviewer_operate_rig_autonomously`).

1. **Host pixel goldens.** The scene fixtures drawn by the mousefood
   renderer into the 320x240 framebuffer, pinned as PNGs, compared in
   bytes; the positive control shifts one timestamp and must break the
   grouping, as the scene goldens do today.
2. **Census asserts.** `m9_scene_census` holds the scene budgets on the
   host now; slice 1 adds the firmware's heap-budget assert on the
   board.
3. **The framebuffer over the wire.** On the rig, a control frame asks
   the board to dump its frame as PBM over the debug CDC; periculum
   compares it against the same golden the host pinned. Key presses go
   down as control frames, so a scripted walk (open, type, send) runs
   without fingers.
4. **End-to-end cells (`needs: rig`).** A message to and from a home on
   the rig's lnsd; join a group; a position on the map. Each against
   the same lnsd the drop-in tests already drive.
5. **Boot proof as for the LNodes**: the `[FW_BUILD]` banner before any
   slice lands, the rule the bring-up plan already states.

### The slices, in order

Stage one (the M9 as a board like the others) and the rig's ESP32
flashing are prerequisites from the order of work above, not slices of
this plan. One pass is a coder batch; the estimates carry the usual
uncertainty of firmware bring-up.

| # | slice | passes | acceptance (one line) | leaves out |
|---|---|---:|---|---|
| 1 | the renderer landed as a crate: the scene cell renderer on mousefood, `no_std`, host goldens, the two-region heap sized by the census on the board | 3 | the fixture scene's PNG golden equal on host and board dump, heap census printed from the board | every live screen |
| 2 | chat list and chat live: the model on the board, store in flash behind the store seam, scenes drawn, scrolling | 3–4 | the walk reads real rows from a flash store after a reboot | writing |
| 3 | compose with keys: the keyboard slice (scancodes, layers, repeat), typed text through `Intent`, send over the mesh | 2–3 | a message typed on the board arrives at the rig lnsd, and one arrives back | pairing |
| 4 | home pairing: the `no_std` client seam, the admission code typed and flashed, the archive pulled, session census | 3 | the board joins a rig home and shows its history | groups |
| 5 | groups: join, member sheet, the group verbs the scene already carries | 2 | the board joins a group made by the phone and reads the digest | the map |
| 6 | the map, last: the map renderer behind a tab, positions from telemetry, region storage decided (SD against flash) | 3–4 | a position shared by the board shows on the phone's map and the reverse, frame time measured | routing/zoom niceties of the desktop |

## Open

- **The licence text for the map data.** OSM data is ODbL: which notice
  ships with the tiles, and where the device shows it ([Licensing and
  third-party notices](licensing-and-notices.md)).
- **Whether the M9 frontend and the terminal share a text layout
  engine.** Since 2026-10-25 the rule for answering it is "One
  interaction model" above: shared where it is the same problem.
  Answered by the plan of 1343: the M9 renders the terminal's scene
  through mousefood, so the layout is the one the terminal has.

## Not yet built

The M9 frontend: all six slices of "The slices, in order" above, and
the `no_std` client seam they need. Of the 2026-10-21 list that stood
here, the rest has landed since: the core builds `no_std`
(`just loquax-core-no-std`), the desktop window ships beside the
terminal, Android went through the JNI facade rather than the C
surface, and the map, its region format and its tooling are built
([Loquax: the map](loquax-map.md)).

## See also

- [The ThinkNode M9](thinknode-m9.md), the first device this is for.
- [Loquax: a terminal LXMF messenger](loquax.md) and [Loquax:
  architecture](loquax-architecture.md).
- [Telemetry](telemetry.md), for the positions on the map.
- [A GUI toolkit for the desktop and the M9](gui-toolkit.md), the
  toolkit candidates measured and the decision for iced and ratatui.
