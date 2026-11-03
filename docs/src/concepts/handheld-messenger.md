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
(`HEAP_SIZE`, `leviculum-esp/src/lib.rs:151`) becomes a measured
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
  `no_std` (`wire`, `loquax/src/home.rs:41`), as are the admission
  code's codec (`code`), the archive relation and the wake rules.
- **The client's ask loop is not.** `home::client` is gated `std`
  (`loquax/src/home.rs:43`): it waits on tokio time and drives
  `leviculum_std::ReticulumNode`'s link API
  (`loquax/src/home/client.rs:10`). The board port is the same request
  flow over `leviculum-core`'s links and the firmware's clock — a seam
  cut like the store's (`StoreWrite`, `loquax/src/ui/history.rs:112`)
  and (`StoreRead`, `loquax/src/ui/history.rs:167`): the
  retry ladder and budgets move down to `no_std`, the waiting stays
  with the platform. Rendezvous over TCP does not exist here and is not
  needed: over LoRa the client reaches the home destination through the
  mesh by path request, the way every destination is reached. Done on
  the host in slice five (1363, "Slice five, host half" below).
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
  before the batch size is fixed. On the host since 1363: a LoRa page
  is one packet, and its pull peaks at 392 B.

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

### Slice one done, on the host (1351)

The renderer is a crate: `loquax-m9`, `no_std` plus `alloc`, built for
`thumbv7em-none-eabihf` and the M9's `xtensa-esp32s3-none-elf` by `just
m9-render-no-std` (in `guards`, with an injected `std::fs` call as its
positive control). It words the chats list scene in cells
(`lines`, `loquax-m9/src/screen.rs:122`) and draws them through
`leviculum-ui-ratatui`'s styled-line bridge, which builds `no_std`
without that crate's new default `terminal` feature, into ratatui over
mousefood (`draw`, `loquax-m9/src/render.rs:45`). Mousefood's own
`framebuffer` is off: the cells go straight into the caller's frame, and
the crate's `framebuffer` feature turns it back on for the board's DMA
measurement. What the slice settled on the host:

- **The goldens.** The ugly store's Compact list drawn into the 320x240
  frame, pinned as `loquax-m9/tests/golden/m9-chats.png` and, with the
  cursor on the fourteenth row so the list scrolls,
  `m9-chats-cursor.png`, compared in bytes. The positive control moves one
  row's time two days back: the frame changes, and only in that row's 20
  pixel rows. A third test redraws with one renderer and must equal a
  fresh draw; it was red on the first run. ratatui never sends a wide
  character's second cell, the atlas has no glyph wider than one, so an
  emoji left the old pixels beside it. The screen now draws a wide
  character as `?` and drops zero-width ones before it measures a line
  (`narrow`, `loquax-m9/src/screen.rs:365`).
- **The tick ladder.** The list's mark stands on the shared glyph table's
  ASCII rungs (`. . v vv VV !` for sending, unconfirmed, mailbox,
  delivered, read, failed; `state_of`, `loquax-m9/src/screen.rs:310`).
- **The key table.** Up, down, open and back are the navigation keys and
  Enter, sent as the terminal's `Up`, `Down`, `Enter` and `Esc`
  (`LIST_ROWS`, `loquax-m9/src/keys.rs:52`); a test resolves each through
  `keys::resolve` and `Intent::from_action` to `Scroll(LineUp)`,
  `Scroll(LineDown)`, `Focus(Messages)` and `Back`. Which scancode is
  which key stays the keyboard slice's.
- **The census** (`m9_scene_census`, 64-bit host, debug build) now counts
  the renderer and the stack, read off a painted stack whose positive
  control is a 64 KiB frame:

| what | held | peak | stack |
|---|---:|---:|---:|
| chats list scene | 26,532 B | 26,816 B | 49,672 B |
| open chat scene | 111,184 B | 145,638 B | 67,384 B |
| the panel's frame | 153,600 B | | |
| renderer made (no mousefood frame) | 107,584 B | 107,584 B | 15,744 B |
| chats list, first draw | 0 B | 86,840 B | 6,528 B |
| chats list, cursor moved | 0 B | 86,816 B | 6,656 B |

  A draw holds nothing; its peak is the screen's styled lines and their
  ratatui text, built per draw, which 1343's spike did not have (7,946 B).
- **Flash on xtensa**, `opt-level = "s"`, fat LTO, one codegen unit, as
  1343 measured: an esp-hal binary that builds the model's list scene,
  against the same binary that also draws it with this crate. The
  renderer adds `.text` +60,028 B and `.rodata` +39,296 B (the plan:
  +57,508 B and +48,772 B), `.data` +52 B. The scene the core builds is
  the bigger part: `.text` +215,968 B and `.rodata` +46,212 B over the
  esp-hal baseline (the probe ran under `/home/lew/scratch/1351/` on the
  rig host and is not in the tree).

Not yet done of slice one: the board's half, which needs the board code
this pass left out. The frame dumped from the board equal to the golden,
the two-region heap and the census printed from the board.

### Slice two done, on the host (1356)

The open chat and the compose line, drawn and typed into on the host. Of
the table above it takes the host halves of slices 2 and 3: the chat
drawn from the scene, and typed text through the model's keys. The board
halves stay: the store in flash, the scancodes, the send over the mesh.

- **The chat in cells** (`lines`, `loquax-m9/src/chat.rs:40`). The
  scene's chat set as the terminal sets a conversation: a bubble wrapped
  to three quarters of the 53 cells (`block_cap`,
  `loquax/src/ui/layout.rs:337`), tinted with a margin cell either side,
  ours flush right, theirs flush left, the time and the mark's ASCII rung
  closing its last row, a blank row between two groups. The day line and
  a group's note line (who joined or left) are the terminal's own
  (`date_line`, `loquax/src/ui/layout.rs:307`, and `note_line`,
  `loquax/src/ui/layout.rs:291`, public since this slice). A group's chat
  names the writer above a group of bubbles, which is how a digest's
  lines read (each a bubble under its writer, the model's work), and a
  line that came after newer ones says "arrived late" under it. Quotes,
  photos, files and voice messages are words (`> …`, `[#] Photo, …`,
  `[f]`, `[v]`); reactions are not drawn yet. Every row goes through
  `narrow` first, the store's emoji and Japanese included.
- **Only the screen is laid out.** The bubbles go from the bottom up,
  from the newest or, when the cursor stands above the last screen, from
  the cursor's bubble (`bubbles`, `loquax-m9/src/chat.rs:104`). The window
  is a function of the scene and the cursor alone; the redraw test moves
  the cursor nine bubbles, opens the line and types an emoji with one
  renderer, and must equal a fresh draw.
- **What the M9 shows of a model** (`scene`,
  `loquax-m9/src/screen.rs:105`, and `Look::of`,
  `loquax-m9/src/screen.rs:83`). The Compact scene, which is the open chat
  alone, with the cursor of the pane the keys are in; and the list while
  the keys are on the list. The model's back from the chat moves the keys
  to the list and keeps the chat open, as the terminal's panes do, so a
  screen of one pane shows the pane the keys are in. That builds the Wide
  scene and drops its chat: the list's peak then carries the chat's.
- **The compose line** (`compose_line`, `loquax-m9/src/chat.rs:382`): the
  hint while empty and not typed into, the text scrolled so the caret
  shows and the caret's cell reversed while it is. The field is
  `loquax-m9`'s own (`Field`, `loquax-m9/src/field.rs:20`): the terminal's
  `tui_input` and its edit table are `std`, the phone's field edits
  nothing by key. It inserts at the caret, erases with Backspace and
  Delete, moves with Left, Right, Home and End; the Emacs chords want a
  Control and an Alt the keyboard has not.
- **The key table, the chat's rows** (`CHAT_ROWS`,
  `loquax-m9/src/keys.rs:78`), each resolved in a test through
  `keys::resolve` and `Intent::from_action`. The M9 binds nothing of its
  own; a letter is the terminal's letter.

| verb | M9 key | pane | resolves to |
|---|---|---|---|
| up / down | Up / Down | chat | `Scroll(LineUp)` / `Scroll(LineDown)` |
| write | `i` | chat | `Focus(Compose)` |
| type | any character | compose | nothing: the field's edit |
| erase | Backspace | compose | nothing: the field's edit |
| caret left / right | Left / Right | compose | `Edit` |
| send | Enter | compose | `Send { to: Open }` |
| cancel | Back | compose | `Back`: the keys to the chat, the text kept as its draft |
| back | Back | chat | `Back`: the keys to the list |

- **The goldens**: `m9-chat.png` (Enter on the pinned conversation of the
  ugly store), `m9-chat-cursor.png` (twelve bubbles up, the window
  moved), `m9-compose.png` (a line typed with an umlaut, the caret two
  left), `m9-sent.png` (Enter sent it: the field empty, the message on
  its sending rung) and `m9-group.png` (a digest of two writers, a line of
  each after it, one late, a member leaving, ours). The group's positive
  control takes the late mark and the names away; each changes the frame.
- **The census** (`m9_scene_census`, 64-bit host, debug build), the open
  chat as the M9 builds and draws it:

| what | held | peak | stack |
|---|---:|---:|---:|
| open chat scene (M9) | 111,184 B | 145,638 B | 81,560 B |
| the list with a chat open (M9) | 26,532 B | 145,638 B | |
| open chat, first draw | 0 B | 81,056 B | 11,288 B |
| open chat, cursor moved | 0 B | 80,976 B | 11,312 B |
| open chat, a line typed | 0 B | 80,456 B | 11,288 B |

  What the M9 may spend on the open chat, from these upper bounds: the
  model (447,639 B), the renderer (107,584 B) and the panel's frame
  (153,600 B) stay. On top, the worst moment is building the next scene
  while the last one is still held, 111,184 B plus a 145,638 B peak; a
  draw over a held scene is less, 111,184 B plus 81,056 B. That is
  965,645 B on the 64-bit host, under the 1.1 MB the rule above puts in
  the PSRAM it gives the UI. The ceilings asserted are about double.

### Slice three done, on the host (1359)

The keyboard as a driver and the pairing's flow, both `no_std`, both
host-tested. Of the table above it takes the host halves of the keyboard
(slice 3) and of the pairing (slice 4); the board halves stay with the
board: the controller's I2C reads, the send over the mesh, the flow over
`leviculum-core`'s links. Nothing was flashed or read off the board in
this slice.

- **What the board has told us of the keyboard, and what not.** The
  controller ACKs at 0x6D on our V1.1 unit, the wiki says it reads a 5 x
  10 matrix, its INT is a 15 ms pulse that does not come for every key
  ([The ThinkNode M9](thinknode-m9.md), "Keyboard", "Two hardware
  revisions" and "The keypad"). Its register map, which cell is which
  key, and whether it debounces, repeats or holds a layer itself are
  unread, and no foreign header stands in for them. So the driver starts
  at a scan of the matrix and the board's half is a decoder from the
  controller's answer to that scan, read in slice four.
- **The scan frame** (`Scan`, `loquax-m9/src/keyboard.rs:63`): 50 bits,
  bit `row * 10 + column`, carried as 7 bytes least significant first
  (`Scan::from_frame`, `loquax-m9/src/keyboard.rs:83`); the six bits past
  the matrix are dropped. The firmware prints one line a poll over the
  debug CDC, `KB <ms> <14 hex digits>`, and `parse_recording`
  (`loquax-m9/src/keyboard.rs:310`) reads a capture as it was taken, the
  firmware's other lines skipped. An example from the walk below:
  `KB 190 00800000000000` is cell (1, 5) alone, byte 1 bit 7, at 190 ms. The board's layout table and its
  first real fixture come from one such capture with each key pressed
  once.
- **The driver** (`Keyboard::poll`, `loquax-m9/src/keyboard.rs:207`):
  polled every 10 ms while the panel is lit (`POLL_MS`, under the 15 ms
  INT pulse; whether the polls see every press is a board measurement),
  a cell changes state after two alike polls, each cell on its own, so a
  second key down while the first is held is seen. A `Cap`
  (`loquax-m9/src/keyboard.rs:108`) carries a key's plain, shifted and
  symbol character; Shift and Sym held with a key apply to it, tapped
  alone they apply to the next key only, so one thumb types a capital or
  a `!`. Characters, Backspace and the arrows repeat after 500 ms every
  80 ms; Enter and Back never do. What comes out is the key events the
  table of slices one and two resolves; the M9 still binds nothing of its
  own.
- **The walk**: `tests/keyboard/walk.txt` under `loquax-m9`, a recording
  in that format over a test layout, synthetic until the board records
  its own; through the driver, the key table and the model it opens the
  pinned chat, types "Hi!" with a tapped Shift, a one-poll bounce and a
  tapped Sym, and sends it (`a_recorded_walk_of_scan_frames_types_and_sends`).

**The pairing: decided, typed on the keys first, written by lnflash
second.** The page's three options weighed against its own facts: the
board cannot scan, so a code the board shows is for a phone to read, and
the phone has no way back to the board but the mesh, a delivery protocol
that does not exist; that direction stays the board's own contact code,
not the pairing. Typing the code needs nothing but the board and the
phone's "Link a device" screen, which already shows the code as text,
so it is the path an owner without a computer has; `lnflash --home
<code>` is the same code over the USB control link for whoever has the
board on a cable, and needs a control fact the envelope does not carry
yet. Both end in the same join.

- **The typed code** (`typing`, `loquax/src/home/code.rs:205`): 140
  characters (`TYPED_LEN`, `loquax/src/home/code.rs:186`), whitespace and
  case ignored as every parser of the code ignores them. A stray
  character is named where it stands, a wrong prefix letter, one outside
  base32, one past the end, not after the last of 140; the complete code
  goes to the same parser and checksum as on every device. `grouped`
  (`loquax/src/home/code.rs:226`) shows it in fives, so the eye keeps its
  place against the code it copies.
- **The join without the waiting** (`Join` then, `Ask` since 1363,
  `loquax/src/home/ask.rs:278`): the seam this page asked for, cut as the
  store's. The path ladder (`PathLadder`, `loquax/src/home/ask.rs:68`;
  requests at 0, 7, 21, 49 and 105 s, then a minute apart), the per-hop
  link budget of rule 3 (`establishment_ms`,
  `loquax/src/home/ask.rs:122`) and what a `/join` answer means
  (`join_outcome`, `loquax/src/home/ask.rs:128`) are `no_std` and shared:
  the desktop's `open` takes the ladder and the budget from there, and
  `HomeError` lives there. The join is a set of `Step`s
  (`loquax/src/home/ask.rs:234`), request a path, connect, identify, send
  `/join`, done, which the platform performs and reports back; a report
  out of turn ends the join instead of being read as the home's word.

What of `home::client` was not `no_std` then, all of it behind
`#[cfg(feature = "std")]` (`loquax/src/home.rs:43`); slice five's host
half (1363, below) moved the rows up to the group verbs:

| piece | where | what it needs on the board |
|---|---|---|
| the link loop: path, connect, identify | `open`, `loquax/src/home/client.rs:590` | nothing more: `Join` is its board form; other requests get the same steps |
| one request and its answer | `request`, `loquax/src/home/client.rs:655` | the firmware's request over a core link, reported to `Join::answered` |
| the join | `join`, `loquax/src/home/client.rs:35` | none: `Join` replaces it |
| the pull and its pages | `pull`, `loquax/src/home/client.rs:464`; `pages`, gone in 1363 | a `Pull` flow like `Join` with the page loop, and the batch size the session census fixes |
| send, mark and the group verbs | `send`, `loquax/src/home/client.rs:150`; `mark`, `loquax/src/home/client.rs:208` | the same steps with their own request; slice five for the groups |
| the client id | `client_id`, `loquax/src/home/client.rs:455` | a random source the firmware has |
| being a client: the home's key, the cursor | `load_client`, `loquax/src/home/client.rs:687`; `save_client`, `loquax/src/home/client.rs:712`; `load_cursor`, `loquax/src/home/client.rs:718` | a record in flash beside the store, the device identity with it |

What moves to make the admit flow run on the board, in order: the
pairing screen in `loquax-m9`, the typed code drawn grouped with its
`Typing` verdict; the firmware's driver of `Join` over `leviculum-core`'s
path requests and links; the home's key and the device identity written
to flash on `Done(Ok)`; then `lnflash --home`, a control fact in the
envelope and its handler, which ends in the same record.

### Slice four, host half (1361)

The first image that boots the renderer on the M9, built and proven on
the host; nothing was flashed in the pass, the board half is the
reviewer's rig window. It is M9-only by nature: the panel, the keyboard
controller and the frame dump exist on no other target, and what it
draws is the scene the terminal, the window and the phone already draw.

- **The image** is a second binary of the ESP32 class crate,
  `thinknode_m9` (`leviculum-esp/src/bin/thinknode_m9.rs`), for the M9's
  ESP32-S3 (`xtensa-esp32s3-none-elf`), with a board file of its own
  (`leviculum-esp/src/boards/thinknode_m9.rs`) carrying the nets from
  [The ThinkNode M9](thinknode-m9.md) and the panel's MADCTL 0xB0. The
  heap is the 8 MB PSRAM (`init_heap_at`, `leviculum-esp/src/lib.rs:173`),
  the stack the 312,440 B of internal RAM the linker leaves; the class
  allocator now counts what is live and its peak for the census. Nothing
  goes on the air: the LR1110's chip select is parked high.
- **The panel:** ST7789 over SPI2 at 40 MHz with DMA, landscape. A draw
  goes through mousefood into `loquax-m9`'s frame, which keeps the band
  of rows it touched (`take_dirty`, `loquax-m9/src/frame.rs:73`), and the
  board sends only that band through the row window (`flush`,
  `leviculum-esp/src/bin/thinknode_m9.rs:153`): the full frame's 30.8 ms
  is the first draw's cost, a moved cursor sends two entries. The
  redraw test drives one renderer through four keys and must equal a
  fresh draw, its band flushed into a second frame
  (`the_boards_redraw_and_its_band_equal_a_fresh_draw`).
- **The model:** a fixture built in code, `no_std`, 13 conversations with
  umlauts, Greek and Cyrillic names, a pinned one of 16 messages over every
  rung of the ladder, the clock standing at 2026-11-03 12:00 CET (`model`,
  `loquax-m9/src/fixture.rs:239`). The ugly store of the earlier goldens
  is a store on disk with signed messages, which the board has neither of
  yet, so the board's two screens have goldens of their own,
  `m9-fw-chats.png` and `m9-fw-chat.png`, walked to with the board's keys
  (`Screen::model`, `loquax-m9/src/fixture.rs:513`).
- **The keyboard:** the controller's answer is read as the community port
  reads it, register 0x01 of 0x6D, eight bytes a poll, and decoded by a
  table (`PROVISIONAL`, `loquax-m9/src/kbraw.rs:71`): a bitmap table or a
  code table, either mapping onto the 5 x 10 matrix. Until a capture
  reads the controller the board takes the answer as the matrix bit for
  bit and types with slice three's test layout. Every answer goes out raw
  before it is decoded, `KBRAW <ms> <hex>`, then the scan as slice three's
  recording line, `KB <ms> <14 hex>`, one pair a 10 ms poll; `parse_raw`
  (`loquax-m9/src/kbraw.rs:135`) replays a capture through any other table
  on the host, so the right table is found without a flash per guess.
- **The dump:** `FRAME_DUMP`, envelope type 0x18 (`TYPE_FRAME_DUMP`,
  `leviculum-core/src/envelope.rs:404`), registered as 0x17 was in 1301.
  Empty, it dumps what is shown; one byte walks a fresh fixture model to a
  screen first, 0 the chats list, 1 the pinned chat. The board acks it,
  then writes the frame as text (`encode`, `loquax-m9/src/dump.rs:64`):

  ```text
  PBM 320 240
  PBR <y> <x> <run> <run> ...     a run is rrrr or rrrr*n, RGB565 in hex
  PBE <fnv32>                     FNV-1a over the pixels, little end first
  ```

  Lines stay under 120 B, plain ASCII, so the dump shares the one UART
  with the log and the frames. The board's chats list is 65,521 B of dump
  and its chat 41,520 B, 5.7 s and 3.6 s at 115200 baud. The same encoder
  and its decoder (`decode`, `loquax-m9/src/dump.rs:139`) now sit in every
  golden comparison (`loquax-m9/tests/support/mod.rs`): the seven goldens
  of 1351 and 1356 and the board's two are each compared after a trip
  through the dump, byte for byte.
- **The census** (`measure`, `loquax-m9/src/census.rs:49`), one routine
  for both: build the screen's scene, then draw it, each step's heap held,
  heap peak and stack below the probe. The budgets are this page's
  ceilings (`CHATS`, `loquax-m9/src/census.rs:118`; `CHAT`,
  `loquax-m9/src/census.rs:129`). The board prints a `[CENSUS]` line per
  screen with its verdict; the host runs the same over the same model
  (`loquax-android/tests/m9_fixture_census.rs`, 64-bit, debug build):

| screen | scene held | scene peak | scene stack | draw peak | draw stack | verdict |
|---|---:|---:|---:|---:|---:|---|
| chats list | 19,551 B | 56,327 B | 66,920 B | 86,184 B | 6,624 B | ok |
| open chat | 36,745 B | 56,327 B | 66,920 B | 80,976 B | 11,056 B | ok |
| renderer made | 107,584 B held | | | | | ok (under 220,000 B) |

- **The image's size**, `just build-m9` (`scripts/build-m9-image.sh`):
  927,520 B of a 16,384,000 B application partition, printed as the nRF
  budget prints its own, `FLASH_HEADROOM board=thinknode_m9 used=927520
  free=15456480`, and `STACK_HEADROOM board=thinknode_m9 stack=312440
  need=184000`, red below 8 KiB free or a stack under the open chat's
  ceilings. `just flash-m9 <tty>` writes it with espflash at 230400 baud.

The boot lines, in order: `[FW_BUILD]`, `[BOARD] id=M9 ... heap=<psram>`,
`[HEAP] region=psram`, `[STACK] bytes=`, `[DISP] ST7789 320x240
madctl=0xb0 spi_mhz=40 state=up`, `[KB] addr=0x6d found=...`, `[CENSUS]`
for the renderer and for each screen, `[DISP] flush rows=0..239`, then
`KBRAW` and `KB` every 10 ms and the banner pair every 5 s. Not yet done
of slice four: everything the board says. Whether the frame on the panel
reads the right way round, whether the dump equals the golden, what the
board's census measures, and what the controller's answer is.

### Slice five, host half (1363)

The board as a Loquax client of its home over LoRa, built and measured
on the host. The board's half (its executor over `leviculum-core`'s
links, the record in flash, the census line at boot) waits for the
image of slice four to run.

**The client moved down.** `home::ask` is now the whole ask, not only
the join: `Ask` (`loquax/src/home/ask.rs:278`) over a `Verb`
(`loquax/src/home/ask.rs:155`), `/join`, `/sync` page by page, `/send`,
`/mark` and `/group`, with an `Outcome`
(`loquax/src/home/ask.rs:203`). The platform performs the `Step`s
(`loquax/src/home/ask.rs:234`) as before; two are new. `Page` hands out
one page of a pull and the ask keeps none of it, so a pull's memory is
one page however long the archive. `Close` says an answer was lost: the
same request goes again on a fresh link, at most `ATTEMPTS`
(`loquax/src/home/ask.rs:151`) times, for every verb but `/join`, whose
token is single-use. The host's executor is `client::run_ask`
(`loquax/src/home/client.rs:59`), over `leviculum_std`; the desktop's
and the phone's `join`, `pull` and `mark` now run the same ask through
it, so the page loop exists once.

| piece | before 1363 | now |
|---|---|---|
| the link loop: path, connect, identify | `open`, std | `Ask` for the five verbs; `open` stays for the desktop's other requests |
| one request and its answer | `request`, std | `Step::Request` and `Ask::answered`; `request` is the host executor's |
| the join | `Join` | `Ask::join`, `Verb::Join` |
| the pull and its pages | `pull`, `pages`, std | `Verb::Pull`, `Step::Page`; `pull` runs it, `pages` is gone |
| send, mark, group | std | `Verb::Send`, `Verb::Mark`, `Verb::Group`; `mark` runs the ask, `send` keeps its own loop, because `send_retrying` already asks three times and would ask nine |
| the client id, the home's key, the cursor | std | still std: the firmware's random source and its flash record are the board half |
| the old-home probe of `/group` | std | std only: a home before 1307 drops `/group`, and the board's homes are newer |

`just m9-render-no-std` now checks the client with the renderer: its
positive control puts a `std::fs` call into `loquax/src/home/ask.rs` as
well as into the renderer, and both must be refused for thumbv7em.

**The page fits a packet.** A link on LoRa keeps the default MTU, 500
bytes, and its MDU is 431 (`LORA_LINK_MDU`,
`loquax/src/home/ask.rs:139`). A response longer than that goes as a
resource (`leviculum-core/src/node/mod.rs:1808`), which is an
advertisement, its parts and a proof. The home's own page bound is
64 KiB of messages (`SYNC_PAGE_BYTES`, `loquax/src/home.rs:285`). So
the client asks smaller pages: `/sync [cursor, page]`, a second element
that a home from before reads past. The board asks `LORA_PAGE_BYTES`
(`loquax/src/home/ask.rs:146`), 412, the longest answer that still fits
one response packet once the response frame (1 + 18 bytes) is around
it. The home cuts the page to that, keeping at least one entry, and
says `more` (`fit_page`, `loquax/src/home/wire.rs:298`; the server's
`sync`, `loquax/src/home/server.rs:1029`). A message larger than a page
still comes alone, as a resource.

**The bytes, measured** (`loquax-android/tests/m9_client.rs`). The
board's node, a mailbox on a node of its own, and a transport node
between them that carries the device link, as on the mesh. A proxy on
the board's link reads every Reticulum packet both ways. A LoRa
interface sends each packet as one or two frames of its own, so these
lengths are what the air carries. The board's packets are `HEADER_2`,
since the home is two hops away. The announces both nodes send are left
out, because they are the nodes' cost and not the client's.

| ask | packets | up | down | the packets, bytes |
|---|---:|---:|---:|---|
| join | 8 | 693 B | 217 B | path request 67; link request 102, link proof 118 (down), RTT 83, identify 211; `/join` 131, answer 99 (down); close 99 |
| watch, kept | 6 | 495 B | 217 B | link 102, 118, 83, 211; `/watch` 99, answer 99 |
| send, 12 characters | 7 | 658 B | 249 B | link 102, 118, 83, 211; `/send` 163, answer 131; close 99 |
| wake | 2 | 115 B | 83 B | channel message 83 (down), its proof 115 |
| pull one message | 7 | 610 B | 345 B | link 102, 118, 83, 211; `/sync` 115, the page 227 (down); close 99 |
| mark one read | 7 | 642 B | 217 B | link 102, 118, 83, 211; `/mark` 147, answer 99; close 99 |
| keepalive | 2 | 20 B | 20 B | flags, hops, link id, context, one byte (`build_keepalive_packet`, `leviculum-core/src/link/mod.rs:2686`), every 230 s |

Five entries pulled in LoRa pages came as two pages, answers of 499 and
467 bytes, each one packet of at most two LoRa frames. The control, the
same five in the home's own page, is one 867-byte answer: over TCP's
larger link MTU that is one packet, but on LoRa it would be a resource.

**The airtime**, at our defaults (BW 125 kHz, SF8, CR 4/5, 18-symbol
preamble), from those lengths through `packet_airtime_ms`
(`leviculum-core/src/rnode.rs:1702`), as [Announce
cadence](announce-cadence.md) reckons its announces. Every entry the
archive gains wakes the watching link, the board's own entries too, and
every wake past the cursor is a pull. So a message in costs a wake, a
pull, the mark read and the mark's wake. A message out costs the send,
its wake and the pull of its own entry.

| per hour | airtime | of the hour |
|---|---:|---:|
| idle client: 15.65 keepalive exchanges of 248 ms | 3,882 ms | 0.108 % |
| one message in | 7,025 ms | |
| one message out | 6,490 ms | |
| idle and one message in | 10,907 ms | 0.303 % |
| idle, one in and one out | 17,397 ms | 0.483 % |
| the board's own share of that, what its duty budget is charged | 11,020 ms | 0.306 % |

A link of its own costs 1,951 ms of every ask: the link request, its
proof, the RTT, the identify and the close. That is two thirds of a
one-message pull (2,977 ms). The watching link is up anyway, and asking
over it would save that time on every pull, send and mark. The watch
does not do that yet (`watch.rs`: the pull "is the `/sync` it always
was, on a link of its own"); it is the first lever, before any change
to the page.

**Pairing, from the keys to `HOME admitted`.** The same test types the
code the phone shows on the board's keys. The scan frames go through
the driver (`Keyboard`, debounce two polls of 10 ms) on the provisional
layout, with digits on a tapped Sym. The key events go into the pairing
screen (`Pairing`, `loquax-m9/src/pair.rs:31`). After 20 characters, a
`9` (Sym on `o`, not base32) reads "character 21 cannot stand there".
Backspace takes it back, the remaining 120 characters follow, and Enter
gives the `HomeCode` the phone showed. The join (`Pairing::join`,
`loquax-m9/src/pair.rs:64`) then runs over the mesh. The home logs
`HOME admitted`, and its store lists the board's identity under the
name its owner gave the code. The board's own `DEVICE_NAME`, "M9", is
what it sends, but the home names a device by its code. The screen
shows the code in fives, wrapped at a group's end to the 53 cells
(`Pairing::lines`, `loquax-m9/src/pair.rs:70`), and one verdict line.

**The session's heap.** `census::session`
(`loquax-m9/src/census.rs:190`) is a routine the board can run as it
runs the screen census. It drives a pull of fed answers with the
platform's probe. The host runs it in `m9_fixture_census` over thirty
LoRa pages of one 200-byte message each: `session=pull held=0 peak=392
verdict=ok`. Between pages nothing is held, and a page costs what it
carries. The ceilings are about double that (`SESSION_PEAK`, 1,000 B,
`loquax-m9/src/census.rs:181`; `SESSION_HELD`, 256 B). The positive
control is the same archive in one page of the home's own bound: a
63,225-byte answer, peak 84,576 B, refused. The page bound that keeps
LoRa answers to one packet also keeps a pull's RAM to one page. On the
board that is SRAM beside the node and not the UI's PSRAM.

All targets:

- **M9:** the ask, the pairing screen and the session census. The
  board's executor and its flash record are still to come.
- **Desktop (TUI and window):** `join`, `pull` and `mark` run the same
  ask through `run_ask`, in the home's own pages over TCP.
- **Android:** the same `client`, through its runner (the facade cells
  pass unchanged).
- **The home:** it answers the page bound. A client from before asks
  none and gets the 64 KiB page as before.

### Slice six, host half (1366)

The group screens on the M9, drawn from the scene the phone and the
window draw, and the group verbs from the board's keys to a real home.
Of the slice table above this is row 5, groups; the pass numbering went
on past the table because slices one to four each took a pass for their
host half. Nothing was flashed: the board half is the rig's.

**The keys.** The scene carries every group node already
(`loquax/src/ui/group_scene.rs`): the member sheet with standing and
verbs, the yes/no question, "New group" with its friends, ticks and
name, the invitation card with its Join. A phone taps a node and runs
its `Action`. The terminal reaches these verbs through its palette only:
the rows from `new group` (`loquax/src/keys.rs:835`) to `start again`
(`loquax/src/keys.rs:867`) have no chord. The M9 has no palette. So its
group keys run the node's `Action` the way a tap does (`hit`,
`loquax-m9/src/group.rs:136`), and nothing new enters the model. The
cursor over the sheet's and the picker's rows is the M9's own, as a
finger is the phone's: a row index the board keeps beside the model
(`Look::row`, `loquax-m9/src/screen.rs:65`), back to the first row
whenever another screen opens, kept when the sheet's question is
answered (`press`, `loquax-m9/src/group.rs:248`). The board's key loop
now goes through `press`; a key no group screen takes goes to the model
as the terminal's key, as before.

| verb | M9 key | where | runs |
|---|---|---|---|
| new group | `n` | list | `Group(New)` |
| members | `m` | a group's chat | the group bar's action, `Group(Members)` (the terminal's `m` is the list's map; a phone's group bar opens members instead of the map) |
| join | Enter | a chat with an invitation | the card's Join, else the Join on the bubble under the cursor |
| up / down | Up / Down | sheet, friends | the M9's cursor |
| leave | `l` | sheet | `sheet.leave` |
| add members | `a` | sheet, admin | `sheet.add` |
| remove | `r` | sheet, admin | the cursor's member's `remove` |
| delete group | `d` | sheet, admin | `sheet.delete` |
| start again | `s` | sheet of a left or ended group | `sheet.again` |
| yes / no | `y` or Enter / `n` or Back | the question | `ask.yes` / `ask.no` |
| tick | Space | friends | the cursor's friend's `toggle` |
| next / add | Enter | friends | `next` (disabled until a friend is ticked) |
| type the name | any character, Backspace | name | `Group(Name(…))` with the field's text |
| create | Enter | name | `create` (disabled until a name is typed) |
| close | Back | sheet, friends, name | `close` |

The table is `ROWS` (`loquax-m9/src/group.rs:96`). A test takes each row
to the screen of its place, presses its key and compares what `hit`
returns with the action the scene's node offers
(`every_group_key_runs_the_action_its_node_offers`). Its control: `m` and
Enter on a peer's chat are not the group's.

**The screens** (`sheet_lines`, `loquax-m9/src/group.rs:401`;
`pick_lines`, `loquax-m9/src/group.rs:504`; `name_lines`,
`loquax-m9/src/group.rs:562`), each the whole 53 x 24 grid. The sheet:
the group's name and how many are in it in the bar, a row a member with
its standing ("admin", "joined", "left", "invited, delivering"), the
reader as "You" in bold, the cursor's row behind the selection bar, then
the reader's verbs with their keys (`[l] Leave group`, and the admin's
`[a]`, `[r] Remove <the cursor's member>`, `[d]`), or the question with
`[y] Yes   [n] No`, and `[Back] Close`. The friends: `[x]` or `[ ]`, the
avatar, the name, "2 selected" in the bar, `[Space] tick  [Enter] Next`
at the bottom, muted while disabled. The name: whom it is for, the
field with its caret, the trust sentence, `[Enter] Create  [Back]
Close`. The card in the invited group's chat adds the trust sentence and
`[Enter] Join` under the invitation (`loquax-m9/src/chat.rs:59`), and an
invitation's bubble in the inviter's chat says `[Enter] Join` too. A
group's row in the list has its member count as the avatar from the
scene; a group with no line yet says "Invitation" or "3 members" where
the last line would be. Every word goes through `narrow`.

**The fixture's groups** (`group_model`, `loquax-m9/src/fixture.rs:359`):
Jürgen's "Familie", the reader in it and Ünal gone; the reader's own
"Kletterhalle", Ivan still invited; Jürgen's "Chor", the reader invited.
`FRAME_DUMP` names nine screens now, one byte each in `SCREENS`
(`loquax-m9/src/fixture.rs:398`), walked with the board's keys through
`press` (`Screen::walk`, `loquax-m9/src/fixture.rs:503`):

| byte | screen | the walk | dump |
|---:|---|---|---:|
| 2 | `groups`: the list, Kletterhalle and Familie on top | none | 65,923 B |
| 3 | `invite`: Chor's card with its Join | Down x15, Enter | 17,067 B |
| 4 | `members`: Familie's sheet, a member's | Down, Down, Enter, `m` | 13,553 B |
| 5 | `admin`: Kletterhalle's, the cursor on Zoë | Down, Enter, `m`, Down | 20,755 B |
| 6 | `ask`: the Delete's question | as 5, then `d` | 18,722 B |
| 7 | `pick`: two friends ticked, the cursor on the third | `n`, Space, Down, Space, Down, Down, Space, Space, Up | 31,647 B |
| 8 | `name`: "Hütte" typed, one Backspace on the way | `n`, Space, Down, Space, Enter, `Hütx`, Backspace, `te` | 17,697 B |

**The goldens**, `m9-fw-<name>.png` under `loquax-m9/tests/golden/`,
each compared after a trip through the dump
(`the_boards_screens_through_the_dump_are_the_golden_frames`). Each was
looked at as a picture: the sheet's bar and selection, the question in
the unread colour, the ticks, the caret after "Hütte", the card's three
lines over the compose line. The positive control: the last key of every
walk changes its frame, and the groups list differs from the board's
first list (`the_last_key_of_every_group_walk_is_in_its_frame`). The
redraw rule holds over the group screens: one renderer walking into New
group and out, into an admin's sheet, its question and back, equals a
fresh draw after every key (`a_group_redraw_is_the_frame_a_fresh_draw_gives`).

**The census** (`loquax-android/tests/m9_fixture_census.rs`, 64-bit
host, debug build), every screen of `SCREENS` with the M9's cursor, held
to `Screen::budget` (`loquax-m9/src/fixture.rs:422`). The group's
screens have a budget of their own (`SHEET`,
`loquax-m9/src/census.rs:143`), about double the largest of them; the
board prints the same lines at boot:

| screen | scene held | scene peak | scene stack | draw peak | draw stack | budget |
|---|---:|---:|---:|---:|---:|---|
| groups | 23,949 B | 60,725 B | 66,920 B | 86,008 B | 6,688 B | `CHATS` |
| invite | 856 B | 24,818 B | 58,464 B | 76,632 B | 7,768 B | `CHAT` |
| members | 5,108 B | 29,057 B | 65,160 B | 76,440 B | 8,600 B | `SHEET` |
| admin | 5,155 B | 29,104 B | 65,160 B | 77,672 B | 8,600 B | `SHEET` |
| ask | 5,191 B | 29,140 B | 65,160 B | 77,064 B | 8,600 B | `SHEET` |
| pick | 28,453 B | 65,337 B | 66,920 B | 79,648 B | 5,768 B | `SHEET` |
| name | 24,703 B | 61,358 B | 66,920 B | 76,048 B | 7,176 B | `SHEET` |

The sheet and the card build on the open chat, whose scene is the chat
alone; the friends and the name keep the list under them, which is why
their scenes hold more. No group screen comes near the open chat's
ceilings, so the image's stack need stays the open chat's. The image
grows by 17,248 B to 944,768 B (`just build-m9`).

**The verbs, asked of a home.** `Verb::Group` of the ask
(`GroupVerb`, `loquax/src/home/ask.rs:182`) now carries all five a
board needs: `Create` (`/group`), `Add` (`/groupadd`), `Delete`
(`/groupdelete`, its `Ok` the group it deleted), and `State`, the
member's Join, Leave or the admin's Remove, which the model makes a state
message to the group (`group_state`, `loquax/src/ui/groups.rs:1349`) and
the home sends as any message (`/send`). `ask_of`
(`loquax-m9/src/group.rs:292`) turns the model's effect into that verb,
with the board's fresh client id; a plain message is no group's ask.
The harness test (`the_boards_group_keys_make_join_add_leave_and_delete_on_real_homes`,
`loquax-android/tests/m9_client.rs:798`) runs it against two real homes
on a transport node, each verb a walk of the board's keys:

- as the admin, on its own home: `n`, Anna ticked, Enter, "Seilschaft",
  Enter; the home logs `HOME group made` and hosts it with Anna, not Bob
  (the control). `m`, `a`, Bob ticked, Enter: `HOME group added`, Bob
  kept. `m`, `d`, `y`: `GROUP Seilschaft deleted ... told=3`, nothing
  hosted.
- as a member of a group another home hosts: Anna's phone makes "Chor"
  with Lew on Anna's home. The invitation reaches Lew's home, the board
  pulls it in LoRa pages into its model, Enter on the card joins, and
  Anna's home keeps Lew joined; before that it kept him invited (the
  control). `m`, `l`, `y` leaves, and Anna's home keeps him left.

All targets:

- **M9:** the screens, the key table, the asks; the image takes its keys
  through `press` and dumps and counts every screen. The board's
  executor of the asks over `leviculum-core` is still the board half of
  slice five.
- **Desktop (TUI and window):** unchanged. The terminal keeps its
  palette for the group verbs, and its shell its own executor of the
  group effects (`client::group`, `group_add`, `group_delete`), which
  also probes for a home too old for `/group`.
- **Android:** unchanged; the phone taps the same nodes.
- **The home:** unchanged; it answers the requests it answered.

What the map slice (the table's row 6) will need, from here: the map is
not a scene, so it is the one screen the cell renderer does not draw.
The M9 needs a tab switch in the key table, the map renderer drawing
into the same `Frame` the cells use (its band flushed the same way),
positions from telemetry in the model the scene already reads, the
region's storage decided (SD against flash), and its census as a frame
time rather than a heap figure, measured on the board (90 to 177 ms per
full draw on this board, `docs/drafts/map-measurement-2026-10.md`).

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

The M9 frontend: the six slices of "The slices, in order" above but the
host halves of slice one, of slice two (1356, the chat drawn and typed
into), of slice three (1359, the keyboard driver, the typed code and
the join's `no_std` steps) and of slice four (1361, the image that boots
the renderer, built, its board half not yet run) and of slice five
(1363, the `no_std` client, the LoRa page, the pairing walk and the
session census, measured on the host) and of slice six (1366, the group
screens, their keys and the group verbs asked of real homes). Of the
client seam, the client's
record in flash and the board's executor over `leviculum-core` are
still to build (the table under "Slice five, host half"). Of the 2026-10-21 list that stood
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
