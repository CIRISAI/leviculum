# Loquax: tracks on the map

Lew, 2026-11-03 (#474): Loquax draws a peer's track on the map. The map
answers three questions about a peer: where is she now, is she moving,
and how did she get there. So the default picture is the **current
movement** as lines, and everything older is **collapsed into places**.
A year of a tracker in a handbag is one large blob at home and a few
small ones, never a scribble. The older lines stay reachable behind a
**time window** control on the map: a map control, not a setting.

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

8. **Quiet peer.** A peer whose fixes came at a median interval *m* over
   the window and who has been silent longer than max(3 *m*, **1 h**)
   gets the line "no fix for 3 h" under the pin; it stays until the next
   fix.

   *Settled by the pictures:* fewer than two fixes in the window have no
   interval, so a position shared once gets no quiet line: it is no
   stream that went quiet, and its pin's age already says how old it is.

9. **Privacy sentence.** The share flow is to say plainly that friends
   who see the position also see the way. No new permission; the
   receiver holds the fixes anyway. Not built in 1385 (see "Not yet").

## The time window

Five named windows and the default: **now** (the current episode),
**today** (since local midnight), **yesterday** (the local day before),
**this week** (since Monday 00:00 local), **this month** (since the 1st,
00:00 local), **all**. A press on the window shown goes back to now.

- **Terminal:** `w` steps to the next window, `W` to the previous, a
  toast names it ("tracks: week").
- **Window:** a strip of the five under the map, beside the credit; the
  one shown is marked.
- **Phone:** a row of five chips under the event chip. Java only draws
  the row and sends the pressed word in the map request
  (`track_window`); the library keeps the window and answers which one it
  drew.

## Where the fixes come from

Every position a peer's message carried, and since schema 14 every
position the mesh announced (`lev.position`, 1302): the store keeps one
row a peer and fix time, `positions.source` 0 for a message's and 1 for
an announced one; a fix a message carried takes the row of the same fix
announced, so the row names its message. Our own receiver's fixes join
our own track while Loquax runs; they are not stored.

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
segments meeting at a fix do not darken it; the quiet line on a light
ground under the pin; the pins last. The window draws its pins on its
own canvas in the same hue.

**The terminal:** the lines as braille dots, two by four a cell, over
whichever technique draws the picture; a dwell `•`, a place `○`, a dot
`·`, the wedge an arrow; in ASCII `.`, `o`, `O`, `,` and `^ / > \ v < `.
A cell holds one track; where two cross the later wins.

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

## Not yet

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
