# The render walk

`just render-walk` renders the map and the screens over many views and
checks each picture against named rules. It covers anything that changes
how the app looks: the map, the UI, its text. A golden pins one front view
per test. The walk exists because one view per test is not enough. On
2026-11-13 the globe build of 6bb85004c passed every golden. On a phone the
same build drew a Bitmap half again the size of the frame, let the world
shrink below the screen, and grew the native heap to 630 MB.

```sh
just render-walk --quick              # per batch: 70 to 95 s warm
just render-walk                      # everything: about 260 s warm
just render-walk --selftest           # the positive controls: 100 to 140 s
just render-walk --out DIR --m9       # elsewhere, with the M9 sim's walks
```

The output lands in `target/render-walk/` unless `--out` names another
directory:

- `index.html` is the contact sheet. It shows every picture with the rules
  it failed, plus a table of each gesture's frame times and heap. A failed
  picture has a red border and links to the whole screen. The others are
  thumbnails.
- `summary.tsv` has one row per rule per frame, gesture or screen. Its
  columns are `variant kind sequence size centre zoom image rule value
  limit verdict note`.
- The exit status is 1 when any rule failed and 2 when the walk itself
  could not run.

The measured times are from schneckenschreck on 2026-11-13, with a warm
build: quick 70 s and 94 s, full 259 s, selftest 103 s and 139 s. A cold release build of the
walk adds about two minutes for each projection.

## What it draws

The walk draws the same code the frontends draw with. There is no second
renderer.

- **The phone's map** is drawn through `loquax_android::map::render_into`,
  the facade call the app makes. It is drawn into a Bitmap that the walk
  sizes the way `MapView.java` sizes it: the screen plus the margins of
  `MapFrame.margin`, a quarter of each side, and no margin while the last
  frame turned the globe. A test reads `MapFrame.java` so that the two
  sides agree. The walk also keeps what the screen shows the way Java keeps
  it. When a frame fails, Java shows the last good frame scaled to the zoom
  the fingers asked for. That is how the failed globe frames of 2026-11-13
  became Lew's "small rectangle".
  The driver is `loquax-android/examples/render_walk.rs`. It is built
  twice: once for Equal Earth and once with `loquax/globe-world` for the
  globe.
- **The zoom ladder** covers every integer zoom from the world at its
  smallest to the deepest zoom the regions hold.
  - Centres: Germany (51 N 10 E), Morocco (31 N 5 W), both poles (85°),
    the antimeridian (17 S 179.99 E), and Berlin and Florence, where the
    fixture regions give street level from data.
  - Frame sizes: 1224x2197 at a density of 2.75 (Lew's phone), 1080x2400
    at 2.625, 1600x2560 at 2.0, 900x560 at 1.0 (the window), and
    2197x1224 (Lew's phone in landscape).
  - Each zoom is asked at rest and settled on a held clock until its fades
    and tiles are done, which is what a person sees once the screen is
    still.
  - `--quick` keeps the phone and the window, Germany, the north pole and
    Berlin, and the zooms 4 to 16 in steps of two plus the floor.
- **Five gestures** run on each frame size. `--quick` runs them on the
  phone only.
  - `pan-seam`: 40 px a frame across three tile seams at z13.
  - `pinch-out-in`: a quarter step a frame from z12 out to the world and
    back.
  - `drag-pole-around`: 30 frames toward the north pole, then 30 frames
    around. This turns the globe and pans Equal Earth.
  - `jump-street-to-z5`: from Berlin z16 to Europe z5, the way a tap on a
    position asks for it.
  - `rotate`: portrait to landscape and back, at rest.

  Each gesture records its frame times on the host and the live heap at
  its start, peak and end. A counting allocator in the walk measures the
  heap.
- **The window's screens** are rendered by iced's software renderer:
  - the chats, light and dark
  - the settings
  - the first start
  - the map with two pins

  The test is `every_window_screen_draws_at_every_walk_size` in
  `loquax/src/gui.rs`. The window has no density of its own, so the
  phones' sizes are taken at their logical size.
- **The terminal's screens** are the empty list, a chat with a late
  message, and the settings. They are drawn at the cells each size holds,
  at 8x16 logical pixels a cell. The test is
  `every_terminal_screen_draws_at_every_walk_size` in `loquax/src/tui.rs`.
- **With `--m9`**, the walk also runs `just m9-sim-walk`, the M9 host
  sim's walks, and puts their dumps on the sheet.

The existing goldens stay with their tests. `cargo test` and
`just loquax-gui` pin each golden pixel for pixel. The walk adds every
other view.

## The rules

Each rule has a name, one threshold and a measured value. The definitions
live in `loquax-android/src/render_walk.rs`.

| Rule | Applies to | Fails when |
|---|---|---|
| `bitmap_is_frame` | every map frame | the frame does not fit the Bitmap Java made |
| `view_covered` | every map frame | more than 0.5 % of the screen is undrawn (magenta on the sheet) |
| `world_spans_view` | a turning globe | more than 2 % of the middle line across the shorter side is space |
| `no_error_text` | every map frame | the frame says an error or a notice |
| `no_map_chip_over_data` | every map frame | the "no map here" chip is up at a zoom where the world's outline is drawn (z10 or shallower) |
| `label_inside_view` | a settled frame | a shown name straddles the screen's edge |
| `label_clear_of_controls` | a settled frame | a shown name lies under the map's two round buttons (56 dp, 16 dp from the end, 16 dp and 84 dp from the bottom) |
| `label_overlap` | a settled frame | two shown names overlap |
| `label_current_zoom` | a settled frame | a name is from a deeper zoom than the frame's, or is still fading out |
| `label_on_world` | a turning globe | a name floats in space: three of its box's corners and middle are space |
| `frame_p95` | a gesture | the 95th percentile frame time is over half a frame at 60 Hz (8.3 ms) |
| `heap_returns` | a gesture | the live heap ends more than 160 MB above its start |
| `heap_peak` | a gesture | the live heap grows more than 320 MB during the gesture |
| `view_kept` | the rotation | the centre or the zoom moved |
| `screen_drawn` | a window or terminal screen | it drew nothing. For the window, that is at most two of four quarters with more than one colour. For the terminal, it is text on less than a tenth of its rows |
| `screen_repeats` | a window or terminal screen | the same screen drawn twice differs |

A moving frame is judged by the frame rules and by `label_on_world` only.
While a finger moves, the names of the last rest frame ride along by
design, so the other label rules wait for a frame at rest.

`RENDER_WALK_NAMES=<image>` makes the driver print the names of that
picture with their boxes, which is useful for checking a label verdict
by eye.

## Positive controls

`just render-walk --selftest` runs a small walk over Berlin on the phone
size. It runs once clean and once for each injected fault. Each fault must
turn its rules red on a frame or gesture where the clean walk is green.

| Fault | Rules it turns red |
|---|---|
| `oversized-bitmap` (the 1796 Bitmap) | `bitmap_is_frame`, `view_covered` |
| `blank-half` | `view_covered` |
| `stray-label` | `label_inside_view`, `label_clear_of_controls`, `label_overlap`, `label_current_zoom` |
| `error-text` | `no_error_text` |
| `slow-frame` | `frame_p95` |
| `heap-kept` | `heap_returns`, `heap_peak` |
| `view-lost` | `view_kept` |

On the host the clean walk fails `frame_p95` on nearly every gesture. The
rotation sometimes passes, but its frames are at rest and the fault does
not slow them. So for `frame_p95` the control asks for a step instead of a
turn: the p95 must grow by at least 15 ms when 20 ms are added to every
moving frame.

Three rules have no injection in the selftest: `world_spans_view`,
`no_map_chip_over_data` and `label_on_world`. Their unit tests in
`render_walk.rs` inject the fault. `loquax-android/tests/render_walk.rs`
runs the Bitmap rules on a real Florence frame.

## What it does not see

- **Java's own state.** The rotation checks the library's half: the view
  Java hands back is kept. It does not check whether `MapView` restores
  its view after an activity is recreated, which is walk finding 10 of
  2026-11-13.
- **Equal Earth's edge.** Outside the Equal Earth shape the frame is
  ocean, the same colour as the sea. `world_spans_view` therefore judges
  the globe only.
- **The phone's own frame times and heap.** The walk measures on the
  host. The rate the rule holds to is 60 Hz, and the walk cannot see the
  display's real rate.
- **Real regions.** The walk draws the fixtures under
  `leviculum-map/fixtures` with the autoload off. A heap peak that only a
  phone with many downloaded regions reaches is out of its view.
