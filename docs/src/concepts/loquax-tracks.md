# Loquax: tracks on the map

Lew, 2026-11-03 (#474): Loquax draws a peer's track on the map. The map
answers three questions about a peer: where is she now, is she moving,
and how did she get there. So the default picture is the **current
movement** as lines, and everything older is **collapsed into places**.
A year of a tracker in a handbag is one large blob at home and a few
small ones, never a scribble. The older lines stay reachable behind a
**time window** control: a map control, not a setting, but a hidden one
in the map's menu (Lew, 2026-11-04: the five chips across the map took
too much room over the place names and are needed rarely).

This page holds the rules and the thresholds the pictures settled
(order 1385). The rules are pure functions in `loquax::ui::tracks`, one
test per rule; the drawing is each frontend's, over the same derivation.

## Why not hiding by age

Lew's first idea was a setting "hide tracks older than". It answers the
wrong question. A peer who walked for an hour this morning and has been
at home since is best shown by that walk, however old; a tracker that
moves every day for a year would show a year of lines at "keep 1 year"
and nothing at "keep 1 hour" while it sits at home. Age says nothing
about which lines tell the story. A stop does: the movement since the
last long stop is the one the reader asks about, and the time spent in
one spot is what makes a place. So the map draws episodes and places, and
age only sets how strongly a line is drawn (rule 5). Nothing is ever
deleted for its age; the store keeps every fix, and the time window shows
any day again.

## The rules

The first values were the order's; the six scene pictures (below) kept
them all and added three refinements, marked *settled by the pictures*.

1. **Segments.** Two consecutive fixes of a peer are joined by a line
   iff the gap is at most **30 min** and the implied speed is at most
   **250 km/h** (69.44 m/s). Otherwise there is a gap. A fix that fails
   the speed rule against both neighbours is an **outlier**: a faint dot,
   never on the line; the first and the newest fix have one neighbour and
   are never outliers (a real jump is a gap, and the newest fix is where
   the peer is). A fix whose accuracy is worse than **500 m** is never on
   the line nor in a dwell, and is drawn as a dot too. Accuracy 0 means
   unknown and passes.

   *Settled by the pictures:* fixes closer than one screen pixel to the
   last fix kept are drawn as one, and two fixes are joined along the
   chain of the fixes between them, not by their own gap. A slow walk at
   a low zoom first broke into gaps after 30 minutes of sub-pixel steps.

2. **Dwell.** Fixes within the larger of their accuracy and **50 m** of a
   stay's first fix, for at least **10 min**, are one dwell. There is no
   gap limit inside a dwell: a board that reports hourly from home stays
   at home between reports. A dwell is a blob whose radius is **4 px at
   10 min and 2 px more for every doubling**, at most **14 px** (20 min
   6 px, 40 min 8 px, 80 min 10 px, 160 min 12 px). A dwell of **3 h** or
   more ends an episode.

3. **Episode.** The current episode is everything after the last
   episode-ending dwell, widened to at least the last **24 h**; without
   one it is everything. Lines are drawn only inside the time window, and
   the default window is the current episode.

   *Settled by the pictures:* a **silence of 3 h** ends an episode as a
   3 h dwell does. A peer that shares only while it moves (a phone that
   shares a walk, then nothing for a week) never shows a dwell, and its
   week-old walk stayed a line beside today's.

4. **Places.** Every fix outside the window is aggregated into places:
   cells of **8 logical pixels** at the render zoom (two cells across in
   the terminal). A cell credits each fix with the time to the next fix
   when that fix is in the same cell, at most **6 h** a fix, so a tracker
   that was off for a week at home did not stay a week more. A cell with
   less than 10 min is passage and drawn as nothing. A place is a faint
   blob (22 % opacity) sized by its total dwell with the dwell's radius
   rule, at most **20 px**; the largest **3** carry their total as a
   label ("149 d").

   *Settled by the pictures:* a cell closer than **2 cells** to a larger
   one joins it, so a place the grid cuts is one blob with one label.

   **The budget.** One peer costs at most **2 000 segments** and **176
   blobs** (64 dots, 64 dwells, 48 places), however many fixes the store
   holds: fixes under a pixel apart are drawn as one, what is left over
   the segment budget is thinned evenly with the newest kept, and the
   newest dots, dwells and places are kept. A test feeds 100 000 fixes of
   a wandering walk (`a_hundred_thousand_fixes_stay_inside_the_budget`)
   in three windows at three zooms.

5. **Age.** Opacity inside the window goes by the absolute fix age, in
   steps: under **10 min** full (100 %), to **1 h** strong (80 %), to
   **6 h** half (50 %), to **24 h** faint (35 %), older very faint with a
   floor of **25 %**, never invisible. The terminal gets four levels:
   full and strong share the top. The newest fix is always the full pin
   with name and age; a stale one (2 h) is dimmed as before, the hollow
   marker the order asks for is not drawn yet.

6. **Colours.** Eight hues from the peer's address, the first byte
   modulo 8, the same on every device: blue, orange, green, red, violet,
   teal, magenta, brown, with a lighter set for the dark map so each
   holds against its ground. The pin's marker and its name take the hue;
   our own track is neutral grey. More than eight peers share hues; the
   label tells them apart. In the terminal: the hue blended into the
   map's ground by level on a colour terminal, the nearest of the sixteen
   ANSI colours (dim below the top two levels) at sixteen, and plain
   glyphs, dim below the top two, on a monochrome one.

7. **Direction.** When the newest fix moves faster than **1.5 m/s**, by
   its own speed or, where it carries none, by the speed from the fix
   before it, the pin carries a small wedge pointing along the field's
   bearing, or along the last two fixes when the field carries none. In
   the terminal an arrow in the cell beside the pin, eight ways.

8. **Quiet peer.** Retired by 1447 (walk 1 finding 7): a position has
   one age on the screen, the card's, in the reader's words ("vor 28
   Std."). The line "no fix for 3 h" under the pin was a second age in
   another language and is drawn on no target any more; how long a peer
   has been silent is that one age.

9. **Privacy sentence.** The share flow is to say plainly that contacts
   who see the position also see the way. No new permission; the
   receiver holds the fixes anyway. Not built in 1385 (see "Not yet").

## The time window

Five named windows and the default: **now** (the current episode,
"Current movement" in the menu), **today** (since local midnight),
**yesterday** (the local day before), **this week** (since Monday 00:00
local), **this month** (since the 1st, 00:00 local), **all**.

Nothing of it is on the map (order 1419). It lives in the map's menu as
"Time window" ("Zeitraum"), a single choice of the six with the default
first and the one shown marked. While a window other than the default is
chosen, its word follows the map's title, "Map · This week" ("Karte ·
Diese Woche"), so the reader knows why older lines show; with the
default the title is "Map" alone. The entries, the choice and the title
come from the core (`TrackWindow::CHOICES`, `TrackWindow::title`); the
frontends draw them.

- **Terminal:** `w` steps to the next window, `W` to the previous, a
  toast names it ("tracks: week"). No strip; the chosen window's word
  stands before the credit on the map's credit row, only when it is not
  the default.
- **Window:** the map bar's More opens the map's menu over the map's
  corner: "Time window" with the six, and "Fit all". A choice closes it,
  Escape too. The bar reads "Map · This week" beside Chats.
- **Phone:** the overflow menu (the three dots) has "Time window" and
  "Fit all" before Settings and Help. "Time window" opens a single-choice
  sheet of the frame's `track_windows` (word, label, chosen); the chosen
  word goes back in the map request (`track_window`), and the frame's
  `title` is the screen's title. Java draws only the menu entries and the
  sheet.
- **M9:** none yet; when the map slice comes, the window is a key on
  the map screen and the title word after the map's title, as on the
  others ([A messenger whose core runs
  anywhere](handheld-messenger.md), "The tracks' time window on the
  M9").

## Where the fixes come from

Every position a peer's message carried, and since schema 14 every
position the mesh announced (`lev.position`, 1302): the store keeps one
row a peer and fix time, `positions.source` 0 for a message's and 1 for
an announced one; a fix a message carried takes the row of the same fix
announced, so the row names its message. Our own receiver's fixes (the
phone's GNSS, the board's, the desktop's mesh position) join our own
track as they come and, since 1386, are stored too, a row a minute at
most (`OWN_TRACK_EVERY_SECS`), whatever the receiver's cadence: the grey
own track is **the way back** to the car after a restart, without a
network. A day outdoors with the receiver on costs at most **1 440 rows**,
about **200 KB** at 142 bytes a row; the phone's 5 s watch alone would
have been 17 280.

**What it costs.** A day at a 5 min cadence is **288 rows**. A year of
one peer at that cadence (105 120 rows) measured **15.1 MB** of SQLite
file, 142 bytes a row: the key holds the identity and the peer as blobs
and the primary key's index repeats them. In memory the model keeps a
16-byte reading and a time per fix, 32 bytes, 3.4 MB for that year.

## The drawing

**Pixel screens** (the window and the phone, through `screenmap`): the
places of every peer first, under everything; then per peer its lines,
oldest step first, at 3 logical pixels, its dwells as a filled disc with
a rim, its dots, its wedge, each peer's layer blended once so two
segments meeting at a fix do not darken it; the pins last. The window draws its pins on its
own canvas in the same hue.

**The terminal:** the lines as braille dots, two by four a cell, over
whichever technique draws the picture; a dwell `•`, a place `○`, a dot
`·`, the wedge an arrow; in ASCII `.`, `o`, `O`, `,` and `^ / > \ v < `.
A cell holds one track; where two cross the later wins.

## Radar without a map

Where the view is deeper than the world outline (z10) and no installed
region touches it, the map has nothing to draw but coasts that are not
there. It draws a **radar** instead (`loquax::ui::radar`, 1386): the
ground blank in the map's paper colour, north up, rings of distance about
the own position, and each pin says where it is from there, "anna · 1.3
km NE, 22 min ago". Without an own position the radar centres on the
newest peer and its note says so ("radar on anna, no own position").
The tracks are drawn over it by the rules above, unchanged; only the
ground differs. The terminal draws the rings as faint `·` (`.` in
ASCII), the centre `+`, the note on the credit's row; the pixel screens
draw them as thin grey circles with their distance on the north
crossing, an "N" arrow at the top right and the note at the top left.
On the phone the avatars carry the same words under them.

**The ring step by zoom** is the largest of 1, 2 and 5 times a power of
ten at most half the view's shorter half: two or three rings around the
centre at any zoom, out to the screen's diagonal. On a 480 by 320 window
at 46° N that is

| zoom | step |
|---|---|
| 11 | 2 km |
| 13 | 1 km |
| 15 | 200 m |
| 17 | 50 m |

and a terminal's 2 by 4 pixel cells give about a quarter of the window's
step on the same cells.

**Zooms.** The outline ends at z10, and a fit to a pin no region holds
used to stop there, 70 km across. Where no region holds the centre a
fit now goes to z15, and the reader may zoom on to z17 (`radar::DEEPEST`); the phone lifts
its zooms only while it has a pin, since without one there is no radar.

## The small marks (1386)

- **Late arrival.** A fix whose receive time is more than 30 min after
  its fix time (`LATE_AFTER_SECS`) arrived late: the segment it ends is
  dashed (6 on, 4 off, running on across a late stretch), the terminal
  draws it `┄` (`:` in ASCII) where no on-time line crosses the cell,
  and the message's detail says "arrived 2 h late". The reader sees
  where the peer was out of reach. The flag rides in the fix's padding,
  so a fix stays 32 bytes in memory.
- **Battery.** When the fix's Telemeter carried a battery sensor below
  20 %, the pin gets a small battery, a fifth of it red, centred above
  the marker (top right of the avatar on the phone, `▯` after the name
  in the terminal, `!` in ASCII). The detail view lists the reading as
  before.
- **Fit all.** `f` in the terminal and the window, an entry of the
  map's menu in the window and on the phone (1419): the view
  fits every peer whose newest fix is inside the tracks' time window; for
  the current episode that is the last 24 h, since a peer silent longer
  ended its episode by the silence. With none inside, every pin. `0`
  still fits every pin.

## The pictures

Six synthetic scenes around Florence (`loquax::ui::tracks::scenes`) are
drawn through the real drawing code as goldens, in cells
(`loquax/tests/golden/tracks-*.txt`) and in the window's pixels
(`loquax/tests/golden/gui-tracks-*.png`): a hiking group of four, one of
whom turned back and went quiet; a family in the Boboli gardens with
crossing loops and a shared stop; a convoy of ten cars on one road; one
peer with a week-old trip and a fresh walk; a walk with a 360 km/h spike
and a vague fix; and a year of a handbag tracker, which collapses into
three places (home 149 d, work 86 d, a café 3 d) and today's commute.

Five more for 1386 (`scenes::marks`): **radar**, a walk on the Seceda
ridge in the Dolomites where no region is, ours grey with Anna ahead
and Bert behind; **way-back**, our own walk from the car park; **late**,
a walk whose middle twenty minutes arrived late; **battery**, Kai at
12 % beside Lia at 64 %; and **fit-all**, two walking in town and Ugo
five days old and 12 km away, whom the fit leaves out.

## Not yet

- **The meeting point** (1386, item 5): the manual position sent to a
  group or a peer drawn as a flag, and every pin's distance to it while
  one is set in the conversation shown. Not built in this pass.

- **The privacy sentence** (rule 9) in the share flow: a line on the
  position sheet of every target.
- **The hollow stale pin** (rule 5): stale pins are dimmed, not hollow.
- **The M9:** the map slice (e) is not built; what the track needs there
  is in [A messenger whose core runs anywhere](handheld-messenger.md),
  "Tracks on the M9".
- **A cache of the derivation** on the phone: the window keeps the last
  derivation while the zoom step, the window, the minute and the fixes
  stay; the phone derives anew each frame.

## See also

- [Loquax: the map](loquax-map.md)
- [Telemetry](telemetry.md)
- [A messenger whose core runs anywhere](handheld-messenger.md)
