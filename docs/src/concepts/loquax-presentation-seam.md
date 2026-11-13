# Loquax: the presentation seam — GPU above, one core below

Part of the [Loquax design record](loquax.md). **Status: decided (Lew,
2026-11-06).** Lew's rule, in substance: wherever a device has a GPU it
is used, on Linux and on Android; SIMD where it measurably pays; the
core is the same code everywhere and only the topmost layer is GPU; the
map stays drawn on the CPU — it is smooth even on the phone — and the
GPU moves and zooms the finished tiles; everything must feel properly
smooth, with a clean separation. Same day, 22:0x: the UI (text,
bubbles, lists) *is* GPU-drawn on GPU targets, as the phone's view
system already does; the map is not, yet. This page is that seam,
written before any GPU code (1454); the first slice is 1442. Tracked
under #474 and #232.

## 1. The seam

Per frame the core hands up three things, and nothing else:

- **The scene**: widgets, text runs and marks as the node kinds of
  `loquax/src/ui/scene.rs`, plus the selection 1444 adds — scene items,
  never pixels.
- **The map**: finished RGBA tiles — `TILE` (`loquax/src/tiles.rs:70`),
  256 px a side, `TILE_BYTES` (`loquax/src/tiles.rs:75`), 256 KiB each —
  with the viewport transform (offset and scale, `ExactView`) and the
  ladder's choice of passes (`leviculum-map/src/ladder.rs`).
- **The overlays**: pins, tracks and placed labels as scene items with
  map coordinates, not pixels.

The platform layer composites those with whatever the device has:

- **Linux**: iced 0.14's wgpu renderer, with the software renderer
  (tiny-skia over softbuffer, today's whole path) as the probed
  fallback — `iced_renderer`'s fallback compositor tries wgpu and falls
  back when no adapter comes up; `ICED_BACKEND` forces either, which
  keeps the golden tests on the software path.
- **Android**: the hardware-accelerated Canvas, not an own GL or Vulkan
  surface. The views are already GPU-drawn by the system's renderer,
  and the map frame Rust fills is a bitmap the Canvas draws
  (`loquax-android/src/map.rs:1`); an own surface would be a second
  renderer and a second seam for nothing the Canvas lacks.
- **The terminal and the M9** have no GPU: the same seam, composited
  by the CPU into cells or the panel's flush window.

The invariant, stated as the test 1442 lands in `just guards`: **the
core crate graph contains no GPU crate** — the dependency graphs of
`loquax` as a library without its `gui` feature, of `leviculum-map`,
`leviculum-mapfill` and `leviculum-ui` contain none of `wgpu`, `naga`,
`glow`, `ash`, `khronos-egl`. GPU crates appear only behind the window
feature and in the Android app.

## 2. Pan and zoom on the GPU

A tile is a texture uploaded **once when rendered**, never per frame.
A pan is a transform of the texture set; a zoom a scaled draw of the
held tiles until the ladder's next finished frame replaces them. A
gesture never waits on the rasteriser.

- **Linux (wgpu)**: one `Queue::write_texture` per rendered tile, the
  visible set drawn as textured quads under the viewport transform; a
  phone-shaped view is 50–66 tiles, 13–17 MB, resident once.
- **Android**: `Canvas.drawBitmap(bitmap, matrix, paint)` per held
  tile; the hardware Canvas caches each bitmap as a texture after its
  first draw and replays only the matrix. The phone already shows the
  held frame moved by the finger between worker frames
  (`loquax-android/src/map.rs:14`); the per-tile hold makes that the
  rule and retires the drawn-margins trick
  (`loquax-android/src/map.rs:22`).
- **No GPU** (software window, terminal, M9): the same seam; the
  compositor is the measured CPU compose, 0.17–0.23 ms at 480 x 320
  (`loquax/examples/map_eye_bench.baseline:1`).

No GPU upload is measured yet; the benches give the stand-ins to beat.
Handing a *fresh* map frame into the window costs 1.19 ms over holding
it at 1276 x 2099 (present 2.50 fresh against 1.31 held, since 1468's
word-wise conversion, `loquax/tests/window_bench.baseline:25`) — about
0.11 ms/MB, about 0.03 ms per 256 KiB tile. The CPU compose the transform replaces is
3.96–4.35 ms per phone frame
(`loquax/examples/map_eye_bench.baseline:4`).

**The label layer** is the one GPU-drawn map element: tiles rasterised
*without* names, the placed names set by the GPU at screen resolution,
crisp at every zoom. Today the names are CPU-blitted sprites
(`Sprite::draw_faded`, `loquax/src/mapnames.rs:1383`), up to 17.08 ms
a phone frame (`loquax/examples/map_eye_bench.baseline:5`); on GPU
targets they move into the UI track's glyph drawing, elsewhere the
sprite blit stays. A GPU vector map — continuous zoom, rotation, tilt,
3D buildings — goes on the experiments list, built only when a feature
needs it.

## 3. The target rate

The target is the display's refresh rate, detected at run time and
synchronised to wherever the platform allows; where it cannot be read,
at least 60. `TARGET_FPS` (`leviculum-map/src/ladder.rs:34`), today the
constant 30, becomes that value, handed in by the frontend, and
`BUDGET_US` (`leviculum-map/src/ladder.rs:36`) follows. The sources:

- **Linux window**: winit 0.30's `refresh_rate_millihertz` on the
  monitor; Wayland frame callbacks pace the presents.
- **Android**: `Display.getRefreshRate`, frames paced by the
  Choreographer.
- **M9**: the panel's own rate, measured via the TE pin (pin 19,
  [ThinkNode M9](thinknode-m9.md), "Pins"), never assumed; 60 until
  measured.
- **Terminal**: none — the ladder takes 60.

A frame below the rate is a finding; and when nothing changes, nothing
is drawn — the standing rule since 1455 (commits `3b8f5de1`,
`9f102307`, `4b941f0f`).

## 4. Where SIMD pays

Profiled for this page: `perf record` on the map bench
(`map_eye_bench`, all scenes, the study's Frankfurt region, cycles,
8119 samples, host, 2026-11-06). The three hottest loops:

1. **The name-sprite blit** — `Sprite::of` and `Sprite::draw_faded`
   (`loquax/src/mapnames.rs:1383`), 16.9 % of cycles. Not SIMD first:
   on GPU targets the label layer removes this loop from the CPU frame
   entirely. Where it stays (M9, terminal, software window),
   `std::simd` over a sprite row's u8 alpha blend; expected 2–4x on
   the loop, to be measured.
2. **`memcpy`**, 10.1 %: tile copies in the compose and the frame
   holds. Not a SIMD case — memcpy is already vector code. The step is
   *fewer copies*: the GPU transform deletes the per-frame compose; the
   M9's layout step is composing straight into the flush window's rows.
3. **The coverage fill** — `Filler::fill`
   (`leviculum-mapfill/src/fill.rs:283`), 9.5 % across both
   monomorphisations. Layout first: coverage accumulated in contiguous
   rows, spans blended as straight slice loops the autovectoriser can
   take, instead of per-pixel draw-target calls; `std::simd` on the
   blend only if the A/B still shows the loop after that. Expected 2–3x
   on the loop.

(Fourth: libm `round` in the projection, 4.2 % — a fixed-point layout
change, no SIMD.) The three together are 36.5 % of cycles, so halving
them cuts a frame by roughly 18 %: the gate is the map bench's A/B,
never a promised factor. No SIMD code in this order.

## 5. The slices

Each slice is measured by `just window-bench` and `just map-bench`
against their committed baselines, and by the M9's flush measurement.

- **S1 — the window on wgpu (1442, runs next and follows this note).**
  A moving frame at Lew's 1276 x 2099 window averaged 9.3–13.0 ms,
  2.0–10.7 ms after 1442's CPU fast paths, and averages 0.8–2.4 ms
  since 1468 and 1473 (map drag 1.65, map wheel 1.70,
  `loquax/tests/window_bench.baseline:25`; every wheel, touchpad,
  scrollbar and map gesture's p95 at or under 4.7 ms at 1276 and
  1527 x 2099, see "The software present" below). Target: update +
  draw + present inside the detected rate's budget at 1276 x 2099,
  8.3 ms on a 120 Hz display — met on the software path for those. Not
  for the drag on the messages, which selects text since the window's
  text selection (`91c65908`): 0 to 15 frames a run, at 1276 x 2099
  7.6 ms mean and 12.9 p95 (`loquax/tests/window_bench.baseline:34`),
  open.
- **S2 — phone compositing.** The host proxy for a phone frame: pan
  4.52–11.07 ms, zoom 18.18–22.32 ms
  (`loquax/examples/map_eye_bench.baseline:4`) — a zoom frame misses
  even the 60 Hz budget. Target: every gesture frame a transform of
  held tiles within the `Display.getRefreshRate` budget; the
  rasteriser's frames land beside the gesture, never in it.
- **S4 — the M9's flush.** A full ST7789 frame is 153,600 bytes and
  flushes in 30.8 ms by DMA at 99.8 % of the SPI line rate
  ([ThinkNode M9](thinknode-m9.md), "The board as the UI sees it") —
  two 60 Hz budgets of bus physics, while *drawing* an M9 frame costs
  0.58–1.25 ms (`loquax/examples/map_eye_bench.baseline:7`). Target:
  flush the changed window alone, paced by the TE rate; up to half the
  panel (76,800 bytes, 15.4 ms) inside one 60 Hz budget; a full flush
  only on a scene change.
- **S3 — SIMD in the rasteriser.** Last, because the label layer
  already removes the hottest loop from every GPU target's CPU frame;
  what remains CPU-bound is the M9 and the terminal, known after S4.
  Target: the map bench A/B at least 15 % off `frame_ms` on the phone
  scene (24.93 ms to at or under 21.2 at zoom 15,
  `loquax/examples/map_eye_bench.baseline:5`).

The order is S1, S2, S4, S3: first the two compositors the rule names,
then the no-GPU device's equivalent seam work, SIMD last and only where
the benches still show it.

### The software present (1468, 1473)

The software path is the fallback wgpu leaves, and the path the golden
tests and the terminal-less hosts draw on, so it got the CPU work the
bench pointed at first. 1468 profiled a present under callgrind and
took, in order: the glyph cache (half a chat frame was swash re-hinting
spaces), the blend's rounding, the text clip, the opaque quad as a row
fill, the scroll drawn as a shift of the frame plus the uncovered strip,
and the map frame's conversion. 1473 replaced the last tiny-skia draw a
gesture paid for: every frame of a wheel zoom draws the map's held
picture scaled, through tiny-skia's high-precision pipeline (bilinear or
gather, the clip's mask, load and store per pixel). Its own sampler
(`fast::draw_scaled` in the vendored `iced_tiny_skia`) computes the same
f32 arithmetic in the same order, byte for byte against tiny-skia over
6200 random draws (`leviculum-ui-iced/src/fast_paths.rs`), clipped by the
rectangle so the mask is never written. Map wheel at 1527 x 2099: present
12.5–12.9 ms to 2.3–2.4, p95 16.1–17.8 to 3.4–3.5 (single gestures A/B
at load 1.0–1.7); a full-window draw nearest 13.4 to 1.2 ms, bilinear
29.2 to 17.6 ms.

**Bands stay open, by measurement.** Rasterising the damage in
horizontal bands on several cores (stage 4 of 1442) needs each band
drawn into its own `PixmapMut` with a transform moved by the band's top,
as tiny-skia has no strided sub-pixmap. 1468 measured that this changes
pixels: of 400 random scaled draws drawn whole and as two bands, 30
differed (bilinear by one at scattered pixels, the inverse transform
rounding otherwise; nearest by up to 248 over a whole row), and glyph
rows move by a pixel where a band starts inside a paragraph, since
cosmic-text truncates glyph rows toward zero. Pixel-identical bands
need every draw in absolute coordinates — the own blits (text, the
picture copy, the sampler) are, tiny-skia's quads and paths are not.
With every wheel, touchpad, scrollbar and map gesture's present under
2.5 ms on one core there is no frame left that needs them; they are
revisited when a case misses the budget (the selecting drag above is
the first candidate to measure).

**One core count.** The map's tile painters and the region loader run
as many threads as the process may use, one `available_parallelism`
value read once (`loquax::picture::cores`), without the painters' old
cap of eight and without the loader's halving.

## 6. What stays as it is

- **The map is drawn on the CPU.** One rasteriser serves the M9, the
  terminal, the phone and the window; a GPU vector renderer would be a
  second complete one, which the no-duplication rule forbids. The CPU
  holds: 1.28–1.69 ms a frame at 480 x 320
  (`loquax/examples/map_eye_bench.baseline:1`), and the phone's worst
  cost moves out of the gesture in S2.
- **The ladder is the one quality knob.** Rungs, passes and rest
  recovery are untouched; only its budget constant becomes the detected
  rate.
- **No platform constants.** No device gets a table of its own: what
  differs per platform is detected (the rate, the GPU) or measured (the
  benches), never written down.
