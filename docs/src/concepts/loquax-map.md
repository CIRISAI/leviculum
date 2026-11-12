# Loquax: the map

Part of the [Loquax design record](loquax.md). Lew, 2026-10-27: the vector
map is part of Loquax. Messenger and map belong together because telemetry
travels over the same LXMF channel as the messages, and the user wants to
see the positions inside the messenger. The map comes to the desktop
Loquax, its terminal frontend and the planned iced GUI, not only to the
board. [A messenger whose core runs anywhere](handheld-messenger.md), "The
map", already calls the map a product of its own and a view inside the
messenger, on the desktop and embedded. This page plans it in slices.

**Status: decided 2026-10-27 (Lew), all five points of section 7 as
proposed.** Slice (a), positions in the model shown as text, is built
(2026-10-27, leviculum 1059, commit `230c5e4d`); slice (b), the map core
`leviculum-map`, the region tool `leviculum-map-tool`, the region file and
its size measurement, is built (2026-10-27, leviculum 1062); slice (c),
the terminal's map, is built (leviculum 1065); slice (d), the window's map,
is built (2026-10-28, leviculum 1084); slice (e), the M9's, is not. The measurement it rests on is
`docs/drafts/map-measurement-2026-10.md`; the decisions it rests on are the
toolkit ([A GUI toolkit for the desktop and the M9](gui-toolkit.md),
option B) and the vector path with our own filler (handheld-messenger.md,
"The map", 2026-10-25). Tracked under #232 (the messenger core) and #474
(the map).

## What Loquax receives of telemetry today

Since slice (a), every position. The engine passes an incoming message's
fields on (`Message::fields`, `leviculum-lxmf/src/message.rs:84`):
`RouterEvent::MessageReceived` becomes `OutboxEvent::Received` with the
fields beside id, source, title, body and the verification
(`loquax/src/engine.rs:520`, `loquax/src/outbox.rs:319`). The core reads
the two telemetry fields into positions (`telemetry::read`,
`loquax/src/telemetry.rs:166`), the screen model keeps each peer's latest
and hands every one to the store (`take_telemetry`,
`loquax/src/ui.rs:6529`), and the store keeps the fields whole in its
`fields` column (`loquax/src/store.rs:3659`) and the positions in a table of
their own (`SCHEMA_V2`, `loquax/src/store.rs:203`). The sender's timestamp
is still not carried: `OutboxEvent`, the screen's `Message` and every type
the model folds derive `Eq`, which an `f64` cannot, so carrying it is a
change of its own and not the one line of plumbing the fields were.

Before slice (a) the fields were dropped at that hand-over, and a telemetry
report, whose body and title are empty ([Telemetry](telemetry.md), "A
reporting message carries no text"), arrived as an empty block in the
peer's conversation with its position lost.

What the core reads them with, all in `leviculum-lxmf`, `no_std + alloc`:

- the field numbers, `FIELD_TELEMETRY` and `FIELD_TELEMETRY_STREAM`
  (`leviculum-lxmf/src/constants.rs:61`, `leviculum-lxmf/src/constants.rs:62`);
- the Telemeter codec, `Telemetry` (`leviculum-lxmf/src/telemetry.rs:176`)
  with `Telemetry::decode_field_value`
  (`leviculum-lxmf/src/telemetry.rs:341`), and its `Location` in the wire's
  scaled integers (`leviculum-lxmf/src/telemetry.rs:56`);
- the stream decoder for a collector's rows, `decode_stream_field_value`
  (`leviculum-lxmf/src/telemetry.rs:493`);
- the request reader, `telemetry_request_timebase`
  (`leviculum-lxmf/src/telemetry.rs:800`), whose command id is a
  documented assumption (`COMMAND_TELEMETRY_REQUEST`,
  `leviculum-lxmf/src/telemetry.rs:790`).

## 1. What the user sees

**In the conversation.** A peer with a known position carries it in the
header, at 120 columns on the header's second row beside how the peer is
reached: `at 43.77310 N 11.25600 E · ±5 m · 4 min ago`. At 80 columns the
header shows only the age, `pos 4 min`, so the reader knows there is one.
`Enter` on a report's line, or a click on it, opens the map at that
report's fix with the sender's pin selected, and a click on the header's
position, `L` or `Alt-l` puts the peer on the map at its latest fix
(1076). The detail view (`v` on any message, `Enter` on one that is not
a report, slice 2 of [keys, mouse and
layout](loquax-keys-and-layout.md)) of a telemetry message lists every
sensor the codec decoded: position with altitude, speed, bearing and
accuracy where given, the fix time, battery, link and temperature. A
telemetry message is not drawn as an empty block. It is one line in the
conversation, `◎ position 43.77310 N 11.25600 E, 14:05`, so the exchange
still shows when a report arrived, and the rows of one peer's reports
fold into the latest one when they follow each other. As built, the `14:05` is the
block's own arrival time beside the line, as every block carries it; the
fix's own time is in the header's age and the detail view. A run folds
only within one day, and not across a name note or the `new` rule.

**A run is one row on every screen** (1184). Columba sends a report a
minute; on the phone each was a bubble, an unread count and a
notification. One model rule, `Conversation::report_folded`
(`loquax/src/ui.rs`), now decides the fold for the terminal's blocks,
the bubbles of the phone and the window, the list and the toast. A
reaction does not break a run; any other message does, a text of
either side included. The row is the newest report, its line followed
by the count, `· 14 updates since 14:45` (the newest run says whether
the position is alive instead, 1209 below). A report that joins a run
updates the row in place: no new row, and no toast. The conversation's own unread counter
still counts every message, because the home's read marks name each
one. Nothing is merged on disk: the store and the archive keep every
report. On the phone a long press on the run's row offers "Show all 14
positions", which lists them, newest first, through the facade's `run`
query; a tap on one opens the map there.

**A report is a state, not a message** (1209). Users opened the app
for a human and found a coordinate. A position report (a telemetry
field and no text, `Message::is_report`) now asks for no attention at
all: in `loquax/src/ui.rs`, `OutboxEvent::Received` gives a report no
toast, no `new` rule, no move in the list (the conversation's
`activity` stays, `Conversation::list_activity` orders it by its newest
message that is no report, after a restart too) and brings no archived
conversation back; `Conversation::unread_rows` counts no report, so the
phone's notification, which grows with that count, stays quiet. The raw
`unread` counter still counts reports for the home's read marks, and an
open chat reads them silently. A text that carries a position stays a
message. What is worth seeing is whether the position is alive:
`ui::liveness` learns the cadence from the median of the last five gaps
between reports (at least 60 s, from three reports on) and calls the
peer `live` while the newest is younger than twice the cadence (15 min
before a cadence), `recent` within 24 h, `stale` after that or without
a report. The newest report's row says `live · every 10 min · 2 min
ago` or `last position 3 h ago` (older runs keep their count), the
list's line `📍 live` or `📍 3 h ago` (the terminal `◎ position ·
live`), and every avatar of the peer carries a thin ring: the accent
while live, the secondary text colour when recent, none when stale, in
the chat's bar, the Friends sheet and on the map's pin, where
a live ring pulses (one alpha cycle of 2 s, redrawn only while the map
is on screen) and a stale pin is drawn grey. The phone's card says the
row's words. The facade carries `liveness`, `cadence_s` and
`last_fix_at` on the list's, the friends' and the chat's rows.

**The list's map pin** (1224, Lew 2026-10-31). In the conversation list
the ring gave way to a small map pin, one element with one meaning: a
row whose peer has a fix younger than a day (the Friends sheet's rule,
`scene::PositionBadge`) shows it under the time, left of the delivery
mark or the unread count, tinted as the ring was (accent while live,
the secondary text colour otherwise). A tap on it is the intent
`OpenMapAt`: the map centred on the friend's newest fix with the card
open, and Back returns to the list, not to the map's previous view; the
rest of the row still opens the chat. The chat's bar and, on the phone,
the newest report's line raise the same intent. The window draws the
pin as a button, the terminal a `⌖` before the count with `m` on the
highlighted entry, the phone a glyph of 18 dp in a 48 dp target
labelled "Position of Anna, 2 min ago". Older fixes show no pin; the
map still draws them grey. No distance is shown and there is no
setting.

**A text with a position** (1132). Sideband and a stock LXMF client can
send a body and a telemetry field in one message. Such a message from a
peer is drawn as its text with the report's line under it, and `Enter`
or a click opens the map at its fix with the sender's pin selected,
exactly as on a report: one model rule, `message_fix`
(`loquax/src/ui.rs`), names the fix of any received message that carries
a readable one, and every screen asks it. `v` still opens the detail
view. One of ours with a telemetry field is our own position and opens
nothing. On Android the bubble carries the same line under its text: a
pin, the coordinates, the accuracy where given and the fix's age, dimmed
once stale; a report's bubble is the line alone. A tap opens the map
centred on the fix at the region's deepest zoom with the sender's pin
selected, and Back returns to the conversation where it was scrolled.

**The age** is always shown and always counts from the fix time
(`Location::last_update`), not from when the message arrived: a report
that waited at a propagation node for an hour is an hour old. Accuracy is
shown only when the sender gave one; nothing is invented for a missing
field, the same rule as the producer's ([Telemetry](telemetry.md), "No
fix, no position").

**The map view** is a palette action, `map`, with a key the keymap table
of [keys, mouse and layout](loquax-keys-and-layout.md) assigns. It
takes the conversation pane, so the list stays and `Esc` returns. It shows
every peer with a position, our own if we have one, a pin per peer with
its name by the display rule of [the mailbox page](loquax-mailbox.md)
(petname, else announced name, always with the short hash; slice 5), and
the age beside each name. A stale fix (below) is drawn dimmed with its age.
Selecting a pin, by `Tab` through the pins or by click, opens that peer's
conversation. The view fits all pins on open; `+`, `-` and the arrows zoom
and pan, `0` fits again. A fit goes no deeper than the region under its
centre has data for (its max zoom, the outline's z10 where no region is),
so one pin is not shown as a few metres of plain colour; `+` goes on into
the overzoom.

## 2. The data path

**In.** Two fields carry positions, both read with the codec above:

- `FIELD_TELEMETRY` (0x02), one node's own readings, whose source is the
  message's signed source.
- `FIELD_TELEMETRY_STREAM` (0x03), a collector's rows, each
  `[source_hash, timestamp, packed_telemetry, appearance]` ([Telemetry](telemetry.md),
  "The collector exchange"). Here the row's source is who measured, and
  the message's source only who delivered.

The format is Sideband's, its `Telemeter` class in `sense.py`, with
`Location.pack` as the position's layout and `Telemeter.from_packed` as
the reader that skips a sensor it does not know (Sideband `2000d81`, cited
by symbol for the reason [Telemetry](telemetry.md) gives). Columba's
`TelemeterCodec.kt` reads and writes the same two sensors, time and
location (Columba `0930293`). So a position from Sideband or Columba, and
from one of our boards, is the same bytes, and Loquax reads all three with
one decoder. A SID mapped to nil is a sensor without a reading, accepted.

**The cut.** The engine passes `message.fields` on in
`OutboxEvent::Received`; the core (not the frontend) decodes the
telemetry fields and turns each reading with a location into a position
event. A blob that does not decode is kept raw and named in `tui.log`,
never dropped silently, the archive rule of `lntd` ([Telemetry](telemetry.md),
"What a receiving node owes").

**Stored where.** Slice 6's SQLite store fills its `fields` column with
the raw msgpack and gains one table, through the schema version
(`SCHEMA_VERSION` 1 to 2, a migration that only adds):

```sql
CREATE TABLE positions(
    identity BLOB NOT NULL,      -- our own address, as on every table
    peer BLOB NOT NULL,          -- who measured
    via BLOB NOT NULL,           -- who delivered; differs for a stream row
    message INTEGER REFERENCES messages(seq) ON DELETE SET NULL,
    fix_time INTEGER NOT NULL,   -- Location.last_update, unix seconds
    received_at INTEGER NOT NULL,
    lat_e6 INTEGER NOT NULL, lon_e6 INTEGER NOT NULL,
    alt_e2 INTEGER, speed_e2 INTEGER, bearing_e2 INTEGER, accuracy_e2 INTEGER,
    telemetry BLOB NOT NULL,     -- the raw Telemeter, for what we cannot decode yet
    PRIMARY KEY (identity, peer, fix_time));
```

The wire's integers are stored as they are, so no rounding happens between
the wire and the disk. As built, `received_at` may be NULL: a report
that arrives before the shell first told the screen the time has no
receive time to store, and inventing one would be the placeholder the
telemetry page forbids. A fix the wire gives no accuracy for (the field
is always packed, so zero is the only absence it can say) is stored as 0
and shown without one. The key `(peer, fix_time)` is Sideband's dedup rule
(exact source and timestamp), so the same fix arriving directly and again
in a collector's stream is one row. The map reads the latest row per peer;
the table keeps the track, which a later slice can draw. Retention follows
the messages: kept until the reader deletes them, with the conversation or
by a palette action `forget positions`.

**A future fix** is clamped for ordering, kept for display, the ingress
rule of [Time and clocks](time-and-clocks.md#ingress-clamp-for-semantics-keep-for-display):
a fix stamped ahead of our clock sorts at receive time and shows its claimed
time with a mark. Without the clamp, one wrong clock would pin a peer's
pin at "now" forever.

**Stale.** Decided 2026-10-27 (Lew): a fix is drawn as current up to 2 h old and dimmed
after that, with its age always beside it. Two hours is twice the station
profile's heartbeat (`max_interval_ms`, 60 min,
`leviculum-nrf/telemetry-policy/src/lib.rs:190`; the tracker's is 15 min,
`leviculum-nrf/telemetry-policy/src/lib.rs:175`), so a board that misses
one report stays current and one that misses two goes dim. A fix is never
removed for its age alone; it goes with a delete. This closes
handheld-messenger.md's open "how other nodes' positions age out of the
map".

**Asking a peer for its position** is a later step, not in slices (a) to
(e). The request is an LXMF message whose `FIELD_COMMANDS` carries
`TELEMETRY_REQUEST` with a timebase ([Telemetry](telemetry.md), "A request
from the target triggers one report"); sending one is a palette action
`request position` on the open conversation. Answering one needs a
position of our own and the user's per-peer switch of section 7, so it
waits for that.
Its command id must first be pinned against a captured Sideband request,
as the codec's own comment says.

## 3. The renderer split

One map core, three frontends. The core does what is the same problem on
every screen; a frontend only turns the core's output into its pixels or
cells and its input into the core's events, the rule of "The core/frontend
cut" on handheld-messenger.md.

| crate | what | `no_std` | used by |
|---|---|---|---|
| `leviculum-mapfill` (exists) | scanline filler, wide lines, the flat geometry reader, the render pipeline into any `DrawTarget`, and since 1062 a per-feature `Pass` that draws one feature at a placement, so several fixtures share one frame | yes, no `alloc` | the core, all three frontends |
| `leviculum-map` (built, 1062) | the region file reader, Web Mercator projection, the viewport (centre, zoom, pan, fit), tile selection and feature culling, the layer profile per zoom, the style tables (`leviculum-mapfill`'s, re-exported), pins and labels as data, the class-by-class draw of a view's tiles | yes, `alloc` | all three frontends |
| `leviculum-map-tool` (built, 1062, `std`) | cuts a region file from Protomaps PMTiles archives (section 4); prost and the tree's one MVT decoder live here, and `leviculum-mapfill/measure/` reads its tiles through it; no package ships it | no | the desktop, by hand |
| Loquax core | positions in the model, the map view's state (open, viewport, selected pin), the events | yes, `alloc` | all three frontends |

The frontends:

- **The TUI** (`loquax`, ratatui). The core renders the view into an
  Rgb565 or Rgb888 frame through `leviculum-mapfill`, and the frontend
  hands that frame to the terminal: through `ratatui-image`'s graphics
  protocols where the terminal has one, else as Unicode half-blocks, else
  as a braille line drawing (the terminal options below). Pins and names
  are drawn as text cells over the picture, not into it, so they stay
  sharp and clickable on every backend.
- **The iced GUI.** The same frame as the phone's (1144,
  `loquax::mapframe`, below), at the window's size with an eighth of the
  screen of margin on each side, drawn on a canvas where the view moved
  it until the next frame lands; pins and labels as iced text over it,
  placed by the frame with the window's font. A drag pans by whole
  pixels; the wheel zooms about the pointer, a notch a step, a
  touchpad's scroll 120 pixels a step, and the view keeps the zoom it
  ends at (iced 0.14 has no pinch gesture). The frames are composed on
  iced's executor, never on the window's thread; a drag or a turning
  wheel asks moving frames, which keep the names, and 200 ms after the
  wheel's last turn the frame at rest places them. At rest, with the
  window focused, the prefetch runs a batch a job until the plan is
  done or the view moves.
- **Android** (1128). The same frame as the window's, drawn by the same
  functions (`loquax::screenmap`): the picture, the map's names set in
  Fira Sans by cosmic-text and swash (`loquax::mapnames`, carried in
  `loquax/fonts/`), and here the pins and their labels too, all into one
  RGBA buffer that Java blits into a bitmap in a plain View. Java does
  the gestures (a finger pans and shifts the last bitmap, a pinch or a
  double tap zooms and scales it, a tap names a pin) and hands them back
  with the last view; the view's rules stay the model's, one pixel a
  cell as in the window. No second map in Java. The phone's view keeps
  the zoom a pinch leaves, a real number to a thousandth of a step
  (`leviculum_map::viewport::ExactView`, 1134), and its frame is composed
  from Loquax's tile cache (`loquax::tiles`): the map without names and
  pins, in tiles of 256 pixels of the nearest whole step, kept by last
  use up to 96 MB, missing ones drawn on up to eight threads, scaled to
  the exact zoom (one to one at a whole step). The names are placed at
  rest over the frame at the exact scale and kept; a pan at that zoom
  draws them where the view moved them and places none. Since 1225 a
  frame that zooms places its names anew, as a frame at rest at its
  zoom would: the data's `min_zoom` ladder at the step nearest the
  zoom, the anchors and the street lines of the frame's own geometry,
  the text at its screen size (names never scale with the map), the
  rule's collisions. Into the step's frame the Canvas scales up by
  1/k it draws each name sampled at k, below k 0.75 from the name's
  coverage averaged two by two, so on screen it has the size and the
  place a frame at rest gives it. The names it looks up are kept while
  the pinch stays at the step and inside a view 1.6 times the frame's
  each way, so a pinch reads the region file about twice a step rather
  than every frame. 1205's ladder filter and collision pass over the
  names carried from rest, and its step limit past which a pinch showed
  the ground alone, are gone: Lew saw names vanish when zooming out and
  carried names detached from the streets that stretched under them. A
  name that enters fades in over 150 ms and one that leaves fades out
  where the map moves it, an alpha on the sprite's levels at blend
  time by the frontend's clock, and a frame says it fades so the
  frontend asks again; a frame at rest with its fades done is the same
  bytes. A pinch out over the Berlin fixture from z14 to z10 in half
  steps shows 20, 18, 13, 12, 9, 5, 3 and 1 names, each frame the set
  a frame at rest at its zoom places, every anchor within a pixel of
  it.

  The cost, measured 2026-10-31 on the coder host (10 vCPUs, eight
  workers) with `names_cost` (folded into `map_eye_bench` by 1422) over Berlin Mitte (cut
  from the Protomaps build of that day, `--bbox
  13.33,52.49,13.45,52.55`, and a z12 cut of the country around it),
  the median of ten frames of a pinch out by 0.05 a frame, in ms (the
  full placement: the names looked up, placed and sampled as a frame at
  rest at a zoom between steps runs it, on the code before 1225):

  | frame | zoom | names | ground | full placement before 1225 | zooming frame since 1225 | its names: kept / placed / sampled |
  |---|---|---|---|---|---|---|
  | phone 1080x2400 at 2.75 | 10 | 31 | 2.0 to 2.2 | 7.0 to 7.3 | 4.5 to 4.6 | 0.01 / 0.42 to 0.45 / 1.0 to 1.1 |
  | phone | 13 | 120 | 0.7 to 1.2 | 42.9 to 43.7 | 10.1 to 10.7 | 0.01 / 2.8 to 3.2 / 3.5 to 4.0 |
  | phone | 16 | 94 | 1.1 to 1.3 | 13.2 to 13.9 | 9.1 to 10.2 | 0.01 / 1.9 to 2.1 / 2.6 to 3.3 |
  | window 1920x1080 at 1 | 10 | 67 | 0.8 to 1.1 | 5.0 to 5.2 | 2.0 to 2.6 | 0.02 / 0.30 to 0.35 / 0.6 to 0.8 |
  | window | 13 | 210 | 0.6 to 0.8 | 8.3 to 8.7 | 5.1 to 7.4 | 0.02 / 1.3 / 1.8 to 3.3 |
  | window | 16 | 199 | 0.7 to 0.9 | 8.1 | 5.5 to 6.7 | 0.02 / 1.8 to 1.9 / 1.4 to 2.2 |

  Three runs each side, the coder host's load under 3.5. "Kept" is the
  median over the frames, most of which took the names looked up
  before; two of the eleven frames of each pinch looked them up, in the
  view 1.6 times the frame's each way.

  A full placement as a frame at rest runs it (the names looked up, 2.5
  to 5 ms; placed, and their sprites sampled) did not fit beside the
  ground in 16 ms at the phone's z13: the typesetter's store of masks
  and halos was 4 MB, which the phone's density passes at z13, so every
  frame shaped and rasterised its names again (32 ms of the 43). With
  the store at 16 MB the same full placement there is 12.1 to 12.4 ms,
  which a frame at rest gains too; with the names looked up kept as
  well, and the sprites sampled over each name's own band, a zooming
  frame is 2 to 11 ms on this host. The phone itself was not measured
  in 1225 (no device on the coder host); a frame there that misses
  16 ms is shown late, the Canvas scaling the last frame, names and
  ground together, until it lands.

  The gui feature's tests pin the same rule; `just loquax-gui` runs them.

  The pins
  are drawn over the tiles every frame, so a pin costs no tile. A pinch
  keeps the step it is composed from while it stays within a step of it,
  and a frame shows other steps' cached tiles where the new step's are
  still owed (1143): the coarser steps scaled up, the nearest first, up
  to three; where none is held the finer ones scaled down, 2x2 tiles of
  the next or 4x4 of the one after, which is what a zoom out has. Where
  no cached tile covers a pixel the world outline is drawn under it in
  the theme's colours, and off the projection the theme's water: a frame
  is never black. A view at rest with every tile drawn prefetches (1135,
  `loquax::tiles::plan`): after a pinch out the step out over the whole
  frame first, then the ring of tiles around the frame, then the next
  step in over its central half, then a second ring into spare room
  only; each group ordered by
  the pan's velocity, a moving average that fades within about two
  seconds of rest, the tiles ahead of the last movement first, nearest
  first without one. At most 128 tiles a view, a tile a worker a call,
  so a gesture waits one tile at most and drops the plan; a prefetched
  tile no frame used is evicted first, and a prefetch evicts neither a
  tile on screen nor one it drew. Only while the map is in front and
  the battery saver is off. Which regions the screen and its ring need
  is the frames' scheduler's since 1292 ("The screen first", below), no
  longer the prefetch's: a prefetch answer is no frame, so a load it
  started showed no chip, and it asked nothing over a frame no region
  held. The frame
  itself, the names kept, the prefetch's plan and the view held to one
  world are `loquax::mapframe`, which the window calls too (1144); the
  phone's glue keeps its requests, its fit and its autoload. The
  terminal keeps whole steps. Since 1154 a frame at rest sets its names
  from what the last one set: every name in view is shaped once for its
  size, only one with a spot in the picture is rasterised and only a
  placed one gets its halo, all kept by text, size, pad and slant
  (`loquax::mapnames::Typesetter`, at most 8192 masks, 8192 halos and
  4 MB); the halo is widened by row spans, and a placed name's sprite is
  sampled over its turned box only, the names dealt over the cache's
  workers. A frame with every tile there is composed into the last
  frame's buffer, which the phone's facade hands back
  (`loquax::mapframe::Frames::recycle`), copies the tiles' rows whole at
  a whole step and mixes two channels a `u64` between steps. Every frame
  is the same bytes as before, which
  `the_fast_paths_give_the_frames_of_the_old_ones` in
  `loquax/src/mapframe.rs` holds against the old paths, kept as test
  references. On the coder host, through `map_fps` (the facade's frame
  copy included, the host shared with other builds), the phone's frame
  at rest with the tiles cached went from 202 to 212 ms to 21 to 30 ms on
  eight workers and from 213 to 224 ms to 30 to 39 ms on one, a pan with
  the tiles cached from 20 to 42 ms to 8 to 16 ms. Since 1155 the
  phone's frame is composed straight into the Bitmap the app shows next,
  locked through the NDK's jnigraphics
  (`loquax-android/src/bitmap.rs`, `AndroidBitmap_lockPixels`): only the
  frame's description crosses JNI, no `byte[]` of the frame's size and
  no copy into the Bitmap. While a pinch or the window's wheel zooms, a
  frame is the tile step one to one, never a step above the zoom, and
  the phone's Canvas or the window's image scales it the rest of the way
  (`Ask::scaled_while_moving`); a pan at the zoom at rest stays drawn at
  that zoom. A painter keeps its filler's storage from tile to tile and
  turns a tile to RGBA in one pass. Through `map_fps` on the coder host:
  the facade's own frame on a warm pan from 13.9 to 16.5 ms copied out to
  4.3 to 6.4 ms into the Bitmap, a pinch with its tiles held from 36.6 to
  5.1 ms on one worker and from 9.7 to 4.9 ms on eight, a cold frame's
  tile time on one worker from 71 to 56 to 58 ms. Since 1163 a frame
  while the view moves that owes tiles covers only the owed tiles'
  pixels: the tiles held are copied, the stand-in steps taken nearest
  over the owed tiles' rectangles alone, their pixels doubled or halved,
  then the outline there; at rest the stand-ins stay scaled bilinear
  over the frame (`loquax::tiles::Frame::moving`). A pinch's frame at
  the step below covers more than the frontend shows of it scaled up: a
  tile outside the screen and the margins over the scale is used when
  held, and otherwise neither drawn nor owed, its pixels water
  (`Frame::asked`). Frames at rest and frames with every tile held are
  the same bytes as before. Through `map_fps` on the coder host, the
  pinch that owes tiles from 62 ms to 6.7 ms on one worker, the tiles
  it owes over the pinch from 7 to 4 on eight workers and from 20 to 10
  on one. Since 1156 a placed name's sprite is a box of
  `(halo, ink)` levels out of 255 blended in integers row by row
  (`loquax::mapnames::Sprite`): a level name is the same bytes as
  before, a turned one's coverage is rounded to a level, so a channel
  may move a step, or two where the halo's step carries into the
  glyphs' blend (2159 of the 460800 bytes of each Florence golden, four
  of them by two). Through `map_fps` on the coder host the phone's
  sprites went from 9.19 MB to 0.89 MB and drawing them from 2.7 to
  3.1 ms to 1.2 to 1.3 ms. Since 1188 a pan the finger lets go of while
  it moves coasts on and slows down, with the platform's physics so it
  feels like every other Android surface: VelocityTracker measures the
  finger, an `OverScroller` flings with ListView's curve
  (ViewConfiguration's scroll friction, the screen's density; AndroidX's
  `FlingAnimation` is not used, the app has no AndroidX), and each
  Choreographer frame moves the last bitmap by the scroller's step and
  asks a moving frame, as a finger would, so the ground stays drawn and
  the names are carried. The frame says how far its view may still go
  north and south before the view is held to one world
  (`room_north`, `room_south`), the scroller's y bounds, so a coast stops
  at the Mercator edge without a bounce; x goes round. When the scroller
  stops, the view is drawn at rest and the names are set, as after a
  pan. Which gestures fling is `Fling.java`'s, plain Java the host's
  tests compile and run (`loquax-android/tests/fling.rs`): a pan of one
  finger, at ViewConfiguration's minimum fling velocity at least, still
  moving 40 ms before it lifted (AOSP's VelocityTracker assumes a finger
  stopped after that only since Android 13); a second finger at any time,
  a pinch or a pan that ends under another finger, never flings, and a
  finger down ends a coast where it is. The config's `map_fling`, on by
  default, turns it off, the switch under Settings, Advanced. Through
  `map_coast` on the coder host, the phone's 1620 x 3600 frame on a warm
  cache, a coast of 10000 pixels a second asked one frame a vsync drew
  60.4 frames a second in median 2.9 to 5.4 ms with no vsync missed on a
  quiet host, the screen at most 217 to 266 pixels ahead of its frame
  against a margin of 270; with the other lanes' builds on the host it
  missed up to 15 of 103 vsyncs and ran up to 647 pixels ahead.
- **The M9.** The same pipeline straight into the panel's
  `embedded-graphics` target, as the measurement firmware does
  (`leviculum-mapfill/m9/`), with mousefood's cells for the labels.

One drawing path for all three is the point: the measured renderer is the
renderer, and a style change shows on every screen at once. The desktop
does not need a second, richer renderer to look acceptable; at desktop
sizes the pixel count grows (a 1280x800 view is 13 times the M9's), and
the host is fast enough for that by the measurement (1.25 ms for the full
240x320 view, Table "Host numbers").

## 4. Tiles

**Format.** One region file, read by all three frontends. It is a tile
pyramid of the flat i16 geometry that `leviculum-mapfill` already reads in
place (the `.lmf` fixture layout, one per tile) behind a directory index,
already cut to the per-zoom layer profile: z15 and z14 without
`buildings` and `pois`, the region zooms with `landuse` and `natural`
thinned and their geometry simplified ("Simplifications the numbers ask
for", items 1 and 2). Pre-decoded geometry is what the device measured
(177 ms full, 90 ms stripped, 31 ms flush); decoding MVT on the M9 was the
cost the measurement found to dominate, and the region file removes it.
The desktop could read MVT, but one format for all three is one thing to
test.

**Region file** (built, 1062; the reader is `leviculum-map/src/region.rs`,
whose module comment is the normative layout). All integers little-endian,
offsets from the start of the file:

- a header: `LMR1`, the version (1, 2 since 1100 with names, 3 since
  1191 with buildings, 4 since 1291 in arrival order, 5 since 1685 with
  pois), the header length
  (64, 96 for version 2, 112 for version 3 and 4, 128 for version 5),
  the bounds in
  degrees x 10^7 (west, south, east, north; west greater than east means
  across the antimeridian), the zoom range (at most 16, so x and y fit a
  `u16`), the layer profile id, where the attribution and the notice lie,
  the tile count, where the index lies and the file's length; version 2
  adds where the label index and the string table lie, version 3 where
  the building index lies and how many sections it lists, version 5 the
  same pair for the poi index;
- the two texts, UTF-8 and both required: the attribution, one line every
  map view shows ("© OpenStreetMap contributors"), and the notice, the
  ODbL text, the derived-database sentence and the source the region was
  cut from (name, schema version, OSM replication time);
- one `.lmf` blob per tile, the fixture layout `leviculum-mapfill` reads,
  written for a 256 x 256 view with one tile at offset (0, 0) and the shift
  that takes its extent to 4,096 sub-pixels. Each feature's flags byte,
  a zero until 1099 and never read by a reader of that age, carries a
  line's bridge and tunnel and the class byte's newer half: forest on a
  park's byte, a secondary road on a major road's. An older reader draws
  those as the park and the major road it drew before, so the version
  stays 1 (`leviculum-mapfill/src/fixture.rs`);
- version 2 (LMR2, 1100): a label section per tile that has names, one
  string table for the whole region (UTF-8, u32 offsets, sorted, so a name
  is found by binary search), and a label index like the tile index right
  before it. A section holds point labels (kind, population rank, min
  zoom, flags, string, anchor: 12 bytes) and line labels that refer to a
  line feature of the tile's own blob (8 bytes), so a street name reuses
  the geometry the tile holds; and an alternate name per label where the
  name in the locale the cut was asked for differs from the local one.
  The layout is `leviculum-map/src/labels.rs`'s module comment. A version
  1 reader refuses version 2 with `RegionError::Version`; this reader
  reads both, and a frontend that draws no names never opens the label
  index or the table;
- version 3 (LMR3, 1191): a building section per z15 tile that has
  buildings, after the label sections, and a building index like the
  tile index between the label index and the tile index. The layout is
  `leviculum-map/src/buildings.rs`'s module comment; the paragraph
  "Buildings" below says why it is so;
- the index last, 20 bytes per tile ascending by (z, x, y): z, x, y,
  offset, length. Last, so the cut can stream tiles and learn their count
  at the end;
- version 4 (LMR4, 1291) keeps every field and part and changes their
  order, so a file is usable while it arrives. The tile index comes right
  after the two texts, one slot for every tile the box lists, the count
  known before the cut starts. A slot is an entry, or with length 0 either
  offset 0, "not here yet", or offset `u64::MAX`, "nothing there". The
  tile blobs follow in index order, which is (z, x, y), so coarse to fine:
  the outline and the main roads of the shallow zooms in the first few
  hundred kilobytes, the deepest zoom last. The label sections, the
  building sections, the string table, the label index and the building
  index come after every tile, the building index ending the file. The
  writer fills a slot after its blob is out, its offset before its
  length, and writes the file length last, after the names' and
  buildings' fields; the length is 0 until then. So a reader of a file
  still being cut in place, or of a copy still downloading (its length
  already set, the file shorter), knows from the slots alone which tiles
  are whole: a slot with a length whose bytes lie inside what is there.
  The rest are pending, and the draw stands the nearest coarser tile
  that is there in for each (`Region::stand_in`); the names and the
  buildings show once the file is complete;
- version 5 (LMR5, 1685): a poi section per tile of the region's deepest
  zoom that has any, after the building sections, and a poi index after
  the building index, last in the file. Every point of interest of the
  `pois` layer goes in, every kind verbatim, named or not, and with it
  the travel attributes the source carries about ways and areas: the
  state of a way (surface, track grade, hiking difficulty, trail
  visibility, fords, access), a marked route's `ref` and `network`, a
  protected or military area's kind. Kinds, names, attribute keys and
  values are all indices into the region's one string table, so a reader
  skips a kind or key it does not know and new ones need no new version.
  The layout is `leviculum-map/src/pois.rs`'s module comment; the header
  grows to 128 bytes. A cut without pois writes version 4, which a
  version 4 reader still reads.

**Buildings** (1191, the format and the cut; 1192, the draw).
Protomaps' z15 tiles carry every OSM building. They are not in the
tile blobs, which stay as profile 1 cuts them, byte for byte, but in a
section of their own per z15 tile: in the blobs, a view that draws no
buildings would still pay for them (every blob is checked whole when read,
and the name placement walks every feature of a tile), a z16 or z17 view
would project and cull the whole z15 tile, and the M9 would read 3.3 times
the bytes of every z15 view from its card (the 1191 study, measured with
buildings in the blobs).

A section is a head of 16 cells, a 4 x 4 grid of 1,024 units square, 16
bytes each: where the cell's records end (`u32`), how many footprints it
holds (`u32`) and the box every vertex of them lies in (four `i16`). A
footprint is stored once, in the cell holding the centre of its box; a
reader takes every cell whose box meets the part of the tile it draws, so
a z16 tile reads its four cells and a z17 tile its one, plus a neighbour
where a footprint reaches over the cell edge, and skips a cell from its
head entry alone. A footprint is its rings, the outer one first, then its
holes: a ring's vertex count and a flag for another ring of the same
footprint in one LEB128 varint (one byte up to 63 vertices), its first
vertex as two `i16`, then each step to the next vertex as two `i8`, or
the byte `0x80` and two `i16` where the step is longer (24 m at z15). The
ring closes on its first vertex. Coordinates are the z15 tile's units at
extent 4,096 and lie within one extent of the tile, as a blob's do; a
footprint holds at most 4,096 vertices and 1,024 rings, the M9's
capacities. A cell counts its footprints in a `u32`, so no tile is too
full for the format (the study's worst z15 tile holds 2,228). Every
record is checked as it is read: a truncated or corrupt one is an error,
never a panic.

A version 2 reader refuses a version 3 file with `RegionError::Version(3)`
before it reads past the version field, as a version 1 reader refuses
version 2, so the reader has to ship before regions with buildings reach
it; Loquax cuts its own regions, so in practice only a file copied by hand
meets an older reader. Since 1291 every cut writes version 4, with
buildings or without (a section count of 0), and since 1685 version 5
when pois are cut (the autoload cuts them); this reader reads versions 1
to 5, a version 4 reader refuses version 5. No region file is
published anywhere today: the autoload cuts its regions on the device
from the Protomaps archive, and the committed fixtures stay version 1 to
3, which this reader keeps reading.

The cut (`leviculum-mapcut/src/cut.rs`, `footprints`) takes the
`buildings` layer of each z15 source tile, the same decode as the tile's
blob, and keeps every building piece the tile holds, a building across a
tile edge once in each tile as that tile's piece. Since 1707 the rings
are not simplified at all: footprints are drawn from z16 only, always
overzoomed, and the old half-pixel tolerance put corners 8 units (1.1 m
in Frankfurt, 4 px at a z18 view) off the data; exact corners cost the
Römer box 36 % more section bytes (78,368 to 106,569). A zero-area ring
still goes: a hole on its own, an outer ring with its footprint. A
footprint over the M9's capacities would be simplified again at doubled
tolerance up to four pixels; none is in the two boxes below.
`leviculum-map-tool cut` cuts buildings by default (`--buildings`), and
so does the autoload; the M9 and the terminal never open the section.

Measured 2026-11-01 on the coder host from the Protomaps build of
2026-10-28 (Basemap 4.15.2, the build the regions in use were cut from):
the two boxes extracted with `leviculum-map-tool extract` (Frankfurt
`8.47,50.01,8.80,50.23`, 27,409,373 bytes; Berlin `13.05,52.32,13.80,52.70`,
89,323,845 bytes), then cut z0 to z15, profile 1, `--locale de`, with and
without `--buildings`:

| Region | without | with | added | footprints | vertices | dropped as degenerate | sections | heads and index |
|---|---|---|---|---|---|---|---|---|
| Frankfurt | 10,290,565 | 14,601,696 | +41.9 % | 322,436 | 1,494,946 | 2,800 | 881 | 243,156 |
| Berlin | 36,363,478 | 49,373,566 | +35.8 % | 912,127 | 4,451,470 | 11,254 | 3,217 | 887,892 |

The records alone add 39.5 % and 33.3 %, the study's estimate for the
encoding (+40 % and +33 %); the 256-byte head per section and the 20-byte
index entry add the rest. The tile blobs of both cuts are the same bytes.
The footprint counts are the source's pieces (325,236 and 923,381) less
the degenerate ones, slivers a tile's buffer cut off and outlines under a
pixel.

**Drawing the buildings** (1192). The map core draws a tile's
footprints at view zoom 16 and deeper (`BUILDINGS_FROM`), where the
style asks for them: `Style::buildings`, set in `Style::SCREEN`, the
finish of the phone and the window; `Style::PC` (the terminal's sixel
and kitty pictures) and `Style::M9` leave it off, and a view that does not
draw them never reads a byte of the sections (`Region::read_buildings`).
In `leviculum-map/src/draw.rs` they go after the areas, so over water,
and before every line, so roads, paths, rails and the names over the
frame stay on top; placement never looks at them, so they move no name.
Each cell of the section that meets the tile's part of the view is read,
each footprint culled by its box before a vertex is projected
(`Pass::building` in `leviculum-mapfill/src/render.rs`), filled through
the scanline filler in the building's area colour, then outlined as a
1 px aliased line in the table's `building_edge`, a step darker: light
`rgb(221,213,205)` with `rgb(194,184,175)`, dark `rgb(62,62,70)` (a
step lighter than the study's, whose fill was a step off the ground
alone) with `rgb(46,46,52)`, between the fill and the ground as CoMaps
draws it. A tile whose section holds more than `BUILDING_CAP` (2,048)
footprints is drawn as blocks: filled, no outline, so touching buildings
merge into their block. The outline is about half a building's cost, so
the cap bounds the dearest tiles; it counts the whole section, so a tile
looks the same in the tile cache's 256 pixel tiles as in a view drawn
whole. Of Frankfurt's 881 sections one is over it (z15/17175/11110, 2,229
footprints; the 99th percentile is 1,640), of Berlin's 3,217 none (1,427 at
most).

Measured 2026-11-01 with `map_fps` (`MAPFPS_ZOOM`, `MAPFPS_QUICK`) on the
coder host (4 cores, so 4 workers, not the study's 8), the phone frame
over Frankfurt, three runs each over the region cut with and without
buildings, ranges in ms:

| Row | workers | z16 without | z16 with | z17 without | z17 with |
|---|---|---|---|---|---|
| cold, at rest | 4 | 26.1-43.0 | 30.4-41.3 | 20.1-20.6 | 23.3-26.8 |
| cold, at rest | 1 | 65.3-66.0 | 82.1-82.6 | 55.8-56.5 | 66.8-69.2 |
| tile time, summed | 1 | 38.0-38.8 | 55.1-55.8 | 33.7-34.3 | 45.1-46.8 |
| pan, cache warm | 4 | 3.6-5.6 | 3.0-3.5 | 2.9-3.3 | 2.8-2.9 |
| at rest, cache warm | 4 | 11.5-20.5 | 11.2-16.5 | 9.4-15.0 | 9.5-10.3 |
| pinch, a step either side (median) | 4 | 12.5-21.3 | 13.0-17.6 | 9.8-11.0 | 9.5-10.4 |

The z15 cold frame at rest, the bar for z16, is 33.1-35.7 ms on 4 workers
and 78.9-79.7 on 1 here. The frames at z15 and shallower are the same
pixels with buildings as without (`leviculum-map/tests/golden.rs`).

Measured again 2026-11-01 (1193) on the same host given back its 10
vCPUs, so with the study's 8 workers: five runs each, interleaved, over
the same two regions, the load average 3 to 6 from other work on the
host; medians, ranges in brackets, in ms:

| Row | workers | z15 without | z16 without | z16 with | z17 without | z17 with |
|---|---|---|---|---|---|---|
| cold, at rest | 8 | 26.5 (24.9-27.5) | 21.3 (18.6-62.9) | 24.8 (23.6-57.9) | 22.1 (17.3-44.5) | 20.4 (18.5-77.9) |
| cold, at rest | 1 | 80.5 (78.9-82.5) | 67.0 (64.8-68.2) | 86.0 (83.7-92.6) | 57.9 (57.0-62.2) | 69.1 (68.5-69.7) |
| tile time, summed | 1 | 56.7 | 39.2 | 58.0 | 35.0 | 46.4 |
| pan, cache warm | 8 | 3.1 | 5.3 | 4.2 | 5.3 | 3.9 |
| at rest, cache warm | 8 | 14.5 | 15.1 | 11.5 | 10.6 | 11.0 |
| pinch, a step either side (median) | 8 | 8.5 | 10.5 | 9.8 | 9.6 | 8.8 |
| pinch, tiles held (median) | 8 | 8.6 | 9.7 | 10.2 | 12.1 | 9.6 |

Every acceptance number of the study holds: the cold z16 frame at rest
with buildings (24.8) is under the cold z15 frame at rest (26.5) on 8
workers; on 1 worker it is 1.28 times z16 without (the bar 1.4, 93.8
ms); the warm, pinch and held rows are within 1 ms or faster. The single
runs of 58 to 78 ms on 8 workers are in both columns, the other work on
the host, so the medians decide. Nothing was tuned in this pass: no row
missed.

**Region sizes with buildings** (1193), cut 2026-11-01 from the Protomaps
build of 2026-10-28 with `--locale de`, with and without `--buildings`.
The autoload's places are its 12 by 12 km boxes at z10 to z15 (the three
measured under "The autoload", the same bytes fetched), Hessen and Germany the
boxes of the table in section 4 at z0 to z15, Germany fetched in three
latitude bands as there. Sections over the cap are drawn as blocks:

| Region | without | with | added | sections | over the cap | most footprints in a section |
|---|---|---|---|---|---|---|
| central Frankfurt (place) | 4,041,375 | 5,699,692 | +41.0 % | 255 | 0 | 1,983 |
| central Berlin (place) | 8,127,178 | 10,221,288 | +25.8 % | 289 | 0 | 1,369 |
| the Vogelsberg (place) | 1,290,432 | 1,449,420 | +12.3 % | 176 | 0 | 494 |
| Hessen | 401,045,416 | 489,009,508 | +21.9 % | 48,710 | 12 | 2,542 |
| Germany | 4,014,983,480 | 4,886,283,750 | +21.7 % | 554,101 | 248 | 4,024 |

A city adds a quarter to two fifths, the countryside an eighth, a state or
the country a fifth, since most of their tiles are fields. Germany with
its buildings still fits a card, Hessen at about 0.49 GB too. The
autoload cuts with buildings, and the phone's Maps page sizes a region by
its file (`loquax-android/src/map.rs`, `RegionRow::bytes` from the file's
metadata), so it shows the size with buildings as it is. Cutting Germany
found `Archive::locate` copying the PMTiles root for every tile, which an
extract's root of every tile made quadratic (the cut did not end in ten
minutes); it searches the root in place now and cuts Germany in 77 s
(`leviculum-mapcut/tests/pmtiles.rs`,
`a_root_of_every_tile_is_searched_in_place`).

**Names** (1100). The cut keeps the names of places (city, town, district,
village, suburb, neighbourhood; not localities, hamlets, isolated
dwellings or farms, which are 1,774 of the 1,933 places of a Frankfurt z13
tile), of water points (the `water` layer's own from z16, and the lakes,
ponds and harbours 4.x names as `pois` of kind `water` from z12) and parks,
each where its anchor lies in the tile and the tile's zoom reaches its
`min_zoom` (the deepest zoom keeps all, for the views that overzoom it),
of named roads from z14, and of rivers, canals and streams from their own
`min_zoom` (10 for the Main, 14 for a stream), every piece a line feature
of its own. A river's name is on its line alone: its area has none, which
is why the first cut named no river (1126). Street names show from z14 for
major and secondary roads, from z15 for the rest. Measured on the 4.x
extracts with `--locale de`: Frankfurt 9,878,095 to 10,270,116 bytes
(+4.0 %), Berlin 35,176,663 to 36,309,038 (+3.2 %); the rivers and the
`pois` waters add 17,589 and 54,440 bytes (+0.17 % and +0.15 %).

The source cuts a street at every junction, so its pieces are often
shorter than its name. The reader joins the pieces of one name and kind
end to end across the view's tiles (`labels::join_pieces`) before the
placement, not the cut: the format and the geometry stay as they are, the
join costs a pass over the view's few hundred pieces, and it also joins
pieces a tile edge cut. In the window, Frankfurt at z15 (1280 x 800) names
54 streets instead of 42, its 1,215 pieces joined into 850 lines, Berlin 41
instead of 38 (896 into 650).

The name shown follows a chain: `name:<locale>`; else `name:en` where
`name` is in a script the locale does not read and `name:en` is in one it
does (Latin is read by every locale, so a German map of Athens says
Athens); else `name`.

A tile with nothing left under the profile has no entry; a reader draws
the background there. Two entries may share a blob (the cut stores
identical tiles up to 4 KB once: 99,799 of Germany's 948,560 z15 tiles).
The reader never loads the file or the index: it opens through a `Storage`
(a slice in flash or memory, a file, later a card), checks the header
against the length, and finds a tile by binary search over the index in
place, 20 bytes a probe, because Germany's index alone is 25 MB. Every
entry and every blob is checked when it is used, so a truncated or
corrupt file is an error, never a panic; `verify` walks the whole index
once for a file a frontend did not make.

**Layer profile 1** (`leviculum-map/src/profile.rs`): no buildings at any
zoom in the tile blobs (from version 3 they are a section of their own,
above; points, `pois` among them, are not in the flat format at all); from
z13 the extent stays 4,096 and the Douglas-Peucker tolerance is half a
pixel; z12 and shallower, the region zooms, are quantised to an extent of
512, simplified at one pixel and lose the landuse, natural and landcover
areas under 16 square pixels. A part (an outer ring with its holes) the
device could not hold whole (4,096 vertices, 1,024 rings, the M9's
capacities) is simplified again at doubled tolerance up to four pixels,
long lines are cut into pieces, and parts are packed into as few features
as fit, so no feature is skipped as an overflow on the device.

**Layer profile 2** (1707) keeps profile 1's tables with one change: the
deepest zoom of the region — the only tiles a view overzooms — is not
simplified (tolerance 0, extent 4,096), because overzoom magnifies the
half pixel past a pixel (4 px at a z18 view of z15 data, 16 px at z20)
and road ends visibly parted from the ways they join. At the simplified
zooms the cut now also holds every line end that rests on another line
in the source to within one unit of it after the cut, and keeps the
64-unit tile-edge strip on the source line, so the two copies of a line
in neighbouring tiles' buffers agree instead of drawing doubled
(`leviculum-mapcut/src/simplify.rs`, `douglas_peucker_held`; the cut
does this under any profile — the profile byte is what tells an
installed region cut the old way from a new one). The autoload and
`leviculum-map-tool cut` write profile 2; a region whose header still
says profile 1 no longer counts as covering the screen
(`loquax/src/ui/viewload.rs`), so an autoloaded one is asked for again
when a view shows it and is rewritten under its own name: the new file
is written beside the old one and renamed over it once it verifies
(1727), so the old region draws until then and the area repaints once,
at a download the size of the new region (up to 31 % more than the old
one on the fixtures' boxes). A load that fails, offline or on a link that
breaks mid-download, leaves the old region as it was. A
hand-placed region is never deleted: it stays and draws, and a fresh
autoloaded region lands beside it. A region flashed onto the M9 is not
autoloaded; it draws as it is until it is flashed again.

**Projection.** The world coordinate is two `u32`, 2^32 units around the
world: 9.3 mm at the equator; at z15 a pixel is 512 units (4.78 m at the
equator) and a sub-pixel 32. Longitude to x is integer and exact; latitude
to y uses `f64` through `libm` (software on the M9's single-precision FPU),
once per pin and per viewport change, never per vertex, since the region
holds geometry already projected. Both round-trip to the same 10^-7 degree
step. x wraps, so the antimeridian is wrapping arithmetic.

**One world at most** (Lew 2026-10-29, 1131). No frontend shows the
world twice. `Viewport::min_zoom` is the shallowest zoom at which the
world, 256 x 2^z pixels, is at least as wide as the view, so no copy
stands beside another; zooming out stops there. `Viewport::one_world`
holds a view to it: the zoom at least that, y inside the projection
(85.05 degrees north and south), and a world shorter than the view, as
on a portrait phone at its floor, centred in it. x still wraps, by moving
the one world. The terminal, the window and the phone get their view
through `loquax::ui::map::viewport`, which applies it, and `settle` writes
it back into the state after a zoom or a pan. The first view without pins
fits the newest installed or cached region (by file time, which a view of
an autoloaded region renews), else the world at that floor; the phone's
locale country is not used, since nothing maps a country to its box
cheaply yet.

**Where it comes from.** The cut lives in `leviculum-mapcut`, which
`leviculum-map-tool cut` and Loquax's autoload both call (1089). It reads
Protomaps PMTiles archives (one, or the parts of one build downloaded
separately, from a file or by HTTP range requests, only the directories
and tiles of the box) for a bounding box and writes the region file; the output does not depend on the
thread count or on the order the archive lists its tiles, and the same
input gives the same bytes. A named region and an OSM PBF are later steps:
a PBF reader exists (`osmpbf`, MIT OR Apache-2.0), but a PBF holds raw
nodes, ways and relations, and turning them into tiles (assembling
multipolygons, the coastline, generalising per zoom) is what planetiler does
for Protomaps; it is not cheap, so the PMTiles path stays the one input. Attribution and licence travel inside the file: the data is
ODbL ("© OpenStreetMap contributors"), every frontend shows the line on
the map view, and the region file's notice is what the open licence item
of handheld-messenger.md asks for.

**How a user gets one, zero config.** The 90 % rule (Lew, 2026-10-25; zero
config, #471) says the map works without a step. Decided 2026-10-27
(Lew), point 2 revised by Lew on 2026-10-28 (leviculum 1089):

1. Without any region file, the map view still works: pins on the world
   outline (section 7, Natural Earth). Positions are useful at once.
2. **Map data loads by itself** and is shown at once, with no further
   step: when the user goes to a place (a position report, the peer's
   position, `L` or `Alt-l`, or the map opened with pins no region holds),
   Loquax loads the region around it; a key (`d`) and the palette's `load
   map area` load the visible area. It can be switched off in the config
   (`map_autoload = off`). The first version of this point (2026-10-27)
   said nothing downloads without an explicit action; Lew revised it
   because a map that stays empty until the user learns a command fails
   the 90 % rule, and a user who wants no download has the switch.
3. The Debian package ships no region; a board's flash image may carry
   one cut for its deployment.

**The autoload** (1089), `loquax::mapload` over `leviculum-mapcut`:

- *Source.* A PMTiles archive read by HTTP range requests, so only the
  tiles of the box and the directories above them are fetched, never the
  planet. Default: the newest Protomaps daily build that answers
  (`https://build.protomaps.com/<yyyymmdd>.pmtiles`, looked for back ten
  days, found once a session). `map_source = <url>` in Loquax's config
  names another, for instance the project's own server later. Terms of
  the default, as Protomaps' download page (`docs.protomaps.com/basemaps/
  downloads`, read 2026-10-28) states them: the basemap is an ODbL
  Produced Work, OpenStreetMap attribution required, which the region
  carries in its attribution and notice; the bucket keeps the builds of
  the past week; and "URLs may change and hotlinking to these downloads
  are discouraged", the page advising a copy on one's own storage. The
  page sets no usage limits and says nothing of range requests from apps.
  So the default works and is honest about its source, but a fleet of
  messengers on it is the hotlinking the page discourages: a copy on the
  project's server, set as the default `map_source`, is the follow-up.
- *Around a place:* a box 6 km to each side (12 by 12 km), z10 to z15; a
  place counts as held when a region holds 3 km around it at z15. Measured
  2026-10-28 from the build of that day, cut on the coder host over its
  link: central Frankfurt 11.1 MB fetched, 3.87 MB region, 384 tiles, 8.3
  s; central Berlin 20.1 MB, 7.90 MB, 412 tiles, 11.7 s; the Vogelsberg
  countryside 2.5 MB, 1.26 MB, 360 tiles, 8.4 s. At most four places per
  opening of the map, one per group of pins a few streets apart.
- *The visible area:* the view's box, from three zooms above the view's to
  z15. Before anything is fetched the box's tiles are located from the
  directories and their bytes summed; over 64 MB of archive the deepest
  zoom is lowered, down to the view's own, and an area too large even
  there is refused with "zoom in" instead of a question. An area a region
  holds at the view's zoom loads nothing.
- *Showing it.* The region is written in place under its own name as it
  arrives (1291, Lew 2026-11-01: "watch the map fill in"): version 4,
  fetched and cut a round at a time, coarse to fine, each zoom of z0 to
  z12 a round of its own. A painter draws every tile there and stands the
  nearest coarser tile in for one not there yet; a round bumps the maps'
  progress (`Maps::arrived`, at most every 250 ms), a painter that sees it
  reads the directory again, and the tile cache draws again only the
  tiles it drew with stand-ins. A frame that shows such tiles says
  `filling`, and the phone asks for the next one in a quarter second.
  The names and the buildings come with the last round. Once complete
  the file is checked whole and the directory's epoch bumped; each
  painter reads the directory again before its next paint. A load that
  fails removes its file. Measured with `leviculum-mapcut`'s example
  `arrive` on the 1191 Berlin extract (89 MB, z0 to z15) behind the
  loopback server throttled to 2 MB/s and 50 ms: the first tile after
  0.27 s, every z13 tile after 10.7 s, the whole region after 58.2 s,
  where fetching everything first took 53.6 s with nothing to draw
  before the end. Every region that reaches into a view is drawn, each
  into its own box (1077's clip), the deepest last, so loaded patches
  join up (this replaces "one region or the outline" of section 7). A
  place's map is fitted again to the region when it lands, unless the
  reader moved it.
- *The screen first* (1292, Lew 2026-11-01: "the area he is looking at
  loads as fast as possible, always with the highest priority"). On the
  phone every frame, during a pan, a fling's coast and a pinch too, goes
  to one scheduler in the core, `loquax::ui::viewload`: the screen is
  sampled 16 by 16, and when the regions leave more than 10 % of it
  uncovered at the view's zoom, the cells under the uncovered samples
  are asked for at once, nearest the screen's centre first. A cell is the
  tile three zooms above the view's (z13 at most, about 3 km in Europe;
  above z10 only), so a pan asks for the same boxes again and a landed
  cell is never asked twice; its load reaches from three zooms above the
  view's to z15 as the visible area's does. Then the uncovered cells
  within two tiles of the screen, those in the pan's direction first, and
  while the view moves only those (from z12). One decision every 250 ms
  while the view moves, counted from the last decision, so the frames of
  a pan never restart the wait; every new frame at rest decides; the same
  view and regions again decide nothing. The loader orders its queue by
  that decision: a box asked for by hand (a place, the pins, "load this
  area"), then the screen's, then the ones next to it, then those the
  view left, of which it keeps the newest eight. A load the view left
  cannot be paused, the cut reads the archive in one call, so the
  screen's load starts beside it: two at most. Measured with the facade
  and a source that never answers (`a_pan_into_the_empty_loads_what_is_on_screen_at_once`,
  a frame every 16 ms): a pan of a screen and a half that rests, one that
  goes on slowly, a fling that coasts and a zoom out by two steps each
  ask within 32 to 112 ms of the first frame more than a tenth uncovered;
  before 1292 none of them asked at all.
- *Quiet and robust.* One load at a time on a thread of its own, two
  when the screen's starts beside a load the view left; the map title
  says `loading map`; `tui.log` has `MAPLOAD start`, `done` and `failed`
  lines with the box, zooms, bytes and `elapsed_ms`; a failure is one
  toast; a failed box backs off 30 s doubling to an hour, but a box that
  found no internet does not: "Offline" holds until the next load
  starts, which the next view the user moves to asks for, and no internet
  is said once until a load succeeds again. The phone's chip says
  "Loading the map of this area" only for a load for the screen
  (`loading_here`, 1292); a load for an area the view left says nothing,
  and "No map here yet" shows over it.
- *Disk.* Loaded regions are named `auto-<box>-z<min>-<max>.lmr` and are a
  cache of `map_cache_mb` megabytes (500 by default, a hundred or more city
  places), the least recently viewed evicted first, viewed meaning drawn,
  marked on the file's modification time at most once a minute. A file
  under any other name was placed by hand and is never evicted nor
  counted.
- *Privacy.* The source learns which areas are looked at, as any map
  server does. `map_autoload = off` stops the loads that start by
  themselves; `map_source` points them at a server the user trusts.

**Size per region**, measured 2026-10-27 (leviculum 1062) for layer profile
1, z0 to z15, from the Protomaps daily build of 2026-10-26 (Basemap 4.15.2,
OSM of 2026-10-26), cut on the coder host (10 vCPUs) with all threads. Each
region is a bounding box, so it holds more than the place: Frankfurt's box
is about 576 km² (the city 248), Hessen's about 44,000 (the state 21,100),
Germany's about 554,000 (the country 357,600). Germany's box was fetched
as three latitude bands of one build and cut as one source. Bytes include
each zoom's share of the index.

| Region | bbox (W, S, E, N) | PMTiles in | region | z0-10 | z11-12 | z13 | z14 | z15 | tiles | cut |
|---|---|---|---|---|---|---|---|---|---|---|
| Frankfurt am Main | 8.47, 50.01, 8.80, 50.23 | 27,400,758 | 9,878,095 | 461,584 | 839,112 | 2,011,232 | 2,783,328 | 3,782,294 | 1,363 | 0.18 s |
| Hessen | 7.77, 49.39, 10.24, 51.66 | 730,833,102 | 390,597,291 | 4,447,086 | 37,684,014 | 81,945,712 | 113,293,282 | 153,226,652 | 98,767 | 3.4 s |
| Germany | 5.87, 47.27, 15.04, 55.06 | 7,253,481,102 | 3,914,890,897 | 42,762,388 | 372,151,372 | 798,104,326 | 1,124,204,042 | 1,577,668,224 | 1,266,178 | 40 s |

What it says for the default zoom range: the region is a third (the city)
to a half (the state, the country) of the PMTiles archive it was cut from,
and z14 and z15 are two thirds of it.
Without z15 Germany's box is 2.34 GB, without z14 too 1.21 GB; a Bundesland
to z15 is 0.4 GB, a city 10 MB. So a city or a state fits the M9's card at
every zoom, and a whole country fits a card but not the 16 MB NOR, where
only a city-sized cut does. A view never reads more than its four to six
tiles, so the file's size costs disk, not drawing time. The z15 tiles of
these cuts are smaller per tile than the measurement's Florence centre
(1.6 KB a tile in Germany's box against 13 KB in Florence), because most
of a country is fields and forest.

Earlier, unchanged: the Protomaps sample of Florence centre (about 5 x 5
km, z0 to z15, all layers, gzip MVT) is 6,601,156 bytes, and cut under
profile 1 it is 1,724,066; one z15 view of it, decoded to flat geometry
without points, is 278,236 bytes for four tiles.

## 5. Slices

In order. Each has its acceptance; where it touches a real exchange, a
phase in the 1001 cell (periculum's `regression/loquax_tui_chat_session.toml`,
`loquax` against the stock Python LXMF client `lxmf_node.py` on one
`lnsd`).

**(a) Positions in the model, shown as text.** The engine passes the
fields; the core decodes telemetry and stream fields into positions; the
store gains `positions` and fills `fields`; the header and the detail
view show position and age; a telemetry message is one `◎ position` line
instead of an empty block. Depends on slice 6 (history, built).
*Acceptance:* unit tests over the decode with fixtures of each producer
(a board report, Columba's location layout, a Sideband blob with a nil
sensor, a stream row, an undecodable blob kept raw); the store's migration
from version 1 with the durability tests; a `tui_snapshot` of the header
at 80 and 120 columns with a position. *1001 phase:* Bob sends a
`FIELD_TELEMETRY` message with a position and an empty body; Alice's
header shows it within 5 s with its age, and her conversation has one
position line and no empty block. The Python helper has no verb that sends
telemetry today (`lxmf_node.py` has `send` and `send_propagated`), so the
phase needs a `send_telemetry` twin of `lxmf-node`'s verb, packing a
`Telemeter` blob the way `Location.pack` does: a periculum companion order.

**(b) The map core and the region tool.** Built 2026-10-27 (leviculum
1062): the region file and the size table of section 4; the committed
fixture region `leviculum-map/fixtures/firenze.lmr`, pinned at z15, z13 and
z11 by `leviculum-map/tests/region.rs`; the deterministic cut by
`leviculum-map-tool/tests/cut.rs`; `just map-core-no-std` in `guards`.
`leviculum-map` and
`leviculum-map-tool`, the region format, the size measurement of section 4,
a region of the test area committed as a small fixture. Depends on nothing
open; it can run beside (a). *Acceptance:* the cut is deterministic (same
input, byte-identical file, as `extract` is today); a host render of the
fixture region at three zooms matches pinned counts, as
`leviculum-mapfill/tests/fixture.rs` pins the fixture's; projection and
culling unit tests (antimeridian, a pin at the view's edge); a
cross-compile gate that holds `leviculum-map` to `no_std`, as `just
loquax-core-no-std` holds the core. *1001
phase:* none, no exchange.

**(c) The TUI map view.** Built 2026-10-27 (leviculum 1065): the palette's
`map`, `M` in the list and the conversation and `Alt-m` everywhere; the
pins by the display rule with their ages, the selected one `◉`, a name set
left of its marker at the picture's right edge; fit on open and with `0`,
`+`/`-` and the arrows or `h` `j` `k` `l`, `Tab` through the pins by name,
`Enter` or a click to open the peer's conversation, `Esc` back; the ladder
kitty or sixel, half-blocks, braille, forced with `--graphics` and named
in `tui.log` as `GRAPHICS backend=`; the attribution line under the
picture. Since 1075 a key pans an eighth of the view, `H` `J` `K` `L` and
Shift with an arrow a half, and the picture is painted and encoded on a
thread of its own while the loop reads keys: the keys that queued behind
a frame are applied together, and the newest view wins (section 6,
Sixel). Pinned by `loquax/tests/tui_snapshot.rs`
(`the_map_shows_the_pins_on_the_region_and_the_outline_in_braille_and_halfblocks`,
frames in `loquax/tests/fixtures/tui_map.frames`), the model's tests in
`loquax/src/ui.rs`, the ladder and the protocol bytes in
`loquax/src/tui/map.rs`, and the core's in `leviculum-map/tests/view.rs`.
The core gained per-zoom line widths, a lines-only ink, the world outline,
region choice and the zoom range for it. Left for later: label collisions
between pins (the later-drawn name covers the earlier), and how text over a
sixel picture looks in foot, which no test here can see.
As planned: the `map` action, the pins with names and ages,
fit, zoom and pan, the three drawing backends, the attribution line.
Depends on (a) for positions and (b) for the core. *Acceptance:* a
`tui_snapshot` of the map view in the half-block and braille backends
(graphics protocols cannot be snapshotted as cells; their choice is a unit
test over the backend ladder, as `choose_backend` has today); pins
positioned by projection tests; clicking a pin opens the conversation.
*1001 phase:* after (a)'s phase, Alice opens the map and her frame shows
Bob's pin with his announced name.

**(d) The iced GUI map.** Built 2026-10-28 (leviculum 1084), Lew's
decision that day to start it: `loquax gui`, behind Loquax's `gui` feature,
which no other feature enables, so the CLI, the terminal and the core carry
no iced. The window starts the same shell as the terminal (`loquax/src/
shell.rs`, moved out of `tui.rs`: the engine, the history, the sends and
fetches, the propagation node's schedule, the model) and shows the map: the peers
with a position on the left by the display rule with their fix's age, the
stale ones dimmed; on the right the region from the same maps directory,
the outline where none covers the view, the pins with their labels as iced
text, the credit always in the bar below. Drag pans, the wheel zooms about
the pointer, `+` `-` `0` as in the terminal, a click on a pin or a row
selects it; integer zooms only until 1144, when the window took the
phone's frames and the zoom between the steps. The window counts in the screen's own
pixels and runs the model's map functions with one pixel a cell; a dense
screen draws its own pixels with the lines widened by its scale
(`Look::scaled`). The picture is `loquax::picture`, which the terminal
draws through too, and the label rule moved into the core as
`leviculum_map::pins::labels`, in cells for the terminal and in pixels
with the window's font measuring each label. Pinned by
`the_map_frame_over_the_fixture_at_three_zooms` in
`loquax/src/gui/map.rs` (the window's own pipeline, no display needed),
`a_drag_keeps_the_names` and `a_fractional_wheel_zoom_stays_where_it_ends`
in `loquax/src/gui.rs`,
the label rule's tests in `leviculum-map/tests/labels.rs`, and `just
loquax-gui`. The binary is built for the gnu target: winit opens X11 and
Wayland with dlopen, which a musl-static binary cannot. Conversations
and compose came in 1145 ([the user interface](loquax-ui.md), "Built
2026-10-30, the desktop chat"): the window opens on the chat, the map is
its other screen, and a position line opens it at the sender's pin.
Left for later: the package. *1001 phase:* the
same as (c) once the cell can drive the GUI; until then none.

**(e) The M9 map.** The view on the panel with the region on flash.
Depends on (b), the M9's stage two (its keyboard UI, [The ThinkNode
M9](thinknode-m9.md)) and the M9 frontend of the messenger core.
*Acceptance:* the rig measurement repeated with the region file in place of
the fixture, frame and flush time against the device rows of the draft;
the orientation (MADCTL) settled. *1001 phase:* none; its exchange is a
hardware cell.

**Parallel.** (a) and (b) are independent and can run on two lanes at
once. (c) needs both. (d) and (e) need (b) and their own frontends, and
neither waits for the other.

## 6. The terminal's graphics

What the TUI can draw a map with, in the order the backend ladder tries.

**The kitty graphics protocol.** An APC escape that sends pixel data
(PNG or raw RGB) and places it on a cell rectangle; it can keep an image on
the terminal and only move it, which suits a map that redraws on a pan.
Spoken by kitty, WezTerm, Ghostty and Konsole by their own documentation,
not checked here. The crate is
`ratatui-image` 11.0.6 (MIT), already in the tree behind
`leviculum-ui-ratatui`'s `images` feature
(`leviculum-ui-ratatui/Cargo.toml:40`), built without its default
`chafa-dyn` so no C library comes in; `lnomad` draws pictures through it.
Its terminal query (`Pictures::detect`,
`leviculum-ui-ratatui/src/images.rs:113`) picks the protocol at start and
falls back to half-blocks.

**Sixel.** The DEC bitmap format, palette-based (so a map's flat colours
fit it well), redrawn whole on every change. Spoken by foot, xterm (with
`-ti vt340` or its resource), mlterm, WezTerm and Konsole by their own
documentation; only foot is checked here. `ratatui-image` encodes it through `icy_sixel` 0.5.0
(MIT OR Apache-2.0), pure Rust. Checked on the coder host: foot 1.21.0's
`foot.ini(5)` has `sixel` default `yes`, its `foot-ctlseqs(7)` lists the
kitty keyboard flags (query, push, pop, set) and no kitty graphics escape.
So a terminal that speaks the kitty keyboard protocol need not speak kitty
graphics: foot speaks the one and not the other, and draws the map by
sixel.

`ratatui-image` hands every sixel to `icy_sixel`'s Wu quantiser with
Floyd-Steinberg dithering at its defaults, and offers no setting for
either. A map frame has eight or nine colours, so that was most of a
frame's cost and bought nothing: measured 2026-10-27 (1075) on a 1395 x
1235 pixel pane over Frankfurt, the encode took 43 to 52 ms of a 49 to 71
ms frame. The painter now writes a sixel of the frame's own colours
itself (`leviculum_ui_ratatui::images::sixel_indexed`, `ratatui-image`'s
path stays for tmux and past 256 colours): 1.4 to 4.2 ms, frames of 6 to
22 ms, and at z14 304 KB to the terminal instead of 473 KB, each band
painting its commonest colour as one run. No visual cost: the colours are
the style's, not a quantiser's. Zoom stays in whole steps: a fractional
zoom would scale every tile by a non-power of two, which the viewport,
the tile selection and the draw transform of `leviculum-map` do not
carry today, a change to the core of all three frontends rather than to
the terminal. Lew's terminal is foot (the reviewer checked on his machine,
2026-10-27), which is why the ladder must not assume kitty graphics. The
`KEYS` line in `tui.log` names the keyboard, and slice (c) adds a
`GRAPHICS backend=` line beside it, so `tui.log` names the backend on
any machine.

**Half-blocks.** `▀` with a foreground and a background colour gives two
pixels per cell, in truecolor or 256 colours: the 80 x 24 layout's
conversation pane, about 52 x 20 cells, gives a map of about 52 x 40
pixels. Coarse, but filled areas and roads
are recognisable, and every modern terminal draws it. `ratatui-image`
already falls back to it.

**Braille dots.** `⣿` and its kin give 2 x 4 dots per cell in one
colour per cell: about 104 x 80 dots in the same pane, the sharpest line
drawing a terminal without graphics can do, but no filled areas and one
colour per cell. ratatui's own `Canvas` with `Marker::Braille` draws it
(ratatui 0.30.2, MIT, in the tree), and its `Map` shape is the world
outline of section 4. It is the backend for a terminal with neither
graphics nor 256 colours, and for `NO_COLOR`: roads and coastlines as
lines, pins as text.

The ladder, decided 2026-10-27 (Lew): kitty or sixel where the query finds one, else
half-blocks in colour, else braille. It is the ladder `lnomad`'s pictures
already climb, plus braille at the bottom.

Behind mosh it is braille (Lew, 2026-10-28, 1088). Measured on mosh
1.4.0 on the coder host: mosh answers the DA1 query itself, without
sixel, and drops a sixel DCS and a kitty APC whole, so no protocol
arrives; the half-blocks' `38;5`, `38;2` and `▀` pass unchanged. Lew's
map from Termux over mosh nonetheless showed an empty box in every cell
(`GRAPHICS backend=halfblocks detected=halfblocks`), a client-side
failure this host cannot see, so the ladder does not take half-blocks
there. mosh sets no variable; it is found as a `mosh-server` among the
parent processes (`leviculum_ui_ratatui::images::Between`). tmux and
screen are found too and named in `tui.log`, and change nothing: both
pass 256 colours, and `ratatui-image` wraps sixel for tmux. Inside tmux
a mosh session that holds the tmux client is not an ancestor and is not
seen. `GRAPHICS` names the rung and why: `reason=no-graphics,mosh`.

**The text techniques, and the ladder since 1093.** Lew, 2026-10-28:
Loquax draws the most beautiful map the terminal can show, real graphics
where the terminal has them, then the best text techniques in order, and
a key cycles through every way live, the one the reader stays on
remembered. The techniques and the measurements behind their order are
`docs/drafts/terminal-map-rendering-2026-10.md` (1090). Built in 1093:

- The encoders are the map core's, `leviculum_map::cells`: one view
  drawn at 4 x 8 pixels a cell (one zoom deeper at twice the line width,
  so the view keeps braille's ground) and encoded into one glyph and two
  colours a cell. `octant` (U+1CD00, 2 x 4), `blocks` (quadrants and
  eighth blocks, the best fit a cell), `braille-thin` (one-dot lines in
  their colour over the areas' colours) and `mosaic` (one colour a cell)
  join half-blocks and plain braille. The octant fit runs on the eight
  2 x 2 sums in Gray code order and chooses exactly what the brute force
  over the 32 pixels chooses (a test holds it): 5.8 ms at 155 x 65 cells
  against 31 ms, release build on the coder host.
- The ladder: kitty or sixel where the terminal answers and neither mosh
  nor tmux sits between; octants where `TERM` or `TERM_PROGRAM` names a
  terminal that draws them itself (kitty, foot, WezTerm), and behind mosh
  only from a host whose glibc is 2.41 or later (an older mosh-server
  drops them); `blocks` at 256 colours or more without mosh and outside
  Termux; `braille-thin` otherwise, at 16 colours with each class in its
  own of the sixteen (`leviculum_mapfill::style::ansi16`); plain braille
  without colour. Half-blocks and the mosaic are no rung, only a choice.
  tmux now keeps the pictures off the top: it passes them only where its
  configuration allows.
- `t` and `T` (and `F2`) cycle every way in the ladder's order, the title
  naming the new one a moment; the palette's `map drawing` lists them
  with the current one marked. A way the facts say will not show is still
  offered, marked: the reader's eyes beat the detection. `g` shows the
  glyph test over the map, a line a glyph set and the ways that need it.
- The way the reader stays on is written to the config when they leave
  the map or quit, as `map_drawing.<context>`, the context
  `<mosh|tmux|screen|local>:<TERM_PROGRAM, else TERM>`. A remembered way
  wins over the ladder at the next start unless it cannot work there at
  all (a picture behind mosh), which is said once.

### Sharing for a while (1273)

Lew, 2026-11-01: sharing a position must be possible once, for a
certain time, or until stopped, and ending it must be simple. The
position sheet behind the compose bar's ⌖ (1270) has five rows: "Now,
once" sends one report as before; "For 15 minutes", "For 1 hour", "For
8 hours" and "Until I stop" start a **share** for that chat. The model
owns it (`loquax/src/ui/share.rs`): its end time, its cadence and the
last report it sent. The platform only delivers fixes and the clock.

**The cadence**, constants in `share.rs`, after the board's rule in
[announce cadence](announce-cadence.md) (5 min moving, 1 h still),
shortened at rest so the receiver keeps the stream live:

| Rule | Constant | Value |
|---|---|---|
| the share's first fix goes at once | | |
| a fix farther than this from the last report goes | `MOVED_M` | 25 m |
| never two reports closer than | `MIN_GAP_S` | 60 s |
| after a report for a move, the next at the latest | `MOVING_EVERY_S` | 5 min |
| at rest, one report every | `STILL_EVERY_S` | 15 min |

The distance is the equirectangular one, exact enough at 25 m. A clock
without a new fix still sends the rest report from the newest fix (a
phone at rest may deliver none), and the phone's session wakes at least
every `MIN_GAP_S` while a share runs.

**On the wire** every report of a running share carries the share mark
of [telemetry](telemetry.md), "The extension ladder": `"every": 900`,
the longest the next report waits, as the location's eighth element.
The stop sends one last report from the newest fix with `"ended":
true`. A receiver's liveness (1209, `ui::liveness`) keeps a stream live
for twice the promise whatever cadence it learned, so a sharer who walked
(reports a minute apart) and then sat down stays live for half an hour
between rest reports; the end report makes the stream not live at all,
only as old as it is: the chat's header and the Friends sheet say the
age words (`last position 2 min ago`) from then on. A receiver that does
not know the mark sees the same fixes and an older last one.

**Visible while it runs, stoppable in one tap.** The chat shows a chip at
its top, `scene::ShareChip`: "Sharing your position, 42 min left" (the
time left rounded up, minutes under an hour, else hours) or "Sharing
your position until you stop", and its Stop. The list's row carries
`sharing`, drawn as a small ⌖ in the accent colour on our side. Settings,
Privacy, "Position sharing" lists every running share by name with its
words and a Stop (`scene::share_rows`). The terminal cannot share (it has
no position), but draws the chip row of a share the model holds under
the mailbox's row, `⌖ Sharing your position, 42 min left · click to
stop`, and a click on it stops the share (`tests/fixtures/tui_share_chip.frames`).

**Android.** The foreground service `ShareService` (type location)
keeps the fixes coming while a share runs: GPS and the network provider
every 30 s, and in the debug build the walk's seeded fix every 15 s. Its
notification says "Loquax shares your position", with whom and how long,
and has a Stop that ends every share with its end report. It starts from
the sheet's row and whenever the list says a share runs (a share of a
restart among them), and ends itself once the library lists none. A
share survives a restart (schema 10, [storage](loquax-storage.md)); one
whose end passed meanwhile ends at the first clock with its end report.
No share survives an uninstall: the store goes with the app. Android 14
refuses a location service started from the background; a share
restored before any screen then runs on the fixes a screen brings until
one starts the service.

### Positions the mesh announces (1302)

Lew, 2026-11-01: the map shows every position the mesh announces,
mailbox or not. A node announces a second destination, `lev.position`
(SINGLE, of its identity), beside its delivery destination; its app data
is the fix (the mailbox for friends study of order 1296, section 4.3,
on master). The codec is the board side's, `leviculum_lxmf::lev_position`
(order 1301): a msgpack map with the coordinates, the fix's age and the
node's name, either plain or sealed to the owner's identity
(`mesh_position::read`, 1313).

**Who sees what.** A plain position is read by everyone: a board set to
share with everyone nearby, and `loquax announce`. A sealed one opens only
with the identity it was sealed to, so the owner's own boards show on the
owner's Loquax, which holds the home identity. A stranger's sealed
announce is recognised (`AppData::parse` says `Sealed`) and shown
nowhere. A client of a home node holds no private key and shows the
plain ones only. Privacy over visibility: plain is the board's opt-in.

**The event.** The engine sees every announce its core hears
(`engine.rs`, `on_event`), a client of a home node too, since the check
needs no router: a `lev.position` announce whose app data decodes
becomes `OutboxEvent::MeshPosition { identity, location, name }`, the
identity hash the announce proved, the fix timed by when the node heard
it less the fix's age, and the announced name. The model takes it as
`UiEvent::MeshPosition { identity, fix, name, age }` (the engine's
event folds the same way, with its name and age 0); a shell that holds an
announce itself hands its name and how long ago it heard it.

**The merge rule.** The peer is the identity's `lxmf.delivery` address
(`mesh_position::delivery_of`, the same hash a delivery announce of that
identity proves), so an announced fix joins the peer's telemetry
positions in `UiModel::positions`: the newest fix wins by
`Fix::newer_than` whichever way it came, and `Fix::source` remembers
which (`Telemetry` or `Mesh`). Until 1385 it was a state the store did
not keep; since schema 14 the store keeps every announced fix for the
track, marked `positions.source = 1`, one row a peer and fix time
([tracks on the map](loquax-tracks.md)). An announced fix newer than every
report is the row's newest fix (`UiModel::liveness`), so the list's ⌖
(1224), the ring and the Friends sheet show it unchanged.

**The pin and the card.** A peer whose shown position came from the mesh
and who is none of the reader's people (`UiModel::is_contact`: ourselves,
a conversation, a petname or an address book link) is **nearby**
(`UiModel::is_nearby`, `MapPin::nearby`, `FriendRow::nearby`). A peer
known from a collector's stream keeps the pin it had. Nearby is drawn as
a neutral grey pin with the announced name and no avatar: the phone's
`AvatarView` draws a grey circle without letters or photo, the window a
grey marker, the terminal the muted colour. Its card says "Nearby, not in
your contacts" with **Add** in place of Chat and Share; Add opens its
conversation and makes the announced name the reader's name for it
(`UiModel::add_nearby`; on the phone the `addcontact` request of the
code reader, 1263, with the bare address). In the terminal the map's
title is the card while the pin is selected (`map · Nearby, not in your
contacts · Enter adds`) and Enter adds it; the window's pin list carries
the line and an Add button. No setting: nearby nodes are a fact of the
mesh; hiding them is a later option if someone asks.

**Seeding one.** `loquax announce --position LAT,LON [--from NAME]`
attaches as `send` does and announces the position once, plain and
named, as a manual one (`mesh_position::manual`,
`Command::AnnouncePosition`, which registers the `lev.position`
destination of our identity on first use). The device walk's
`offgrid_positions` uses it with the mailbox stopped
([device walk](loquax-device-walk.md)).

### The light data (1684)

The map core carries four small tables of reference data
(`leviculum-map`, provenance in `leviculum-map/fixtures/README.md` and
THIRD-PARTY.md):

- **The time zone view.** One control, in the map's menu like the tracks'
  time window (the window and the phone) and `z` in the terminal and on
  the M9: the world's zones (timezone-boundary-builder 2026f, the variant
  with the oceans, ODbL, 63 zones in 1.5 MB) with their borders in a
  violet no other line of the map has and the current time written into
  each, `HH:MM`, in place of the names; the clocks turn with the minute.
  The rules come from the system's tzdata on Linux and Android
  (`/usr/share/zoneinfo`, the packed `tzdata` file), the board's own table
  (the TZif footers of tzdata 2026f) where the system has no zone and on
  the M9, which shows the clocks once it knows the time. The borders go
  over the composed frame, never into a cached tile.
- **Height above sea level.** The wire's altitude is the GNSS height over
  the WGS84 ellipsoid, as Android and Sideband send it, and since 1684 the
  M9 too (its GGA height plus the sentence's geoid separation). Every
  place a height is shown subtracts the EGM2008 geoid (15' grid, 2 MB,
  public domain), which lies up to about a hundred metres from it.
- **A place's facts.** The detail of a position names the country it lies
  in (Natural Earth 1:50m), its emergency numbers, the side of the road
  and its currency (Wikidata, CC0), and the magnetic declination there on
  the fix's day (WMM2025, public domain). A compass arrow and a measuring
  tool do not exist yet; the declination is ready for them.

## 7. Decided

Lew, 2026-10-27, accepted the five proposals of this section as written.
Each alternative stays as one line, rejected, so the reasoning stays.

- **Decision 6: the vector path holds.** The device measurement says yes
  for the stripped profile: 90 ms to draw plus 31 ms to flush on the M9,
  177 ms plus 31 ms with buildings
  (`docs/drafts/map-measurement-2026-10.md`, "Device rows, measured
  2026-10-26"). The PNG raster fallback is dropped, and the region format
  of section 4 is the one format for the desktop and the board.
  *Rejected:* the PNG raster tiles of Meshtastic's layout, a second format
  to cut, ship and test that the measurement does not need.
- **TUI graphics: the ladder of section 6.** Kitty or sixel, then
  half-blocks, then braille. Lew's terminal is foot, which draws by sixel
  and speaks no kitty graphics, so the ladder must not assume kitty.
  *Rejected:* braille only, which needs no `ratatui-image` in Loquax but
  gives no filled areas.
- **No default region in the package.** The world outline until a region
  is there (section 4). *Rejected:* a small default region in the
  package, which would have to be chosen for someone. This decision said
  "one explicit action to add one, no download of Loquax's own" until Lew
  revised that half on 2026-10-28: map data loads by itself when the user
  goes to a place, switchable off (section 4, point 2, and "The autoload";
  leviculum 1089). The reason is the 90 % rule of #471: a map that stays
  the world outline until the user learns a command is a map most users
  never see. *Rejected with it:* the explicit `loquax map add` as the only
  way in.
- **Our own position is not sent by default.** Loquax sends no position
  until the user switches it on per peer, and answers no telemetry
  request until then. A messenger that broadcasts where its user is
  because nobody changed a default is the failure this avoids; the cost
  is that two Loquax users see each other on the map only after one step
  each. *Rejected:* sending by default.
- **Stale threshold 2 h** (section 2): dimmed after that, never removed
  for age alone. *Rejected:* removing a fix when it ages out.

The reviewer decided three more for slice (c), 2026-10-27 (leviculum 1065):

- **The world outline is Natural Earth**, public domain, the 1:50m land
  polygons of version 5.1.2, baked by `scripts/bake-world-outline.py`
  into `leviculum-map/data/ne_50m_land.lwo` (159,357 bytes, 1,422 rings,
  59,239 points, a step one pixel at zoom 10) and built into the binary;
  source and checksum in `leviculum-map/fixtures/README.md` and
  THIRD-PARTY.md. Drawn as land over water in colour and as coasts in
  braille, down to zoom 10. Since leviculum 1092 the country borders lie
  over it, Natural Earth's 1:50m land boundary lines of the same version
  (`ne_50m_admin_0_boundary_lines_land.lwl`, 49,512 bytes, 393 lines),
  thin lines in the style tables' border colour, dots in braille, down to
  zoom 10 as well; maritime boundaries are left out. Until leviculum 1073 it was the 1:110m
  polygons quantised to 611 m, 21,298 bytes, which looked coarse without
  a region; one table serves every zoom, a z2 view of a sixel pane
  rendering in under 6 ms.
  *Rejected:* the world file ratatui's `Map` shape draws, whose licence
  is unsettled.
- **`loquax map add` is not in slice (c).** Superseded by the autoload
  (1089), which uses the cut as a library (`leviculum-mapcut`) rather than
  spawning the tool. Regions placed by hand still work:
  Loquax reads every `*.lmr` in `${XDG_DATA_HOME:-~/.local/share}/loquax/maps/`,
  or in `$LNMSG_HOME/maps/` when `LNMSG_HOME` is set. That variable
  replaces the whole default base for the identity and the store, so it
  replaces this one too, and a test or a second profile sees only its own
  maps. The man page says how to cut a region with
  `leviculum-map-tool cut`. Spawning the tool or a library feature is
  decided with `map add`.
- **One region or the outline, never a patchwork.** Superseded by 1089:
  loaded patches must join up, so a view now draws every region that
  reaches into it whose zooms reach the view's, each inside its box, the
  deepest last (`leviculum_map::choice::drawn`), over the outline. Until
  then: a view drew the
  installed region of the deepest max zoom whose box holds the view's
  centre and whose zooms reach the view's (its shallowest down to eight
  past its deepest), two alike by file name; the outline where none does.
  The zoom range offered is from the world down to the deepest a region
  draws at, and at least to zoom 10.
- **A region ends at its box.** The cut writes every tile that touches
  the box, so its data covers the box rounded out to each zoom's tile
  grid: at z8 Frankfurt's reaches 1.04 degrees east of the box, at z15
  none. Drawn whole, those tiles moved the region's edge as the reader
  zoomed. The core draws a region into its box alone (`Region::area`,
  `draw::draw_in`), so the edge is the same place on the earth at every
  zoom, within half a pixel, and the outline is drawn under the rest of
  the view (1077).

## See also

- [Loquax: tracks on the map](loquax-tracks.md): each peer's current
  movement as lines, the rest collapsed into places, the time window
  (1385)
- [A messenger whose core runs anywhere](handheld-messenger.md), "The map"
- [Telemetry](telemetry.md), the format and the collector exchange
- [A GUI toolkit for the desktop and the M9](gui-toolkit.md)
- [Loquax: keys, mouse and layout](loquax-keys-and-layout.md), the slices
  this page's slices follow
- [Loquax: conversation storage](loquax-storage.md)
- [The ThinkNode M9](thinknode-m9.md)
