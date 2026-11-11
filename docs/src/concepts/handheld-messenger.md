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
  talks to the backend only through `Outbox` (`loquax/src/outbox.rs:526`),
  but the crate is `std`. It depends on `leviculum-std` and tokio
  (`loquax/Cargo.toml:284`, `loquax/Cargo.toml:288`), since 2026-10-25 behind its `std` feature,
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
  - delivery (`LxmfRouter`, `leviculum-lxmf/src/router.rs:505`);
  - propagation nodes, both the client and the node
    (`PropagationNode`, `leviculum-lxmf/src/propagation_node.rs:1036`);
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
render (`Scene`, `loquax/src/ui/scene.rs:1017`), and this section records
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
  carries the switch (`ascii`, `loquax/src/ui.rs:1197`); the M9 frontend
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
(`HEAP_SIZE`, `leviculum-esp/src/lib.rs:399`) becomes a measured
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
a key becomes a `keys::Action` (`resolve`, `loquax/src/keys.rs:1059`)
through the shared rows (`ROWS`, `loquax/src/keys.rs:225`, over
`leviculum_ui::keymap`'s scroll rows), and the action becomes a scene
intent (`Intent::from_action`, `loquax/src/ui/intent.rs:510`). Nothing
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
  `no_std` (`wire`, `loquax/src/home.rs:51`), as are the admission
  code's codec (`code`), the archive relation and the wake rules.
- **The client's ask loop is not.** `home::client` is gated `std`
  (`loquax/src/home.rs:48`): it waits on tokio time and drives
  `leviculum_std::ReticulumNode`'s link API
  (`loquax/src/home/client.rs:10`). The board port is the same request
  flow over `leviculum-core`'s links and the firmware's clock — a seam
  cut like the store's (`StoreWrite`, `loquax/src/ui/history.rs:130`)
  and (`StoreRead`, `loquax/src/ui/history.rs:203`): the
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

### The tracks' time window on the M9 (1419)

Nothing is built for it; this is where it goes when slice 6 brings the
map. On the phone and in the window the tracks' time window left the
map for the map's menu ([Loquax: tracks on the map](loquax-tracks.md),
"The time window"), and the M9 has neither room nor a pointer for a
menu over the map. So it takes the terminal's way: a key on the map
screen steps through the six windows (`w` and `W`, the shared rows, so
the M9's keyboard gets them through the same resolver), and while a
window other than the default is chosen its word follows the map's
title ("Map · This week", `TrackWindow::title`), the one line that says
why older lines show. No strip, no chips, no sheet.

### Slice one done, on the host (1351)

The renderer is a crate: `loquax-m9`, `no_std` plus `alloc`, built for
`thumbv7em-none-eabihf` and the M9's `xtensa-esp32s3-none-elf` by `just
m9-render-no-std` (in `guards`, with an injected `std::fs` call as its
positive control). It words the chats list scene in cells
(`lines`, `loquax-m9/src/screen.rs:225`) and draws them through
`leviculum-ui-ratatui`'s styled-line bridge, which builds `no_std`
without that crate's new default `terminal` feature, into ratatui over
mousefood (`draw`, `loquax-m9/src/render.rs:199`). Mousefood's own
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
  (`narrow`, `loquax-m9/src/screen.rs:575`).
- **The tick ladder.** The list's mark stands on the shared glyph table's
  ASCII rungs (`. . v vv VV !` for sending, unconfirmed, mailbox,
  delivered, read, failed; `state_of`, `loquax-m9/src/screen.rs:519`).
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

- **The chat in cells** (`lines_at`, `loquax-m9/src/chat.rs:57`). The
  scene's chat set as the terminal sets a conversation: a bubble wrapped
  to three quarters of the 53 cells (`block_cap`,
  `loquax/src/ui/layout.rs:410`), tinted with a margin cell either side,
  ours flush right, theirs flush left, the time and the mark's ASCII rung
  closing its last row, a blank row between two groups. The day line and
  a group's note line (who joined or left) are the terminal's own
  (`date_line`, `loquax/src/ui/layout.rs:380`, and `note_line`,
  `loquax/src/ui/layout.rs:364`, public since this slice). A group's chat
  names the writer above a group of bubbles, which is how a digest's
  lines read (each a bubble under its writer, the model's work), and a
  line that came after newer ones says "arrived late" under it. Quotes,
  photos, files and voice messages are words (`> …`, `[#] Photo, …`,
  `[f]`, `[v]`); reactions are not drawn yet. Every row goes through
  `narrow` first, the store's emoji and Japanese included.
- **Only the screen is laid out.** The bubbles go from the bottom up,
  from the newest or, when the cursor stands above the last screen, from
  the cursor's bubble (`bubbles`, `loquax-m9/src/chat.rs:257`). When the
  bubbles up to the cursor's do not fill the screen, it starts at the
  chat's first bubble and the newer ones fill the rows under it (1544:
  the bench walk's held ring up showed a folded position run alone at the
  bottom over a black body; `m9-chat-top-held.png`). The window
  is a function of the scene and the cursor alone; the redraw test moves
  the cursor nine bubbles, opens the line and types an emoji with one
  renderer, and must equal a fresh draw.
- **What the M9 shows of a model** (`scene`,
  `loquax-m9/src/screen.rs:198`, and `Look::of`,
  `loquax-m9/src/screen.rs:175`). The Compact scene, which is the open chat
  alone, with the cursor of the pane the keys are in; and the list while
  the keys are on the list. The model's back from the chat moves the keys
  to the list and keeps the chat open, as the terminal's panes do, so a
  screen of one pane shows the pane the keys are in. That builds the Wide
  scene and drops its chat: the list's peak then carries the chat's.
- **The list's cursor is the reader's** (1546, `list_top`,
  `loquax/src/ui.rs:2597`). The model keeps the selected peer and the line
  it stands on; while it stays selected, a pull or an arrival that sorts
  rows in above it moves the window, not the cursor. A key moves the line
  by as many rows as the selection moved in the order. Before, the window
  was a function of the cursor's place in the order alone, and a first
  pull's 61 pages made the cursor jump about the panel with no key
  pressed. Moving the cursor onto a chat loads its page behind the list
  and reads nothing (`behind_the_list`, `loquax-m9/src/home.rs:292`); Enter
  reads, and an Enter before the page arrived names the row's kept unread
  ids. What a type size set asks of the model, the boot's cursor chat's
  page among it, is routed (`set_size`, `loquax-m9/src/house.rs:304`).
- **The compose line** (`compose_line`, `loquax-m9/src/chat.rs:576`): the
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

- **What the board has told us of the keyboard.** This slice first
  built a driver over a 5 x 10 matrix scan, with debounce, latched Shift
  and Sym and its own repeat, because the controller's protocol was
  unread. The bench capture of 2026-11-04 read it, and it is no matrix:
  one key code per press, ASCII for the characters, layers resolved in
  the controller, no key-up ([The ThinkNode M9](thinknode-m9.md), "The
  keyboard controller, measured"). 1427 replaced the matrix driver with
  the measured one; the scan frame, the `KB` line and the test layout are
  gone.
- **The driver** (`Key::of`, `loquax-m9/src/keyboard.rs:160`) keeps no
  state: a non-zero code is one press, printable ASCII a character,
  `0x0d` Enter, `0x08` Backspace, a code of `SPECIALS`
  (`loquax-m9/src/keyboard.rs:132`) its control's role, any other
  `special_0x..`, which makes no key event. A role is a key of the
  shared table (the ring and Back) or a palette command run as a GUI's
  menu runs one (the chats list, the compose line, the map;
  `keyboard::run`), provisional until the walk proves the codes (1440).
  Repeat is the controller's, if it has any: a code read again is a
  press again. What comes out is the key events the table of slices one
  and two resolves; the M9 still binds nothing of its own.
- **The walk**: `tests/keyboard/walk.txt` under `loquax-m9`, controller
  answers in the board's `KBRAW` format made only of codes the capture
  measured; through the read, the driver, the key table and the model it
  opens the pinned chat, types "hi&0" and sends it
  (`a_recorded_walk_of_key_codes_types_and_sends`). The capture itself
  replays to the names of its first 60 presses
  (`the_bench_capture_replays_to_its_key_names`).

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
  `loquax/src/home/ask.rs:294`): the seam this page asked for, cut as the
  store's. The path ladder (`PathLadder`, `loquax/src/home/ask.rs:68`;
  requests at 0, 7, 21, 49 and 105 s, then a minute apart), the per-hop
  link budget of rule 3 (`establishment_ms`,
  `loquax/src/home/ask.rs:122`) and what a `/join` answer means
  (`join_outcome`, `loquax/src/home/ask.rs:128`) are `no_std` and shared:
  the desktop's `open` takes the ladder and the budget from there, and
  `HomeError` lives there. The join is a set of `Step`s
  (`loquax/src/home/ask.rs:250`), request a path, connect, identify, send
  `/join`, done, which the platform performs and reports back; a report
  out of turn ends the join instead of being read as the home's word.

What of `home::client` was not `no_std` then, all of it behind
`#[cfg(feature = "std")]` (`loquax/src/home.rs:48`); slice five's host
half (1363, below) moved the rows up to the group verbs:

| piece | where | what it needs on the board |
|---|---|---|
| the link loop: path, connect, identify | `open`, `loquax/src/home/client.rs:665` | nothing more: `Join` is its board form; other requests get the same steps |
| one request and its answer | `request`, `loquax/src/home/client.rs:825` | the firmware's request over a core link, reported to `Join::answered` |
| the join | `join`, `loquax/src/home/client.rs:35` | none: `Join` replaces it |
| the pull and its pages | `pull`, `loquax/src/home/client.rs:539`; `pages`, gone in 1363 | a `Pull` flow like `Join` with the page loop, and the batch size the session census fixes |
| send, mark and the group verbs | `send`, `loquax/src/home/client.rs:155`; `mark`, `loquax/src/home/client.rs:178` | the same steps with their own request; slice five for the groups |
| the client id | `client_id`, `loquax/src/home/client.rs:530` | a random source the firmware has |
| being a client: the home's key, the cursor | `load_client`, `loquax/src/home/client.rs:857`; `save_client`, `loquax/src/home/client.rs:882`; `load_cursor`, `loquax/src/home/client.rs:888` | a record in flash beside the store, the device identity with it |

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
  heap is the 8 MB PSRAM (`init_heap_at`, `leviculum-esp/src/lib.rs:434`),
  the stack the 312,440 B of internal RAM the linker leaves; the class
  allocator now counts what is live and its peak for the census. Nothing
  goes on the air: the LR1110's chip select is parked high.
- **The panel:** ST7789 over SPI2 at 40 MHz with DMA, landscape. A draw
  goes through mousefood into `loquax-m9`'s frame, which hands the rows
  it changed as runs (`take_runs`, `loquax-m9/src/frame.rs:141`), and
  since 1507 the board sends each run through its own row window by DMA
  straight out of the frame (`window`,
  `leviculum-esp/src/bin/thinknode_m9.rs:1170`): the full frame is the
  first draw's cost, a moved cursor sends two entries. The redraw test
  drives one renderer through four keys and must equal a fresh draw, its
  runs flushed into a second frame
  (`the_boards_redraw_and_its_band_equal_a_fresh_draw`).
- **The model:** a fixture built in code, `no_std`, 13 conversations with
  umlauts, Greek and Cyrillic names, a pinned one of 16 messages over every
  rung of the ladder, the clock standing at 2026-11-03 12:00 CET (`model`,
  `loquax-m9/src/fixture.rs:242`). The ugly store of the earlier goldens
  is a store on disk with signed messages, which the board has neither of
  yet, so the board's two screens have goldens of their own,
  `m9-fw-chats.png` and `m9-fw-chat.png`, walked to with the board's keys
  (`Screen::model`, `loquax-m9/src/fixture.rs:636`).
- **The keyboard:** the controller's answer is read as the community port
  reads it, register 0x01 of 0x6D, eight bytes a poll. Every answer goes
  out raw, `KBRAW <ms> <hex>`, one a 10 ms poll, and `parse_raw`
  (`loquax-m9/src/kbraw.rs:285`) replays a capture on the host. Slice four
  first decoded the answer as a matrix bitmap; the capture that line made
  possible read the protocol (one code per press, 1427), and the board
  now reads it so (`MEASURED`, `loquax-m9/src/kbraw.rs:37`; `read`,
  `loquax-m9/src/kbraw.rs:79`) and prints `KEY <ms> code=0x6b name=k`
  after each press's `KBRAW` line.
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
- **The census** (`measure`, `loquax-m9/src/census.rs:50`), one routine
  for both: build the screen's scene, then draw it, each step's heap held,
  heap peak and stack below the probe. The budgets are this page's
  ceilings (`CHATS`, `loquax-m9/src/census.rs:175`; `CHAT`,
  `loquax-m9/src/census.rs:187`). The board prints a `[CENSUS]` line per
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
`KBRAW` every 10 ms with a `KEY` line after each press and the banner pair every 5 s. Not yet done
of slice four: everything the board says. Whether the frame on the panel
reads the right way round, whether the dump equals the golden, what the
board's census measures, and what the controller's answer is.

### Slice five, host half (1363)

The board as a Loquax client of its home over LoRa, built and measured
on the host. The board's half (its executor over `leviculum-core`'s
links, the record in flash, the census line at boot) waits for the
image of slice four to run.

**The client moved down.** `home::ask` is now the whole ask, not only
the join: `Ask` (`loquax/src/home/ask.rs:294`) over a `Verb`
(`loquax/src/home/ask.rs:171`), `/join`, `/sync` page by page, `/send`,
`/mark` and `/group`, with an `Outcome`
(`loquax/src/home/ask.rs:219`). The platform performs the `Step`s
(`loquax/src/home/ask.rs:250`) as before; two are new. `Page` hands out
one page of a pull and the ask keeps none of it, so a pull's memory is
one page however long the archive. `Close` says an answer was lost: the
same request goes again on a fresh link, at most `ATTEMPTS`
(`loquax/src/home/ask.rs:161`) times, for every verb but `/join`, whose
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
| send, mark, group | std | `Verb::Send`, `Verb::Mark`, `Verb::Group`; `mark` runs the ask, and since 1371 `send` does too: `send_retrying`, which asked a send three times around a loop of its own and with the ask's retries would have asked nine, is folded into the ask's one policy, `ask::ATTEMPTS` = 3 in all |
| the client id, the home's key, the cursor | std | still std: the firmware's random source and its flash record are the board half |
| the old-home probe of `/group` | std | std only: a home before 1307 drops `/group`, and the board's homes are newer |

`just m9-render-no-std` now checks the client with the renderer: its
positive control puts a `std::fs` call into `loquax/src/home/ask.rs` as
well as into the renderer, and both must be refused for thumbv7em.

**The page fits a packet.** A link on LoRa keeps the default MTU, 500
bytes, and its MDU is 431 (`LORA_LINK_MDU`,
`loquax/src/home/ask.rs:139`). A response longer than that goes as a
resource (`leviculum-core/src/node/mod.rs:1843`), which is an
advertisement, its parts and a proof. The home's own page bound is
64 KiB of messages (`SYNC_PAGE_BYTES`, `loquax/src/home.rs:368`). So
the client asks smaller pages: `/sync [cursor, page]`, a second element
that a home from before reads past. The board asks `LORA_PAGE_BYTES`
(`loquax/src/home/ask.rs:146`), 412 B, the longest answer that still fits
one response packet once the response frame (1 + 18 bytes) is around
it. The home cuts the page to that, keeping at least one entry, and
says `more` (`fit_page`, `loquax/src/home/wire.rs:420`; the server's
`sync`, `loquax/src/home/server.rs:1393`). A message larger than a page
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
| keepalive | 2 | 20 B | 20 B | flags, hops, link id, context, one byte (`build_keepalive_packet`, `leviculum-core/src/link/mod.rs:2753`), every 230 s |

Five entries pulled in LoRa pages came as two pages, answers of 499 and
467 bytes, each one packet of at most two LoRa frames. The control, the
same five in the home's own page, is one 867-byte answer: over TCP's
larger link MTU that is one packet, but on LoRa it would be a resource.

**The airtime**, at our defaults (BW 125 kHz, SF8, CR 4/5, 18-symbol
preamble), from those lengths through `packet_airtime_ms`
(`leviculum-core/src/rnode.rs:1668`), as [Announce
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
code the phone shows on the board's keys. The controller's answers, one
ASCII code per press as the board measured (1427), go through the read
and the driver (`MEASURED`, `Key::of`). The key events go into the pairing
screen (`Pairing`, `loquax-m9/src/pair.rs:32`). After 20 characters, a
`9` (not base32) reads "character 21 cannot stand there".
Backspace takes it back, the remaining 120 characters follow, and Enter
gives the `HomeCode` the phone showed. The join (`Pairing::join`,
`loquax-m9/src/pair.rs:69`) then runs over the mesh. The home logs
`HOME admitted`, and its store lists the board's identity under the
name its owner gave the code. The board's own `DEVICE_NAME`, "M9", is
what it sends, but the home names a device by its code. The screen
shows the code in fives, wrapped at a group's end to the 53 cells
(`Pairing::lines`, `loquax-m9/src/pair.rs:75`), and one verdict line.

**The session's heap.** `census::session`
(`loquax-m9/src/census.rs:276`) is a routine the board can run as it
runs the screen census. It drives a pull of fed answers with the
platform's probe. The host runs it in `m9_fixture_census` over thirty
LoRa pages of one 200-byte message each: `session=pull held=0 peak=392
verdict=ok`. Between pages nothing is held, and a page costs what it
carries. The ceilings are about double that (`SESSION_PEAK`, 1,000 B,
`loquax-m9/src/census.rs:267`; `SESSION_HELD`, 256 B). The positive
control is the same archive in one page of the home's own bound: a
63,225-byte answer, peak 84,576 B, refused. The page bound that keeps
LoRa answers to one packet also keeps a pull's RAM to one page. On the
board that is SRAM beside the node and not the UI's PSRAM.

All targets:

- **M9:** the ask, the pairing screen and the session census. The
  board's executor and its flash record came in 1414.
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
the rows from `new group` (`loquax/src/keys.rs:930`) to `start again`
(`loquax/src/keys.rs:962`) have no chord. The M9 has no palette. So its
group keys run the node's `Action` the way a tap does (`hit`,
`loquax-m9/src/group.rs:137`), and nothing new enters the model. The
cursor over the sheet's and the picker's rows is the M9's own, as a
finger is the phone's: a row index the board keeps beside the model
(`Look::row`, `loquax-m9/src/screen.rs:134`), back to the first row
whenever another screen opens, kept when the sheet's question is
answered (`press`, `loquax-m9/src/group.rs:263`). The board's key loop
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

The table is `ROWS` (`loquax-m9/src/group.rs:97`). A test takes each row
to the screen of its place, presses its key and compares what `hit`
returns with the action the scene's node offers
(`every_group_key_runs_the_action_its_node_offers`). Its control: `m` and
Enter on a peer's chat are not the group's.

**The screens** (`sheet_lines`, `loquax-m9/src/group.rs:472`;
`pick_lines`, `loquax-m9/src/group.rs:575`; `name_lines`,
`loquax-m9/src/group.rs:633`), each the whole 53 x 24 grid. The sheet:
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
`[Enter] Join` under the invitation (`loquax-m9/src/chat.rs:97`), and an
invitation's bubble in the inviter's chat says `[Enter] Join` too. A
group's row in the list has its member count as the avatar from the
scene; a group with no line yet says "Invitation" or "3 members" where
the last line would be. Every word goes through `narrow`.

**The fixture's groups** (`group_model`, `loquax-m9/src/fixture.rs:467`):
Jürgen's "Familie", the reader in it and Ünal gone; the reader's own
"Kletterhalle", Ivan still invited; Jürgen's "Chor", the reader invited.
`FRAME_DUMP` names nine screens now, one byte each in `SCREENS`
(`loquax-m9/src/fixture.rs:512`), walked with the board's keys through
`press` (`Screen::walk`, `loquax-m9/src/fixture.rs:626`):

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
to `Screen::budget` (`loquax-m9/src/fixture.rs:538`). The group's
screens have a budget of their own (`SHEET`,
`loquax-m9/src/census.rs:202`), about double the largest of them; the
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
(`GroupVerb`, `loquax/src/home/ask.rs:198`) now carries all five a
board needs: `Create` (`/group`), `Add` (`/groupadd`), `Delete`
(`/groupdelete`, its `Ok` the group it deleted), and `State`, the
member's Join, Leave or the admin's Remove, which the model makes a state
message to the group (`group_state`, `loquax/src/ui/groups.rs:1626`) and
the home sends as any message (`/send`). `ask_of`
(`loquax-m9/src/group.rs:363`) turns the model's effect into that verb,
with the board's fresh client id; a plain message is no group's ask.
The harness test (`the_boards_group_keys_make_join_add_leave_and_delete_on_real_homes`,
`loquax-android/tests/m9_client.rs:771`) runs it against two real homes
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

### Slice seven, host half (1375)

The map on the M9, measured first and then built: the model's map, the
one the phone, the window and the terminal show, drawn on the board's
screen. Of the slice table above this is row 6, the map; nothing was
flashed, the board half is the rig's.

**What was measured.** The tool is `loquax-m9/examples/map_measure.rs`
(release build, the median of 50 runs after a warm-up, two views an
eighth apart drawn in turn so that a redraw sees what a pan gives it).
The tile set is a 2 km box of central Florence cut at the phone's zooms
(`PLACE_ZOOMS`, z10 to z15, `loquax/src/ui/map.rs:119`) from the
Protomaps sample archive the map core's fixture comes from:

```sh
leviculum-map-tool cut -o two-km.lmr firenze.pmtiles \
    --bbox 11.2436,43.7643,11.2684,43.7823 --min-zoom 10 --max-zoom 15 \
    --locale de --buildings false
```

820,244 B, 25 tiles, sha256
`e73df1129217b9aec3629463c47f4a7fd1ac46213ae0191ced9960b9e06dbab9`
(994,525 B with the building sections, which the M9 never reads: its
finish draws none). It is not in the tree; the cut gives the same bytes
again. The picture is the M9's: 53 x 21 cells of the 6 x 10 atlas under
the bar, 318 x 210 pixels, the frame 320 x 240 RGB565. The host had a
load of 14 on 10 cores during the run, so the times rank the two ways
and bound nothing on the board, which the map measurement found about
twenty times slower (`docs/drafts/map-measurement-2026-10.md`).

| zoom | view's tiles | their bytes | decode, in place | pixels: picture | pixels: whole screen | cells, braille | cells, quadrants |
|---|---:|---:|---:|---:|---:|---:|---:|
| z13 | 4 | 201,070 B | 0.094 ms | 1.166 ms | 1.392 ms | 7.679 ms | 1.542 ms |
| z14 | 4 | 134,612 B | 0.076 ms | 1.508 ms | 1.694 ms | 3.570 ms | 1.458 ms |
| z15 | 6 | 81,850 B | 0.048 ms | 0.944 ms | 1.096 ms | 1.895 ms | 0.862 ms |

- **Decode** is opening the region and checking every tile the view
  needs (`Region::view`): the index searched and every point of every
  feature range-checked. The region's tiles are already geometry; there
  is no other decode.
- **Pixels** is the map core's filler painting the view straight into
  the panel's frame (`Painter::picture`, `loquax-m9/src/map.rs:815`); the
  whole screen adds the bar's and the legend's cells and the pins
  (`Renderer::show`, `loquax-m9/src/render.rs:260`).
- **Cells** is the terminal's way (`leviculum_map::cells`): the view
  drawn at four by eight source pixels a cell, encoded into glyphs and
  drawn by ratatui through mousefood. The atlas has two of the
  terminal's glyph sets, braille (U+2800 to U+28FF) and the quadrants;
  it has no sextants or octants. Braille needs the view drawn three
  times (colour, lines, thin lines); the mousefood draw itself is 0.1 to
  0.2 ms of the sum.

The bytes, for a board that holds the region in its flash and reads it
in place:

| what | bytes | where |
|---|---:|---|
| the 2 km region, z10 to z15 | 820,244 B | flash, read in place: none in PSRAM |
| the fixture region the image carries | 197,972 B | flash |
| the view's tiles, read out of a card instead | 81,850 to 201,070 B | PSRAM, per view |
| the painter's storage, made once (`Painter`, `loquax-m9/src/map.rs:771`) | 217,508 B | PSRAM |
| a map draw's heap peak (census, below) | 75,404 B | PSRAM |
| the cells' own per view, besides the painter's (source and cells) | 191,436 B braille, 120,204 B quadrants | PSRAM |
| the panel's frame | 153,600 B | SRAM or PSRAM |

Beside the plan's heap table ("Memory budgets, measured") the map adds
the painter and one draw's peak, about 290 KB, to the about 1.1 MB the
UI takes: far inside 8 MB.

**The painter: pixels, not mousefood.** The decision of 2026-10-25 put
the cells through mousefood, and they stay there; the map is the
exception the plan foresaw ("the map stays the separate renderer it
is"), now measured. Through mousefood the map is at best 106 x 84
braille dots or 106 x 42 quadrant blocks of two colours a cell, for the
318 x 210 colour pixels the filler paints; and the cells cost as much
or more to make, since they are a picture drawn first and then encoded
(braille 1.9 to 7.7 ms, quadrants 0.9 to 1.5 ms, pixels 0.9 to 1.5 ms),
plus 120 to 191 KB a view the pixels do not need. The filler is the
same code the phone, the window and the terminal draw through, so no
renderer is written twice; the cell renderer keeps the bar and the
legend.

**The screen** (`lines`, `loquax-m9/src/map.rs:244`; `pins`,
`loquax-m9/src/map.rs:861`), as 1531 left it after Lew's evening at
the board on 2026-11-07: the map is in the light table, in the M9's
finish (square caps, no casings, no buildings); the UI around it keeps
its dark tokens. There is no title row and no legend: the picture is
the whole panel, rows 0 to 10 for the model's fit and pins (320 x 220),
and row 11 is the picture too whenever it has no words. Its words
(`Chrome`, `loquax-m9/src/map.rs:307`; `bottom`,
`loquax-m9/src/map.rs:421`): the key hint, "T näher  Y weiter
Pfeile schieben", for three seconds after the map opens and after a key
it does not know, the pins' page for three more when there is a pin;
then the chosen pin's card, its age and the data's credit, "©
OpenStreetMap", at the right end; and the credit for five seconds in the
picture's top right corner the first time the map opens after a boot.
The names (`names`, `loquax-m9/src/map.rs:546`) are set after the
picture and before the pins, by the map core's placement rule in three
rounds: places, then streets and rivers centred on a straight stretch
of their line (horizontal, the atlas cannot turn text), then from z15
parks and waters; one per 11 x 3 cells, at most eight. A draw is the
cells first, every row in the map's ground, then the map over them, the
names, the pins and the words; ratatui draws only cells that changed,
so the next map leaves the blank rows alone, and the renderer clears
its cells when the screen leaves the map (`leave_map`,
`loquax-m9/src/render.rs:383`), or the list would keep map pixels where
its cells are blank.

**The pins are the model's**, as on every target (`map_pins`,
`loquax/src/ui.rs:6583`): the shares the store kept, a `lev.position`
announce, and the board's own fix. That last one is new in the core:
the shell's own fix (`UiEvent::OwnFix`, from the receiver or typed by
hand) is now our pin where it is newer than our last share, so a board
with a GNSS shows where it is before it shared anything. The core
places them in the picture's cells (`map::place`), the M9 asks for the
ASCII markers: the atlas has `●` but not the chosen pin's `◉`. Each pin
is set in the atlas on the bar's colours, the chosen one reversed, an
old one's and a nearby node's muted. Where the picture is, the M9 tells
the model (`map_picture`, `loquax/src/ui.rs:1401`): the fit, the pan's
step and the pins' cells go by its 53 x 21 cells, not by the terminal's
conversation pane.

**The keys** (`ROWS`, `loquax-m9/src/map.rs:156`) are the terminal's map
keys where the M9 has the key; Space and Backspace stand in for Tab and
Shift-Tab, which it has not, and T, I, Y, O for `+` and `-`, which sit on
the controller's Sym layer (`key`, `loquax-m9/src/map.rs:179`). A test
takes every row through the shared resolver
(`every_map_key_runs_the_terminals_map_command`).

| verb | M9 key | runs |
|---|---|---|
| the map of every pin | `M` (on the list) | the palette's `map` |
| this chat's peer on the map | `m` (on the list) | `PeerOnMap` |
| pan | the arrows | `Pan` by an eighth |
| zoom in / out | `T` / `Y` (the keys under the ring, 1531), `I` / `O`, `+` / `-` | `ZoomIn` / `ZoomOut` |
| next / previous pin | Space / Backspace | `Step` |
| every pin again | `0` | `Fit` |
| open the pin's chat | Enter | `Open` |
| close | Back | `Close` |

**The fixture** (`map`, `loquax-m9/src/fixture.rs:328`) gives the
board's model the map core's Florence region (`FIXTURE`,
`loquax-m9/src/map.rs:83`, read in place from the image), the picture,
and five positions: the board's GNSS fix by the cathedral, Jürgen's and
Mum's shares, Zoë's three hours old, and Rifugio, a node nearby that
announced itself. The existing screens' goldens did not move. Two
`FRAME_DUMP` screens join the nine:

| byte | screen | the walk | dump |
|---:|---|---|---:|
| 9 | `map`: every pin, Jürgen chosen, z14 | `M` | 51,154 B |
| 10 | `pan`: zoomed in to z15, Mum chosen, panned east | `M`, `+`, Space, Right | 50,176 B |

Both goldens (`m9-fw-map.png`, `m9-fw-pan.png`) were looked at as
pictures: the streets and the river in the dark table, the pins with
their names and ages, the chosen one reversed, Zoë and Rifugio muted,
the band of the fixture's small region with the background past it. The
positive controls: each walk's last key changes its frame
(`the_last_key_of_every_map_walk_is_in_its_frame`), and one renderer
walking into the map, around it and out to the list equals a fresh draw
after every key (`a_map_redraw_is_the_frame_a_fresh_draw_gives`); with
the clear on leaving the map disabled that test goes red after Back.

**The census** (64-bit host, debug build): the map's scene step is its
view and the pins placed, its draw the whole screen, held to a budget
of its own (`MAP`, `loquax-m9/src/census.rs:218`), and the painter's
storage to `PAINTER_HELD` (`loquax-m9/src/census.rs:230`); the board
prints both lines at boot.

| screen | scene held | scene peak | scene stack | draw peak | draw stack | budget |
|---|---:|---:|---:|---:|---:|---|
| map | 559 B | 2,793 B | 8,384 B | 75,404 B | 15,072 B | `MAP` |
| pan | 228 B | 2,793 B | 8,384 B | 75,073 B | 15,072 B | `MAP` |
| the painter, made once | 217,988 B held | | | | | `PAINTER_HELD` |

The image grows by 255,840 B to 1,200,608 B (`just build-m9`), the
region 197,972 B of it.

**What the board needs beyond the host half:**

- **Where the tiles live: the flash.** The 16 MB NOR is confirmed on our
  unit (the ZD25Q128, [The ThinkNode M9](thinknode-m9.md), "Storage");
  the image takes 1.2 MB of it and `just build-m9` reports 15,183,392 B
  free in the application partition. A 2 km set is 820 KB, a town of 12
  by 12 km 1.3 to 7.9 MB by the autoload's measurements ([Loquax: the
  map](loquax-map.md), section 4), so a region partition of a few MB,
  read in place through the flash cache, holds a deployment's town and
  costs no PSRAM. The microSD slot exists (chip select GPIO48 on the bus
  the LR1110 and the panel share, "Storage"), but the card is not
  reachable from outside on our unit, every view would be read out into
  PSRAM (82 to 201 KB), and its reads contend with the flush and the
  radio on that bus. Flash first; the card is the place for a region too
  big for it, and `Region::read_view` already reads one there.
- **Getting a region onto the board.** The phone loads its regions by
  HTTP range requests (`loquax::mapload`); the board has no IP. No
  `Ask` verb carries a region today: the five are join, pull, send, mark
  and the group verbs (`Verb`, `loquax/src/home/ask.rs:171`). The shape
  that fits is a sixth, a region asked of the home in LoRa pages the way
  `Pull` pages the archive (`LORA_PAGE_BYTES`, 412 B,
  `loquax/src/home/ask.rs:146`, the link MDU less the response frame),
  the home cutting it with the autoload
  it already runs. **The budget line that is red today:** the 2 km set
  is 1,991 such pages; at SF8 and 125 kHz a 431 B packet is on the air
  about 1.16 s, so 38 minutes of airtime before requests and proofs, and
  at the 10 % duty cycle of the 869.4 to 869.65 MHz sub-band at least
  6.4 hours. A region over LoRa is a trickle for one place at a time at
  best; the deployment's region goes on over USB, written by lnflash as
  the radio settings are, and the mesh carries only what changed.
- **The frame time.** On the board the filler measured 90 to 177 ms for
  a full 240 x 320 view of dense Frankfurt plus 31 ms of flush
  (`docs/drafts/map-measurement-2026-10.md`); the M9's picture is 318 x
  210 and the screen flushes only the band that changed, so a pan is a
  few frames a second. Measured on the board with the slice's walk,
  before anything is claimed.
- **The world under the region.** Below the region's zooms the map is
  background: the world outline (159 KB, `leviculum-map/data`) is not in
  the image.

All targets:

- **M9:** the map screen, its pins, legend and keys, two dump screens,
  the census; the image opens it with `M` over the fixture region.
- **Desktop (TUI and window):** the own fix typed by hand (1371) is now
  our pin on the map where it is newer than our last share; otherwise
  unchanged.
- **Android:** the phone's own fix (`Intent::OwnFix`,
  `loquax-android/src/session.rs:1719`) is our pin the same way;
  otherwise unchanged.
- **The home:** unchanged; it carries no region yet (above).

### Explore on the M9: what it would need (1578, not built)

Explore is the bar's third and last button on the phone, the window and
the terminal (Lew, 2026-11-08: three buttons, no fourth). The M9 gets no
third button: it has no tab bar of its own and no named key free for one
(the home key belongs to the list, 1440). To carry Explore it would need
a key that brings `TabId::Explore` to the front (a row in
`loquax-m9/src/keys.rs` beside the list's, through `keys::event`); a
branch in `lines` (`loquax-m9/src/screen.rs`) for the scene's `explore`
node (`ExploreNode`, `node` in `loquax/src/ui/explore.rs`), which the
model already returns while the list is in front (`explore_scene`,
`loquax/src/ui/scene.rs`), drawn as the section titles, the headings and
one line a row (`name ·hash4` and its age, through `narrow`); and no
verbs of its own, because the model takes the terminal's keys while the
list is in front (`key` in `loquax/src/ui/explore.rs`: characters into
the search, Up and Down, Enter opens, Esc leaves). Opening a page needs
the Browse tab's renderer on the board, and a hub's rooms the section
below. The board keeps lnomad's 500 nodes (`REGISTRY` in
`loquax/src/ui/browse.rs`), where a desktop and a phone keep the home's
4096.

### The Rooms on the M9: what they would need (1384, not built)

The Rooms tab (Reticulum Relay Chat, 1379) is drawn on the terminal and
the window and since 1384 on the phone; the M9 is the one target where it
is not, because its renderer is still host-only. On the renderer it needs
a branch in `lines` (`loquax-m9/src/screen.rs:225`) for the scene's
`rooms` node (`RoomsNode`, `node`, `loquax/src/ui/rooms.rs:948`), which
the model already returns while the tab is in front (`rooms_scene`,
`loquax/src/ui/scene.rs:1204`), and its tab in the tab bar the screen
already draws: the hub's line under the top bar, then either the room
list (name, unread count) or the open room's one live line and its lines
as `who text`, and one input line at the bottom on the chat's editor
(`loquax-m9/src/field.rs:20`), nicks through `narrow` as every other
text. In the key table it needs no verbs of its own: while the tab is in
front the model takes the terminal's keys itself (`key`,
`loquax/src/ui/rooms.rs:687`: characters into the line, Backspace,
Enter sends, Up and Down step the rooms, Esc closes the room and then
the tab), so a `ROOMS_ROWS` table beside `LIST_ROWS` and `CHAT_ROWS` in
`loquax-m9/src/keys.rs` maps the M9's navigation keys, Enter, back,
erase and the character keys onto those events through `keys::event`,
with a test that resolves each row as the list's and the chat's tests do,
and one row in the list pane for the terminal's `R`
(`loquax/src/keys.rs:651`) to bring the tab. Two things beyond the
screen decide whether it is worth building there: the hub link is `std`
on tokio (`loquax/src/rooms_link.rs`, behind `std`), so the board drives
`leviculum_rrc::Session` (`no_std` already) over a link of its own once
its executor over `leviculum-core` exists, or its home relays the room;
and a room keeps up to 500 lines of up to 350 bytes (`LINES_KEPT`,
`loquax/src/ui/rooms.rs:39`), which the census has to count before the
M9 keeps as many.

### The board's node and the client's board half (1414)

Lew, 2026-11-04: the M9 runs Loquax under the same identity as Lew's phone
and pulls from the same mailbox, so that several devices of one person
get tested. In this page's model that is the second-device flow of
[Reaching the mailbox from anywhere](loquax-reachability.md) ("A second
device for a phone's mailbox"): the M9 becomes a device of the home,
joins with an admission code, pulls the archive the phone pulls and
sends as the home's identity. This pass is the first of two; typing the
code on the board and the bench walk are 1415.

**The node in the image.** `thinknode_m9` now carries a `leviculum-core`
node (`Handheld`, `leviculum-esp/src/handheld.rs:110`): its identity in a
flash sector of its own, saved on the first boot and loaded on every
later one, said as `[IDENTITY] identity=<hash> ... source=<flash|generated>
stored=<yes|no>`; one interface, the host's serial line
(`SerialIface`, `leviculum-esp/src/serial_iface.rs`), Gateway mode and
HW_MTU 564 as an LNode's. On the bench the line is lnsd on
schneckenschreck, the rendezvous on `leviculum.network`, port 4242, and the home
behind it. The LR1110 in Mesh mode is the node's second interface and
the next step: until then nothing goes on the air.

**One UART for both.** The CH340K is the board's only port, so the node's
frames, the log, the keyboard's `KBRAW`/`KB` lines and the frame dump
share it under the rules of `leviculum-nrf/serial-mux`, which were made
for this board (1238): text is written in whole lines between frames and
never carries a raw 0x7E, 0x7D, 0x11 or 0x13; every frame leaves with its
XON and XOFF escaped (`UartLog::frame`, `leviculum-esp/src/lib.rs:1182`);
the capability report states `serial_mux=1`, so lnsd escapes its own
flow bytes and strips the line's. A frame that is not the control
envelope is a packet for the node. The board sends no flow bytes: esp-hal
1.2 keeps `SwFlowControl` in its config and never programs it. Instead
an interrupt empties the 128-byte receive FIFO into 8 KiB of memory
whenever it holds more than 64 bytes (`uart0_rx`,
`leviculum-esp/src/lib.rs:832`, since 1440), so a frame that arrives
during a 5.7 s dump or while the loop checks an announce is read
afterwards and not lost. Until 1440 the FIFO was emptied only when the
loop wrote or read the line; a turn longer than the FIFO's 11 ms let it
overflow, esp-hal reset it, and the bench of 2026-11-06 lost three
`/sync` answers behind backbone announces
(`leviculum-nrf/serial-mux/src/rx.rs` models both drivers). Every loss is
said as `[UART] rx_lost= overrun= deframe_drop= rx_err=
loop_gap_max_ms=`, on change and with each banner. The send side is a
ring the interrupt feeds the FIFO from (1507), which drops its oldest
whole record when the loop writes faster than the line takes. A record
is a whole line with its line end, or a whole frame; a frame dump's
line is one the ring never drops for another, and the dump's writer
waits for room and then for its tail before the loop writes lossy again
(1553: window 13 lost 2 to 28 lines in 19 dumps of 20 while line ends
were records of their own). The `[UART]` line names the drops by kind as
`tx_drop_kind=line:<n>,frame:<n>,dump:<n>`. What
the bench pays: the keyboard lines are about 6 KB/s of the 11.5 KB/s the
line carries at 115200 baud, so the node has the rest, which is more
than a LoRa interface carries and less than a TCP one.

**The stores need a table.** espflash's default table gives the
application all of the flash, so the M9 ships its own,
`leviculum-esp/partitions-m9.csv`: an 8 MiB application, then `identity`
and `loquax_client`, one 4 KiB sector each, and since 1476 `messages`,
4 MiB from 0x812000 (see "The board keeps what it pulled" below); the
3.93 MiB after it are the map region's. `just build-m9` and `just
flash-m9` pass it to espflash; the firmware finds every store by label
(`leviculum-esp/src/flash.rs`) and says `[FLASH] identity=none/no-entry`
under any other table, running without persistence. Every buffer the
flash routines touch is on the stack, because the heap is PSRAM behind
the cache those routines switch off.

**The client's board half.** `loquax::home::board` (`no_std`):

- **The executor** (`Executor`, `loquax/src/home/board.rs:169`) performs
  an `Ask`'s steps over a `NodeCore` and the events the node raises:
  path requests, `connect`, `identify_link`, `send_request`, the
  response or its timeout, a closed link. The board's loop hands it the
  node, its clock and every event; it only adds to the loop's
  `TickOutput`. A link that closes with a request in flight is a lost
  answer, so the ask opens a fresh link and asks again from the last
  entry it took (`ATTEMPTS` = 3).
- **The session** (`Session`, `loquax/src/home/board.rs:565`): an ask
  that does not get through waits 30 s before the next, doubling up to
  15 min; one that gets through resets it, and a pull goes every 5 min
  while no wake says sooner. What it shows is the phone's sentence
  (`notice`, `loquax/src/home/board.rs:611`): "Your mailbox is not
  connected" for no path, no link, no answer or a refusal.
- **The batch** (`Batch`, `loquax/src/home/board.rs:56`): the page is
  the LoRa page while the board's session census keeps `SESSION_PEAK`,
  a smaller one in proportion when it does not (`census::batch`,
  `loquax-m9/src/census.rs:357`); the board runs the census at boot and
  picks its batch from what it measured. An answer longer than the
  board takes asks the same page smaller on the same link
  (`Ask::refit`, `loquax/src/home/ask.rs:500`) down to 64 B; a single
  message that fits no page ends the pull with an error, never a panic
  and never a skipped message. The answer ceiling is the largest message
  the board takes in, 256 KiB on the PSRAM heap, not the session's room.
- **The record** (`ClientRecord`, `loquax/src/home/record.rs:78`), the
  board's form of the desktop's three files: the device identity's
  private key, the home's public key, the admission (pending with its
  token, or admitted), the sync cursor and the device name, 196 bytes
  with four bytes of SHA-256, so a write cut by a power loss reads as no
  record. A pending record joins, an admitted one pulls; the cursor is
  written once per pull, not per page. The board has no message store
  yet, so a page's entries are counted (`[HOME] page entries=...`) and
  dropped; keeping them is the store's step.

**Tests.** On the host, the executor against a fake home on a second
core node (`loquax/src/home/board/tests.rs`): a join and a pull in LoRa
pages; the link closed by the home with a `/sync` in flight, resumed
from the cursor on a fresh link before any timeout; a home that answers
nothing, ending in `NoPath` after the ladder's requests and the session
backing off 30, 60, 120, 240, 480, 900, 900 s; answers larger than the
board takes, asked smaller until they fit, and one message that fits no
page ending in an error. Positive controls: with the executor deaf to a
closed link the resume test fails, with the size check off the batch
test does. Against the real mailbox daemon behind a Rust transport node
(`loquax-android/tests/m9_board_client.rs`): the board's core node joins,
sends three messages, pulls them back in LoRa pages of one packet each,
and a second pull from the saved cursor brings nothing, with a debug
line written before every frame and the flow bytes escaped as the M9
writes its UART.

**The image** (`just build-m9`, 1414): `FLASH_HEADROOM board=thinknode_m9
used=1782944 free=6605664` of the 8 MiB application, 481,968 B more than
the UI-only image; `STACK_HEADROOM board=thinknode_m9 stack=310964
need=184000`. The boot lines it adds: `[FLASH]`, `[IDENTITY]`, `[CENSUS]
node held=... box=... links=8 max_answer=262144`, `[NODE] state=up
iface=serial hw_mtu=564 serial_mux=1`, `[CENSUS] session=pull ...`, and
`[HOME] client=none page=<n>` on a board that is no device of a home yet.
What the board's census measures with the node up is the rig's to read.

### The M9 joins its home (1415)

The second half of "the M9 as Lew's second device": the code reaches
the board, the board joins, pulls, draws and sends. Built and tested on
the host; the board half is the reviewer's walk below, nothing was
flashed in the pass.

**The code, two ways.** Typed: a board without a client record opens on
the pairing screen (`pair::screen`, `loquax-m9/src/pair.rs:102`), the
code in fives as it is typed on the keys with the verdict under it;
Enter on a whole code that checks takes it, Back leaves for the
fixture. Its golden, the twelfth beside the fixture's eleven, is
`m9-fw-pairing.png` (1440): nothing typed, no home, and the board dumps
it at `FRAME_DUMP` screen byte 11 (`pair::DUMP_BYTE`), so the rig
compares the panel with the same file. Written: `lnflash --home <code> --port <tty>`
(`lnflash/src/home.rs:36`) sends it as one control frame,
`TYPE_HOME_CODE` (0x19, `leviculum-core/src/envelope.rs:416`), the code
as text with or without its rendezvous list. The M9 has no transport
CDC, so the session opens the port it is named, which resets the board,
and asks for the capability report until the boot is through (60 s at
most). Both end in `Handheld::take_code` (`leviculum-esp/src/handheld.rs:229`):
a new device identity, the pending record in flash before the ack, the
client that joins with it. A text that is no code is refused
`REFUSE_VALUE`, a record flash did not take `REFUSE_PERSIST`.

**The join, the pull, the screen.** The client of 1414 now hands every
pulled page to the board's loop, which folds it into the model the
screen draws (`board::fold`, `loquax/src/home/board.rs:861`; the
board's `take_in` without an engine: a message the home took in, one of
the home's devices sent, a mark, a name). A board that is a device of a
home draws that model from boot: its chats list and its chats come from
the archive, and a line typed in a chat is the home's `/send`, queued
ahead of the next pull. The board checks no signature (it holds no
sender's identity), so received messages show unverified. The top bar
says the home at its right end (`board::Word`,
`loquax/src/home/board.rs:653`): "Joining your mailbox", "Fetched N of
M", "Mailbox unreachable", "You. Your mailbox is connected.", and for a
refused code "The mailbox did not take this code. Make a new one on the
computer." M is the home's: a paged `/sync` answer now carries how many
entries wait after it (`Page::left`, `loquax/src/home/wire.rs:173`), a
fifth element only a client that asked a page bound gets, so every
client from before reads the four it read. The home cuts the page
counting with the longest such number, so the count never pushes an
answer past the LoRa page.

**A stale link carries no answer.** The cell for a line cut mid-pull
was red first: the core says `LinkStale` about ten seconds after the
line went, but the request's timeout is the whole ask's budget, so the
pull waited out its 120 s with 4 of 24 entries. The executor now takes
a stale link with a request in flight as a lost answer and asks again
on a fresh link from the cursor (`loquax/src/home/board.rs:363`); the
cell went green, 24 of 24, none twice, 13.9 s. On LoRa a link goes
stale much later than on the host's line, so the recovery there takes
that much longer.

**Not kept across a reset.** The board has no message store in flash
yet. The cursor is saved once per pull (1414), so after a reset the
board shows only what arrives after it; the store is the next step.

**The cells** (`loquax-android/tests/m9_join_cells.rs`), each the client
the board runs on the line the M9 writes, through a transport node to
the real mailbox daemon, each with one `[CELL]` line it asserts on; the
numbers are one run of all five, 25 s:

| cell | measure line, as measured |
|---|---|
| `m9_cell_join_and_pull` | `joined=true join_ms=1140 taken=24 of=Some(24) pages=12 answer_bytes=3912 dup=0 pull_ms=714 word="Fetched 24 of 24"` |
| `m9_cell_wrong_and_expired_code` | `m9_wrong_code result=Refused(BadToken) ms=413`; `m9_expired_code result=Refused(BadToken) ms=85`; both `word="The mailbox did not take this code. ..."` |
| `m9_cell_home_unreachable_at_join` | `result=NoPath ms=25008 word="Mailbox unreachable" next_ask_ms=30000` |
| `m9_cell_link_breaks_mid_pull` | `cuts=1 taken=24 of=Some(24) pages=12 dup=0 pull_ms=13880` |
| `m9_cell_second_device_joins_mid_pull` | `archive=40 first=40 first_dup=0 second=40 second_dup=0 second_joined=Joined` |

Board-only, and so the walk's: the keys, the panel, the flash record,
lnflash's frame on the real UART.

**The walk, for the reviewer's window.** Lew's home is the mailbox the
phone pulls from; the bench line is lnsd on schneckenschreck, the
rendezvous, the home behind it. lnsd's serial interface to the M9 needs
`interface_mode = gateway`: in the default full mode a path request
from the board for the home is not discovered onwards
(`leviculum-core/src/transport.rs:12958`, only access point, gateway
and roaming re-originate), and the join ends in `NoPath`. The M9 sets
its own end to Gateway already (`leviculum-esp/src/handheld.rs:210`).

1. Build and flash: `just build-m9`, then `just flash-m9
   /dev/serial/by-id/usb-1a86_USB_Serial-if00-port0` (it carries the
   partition table). First boot, expected: `[FLASH] identity=<offset>
   client=<offset>`, `[IDENTITY] ... source=generated stored=yes`,
   `[CENSUS] session=pull held=0 peak=<about 392> stack=<n> verdict=ok
   pages=30 entries=30` (the host measures `peak=392`), `[HOME]
   client=none page=412`, and the pairing screen on the panel.
2. On the home: `loquax mailbox admit --name M9` prints the code (on a
   phone that holds the mailbox, its "Link a device" screen shows it).
3. The written way: stop lnsd on the port, `lnflash --home '<code>'
   --port /dev/serial/by-id/usb-1a86_USB_Serial-if00-port0`, expect
   "the board took the code and joins its home now" and on the line
   `[HOME] code=taken saved=yes`, `[HOME] client=yes admitted=no
   cursor=0 page=412 max_answer=262144`. Start lnsd again; then
   `[HOME] joined=yes reach=connected saved=yes`, the home logs `HOME
   admitted device=... name="M9"`, and the panel's top bar says
   "Joining your mailbox" until then.
4. The pull: `[HOME] page entries=<k> last=<seq> bytes=<b> more=<m>
   taken=<n> of=<M> link=serial asked=11520` per page, the top bar
   "Fetched n of M", the home `HOME sync device=... left=<M-n>
   fold=board`, then `[HOME] pulled entries=M pages=<p> cursor=<c>
   saved=yes reach=connected next_in_ms=300000`. M counts only what the
   board shows (1439): on Lew's archive the 796 imported entries are not
   in it, and the first page starts at the first name, seq 797. A page
   of 11 520 B carries about 70 short texts, so p is about M/70; over
   LoRa the same board asks `link=radio asked=412`, two texts a page.
5. The phone and the board show the same conversations: the chats list
   on the panel against the phone's, by name and last line.
6. A line from the board: open a chat, `i`, type, Enter; `[HOME] sent
   id=<8 hex> waiting=0`, the line on the phone, and after the next pull
   the bubble on the board named by the pulled sent entry, one bubble.
   The reverse: a line from the phone arrives on the board at its next
   pull (every 5 min, sooner after a send).
7. Once by keys, for the record: `loquax mailbox admit` again, then on
   a board whose client record was erased (`espflash erase-region
   0x811000 0x1000`, the `loquax_client` partition of
   `leviculum-esp/partitions-m9.csv`), type the 140 characters; `[HOME] code=typed`, then as in 3.
8. Unhappy, by hand: a code typed with one character changed is
   refused on the screen ("the home code has a typo"), Enter does
   nothing; `lnflash --home hello` is refused `REFUSE_VALUE`; with the
   home's daemon stopped the top bar says "Mailbox unreachable" and the
   line `[HOME] failed ... reach=unreachable failures=<n>
   next_in_ms=<30000, 60000, ...>`.

### The board pulls what it shows, in pages its link carries (1439)

The reviewer's window of 2026-11-06 (image `9de91477`) joined Lew's
home and ended with an empty chat list at "Fetched 30", for two
measured reasons: the archive's first 796 of 5103 entries are imported
history (kind 9), which `board::fold` drops, and a LoRa page of 412 B
carries two entries, so the first name at seq 797 was some 400 pages
away over a serial line that carries a page in milliseconds.

**The home serves a board what it folds.** Every paged `/sync` of the
board carries a third element, the capability word `board`
(`wire::Fold`, `loquax/src/home/wire.rs`; `ClientRecord::pull` says it,
`loquax/src/home/record.rs`). The home then serves only the kinds
`board::fold` reads, a message received or sent, a mark, a name
(`wire::BOARD_KINDS`), none longer than 16 KiB (`BOARD_ENTRY_MAX`, a
photo the board neither shows nor holds), and counts only those in
`left`, so "Fetched N of M" is honest. The filter is the query's
(`Store::archive_since_folded`, `loquax/src/store.rs`), so the imported
head costs no page. The phone and the desktop say no word and keep the
whole archive; a home from before reads two elements and serves
everything, as before.

**The page follows the link.** The board's executor fits a pull's page
once its first link is up (`Executor::fitted`, `LinkClass`,
`loquax/src/home/board.rs`), from the link's negotiated MTU and the
next hop's bitrate when it reports one:

| link | how it is known | page the M9 asks |
|---|---|---|
| radio | MTU under 564 (LoRa's 500, an RNode's 508, no MTU said), or a next hop under 100 kbit/s | one response packet, 412 B at MTU 500, never more than the census's page |
| serial to a host's daemon | MTU 564, the KISS/serial HW_MTU (link MDU 495) | a second of 115200 baud: 11 520 B |
| wide (TCP, UDP, local) | MTU 1024 and up | the device's answer ceiling up to the home's page: 64 KiB |

A next hop that reports its rate sizes the serial and wide page to a
second of it, never under one packet. The census still bounds the
radio page as 1363 measured; the answer ceiling, 256 KiB on the M9's
PSRAM, bounds the others. The board's line says the fit on every page,
`link=serial asked=11520`.

**Tests.** `a_board_is_served_what_it_folds_and_the_phone_everything`
(`loquax/src/home/server.rs`): 800 imported entries, then 100 texts, a
delivery state and a photo over the bound; the board walks the 100 in
serial pages with `left` exact on every page, the phone all 902.
`a_fitted_pull_asks_the_page_its_link_carries`
(`loquax/src/home/board/tests.rs`): the board's core node against a
fake home, over a link of MTU 564 one page of 11 520 B and over MTU 500
fifteen LoRa pages for the same 30 texts. `the_page_follows_the_link_class`
and `the_board_word_keeps_every_kind_fold_reads` hold the table and the
word against `board::fold`.

**Zero-config.** Its serial step (step 5 of
[Zero configuration](zero-config.md)) is not built: no auto-attach
sets any mode for a serial board yet. When it lands, a serial LNode
or M9 it attaches gets `interface_mode = gateway`, as the bench's lnsd
needs by hand today.

### An answer that goes silent is cancelled and asked again (1461)

**The silence rule.** A `/sync` page that comes as a resource is
watched from its advertisement on (`Executor::watch_answer`,
`loquax/src/home/board.rs`). When its progress has not moved for the
request bound Reticulum gives every request on the link (six round
trips plus 11.25 s, `request_timeout_ms`; about 12 s on the serial
line), the board cancels it with an RCL
(`NodeCore::cancel_incoming_resource`, `leviculum-core`) and asks the
same page again on the same link. Before 1461 only the resource
watchdog was left: 16 part requests with a growing wait, about 67 s,
past a pull's budget.

**The busy sender.** The RCL is not politeness. A home whose sender
still holds an abandoned answer refuses every new one on that link as a
transfer in progress: without the RCL the board's ask after its
watchdog gave up was refused, and the cell's control timed out at
120 s; with it the page came on the second ask in 11.6 s
(`loquax-android/tests`, the M9 cells that lose a resource answer
whole; `a_resource_answer_lost_whole_is_cancelled_and_asked_again_within_the_rtt_bound`
in `loquax/src/home/board/tests.rs`).

### The board keeps what it pulled (1476)

**The store.** `loquax::home::board::store` (`no_std`) is an
append-only log in the `messages` partition's 1024 erase sectors of
4 KiB, used as a ring. A sector opens with a 24-byte header (magic
`LQXM`, version 1, the sector's sequence, the home's tag, the first
eight bytes of its LXMF address, a CRC-32), then frames written into
erased flash and never rewritten: a length, flags (an entry's first
part, its last, a cursor mark), a CRC-32, the payload padded to four
bytes. An entry is its sequence, kind, time and packed message; one
too long for the sector's room goes on in the next. The board keeps
what `board::fold` keeps of each page, written before the model takes
it, one flash write per sector the page touches, and a cursor mark of
the page's newest sequence, so a page of nothing the fold keeps still
moves the store's end.

**The ring.** A full sector closes; the next one in the ring is erased
and opened under a sequence one past the newest; once the ring is round
the oldest sector is the one erased, and its entries leave the count.
4058 short texts took 152 sectors on the host, so 4 MiB holds about
27,000 of them. A sector of another home's tag is not shown and is
erased in its turn.

**The restore.** At boot `Store::open` reads every header, sorts this
home's sectors by sequence and hands each whole entry to the model
(`loquax_m9::home::restore`), said as `[STORE] sectors= used= entries=
cursor= torn= foreign= restore_ms=`. A power loss leaves at most a
frame that does not check: the scan stops there, the sector is closed,
and the next page opens a fresh one.

**The agree rule.** The store carries the cursor; the client record's
is only what the board believed when it last wrote it. `board::agree`
sets the record's cursor to the store's end at boot and says how they
differed: `store=empty cursor=reset` (a store lost under a cursor that
says everything was fetched: the board pulls again from the start),
`store=behind`, `store=ahead` (a power loss before the record was
written). Since 1463 the record is written only when a code makes it
and when the board joins (`Handheld::take_code` and the `Joined` arm of
`Client::done`, `leviculum-esp/src/handheld.rs`), never for a pull: its
cursor is never read, and the sector at 0x811000 was erased at the end
of every pull that moved it, up to 288 times a day. Tests:
`the_chats_come_back_from_the_flash_store_after_a_reboot` and
`the_store_keeps_what_the_fold_keeps` (`loquax-m9/tests/home.rs`).

### A turn of the loop in bounded time (1463)

**Measured.** The board of 2026-11-07 (image `1d6527e0`, 4067 entries):
`restore_ms=46760`, a key on the chats list 3.15 s to its flush
(`loop_gap_max_ms=3235`) where the flush itself is 56 ms, page gaps
growing from 1.3 to 8 s over the pull, and ring presses made during the
stalls never read.

**Where the time went.** Host profile (callgrind, 2880 entries): of a
list scene, 41 % the tab badge (`order()` and the unread count), 45 %
the list (`order()` again: `Conversation::last_written`, a walk over
the conversation once per comparison of the sort), 14 % the open
chat's bubbles, built and thrown away by the M9's list screen. Every
arrival, pulled sent entry and read mark looked for its id by walking
every message of every conversation. The board runs this code from
flash at about sixty cycles an instruction: the bootloader said DIO at
40 MHz, half the clock the vendor's image runs this flash at.

**What changed.**

- Each conversation carries a summary (`loquax::ui::summary`) that
  `update` brings up to its messages after every event, reading only
  the new tail: the newest times, the unread rows as prefix counts, the
  avatar's appearance, the newest reports for the liveness, and an id
  index. A reader checks the summary was made over exactly the messages
  it sees and walks them as before otherwise. On the host the list's
  scene at 4000 entries went from 1410 to 26 µs, and a page's take is
  flat between 400 and 4000 entries.
- The M9's list screen builds the list scene alone
  (`UiModel::list_scene`), no chat.
- The image is written and flashed for DIO at 80 MHz, as the vendor's
  header on this unit's ZD25Q128 has it (`0x4f`).
- The keyboard is read by a TIMG0 interrupt every 10 ms into a queue
  (`kbraw::Presses`) the loop drains, so a press is never lost to a
  long turn. A timer and not the second core: the poll is a 1 ms I2C
  read every 10 ms, which an interrupt serves without a second stack,
  a second heap user or a core to start.
- A turn longer than 100 ms says `[LOOP] turn_ms= stage=` with each
  stage's share.

**Tests.** `a_turn_costs_the_same_at_4000_entries_as_at_400`
(`loquax-m9/tests/turn.rs`, the full scene as its control),
`the_summary_agrees_with_a_walk_over_every_message` and
`a_summary_behind_its_messages_is_not_trusted`
(`loquax/src/ui/summary.rs`), `a_press_inside_a_three_second_stall_is_kept`
(`loquax-m9/src/kbraw.rs`, the read per turn as its control).

**Open.** The open chat still builds every bubble of its conversation
(about 0.9 ms for 339 messages on the host); a chat body of only its
visible bubbles is the next step. The board numbers (restore, KEY to
flush, page gaps, `overrun` during the store's erase, which runs with
interrupts off) are the reviewer's window.

### The M9 reads by what it showed (1541)

**Before.** On the board "read" was the open (`UiEvent::Open`, 1493):
the board never said `UiEvent::Viewport`, so the line rule of 1529 (a
message is read when its last line stood on screen for the dwell) did
not apply there, and a chat of 1300 entries opened once was seen no
further than the open marks it. The model had no clock either: the board
never said `UiEvent::Elapsed` or `UiEvent::Clock`, and the dwell
(`loquax::ui::receipts`, `SEEN_MS`) runs on both.

**What the draw says.** The chat's layout (`loquax-m9/src/chat.rs`,
`windowed` for a chat scrolled by the line, `bubbles` for the newest at
the bottom) hands back, beside its rows, the keys of the bubbles whose
last line is on screen, by the same rule the terminal uses
(`loquax::ui::pager::last_line_shown`). The renderer keeps them for the
last draw (`Renderer::seen`); a draw of the list, a group's sheet, the
map or the board's own screens keeps none.

**The hook.** `loquax_m9::read::Reader`. After each draw the board's
loop (`leviculum-esp/src/bin/thinknode_m9.rs`, right after `redraw`,
before `drawn.drawn`; the sim's `ui_turn` the same) calls
`Reader::say`: it says `Front(true)` once (the panel is the board's
front while it draws) and `Viewport { peer, keys }` only when the keys
moved, so a frame that showed the same says nothing; a draw without a
chat says the open chat's empty viewport. What it hands back routes on
the next turn, before that turn's sends. The viewport's ask for the page
above the first loaded bubble is dropped: the M9 asks that page by its
own lines (1529), and a second ask would read the same page twice. Once
a second of uptime `Reader::tick` says `Elapsed` and, when known,
`Clock`; both are the model's clock events, which move its revision only
when something shown moved, so they cost no draw.

**The board's day.** The board has no clock for the day. The home stamps
every archive entry with its own time (`Entry::at`); `Reader::saw` takes
the newest stamp of every pulled page and every page read from the ring,
and carries it forward by the board's uptime. That is never later than
the true time and never earlier than the arrival of anything the board
shows, so a seen mark's time falls between the message's arrival and
its reading, and the sender's side keeps a read time within when it sent
and when the receipt came anyway. The model's `now` is set from it, so
the board's list shows its times as the goldens always drew them (in
UTC: the board knows no zone).

**What flows.** The rest is the core's, as on every target: the bubbles
that count, the dwell, and, the board being a device of its home, a
seen mark (`MarkFlag::Seen`) in place of a receipt of its own, routed by
`home::route` as a `/mark` ask and kept in the store as a `KIND_MARK`
entry (`home::keep_marks`), so what was seen stays seen after a reboot
and owes no second receipt. The home signs the receipt.
`UiEvent::Open` stays what it was: the open marks the chat's unread
read (`MarkFlag::Read`), which is the list's count, not a receipt.

**What it costs.** A viewport is one `update` a drawn window whose
seen keys changed: a scroll key, an arrival in view, the open; never one
a frame. The clock is one or two `update`s a second of uptime, no draw.
In flash and on the air, the seen marks coalesce per chat: everything
seen in a chat gathers into one owed mark, sent `BATCH_MS` (5 s) after
the last bubble seen there, so a reader scrolling through a long chat
writes one `KIND_MARK` entry and asks the home once, not once a bubble.
A mark carries up to 64 ids (`MARK_IDS_MAX`) and is split beyond; its
payload is 8 bytes and 34 an id (`wire::mark_request`), plus the store
frame's header. Opening a chat with unread messages also writes the
open's read mark, as before.

**Tests.** `a_long_message_is_read_once_its_last_line_showed_for_the_dwell`
(`loquax-m9/tests/read.rs`): 1529's walk drawn as the board draws; twenty
presses in the long message is not read however long the reader stays,
it is read at the dwell once its last line came on and not a moment
before, the short one under it, on screen only for the moment of the
open, is not, and the seen mark carries the home's time carried forward.
Its control `without_the_viewport_nothing_is_seen`: the same keys
without the hook see nothing. `a_seen_mark_from_the_viewport_survives_a_reboot`:
after a reboot the message seen on the board owes no second seen mark;
its control, the same walk without `keep_marks`, sends it again.

### The M9 on the host: `loquax-m9-sim` (1530)

The board's screens can be walked without the board. `loquax-m9-sim`, a
bin of `loquax-m9` behind its `sim` feature (the library stays `no_std`),
runs the image's UI loop (`ui_main` in
`leviculum-esp/src/bin/thinknode_m9.rs`) through the same library calls:
a key is the controller's code, queued as the controller's answer for it
(`kbraw::answer_of`), read by `Protocol::read` and `Key::of`, run by
`keyboard::run` or `group::press`, routed by `home::route`, drawn by the
board's `Renderer`, said in a `[PAGE]` line by `page::of`. Its I/O side is
the store half of `leviculum_esp::handheld`: the restore at boot, the
asks and marks kept, the summary log, a chat's page read from the ring,
the idle erase. The two cores are one thread, a UI turn and then an I/O
turn until neither queue holds anything.

What it stands in for, and what that cannot show: the panel is the frame
the renderer draws into (no SPI, `[DISP] flush ... ms=0.000`); the flash
is a memory flash of the `messages` partition's 1024 sectors, saved to a
file (no erase timing, no power cut); the keyboard is the code a script
or the window names (no I2C, no lost polls); the heap is the host's (no
PSRAM census); the boot's census of every screen is not run; the clock is
the script's in a headless run, so the `[MAP]` timings read 0. There is
no node and no UART yet: nothing is pulled from a home, an ask stays
queued, and the top bar's word is the one a board shows before its
first ask ends. `--mailbox` says so and exits 2; the client
(`handheld.rs`) is built on the ESP's types, and moving it to the host
changes the image, which this order kept byte for byte.

Headless, as an agent or the reviewer drives it:

```text
cargo run -p loquax-m9 --features sim --bin loquax-m9-sim -- \
    --store real --save flash.img --keys "down enter up up" \
    --reboot-after 2 --dump last.png --dump-every 5 --text
```

- `--store fixture` (the default) is a board without a client record:
  the pairing, Back to the fixture model. `--store real` is a home's
  board whose first pull kept the 1489 fixture (300 conversations, 4000
  messages, `loquax/tests/support/real_store.rs`), pulled page by page
  through `home::keep` and `home::take`. Any other value is a flash image
  a run saved with `--save`, so a second run is a power cycle.
- `--keys` names keys as the `KEY` line does (`ring_down`, `enter`,
  `back`, `map`, `t`) or as the host does (`down`, `esc`, `f5`);
  `--script <file>` takes timed steps (`key`, `hold <key> <ms>`, `wait`,
  `text`, `reboot`, `dump <png>`, `screen <name> <png>`, the last the
  board's `FRAME_DUMP` of a fixture screen). Each press advances the
  clock 150 ms.
- `--reboot-after <n>` power-cycles the board on its flash after the
  `n`th press: "read on the board survives a reboot" runs on the host.
- `--dump`, `--dump-every <n>` (every `n`th frame drawn, as
  `<stem>-f<frame>.png`), `--text` (the screen's rows on stdout, `PAGE`
  then `ROW nn |...|`; the map's picture, pins and names are pixels and
  not in them). The board's lines go to stderr in the firmware's shape.

`just m9-sim-walk` runs the bin's tests and the two walks under
`scripts/m9-sim-walks/` (the fixture from the pairing on, the 1489 store
and a reboot), with the dumps, the log and the text rows under
`target/m9-sim-walk/`. The parity tests reach every golden
(`tests/golden/m9-fw-*.png`, the eleven fixture screens and the pairing)
through the sim's board, by the `FRAME_DUMP` path and, for the four the
board's first model reaches, by its keys from power-on, and compare them
pixel for pixel; the walk's `screen` dumps are byte for byte the golden
files.

In a window, for a hand or an eye: `just m9-sim [store] [scale]`, the
panel at 2x or 3x, the arrows the ring, Enter its centre, Esc Back, F1 to
F5 the bubbles, the house, the square bubble, the pin and the map, the
letters the QWERTY block (`+`, `-`, `t`, `y` the map's), the page and the
last `KEY` line in the title. It builds for the host's own triple, as
`loquax gui` does.

### The map's picture, the boot's census and the bench walk (1554)

**The picture's key is the view and the pins.** Window 13 showed the kept
picture (1507) drawn three times on one open of the map: at the open,
when the key hint's row went after 3 s, when the corner credit went
after 5 s; and Map, Back, Map after 7 s drew it again (`kept=0
picture_ms=142`). The key carried the cells the names kept clear of the
words, and the picture was the shorter one while the bottom row had
words. Now the picture always takes the bottom row and its names keep
clear of the pins only; the hint, the pin's card and the credit are
drawn over the copy (`Renderer::show`, `loquax-m9/src/render.rs:260`;
`Kept`, `loquax-m9/src/map.rs:1096`), so a word that goes uncovers the
picture kept under it. A name under a word stays hidden while the word
stands. `the_words_over_the_map_never_draw_its_picture_again`
(`loquax-m9/tests/map.rs:219`) walks the board's chrome clock: open with
hint and credit, the hint's second page, both gone, Back, Map: one
picture drawn, three copied back, every frame a fresh uncached draw's.
Under the old key it counts two drawn. `Kept::copied` counts copies
instead of draws; a draw's own copy is not counted.

**The census in the reader's size.** 1532's census walked every screen
in both type sizes and core 0 waited 6026 ms for it (window 12: 2857).
A release boot now walks the reader's size only; the bench image walks
both (`thinknode_m9.rs:2856`).

**The walk.** `expect <field> changed` waits for a `[PAGE]` line that
moves the field, up to 1 s after the action began
(`CHANGED_WAIT_S`, `scripts/m9-walk.py:162`): T on the map flushes the
hint's row first and says the zoom about 400 ms later. `key_ms` still
measures the first flush after the key, and walk.md names its `rows=`.
The house key opens the house page after its tap's 300 ms gap
(`GAP_MS`, `loquax-m9/src/house.rs:43`), so its bar is that much longer
(`HOUSE_GAP_MS`, `scripts/m9-walk.py:159`); `bench.walk` waits for
`page=house` and leaves it with Back. An lnsd on the proxy's pty
re-attaches by itself after a flash; a second one is refused with
"shared-instance name already served" (`scripts/m9-walk.sh`).

### A chat opens with its own marks (1556)

**What the open cost.** 1521c left the Enter latency open: the host
decoded a page of 30 entries in 224 µs, the board's `[OPEN]` said decode
858 ms median (window 13). Window 15's line named it before any new
probe: `entries=1761 ... earlier=0` for a chat of under 30 messages.
`Index::page` handed on every mark of the ring from the page's oldest
sector on, every chat's: about 1730 marks read from nearly all 284
sectors (`page_read` 228 ms, also the `[LOOP] take=230` turn at boot,
the cursor chat's read) and folded one by one through the scratch model
on the UI core. The marks explain the size of both numbers; whether the
first-fit heap adds to them is the new `alloc_ms` and `[OPEN_HEAP]`'s to
say on the board.

**A mark files under its conversation.** The index learns each
message's id where the fold already unpacks it (`home::keep`, the
rebuilt boot, and the page read itself, so the board's own read mark
after a resumed boot has a home too, `Index::learn_event`,
`loquax/src/home/board/rows.rs:375`). A mark naming a known message
adds its sector to that conversation's sectors, which the summary log's
ROW record already carries; one naming a message the index never saw
stays in the ring-wide marks (`Index::take_mark`,
`loquax/src/home/board/rows.rs:400`). A page reads the marks of its own
sectors and of the unplaced ones from its oldest sector on, and hands on
only those naming one of its messages (`Index::page`,
`loquax/src/home/board/rows.rs:596`). The STATE record ends with a byte
saying the marks are filed this way (`State::placed`,
`loquax/src/home/board/rows.rs:174`); a state an older image wrote has
none, and that boot folds the ring once (`[STORE] restore=rebuilt`,
why `marks-unplaced`) to place them. Unplaced marks still cost the old
read: those of messages pulled before a resumed boot and not opened
since.

`a_chat_opens_with_its_own_marks_only`
(`loquax-m9/tests/restore_counts.rs:1101`) keeps the archive with 1800
read marks of another chat and a deleted mark of the small chat's: after
a resumed boot the small chat's page is its 5 messages and the one mark
from 8 sectors, its deleted message stays deleted, and the bubbles are
the old page's. Host, release: the old page of 1806 entries decodes in
1057 µs, the new one of 6 in 39 µs.

**The bench's line.** `[OPEN]` now says `messages= marks= sectors=` of
the read, and splits the decode into `unpack_ms` (in `Message::unpack`)
and `alloc_ms` (inside the allocator during the decode and the fold, a
cycle count per core in the bench image's allocator only,
`leviculum_esp::alloc_clock`); `[OPEN_HEAP]` says the heap's free bytes
and its largest block before the decode and after the fold
(`heap_largest`, a halving search outside the clock). The bar is wait
plus decode under 100 ms for the long chat, window 16's to prove.

**An invitation keeps its row.** A group the reader is invited to that
no message reached has the invitation's listing, "Einladung", from when
the invitation came (`UiModel::listing_for`, `loquax/src/ui.rs:2325`);
the index keeps the group's name from the invitation, so both boots
name and list it alike.

**The boot's census.** The release image's census leaves out the map
and its pan (`thinknode_m9.rs:2868`): window 15's boot `[MAP] zoom=15
picture_ms=241` before the list was the census's own pan draw, kept by
no reader's map. The census's map stats are dropped after it. A chat's
draw may keep the renderer's seen record (ea3ffd02), and the next screen
gives it back: `draw_held` +64 / -64 B (+32 / -32 large) is within
`SEEN_HELD` (`loquax-m9/src/census.rs:170`), more either way stays over.

### A pull never starves the panel, a reset never refolds the ring (1563)

Lew's board, its store erased and pulled again from his home (bench
2026-11-08, 13:10): 4258 entries came back, and the board reset by
MWDT1 four times on the way, each `[PREV_RUN] reason=wdt1` with core 1
in `draw`, beside `[DISP] flush` lines of 12,800 bytes taking 2 to
4.4 s. Every boot after a reset folded the whole ring
(`index=rebuilt why=ring-moved`, 12.6 s at 1714 entries, 55.2 s at
4066), so the pull took 472 s and a bigger store would never finish.

**The panel's DMA is owed a gap.** The 1507 arbitration let a DMA out
of PSRAM start only while no flash op was announced. During a pull
core 0 writes op after op, each op parks core 1, and the gap between
one op and the next announce is a few instructions, shorter than core
1's wake: the flush waited as long as the pull wrote. Now a DMA that
finds the flash busy records when it first asked, and core 0 holds its
next op until that DMA started, at most `YIELD_US`, 2 ms, after the ask
(`Arbiter::flash_yields`, `loquax-m9/src/mspi.rs:108`). The protocol is
in `loquax-m9` so the host steps both cores a microsecond at a time
with the bench's numbers: without the yield a five-chunk flush is not
out after a second (`without_the_yield_a_flush_starves_under_a_pull`,
`loquax-m9/src/mspi/tests.rs:109`), with it it is out in 37.8 ms, 5.2 ms
of it waiting (`a_flush_gets_a_gap_between_two_flash_ops`,
`loquax-m9/src/mspi/tests.rs:122`). On the board the flush line says
`waited_ms=`, the wait before a draw is its own stall stage, `flush`,
and a wait that moves feeds MWDT1 once per chunk started
(`Panel::settle_fed`, `leviculum-esp/src/bin/thinknode_m9.rs:1076`); one
that stopped does not.

**A reset reads the tail.** The summary written before a reset stands
for everything before its end. `Store::open_after`
(`loquax/src/home/board/store.rs:252`) reads only the frames after it,
and the boot folds them into the model and the index as the live pull
folded them, then writes the summary at the ring's end
(`resume_tail`, `loquax-m9/src/home.rs:850`); `[STORE] index=summaries`
says `tail=<n>`. A state whose sector went round the ring, or an end the
log does not reach, still rebuilds. On the host the fixture's 4000
entries with a 360-entry tail resume in 22 ms decoding the 360, against
4360 for the whole fold, and list what the whole fold lists
(`a_reset_mid_pull_resumes_from_the_summary`,
`loquax-m9/tests/restore_counts.rs:587`).

That test found a model bug on the way: a chat listed from its kept row
counted only its loaded messages once a live one came in, 96 of 776
unread. The unread beyond the received messages loaded now counts a row
each (`Conversation::unread_rows`, `loquax/src/ui.rs:986`), on a live
pull as on the tail boot.

### The pull starved both cores in the allocator (1569)

Window 16a (bench image of 9f53b300, store erased, 4266 entries
re-pulled) refuted 1563's arbitration on the board: 73 `[DISP] flush`
lines all said `waited_ms=0.000`, yet their `ms=` grew to 3658 — and a
12,800-byte flush is one DMA chunk, 2.6 ms on the wire, so `ms=` was
measuring when the loop's next pump looked, the UI core's turn time.
Both wdt1 resets had core 1 in `drain`; core 0's `[LOOP] host=` grew
from 170 ms to 2473 ms a turn along the pull and fell back to ~230 ms
after each reset — the store unchanged, only the live heap emptied.
The parks cannot say it: the whole pull wrote 417 sector writes and
129 erases, about ten seconds inside `parked` of a 169 s pull.

**The walk.** What both cores share and a reset empties is the heap.
`embedded_alloc`'s llff choice is one free list, walked first-fit on
every alloc and address-sorted on every free, inside the one
cross-core critical section — so every allocation on either core
walks a list that grows with the model's churn, and the other core's
allocations spin on the section meanwhile. The fold itself is flat on
the host (`loquax-m9/tests/turn.rs`, 1463, system allocator: a page's
take 126 µs into 400 entries, 170 µs into 4000). The same fold under
the firmware's own two choices (`leviculum-heap-lab/tests/pull.rs`,
the lab routes the host's global allocator into either): llff 134 µs
at 400, 1919 µs at 4000 — fourteenfold; tlsf 75 µs and 110 µs — flat.
The firmware links tlsf now (`leviculum-esp/src/lib.rs:77`, constant
time both ways, same `critical-section` lock held O(1) instead of
O(walk)). Not through `embedded_alloc::TlsfHeap` since 1574: that
wrapper keeps rlsf's 4228 B control block in its `static`, in `.bss`,
and on the ESP32-S3 `.bss` comes off core 0's stack; the bench image of
1569 fell to 107,100 B, under the 111,000 B floor, and its own build
script refused it. `leviculum-esp/src/pool_tlsf.rs` writes the control
block at the front of the heap region (rlsf's const `Tlsf::new()`, then
`insert_free_block_ptr` on the rest), so the `static` is 24 B and the
stack is back at 111,368 B (release) and 111,332 B (bench). On the M9
the region is PSRAM, where rlsf's free-block headers already were.
`leviculum-heap-lab/tests/placement.rs` holds the placement on the
host, from the same file.

**The instruments stay.** `[IDLE]` says what the flash ops cost the
UI core per window: `park_ms= park_max_ms= park_ops=`, measured around
each op's critical section (`leviculum_esp::flash::park`), and the
bench image adds `alloc_ms0= alloc_ms1=`, each core's time inside the
allocator (`alloc_clock::cycles_of`). The UI core says its own slow
turn as `[LOOP] core=ui ... drain= ... msgs= folded=`
(`thinknode_m9.rs`, 1569): the next starve names its stage and its
queue depth instead of a bare `core1_stage=drain`.

**The list that moves by itself after a boot.** After every resumed
boot the log shows `[PAGE] name=chats`, then a `[STORE] page_read`, a
summary record, the cursor on row 12 and a bench `[OPEN]` with
`keys=0`. That is 1546's preload: the boot's `house::set_size` asks
the model, the model asks the cursor chat's page
(`thinknode_m9.rs:2929`), and the fold runs behind the list
(`behind_the_list`, `loquax-m9/src/home.rs:292`), so no read mark is
queued — `route` takes marks only from `take_marks`, and the hidden
fold queues none. The summary write beside it is the touched row with
its unread as it was (`UiModel::row`, `loquax/src/ui.rs:2388`, the
a80038c2 rule); the cursor moves because the folded page gives the
chat its real activity and the list re-sorts under a selection that
follows the chat. A preload, not an open: nothing to stop for 1562/1564.

### The drain yields to the keys (1575)

Window 16b (bench image of 99b3b6ec, 4266 entries re-pulled) confirmed
1569 on the board, flat `host=` and no reset, and showed what was left:
108 of 111 `[LOOP] core=ui` lines in `stage=drain`, up to 836 ms
folding one page of 234 entries before the loop looked at the keys.
The drain takes the I/O core's messages in order into a backlog and
folds a pulled page through `home::Folding`: entries until 25 ms of the
turn are spent (`home::FOLD_BUDGET_US`) or a press waits in the
keyboard's queue (`kb_waiting`, asked after every entry), each turn's
entries one batch. The rest of the page waits for the next turn, and
nothing behind it is taken before it is folded. The page counts as
folded once, at its last entry, and the rows the turns said are held
(`Folding::rows_due`) until that turn, so the summary log is written
once a page as before. A time, not a count of entries: 16b's turns
folded 234 entries in 836 ms and 55 in 536 to 620 ms, so no count
bounds the turn.

**Where a fold goes, on the host.** `loquax-android/tests/m9_fold_census.rs`
splits a page into the unpack, the model's step, the batch's note and
the route, by time and by allocator calls and bytes: per entry about 10
calls and 0.4 KB for the unpack, 12 calls and 3.4 KB for the step, and
per batch about 380 calls for the note, however few its entries. A
split page pays the note per chunk. The bench image says each page's
split on the board as `[FOLD]` (`thinknode_m9.rs`, 1575), with the
allocator's time, calls and bytes on the UI core.

**Shown on the sim and in a model test.** `loquax-m9/tests/drain.rs`
plays the loop over a test clock at 3.6 ms an entry: the longest turn
25.2 ms against 842.4 ms unbudgeted. The sim's `--store pulling`,
`--fold-cost-us` and `pull <pages>` (`scripts/m9-sim-walks/pull.walk`,
`just m9-sim-walk`) press the ring every 150 ms through twelve pages of
234: `key_ms` at most 2 against 819 without the budget. The bench walk
holds a `[LOOP] core=ui` line's `drain=` under 100 ms
(`home::DRAIN_BAR_MS`), a dump's and a boot's own turns exempt.

### The bench never touches a real home (1562)

On 2026-11-08 the last few dozen messages in Lew's real chats on the
M9 were called `bench arrival <n>`. Two bench mistakes were behind
them, and both are now closed by how the code is built, not by care.

**Arrivals live in "Bench".** 1540's `TYPE_BENCH_ARRIVE` wrote the
synthetic message into the chat at a row of the list and kept it in
the flash ring like a pulled page. Nothing went out, but it stayed in
his chats across every boot. Now every arrival comes from one
synthetic peer, `BENCH_PEER` (`loquax-m9/src/bench.rs:32`). Those are
sixteen ASCII bytes, and no identity's SHA-256 hash can be made to
equal them. The arrival goes into one conversation the list calls
"Bench", and its signature field holds the bench's flag
(`bench::SIGNATURE`). The frame no longer carries a row. Rules:

- **A release image** restores with `Arrivals::Dropped`. The rebuild
  and the resumed tail skip the bench's entries, and a summary's
  "Bench" row and name are not listed (`listed`,
  `loquax-m9/src/home.rs:981`). A release image never shows "Bench".
- **Read marks.** The board leaves the bench's message ids out of the
  marks it sends its home (`route`). It keeps the read on the board
  itself.
- **`bench forget`** (`TYPE_BENCH_FORGET` 0x1D, bench image only)
  takes the following out of the ring (`home::forget`,
  `loquax-m9/src/home.rs:1149`, over `Store::forget`,
  `loquax/src/home/board/store.rs:286`):
  - the flagged entries;
  - 1540's unflagged entries, which have a zero signature and text
    starting `bench arrival ` (`is_unflagged`,
    `loquax-m9/src/bench.rs:160`);
  - read marks that name nothing else.

  `Store::forget` rewrites only the sectors that hold such a frame,
  keeping their other frames verbatim. The summary log is then
  erased, and core 0 rebuilds the store and hands the UI core the
  new model. The board does not reset, so no `[PREV_RUN]` record
  appears. A power cut between a sector's erase and its rewrite
  loses that sector's entries, the same as a ring that went round.

`scripts/m9-walks/forget.walk` is the clean-up. It opens no chat, so it
runs on a board joined to any home.

**The bench pairs with a mailbox of its own.** The walks of windows 11
to 15 ran on Lew's M9 joined to his mailbox. They opened his chats, and
the board sent read marks for messages he never read (`HOME mark read
device=940b1536…` at 09:06). `scripts/m9-bench-home.sh` keeps a
`loquax mailbox` for the bench on the bench host:

- `setup` gives it an identity of its own and the 1489 fixture as its
  archive (`loquax-m9-sim --seed-home`,
  `loquax-m9/src/bin/loquax-m9-sim/seed.rs:30`), written under that
  identity's address.
- `run` starts it beside lnsd.
- `join` writes a fresh code that this mailbox issued to the board.
- `send` delivers a live arrival from the reference LXMF client.

The board's `[HOME] client=yes` line names its home's address
(`home=`). `m9-walk.py` refuses a walk that opens chats (Enter or
`open`) before its first key unless that address is the bench home's
(`refusal`, `scripts/m9-walk.py:1009`). A board without a home, and a
walk without opens, run as before. Lew gets his own pairing back with
`lnflash --home` or a code from his mailbox. The two never mix.

### A name's collision is a lookup, not a walk over every name (1581)

Window 17 (2026-11-08, the bench image of 1562) pulled the 1489 fixture
into the board from the bench home: 4379 entries, 276 conversations. The
first pull took 28 min with 91 `TG1WDT_SYS_RST`, every record
`core1_stage=draw`. The UI core's `[LOOP]` line said `draw=` 833, 2278,
2942, 4021 and 4967 ms as the entries came in, and after the pull a key
on the list still took about 4 s. The 16b window, Lew's data with 16
conversations, pulled in 110 s without one reset.

**Measured on the host first.** `loquax-m9/examples/list_measure.rs`
cuts the fixture to its first `n` conversations (copied under other
peers past its 300), pulls it into a board's model page by page
(`home::take`), and times a key down the list and every step of the
draw that answers it (`cargo run --release -p loquax-m9 --example
list_measure`). The list's sort, its window, the glyphs and the blit
were flat; what grew was the window's rows built afresh, and of that
the names (microseconds on the host, median of 21):

| conversations | key | rows built | of them, the names | scene | draw |
|---|---|---|---|---|---|
| 16 | 55 | 109 | 33 | 4 | 136 |
| 64 | 168 | 149 | 130 | 5 | 110 |
| 128 | 357 | 331 | 285 | 6 | 111 |
| 276 | 805 | 751 | 712 | 8 | 105 |
| 1000 | 4088 | 3962 | 3866 | 22 | 123 |

**The mechanism.** `Names::collisions` (`loquax/src/names.rs:780`) took
the folded name, a `String` built for it, of every other named peer,
on every call. A list row calls it twice, for its name (`Names::label_in`,
`loquax/src/names.rs:861`, from `chat::list_row`) and for its "two
contacts are called" sentence (`Names::same_name`,
`loquax/src/names.rs:879`, from `scene::row`). The M9's window is 32
rows, so one window over 276 conversations allocated about 17 600
strings. A fold builds the window again whenever a page moves a row,
a key moves the selection or the reader's viewport is said after a draw
(`read::Reader::say`, inside the draw stage on the board), and every
pulled message's fold asks for a label too (`on_engine`). On the host
that is microseconds; on the board each allocation crosses the
allocator's lock with core 0 busy writing the pull, which is the
`alloc_ms1` of up to 1789 ms per 5 s window 17 measured. The draw
stage outlived the 5 s watchdog.

**The fix.** The names keep an index of who looks like whom
(`Looks`, built once by `build_looks`, `loquax/src/names.rs:812`):
every named peer under its look, each peer's place in it, and the met
peers without a name. A row's question is a map lookup and allocates
nothing. Every `&mut self` change of a heard, linked or met name drops
the index (`meet` and `part` update the nameless set in place, an
announce of the same name keeps it), and the petnames it was built
under are compared on every question, because the contacts are a public
field changed without a word to the names. The names' share of the
model's look fingerprint is an edition number taken afresh on each such
change instead of a hash over every name (`Names::fingerprint`,
`loquax/src/names.rs:718`), the next O(n) in the profile. After:

| conversations | key | rows built | of them, the names | say | tick |
|---|---|---|---|---|---|
| 16 | 30 | 98 | 1 | 3 | 2 |
| 276 | 60 | 20 | 1 | 29 | 14 |
| 1000 | 151 | 36 | 1 | 108 | 54 |

What is still linear is the batch's note over every conversation
(`UiModel::note_changes`: each row's fingerprint and summary) and the
order cache's check, hashing without allocating, about 0.15 µs a
conversation on the host. The pull of 1000 conversations went from
1840 ms to 279 ms on the host.

**The bench's side items.** A chat the keys open whose page the model
held already (the bench's own chat, its arrivals folded as they came)
says its `[OPEN]` line too, with nothing read (`Open::held`,
`thinknode_m9.rs`); the hang walk of window 17 had one line for 38
opens. `open longest` in `bench.walk` said "no rows" because
`cursor_to` took a board behind its keys for the list's end: the key's
flush came, its `[PAGE]` line four seconds later. It now waits for that
line (`cursor_to`, `scripts/m9-walk.py:826`); the fixture's first rows
hold the group and Anna (the sim's `--store real` list). The mark guard
has its positive control, `scripts/m9-walks/marks.walk`: a real chat
with an unread message (`m9-bench-home.sh send` right before) opened
must say `HOME mark` in the bench home's own log, a Bench open must not
(`expect [no] home`, `scripts/m9-walk.py:957`).

### The M9's own stack floor, measured (1582)

Until 1582 the image script held core 0's stack to 111,000 B, the
LNode's node peak (#50), not anything measured on the M9; 1521c left
that open. The images sat between 8 and 368 B above it, and 1569's
fell under it (its TLSF control block in `.bss`). The floor is now this board's own.

**What the board measured.** Core 0's `[UART] stack_free=` is the
painted stack never touched since the session census, so its depth is
the `[STACK] bytes=` less the 512 B guard less `stack_free`. Read off
every boot of the rig's proxy log since 2026-11-07 (`rig-run/m9/
2026-11-07h/board.log`), the bench image of 1581 (2f5caa4a, TLSF,
111,008 B of stack):

| load (window 18, 2026-11-08) | stack_free | core 0 depth |
|---|---:|---:|
| boot to the list, idle on the pairing | 72,624 | 37,872 |
| the join from an erased store: `[HOME] code=taken`, `restore=rebuilt`, the first 330-entry page of the 4345-entry fixture pull, folded | 31,904 | **78,592** |
| the rest of that pull (177 folds) | 31,904 | 78,592 |
| a resumed boot of the pulled store, 4345 entries | 44,144 | 66,352 |
| bench walk: list, chat pages, map, zoom, house, reboot, ten frame dumps | 44,144 | 66,352 |
| marks walk, hang walk (fifty opens with the flash owed) | 43,780 | 66,716 |

1575's image (4351fa75) showed the same shape the evening before:
77,648 B at its join, 65,552 to 66,076 B for every resumed boot and
pull after. The deep moment is the join, not the fold: the 5 s window
that holds `code=taken` takes 40 KB off the paint, the window of the
pull's next 4000 entries nothing more. No window ran a bench `forget`:
its depth is not measured. Its code has no symbol of its own (inlined
into `main`), so the static chain from `main` below covers it.

**What the image says it can reach.** `scripts/m9-stack-frames.py`
(`just m9-stack-frames`) reads every `entry a1, <frame>` of the linked
ELF, plus the `l32r`/`sub`/`movsp a1` that grows a frame past entry's
reach, and walks the calls (`call8`, and `callx8` through an `l32r`
literal, which is how the Xtensa backend calls far). Calls through a
vtable or a fn pointer are not followed, cycles and the panic paths are
cut. On 2f5caa4a:

- the largest frames: `Handheld::client` 32,832 B (one frame, a third
  of the deepest chain), `main` 22,464 B, `UiModel::content_scene_of`
  12,880 B, `ui_main` 11,248 B, `home::rebuild` 10,992 B;
- core 0's deepest chain, 88,624 B: `main` → `Handheld::take_code` →
  `Handheld::client` → `home::rebuild` → `home::model` → `map::ready` →
  `ui::update` → `refresh_scene` → `scene::chat_in` → the bubbles →
  `Names::collisions` → a sort. That is a join or a rebuild while a chat
  is open, which the bench did not drive; the measured 78,592 B is the
  same path down to the list;
- the UI core's, 62,288 B: `ui_main` → `fixture::Screen::walk` (the boot
  census) → `group::press` → `screen::scene` → `content_scene_of` → the
  same bubbles;
- the interrupts: the deepest handler is the keyboard's `kb_tick`,
  928 B (an I2C read inside the interrupt), on top of the 256 B
  exception frame and 128 B of dispatch.

**The rule.** A core's floor is the deepest use measured on this board
plus 16 KiB. For core 0 the reserve holds the static chain's 10,032 B
past the measure, two nested interrupts (2 × 1,312 B), and 3,728 B for
the indirect calls no static chain sees: 78,592 + 16,384 = **94,976 B**
(`IO_STACK_NEED`, `scripts/build-m9-image.sh`, with this provenance in
its header). `just m9-gates` on 6d53d28c (1581's lines at 2f5caa4a
were the same two stacks: the UI watch's two statics cost none):

| image (6d53d28c) | stack | old need | headroom under it | need now | headroom now |
|---|---:|---:|---:|---:|---:|
| release | 111,124 | 111,000 | 124 | 94,976 | 16,148 |
| bench | 111,008 | 111,000 | 8 | 94,976 | 16,032 |

The lever for more is one frame: `Handheld::client` holds 32 KB of
locals across the join.

**The UI core.** Nothing on the board read its stack before 1582; its
floor was 184,000 B, the host census's ceilings added up (64-bit, debug
build). The UI core paints its stack after its census and core 0 says
`ui_stack_free=` on every `[UART]` line (`unset` before the paint).
Window 19 (1594, bench image of fbe1e3f6, 2026-11-09) measured it under
the pull, the bench, hang, forget and marks walks: the least untouched
was 138,784 B, in `marks-try1` with a Bench chat opened on a fresh
arrival, so 49,120 B used under the 512 B guard, and the floor is that
plus 16 KiB, 65,504 B (`scripts/build-m9-image.sh`). The static chain
of `ui_main` (62,288 B) lies inside it.

| image | ui stack | old need | need now | headroom now |
|---|---:|---:|---:|---:|
| bench (fbe1e3f6) | 188,416 | 184,000 | 65,504 | 122,912 |

The stack array is not moved yet. The 122,912 B above the floor are
internal DRAM, the memory core 0's stack is made of (30,848 B free at
a join in the same window). The recommendation: give core 0's stack
about 64 KiB of it, which takes its join margin from 30 KB to about
95 KB, past the deepest static chain (88,624 B) that no walk drove yet,
and leave the rest to the UI core's stack until a window measures the
two-pool heap's UI load; heap does not need it, the heap is PSRAM and
has megabytes free.

### The forget's reopen ran out of heap (1603)

Window 22 (bench image of 51b80f57, 2026-11-09): twelve seconds into the
rebuild after `bench forget`, core 0 panicked on `memory allocation of
278528 bytes failed`. The block is 2048 x 136 B: the long chat's
`Vec<Message>` doubling past its 1024th message (`Message` is 136 B on
the image, the 1489 store's long chat has 1200). It failed because the
reopen held a second whole model in the IO pool: the UI core's, every
message of the ring after the boot's own rebuild, and the old client's
shelf. Now the UI core is handed an empty model first and frees its own
(`Handheld::bench_release`, `leviculum-esp/src/handheld.rs:980`), and the
old client is dropped before the restore starts (`Handheld::bench_reopen`,
`leviculum-esp/src/handheld.rs:964`); the sim's `bench forget` frees in
the same order.

Two blocks of the rebuild grew with the ring whatever the chats were,
and both went: the batch's note scope held one peer per folded entry
(8192 x 16 B), now each conversation once (`Batch::fold`,
`loquax/src/ui.rs:3625`); and every received message left a toast that a
board, which never ticks, never aged out, now at most `TOAST_KEEP`
(`loquax/src/ui.rs:196`). The model after a rebuild holds about 470 KB
less on the host. `a_rebuild_of_short_chats_asks_for_no_block_that_grows_with_the_ring`
(`leviculum-heap-lab/tests/rebuild.rs:173`) rebuilds 4400 entries under
the firmware's TLSF heap and holds the largest block to 64 KiB: red at
163,840 B (the toasts), green at 51,712 B, the same as at 1100 entries.
What remains large is the model's shape, which the live pull shares: a
chat's messages (2048 x 136 B for the long chat,
`a_rebuild_with_a_long_chat_asks_for_that_chat_alone`) and the
conversations themselves (512 x 560 B on the image for 302). Holding
those in pieces is a change of the model's storage on every target, not
of the rebuild.

The rebuild says its progress, `[STORE] rebuild chunk=<k>
entries=<n> ms=<t>` every `REBUILD_CHUNK` entries
(`restore_chunked`, `loquax-m9/src/home.rs:713`).

**The boot's rebuild pumps the line.** The boot's restore fed nothing
(the watchdog is not armed yet), so its 25 s lost what the 8 KiB keep
could not hold (`rx_lost=7916`). Its clock now pumps the keep to the
heap at every step, as the forget's reopen and a taken code do
(`leviculum-esp/src/bin/thinknode_m9.rs:2077`). The held bytes live in
2 KiB blocks, at most 35 s of the line (`RxHeld`,
`leviculum-nrf/serial-mux/src/rx.rs:126`): a doubling buffer would ask
the pool for 256 KiB in one piece, and 22 s was shorter than the
rebuild. `a_rebuild_at_line_rate_loses_nothing_when_pumped_per_entry`
(`leviculum-nrf/serial-mux/src/rx.rs:339`) runs the line at its full
rate through 25.5 s: unpumped it loses, pumped per entry it keeps every
byte in order.

**A reset after a rebuild resumes.** After a rebuild the summary log was
written one bounded write a quiet turn, a second apart, its state last,
33 s on the board; the walks reset sooner, and every boot rebuilt again
(`why=no-state`). The rebuild now writes the whole summary before it
returns, in bounded writes with the caller's clock between them
(`SETTLE_CALLS`, `loquax-m9/src/home.rs:465`), and the board says
`[STORE] summary settled records= writes= erased= calls= left=`. A
rebuild runs where nothing draws from the model yet (the boot, the
forget's reopen, a code taken), so 1597's spacing, which was for the UI
core, is not owed there. A reset during the rebuild itself still
rebuilds. `a_reboot_right_after_a_rebuild_resumes_with_the_same_list`
(`loquax-m9/tests/home.rs:420`) was red on both halves (the summary left
owed, the reboot `rebuilt`).

**A dump is its own stage.** Core 0 wrote a screen dump's lines inside
the take stage, so the walks read takes of 1 to 6 s. The dump is now
timed apart: stall stage `dump`, and `dump=` in the `[LOOP]` line beside
`take=` (`leviculum-esp/src/bin/thinknode_m9.rs:2580`); `m9-walk.py`
takes `dump=` as the dump's turn and holds the rest of that turn to the
bar.

### The M9 holds no screen it does not draw (1612)

Window 23 (bench image of 2a3c0c41, 2026-11-09) stood five minutes on a
chat in the hang walk: 41 of 99 Enter and Back presses answered. The
keys were never deaf. One Back frame was lost on the line before core 0
took it (`keys_rx=` stayed at 41; why the frame went is the UART FIFO
during an erase, 1613). The walker's next Enter then came inside the
open chat, where the shared key table's Enter opens the message under
the cursor (1076), its detail view (`loquax/src/ui.rs:5708`). The M9
draws no detail view and its `[PAGE]` line called it `chat`, so the next
Back closed what nobody saw, and every Enter and Back after it did the
same: each one a `KEY` line the UI core wrote itself, no page change. The
two window 23 probes that seemed to clear "Enter in a chat" and "an
arrival in a chat" opened another chat with no message (`[OPEN]
peer=f18f4927 messages=0`), where Enter has nothing to open. `:` on the
M9's keyboard opened the palette the same way.

Now every key the M9 handles ends with `close_unshown`
(`loquax-m9/src/group.rs:322`): the message's detail, the profile, the
settings, the help, the palette and the copy mode are closed again at
once and said as `[UI] notice="unshown=<state> closed"`.
`back_leaves_the_chat_after_an_enter_inside_it`
(`loquax-m9/tests/unshown.rs:64`) was red on the detail,
`no_key_leaves_a_screen_the_m9_does_not_draw`
(`loquax-m9/tests/unshown.rs:85`) on the palette; the sim's replay of
the walk's sequence left the chat on the second Back before, on the
first after. Drawing a message's detail on the M9 is a screen of its own,
not built.

The board now counts both ends of a key: `keys_ui=` beside `keys_rx=`
in the `[UART]` line (`leviculum-esp/src/lib.rs:756`), the presses a UI
turn read into a `KEY` line, and a press that reaches no turn is a
`KEY_DROP reason=queue_full|malformed|value` line. A walk's `idle` step
holds them to the walk (`check_keys`, `scripts/m9-walk.py:668`): the
frames sent since the boot against `keys_rx=`, and `keys_ui=` no less
than `keys_rx=` (more is a hold's repeats or a stray code of the
controller's). `scripts/m9-walks/chatkeys.walk` and `arrivechat.walk`
are the probes again, opening "Bench" so each Enter stands on a message.

### A key frame survives a flash erase (1613)

Windows 21 to 23 pinned the loss 1612 saw on the line: a flash op runs
with core 0's interrupts off — esp-storage wraps every ROM call in its
own interrupt-masking lock (`esp-storage-0.10.0/src/lib.rs`,
`maybe_with_critical_section`), so the mask is not ours to lift — and
with the cache off, so the UI core was hardware-stalled too
(esp-storage's AutoPark writes the RTC's `SW_CPU_STALL` clock stall:
the core is frozen, no interrupt runs on it). For the whole op no CPU
anywhere could take the UART FIFO: 128 bytes, 11.1 ms of the line at
115200 baud, against erases measured per 5-second window over three
pulls (`park_max_ms` in `rig-run/m9/2026-11-09{c,d,e}/pull.log`):

| window | ops' windows | median max | largest |
|---|---|---|---|
| 21 | 147 | 45.3 ms | 58.6 ms |
| 22 | 110 | 45.0 ms | 64.3 ms |
| 23 | 110 | 45.2 ms | 60.0 ms |

A 45 ms erase carries 518 bytes at line rate, a 64.3 ms one 741; the
FIFO keeps 128 and the rest fell. The hang walks lost 4, 2 and 1 of
~100 injected key frames (10 bytes each, `m9-walk.py`'s `key_frame`),
every loss inside an erase, `overrun` rising with it. 1595's
pre-op FIFO drain and 1597's counters shrank and named the loss; they
could not close a window longer than the FIFO.

The options of 1613, each against those numbers:

- **(a) leave the interrupt mask:** the mask sits inside esp-storage,
  and it is right — an interrupt through flash code would crash on the
  dead cache. Lifting it means forking esp-storage AND proving every
  core-0 handler and all it reaches IRAM-resident; the set includes the
  TX ring, which 1507 put on the heap, which is PSRAM, behind the same
  dead cache. The set is open-ended (it grows with every feature) and
  partly unreachable by construction. Excluded.
- **(b) UHCI DMA into internal RAM:** esp-hal 1.2.2's `uart::uhci` is
  one-shot, and nothing can restart it mid-op — core 0 is masked, core
  1 stalled — so the restart gap is the rest of the op, up to ~60 ms
  against a 0.87 ms key frame. Worse, its end-of-frame triggers (line
  idle, a 0xC0 separator our HDLC payloads may carry unescaped, the
  packet threshold) can end the armed transfer *during* the op on
  data- or timing-dependent input, falling back to the same 128-byte
  FIFO. It lowers the probability; it cannot reach "loses none".
  Excluded.
- **(d) a larger FIFO plus shorter erases:** `rx_size` could grow to
  512 bytes at most (the S3's 1 KiB UART RAM is shared by three
  UARTs, and UART1 carries the GNSS receiver) — 44.4 ms, under the
  45 ms *median* and far under the observed 64.3 ms. And the erase
  cannot be shortened: one 4 KiB sector is the chip's atomic erase
  unit and our ops already erase one sector per masked window.
  Excluded by arithmetic alone.
- **(c) input taken on the UI core — chosen, in the form the stall
  made necessary:** the S3's park is a clock stall, so no handler runs
  on a parked core; instead the UI core now parks *itself*
  (`leviculum-esp/src/corepark.rs`). A flash op raises FROM_CPU_INTR2;
  the UI core's handler acknowledges, clears its own INTENABLE, and
  spins in internal RAM polling both receive FIFOs (UART0 host, UART1
  GNSS) into two rings until core 0 says the op is done. One popper is
  always live: core 0's interrupt whenever its interrupts are on, the
  parked spin exactly while they are off.

What makes the spin safe where the stall was needed: the handler is
*entered* while the cache is still on (core 0 waits for the ack before
the op starts, so esp-hal's dispatch through flash tables runs on
readable flash), and from ack to done it touches only its own
`.rwtext` code and literals, two rings and three handshake words in
internal `.bss`, and peripheral registers, with INTENABLE zero so
nothing else can dispatch. That is option (a)'s completeness proof at
a provable size: the set is one function, and
`scripts/build-m9-image.sh` checks the linked ELF on every build —
`park_ipi` whole inside `.rwtext` and zero call or indirect-jump
instructions in its body (removing `#[esp_hal::ram]` turns the gate
red; the same scan counts 1034 calls in `main`).

The protocol cannot recreate the 2026-11-07 hang (1534): the park
interrupt cannot land inside a held critical section (acquire disables
the core's interrupts first), so the UI core is never parked holding
the section, and core 0 holds no lock while waiting for the ack. The
section is therefore no longer held across the op at all; only the
fallback — core 1 not armed yet, or not answering within 20 ms — still
stalls the core the old way, under the section, and is counted
(`park_fallback=` beside `park_coop=` on the `[IDLE]` line).

Sizing, from the table: the host ring is 2048 bytes, 178 ms of the
line, 3.1 times the 57 ms design bound and 2.8 times the worst
observed window; the GNSS ring 1024. Both live in internal `.bss`
(3 KiB against core 0's measured 75 KiB of stack headroom,
`STACK_HEADROOM stack=170820 need=94976`). A ring full anyway counts
into `rx_lost=` instead of hiding.

Proven on the host before the board:
`a_parked_poller_rides_a_57ms_mask_at_line_rate_without_loss`
(`leviculum-nrf/serial-mux/src/park.rs`) drives key frames between
node frames back to back at line rate across a modelled 57 ms mask —
every byte arrives, in order; its positive control
`the_fifo_alone_loses_at_every_size_the_uart_ram_allows` pins today's
loss (528 bytes at 128, 272 at 384, 144 at 512) so the model cannot
pass vacuously. The byte order holds because the handoffs bracket the
window: core 0 masks its interrupts and sweeps the FIFO *before* the
park, and harvests the rings into the keeps *before* it unmasks.

The bench image now says every masked window as `[FLASH] op=erase|
write ms=<n> at=<ms>` (`flash::park::take_records`), the per-op-kind
distribution 1613 asked for first. The bar on the board (window 24):
`keys_rx = keys_ui = presses` through a hang walk and a forget walk,
`park_fallback=0`, and no `overrun` that counts a key frame. The
proxy keeps injecting at full rate — a pacing retry would be the
bench's crutch, and the product's line (lnsd under announce traffic)
does not pace.

### The flash ring counts what it loses, and the boot parks too (1622)

Window 24 proved 1613 on the board (100 of 100 keys, `overrun=0` over
the pull, `park_coop=8606 park_fallback=0`) and left three things.

**The `[FLASH]` ring.** It held 32 records and is drained once per
loop turn, so a turn of more ops lost the rest unsaid: the forget's
128 erases came out as 7 lines. The ring is now `loquax_m9::oplog`,
1024 records (two words each, 8 KiB of the bench image's internal
RAM; the release image keeps none), and a drain that still finds
records overwritten says `[FLASH] lost=<n>`, the `[IDLE]` line
`flash_lost=` per window. `loquax-m9/tests/oplog.rs` runs the real
forget and the rebuild over a 1489-shaped store in the board's 896
sectors, counting ops as the board does (an erase one, a write one
per 512-byte chunk): 571 ops in the turn, of which the old ring says
32 and counts 539 lost, the new one says all.

**The boot.** A rebuild boot counted `overrun=2` before its first
`[IDLE]` line, in windows 23 and 24: the restore ran before the UI
core was started, `corepark::hold` had nobody to park, and every erase
masked core 0 with no popper anywhere. The UI core now starts before
the restore, arms the park as its first act and waits for its model
(`UI_START`); core 0 waits for `corepark::armed` (`[CORES] park=armed
waited_ms=<n>`), restores, hands the model over, and waits for the
census as before, which still runs alone. Draining the FIFO between
sectors instead could not reach zero: the erase of one sector is the
masked window, and 1613's arithmetic gives even a 512-byte FIFO 44 ms
of it. The first `[UART]` line of a rebuild boot is window 25's bar:
`overrun=0`.

**The two turns over 250 ms in hang.walk** (`turn_ms=258`, `take=216`
and `take=212`) are the bench's owe: `bench owe` closes the ring and
the summary log, and the next open's mark and the two summary writes
each open a sector with an erase, four erases of 34 to 42 ms and 23
write chunks in the take stage (the `[FLASH]` records after the turn).
Window 23 had the same take (230, 232, 222 ms). What tipped them over
250 is the host stage, 40 and 41 ms in those turns, which both windows
show in turns that open a chat (window 23: 38 to 105 ms beside an owe). The popper's harvest is inside the
take stage, in `hold` before core 0 unmasks, and is bounded by the
rings' 2048 and 1024 bytes; the `[FLASH]` record of an op includes it,
and the erases it timed (34 to 42 ms) are the bare erases window 23
measured.

### The flash records drain as the line carries them (1630)

Window 25 met 1622's bars but one: the forget's `[IDLE]` line said
`park_ops=550` and the `[UART]` line `tx_drop=389
tx_drop_kind=line:388`, and 162 `[FLASH]` lines arrived, 550 minus
388. The loss had moved from the op ring into the send ring: one drain
wrote 550 lines of 45 to 50 bytes into a ring of 8 KiB and 512
records that UART0 empties at 11.5 bytes a millisecond. The same
drain sat at the top of the loop, and the rebuild boot's first turn
(not yet lossy, so it waits for room) wrote its 355 boot records
there, filled the ring, and the census's screen lines behind them in
the take waited 299 ms for it (`turn_ms=328 stage=take`), the 700 ms
between `census=done` and the `[CENSUS] screen=` lines being the full
ring's 0.7 s of line.

Now the drain is bounded and runs at the turn's tail, outside the take
(`[LOOP]`'s `flash=`): `OpLog::take_at_most` hands out no more than
`oplog::sayable` allows for the send ring's free room past a reserve
of half the ring (4 KiB and 256 records, pinned against `TX_RING` and
`TX_RECORDS` at compile time) in records of the worst-case 72 bytes
(`RECORD_MAX`), so neither the records nor the loop's own lossy lines
after them make the ring drop. What does not fit waits in the op ring
for the next turn; 550 records of 48 bytes leave in about 2.3 s of the line. It
runs every turn, not only quiet ones: a pull's turns are busy, and the
op ring would overflow behind them. `loquax-m9/tests/oplog.rs`
(`a_forget_drains_whole_over_turns_and_the_send_ring_drops_nothing`)
drains 550 records into the serial mux's own `TxRing` at the line's
rate: all said in order, none dropped; drained whole as before, the
same ring drops 427.

`[LOOP]`'s `host=` has parts since: `read=` (the line's bytes read and
deframed), `pkt=` (packets handed to the node) and `ctl=` (the rest:
control verbs and frames), for the two chat opens a window that cost
`host=` 40 to 44 ms.

### Tracks on the M9 (1385, not built)

The tracks ([Loquax: tracks on the map](loquax-tracks.md)) are derived
in the `no_std` core (`loquax::ui::tracks`, the rules and the budget) and
drawn by the frontends; the M9's map slice (e) is not built, so the M9
draws none yet. What it needs there: **1 px lines**, the panel's 320 by
240 at its density gives no room for the pixel screens' 3 px; the
**blend into Rgb565**, one track's coverage kept as the strongest a pixel
gets and blended once into the frame, as `screenmap::draw_tracks` does
into RGBA, with the hue's 5-6-5 value and the age step's opacity (the
lighter hue set on the dark map); and the **decimation** at the panel's
zoom, which the core already does by the screen pixel passed in, so a
year of fixes costs at most 2 000 segments and 176 blobs a peer. What it
does not need is the store: the board holds the fixes it hears in the
model's per-peer list, 32 bytes a fix, so the census has to set how many
it keeps (a day at a 5 min cadence is 288 fixes, 9 KB a peer). The time
window maps onto the key table as `w` does in the terminal.

The M9 is where the **radar** (1386) matters most: a board in a pocket
off-grid will rarely hold a region. `loquax::ui::radar` is `no_std` and
gives the rings, the step and each pin's "1.3 km NE" from the pins
alone; the panel draws the rings as 1 px circles in the muted grey and
the note on the status line. The **way back** costs the board its own
fixes at a minute's cadence, 1 440 a day, 45 KB at 32 bytes, so the
census decides how many days it keeps. **Late** segments dash at 1 px
(3 on, 2 off); the **battery** mark is a 7 by 4 px outline; **fit all**
is a key of the map's table beside `w`.

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
session census, measured on the host), of slice six (1366, the group
screens, their keys and the group verbs asked of real homes) and of
slice seven (1375, the map measured and drawn in pixels, its pins and
keys). The
client's record in flash and the board's executor over `leviculum-core`
are built and in the image since 1414 ("The board's node and the
client's board half"); the code typed or written, the join, the pull
drawn and the send are in it since 1415 ("The M9 joins its home"). The
board has not run any of them yet, and its message store in flash is
not built. Of the 2026-10-21 list that stood
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
