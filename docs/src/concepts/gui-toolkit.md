# A GUI toolkit for the desktop and the M9

The messenger, and after it the browser and the map, get graphical
frontends: on the Linux desktop first, Windows and macOS later, and on the
ThinkNode M9's display and keyboard ([A messenger whose core runs
anywhere](handheld-messenger.md)). This page asks which toolkit draws
them and records what two time-boxed spikes measured on 2026-10-25.
**Decided 2026-10-25 (Lew): option B.** iced on the desktop, ratatui
through mousefood on the M9, the keymap and glyph tables in the core
(`leviculum-ui`). See "Decision" below.

It is stage two of the M9. Stage one has no GUI: the M9 behaves like the
other boards, with the status screen and [one button](one-button.md) on
one key ([The ThinkNode M9](thinknode-m9.md)). Tracked under #232 (the
`no_std` messenger core), #219 (`lnomad`'s core/frontend boundary), #473
(the M9), #474 (the map) and #475.

## The question

Which toolkit draws the messenger on

1. the desktop (Linux now, Windows and macOS later), and
2. the M9: a 2.4-inch 240x320 ST7789 without touch, and a 37-key
   keyboard behind its own I2C controller ([The ThinkNode
   M9](thinknode-m9.md), "The device" and "Keyboard"),

over one core (model, `update`, effects, planned as `no_std + alloc` in
#232), next to the existing ratatui terminal frontend, with one
interaction model across all three?

## Constraints and where they come from

| constraint | source |
|---|---|
| the core knows no frontend; a GUI is a frontend swap | [Loquax: architecture](loquax-architecture.md), "Requirement: the core must not know it has a terminal" |
| one interaction model: same commands, same palette names, same delivery glyphs | [A messenger whose core runs anywhere](handheld-messenger.md), "One interaction model"; [Loquax: the user interface](loquax-ui.md), sections 1 and 2 |
| keyboard first: the palette, no modes, `Enter` sends | [Loquax: the user interface](loquax-ui.md), section 1 |
| the delivery glyphs (since 1232 the tick ladder `◷ ✓ ✓✓ ✔✔ !`) and the braille spinner, with an ASCII fallback | [Loquax: the user interface](loquax-ui.md), section 2 |
| one palette: the GUI takes its colours from the token table of `leviculum-ui/src/tokens.rs`, the truecolor values of each theme, as both terminal programs do | [keys, mouse and layout](loquax-keys-and-layout.md), section 5, "Colour" |
| licence compatible with `AGPL-3.0-or-later` | `Cargo.toml:89`, [Licensing and third-party notices](licensing-and-notices.md) |
| German and Polish text, in the UI and in received messages | Lew, 2026-10-25 |
| Rust only, no webview | Lew, 2026-10-25 |
| the M9 builds on the esp Xtensa toolchain | [The ThinkNode M9](thinknode-m9.md) |
| the M9 has 512 KB SRAM, 8 MB PSRAM, 16 MB flash | [The ThinkNode M9](thinknode-m9.md), "The device" |

## The candidates

Versions and dates are from crates.io, read on 2026-10-25. "Survey" marks
a statement taken from the market survey of 2026-10-25 and not
re-verified for this page.

| toolkit | licence | release | renderer | text | keyboard model | accessibility | MCU path |
|---|---|---|---|---|---|---|---|
| Slint | GPL-3.0-only OR Slint Royalty-free 2.0 OR Slint Software 3.0 | 1.18.1, 2026-09-21 | software, femtovg, skia, vello | embedded glyphs on MCU; system fonts on the desktop | `FocusScope`, `key-pressed` in the DSL | `accessibility` feature (AccessKit); platform coverage not checked | `renderer-software` with `render_by_line`; ESP32-S3-Box-3 the only official board (survey) |
| iced | MIT | 0.14.0, 2025-12-07 | wgpu or tiny-skia | cosmic-text, `advanced-shaping` feature | `operation::focus(id)`, `focus_next` as tasks in `update`; `keyboard::listen` subscription | none: no AccessKit dependency in iced_winit 0.14.1 | none |
| egui | MIT OR Apache-2.0 | 0.37.0, 2026-10-15 | glow or wgpu | own | immediate mode: `request_focus` per frame, built-in Tab order | AccessKit on Windows and macOS only (egui README) | none |
| xilem | Apache-2.0 | 0.4.0, 2025-10-29 | Vello | Parley | experimental (survey) | AccessKit (survey) | none |
| gpui | Apache-2.0 | 0.2.2, 2025-10-22 | own GPU | own | Zed's actions and key contexts (survey) | not assessed | none |
| floem | MIT | 0.2.0, 2024-11-14 | own | cosmic-text | not assessed | not assessed | none |
| dioxus-native (Blitz) | MIT OR Apache-2.0, plus MPL-2.0 stylo | 0.7.10 stable, 0.8.0-alpha.3 on 2026-10-16 | HTML/CSS engine without webview | Parley | browser-like (survey) | not assessed | none |
| mousefood | MIT OR Apache-2.0 | 0.5.2, 2026-06-22 | ratatui onto an embedded-graphics `DrawTarget` | monospace bitmap atlas (`embedded-graphics-unicodefonts`, 6x10 by default) | whatever the ratatui frontend does; input not included | none | yes: `no_std`, tested on ESP32-S3 (survey) |
| embedded-graphics, embedded-menu | MIT OR Apache-2.0; MIT | 0.8.2, 2026-02-15; 0.7.0, 2026-08-07 | primitives only | bitmap fonts | none, no widget toolkit | none | yes |
| lvgl-rs | MIT | 0.6.2, 2023-04-02 | LVGL (C) | LVGL | pointer input only (survey) | none | yes, stale |
| kolibri-embedded-gui | MIT OR Apache-2.0 | 0.1.0, 2025-02-25 | embedded-graphics | bitmap | touch only (survey) | none | yes, alpha |

Text stacks for a shared layout engine: cosmic-text 0.19.0 has a
`no_std` feature; parley 0.12.0 has `std` and `libm` features, which
suggests a `no_std` build, not tried. The shared-core pattern: crux_core
is at 0.20.0 (2026-08-07), not 0.19 as the survey had it. Not assessed at
all: makepad, cushy, vizia. Excluded for C dependencies: relm4/gtk-rs,
fltk-rs.

## The spikes

Scratch crates under `/home/lew/scratch/gui-965`, outside the
workspace, not committed. The desktop builds used stable 1.95.0 with
`RUSTC_WRAPPER` unset (sccache had first produced a cold build of 26 s
from its cache) on a 10-core host. "release" is the default release
profile with `strip = true`; "small" adds `opt-level = "s"`, fat LTO,
one codegen unit and `panic = "abort"`. RSS was read from
`/proc/<pid>/status` under Xvfb with openbox, the window at 640x400.

### The screen

Both desktop spikes draw the same screen: a title line with German and
Polish text (`Grüße aus Köln · Łódź: łąka, ściana, żółw`), a message
list with a coloured delivery glyph per row, an input line where `Enter`
sends, and a palette on `Ctrl+P` that filters six command names taken
from [Loquax: the user interface](loquax-ui.md) (send, sync, announce, new
conversation, message details, quit) and runs the first match on
`Enter`; `Escape` closes it. A script drove each window by keyboard only
with xdotool: type `Grüße łąka żółw`, `Enter`, `Ctrl+P`, `syn`,
`Enter`, then `Ctrl+P`, `quit`, `Enter`.

### Desktop: Slint and iced

iced was chosen over egui for the second spike because its focus is an
explicit operation returned from `update` (`operation::focus(id)`), in
the same message loop as everything else, which is the shape of our core
(`AppEvent` in, `Effect` out). egui's focus is requested anew in every
frame of an immediate-mode pass, and its Tab order is implicit in the
drawing order.

| | Slint 1.18.1 | iced 0.14.0 |
|---|---|---|
| features | `backend-winit`, `renderer-winit-software` | `tiny-skia`, `x11`, `wayland`, `advanced-shaping`, `thread-pool` |
| crates in the normal dependency graph | 382 | 146 |
| cold build, release | 43.9 s | 20.9 s |
| warm rebuild after touching `main.rs` | 3.5 s | 1.8 s |
| warm rebuild after touching the `.slint` file | 3.7 s | n/a |
| binary, release, stripped | 18.5 MB | 8.0 MB |
| binary, small, stripped | 11.5 MB | 4.9 MB |
| cold build, small | 65.0 s | 36.3 s |
| RSS idle | 18.9 MB | 13.7 MB |
| RSS after the input script | 19.7 MB | 13.7 MB |
| dynamic libraries | fontconfig, freetype and their dependencies | libc, libm, libgcc_s only |
| German and Polish glyphs | rendered | rendered |
| typed Polish input, palette, quit by palette | all worked | all worked |
| lines of spike code | 65 `.slint` plus 33 Rust | 111 Rust |

Licences that would ship (count of crates per licence expression):

- **Slint**: 12 crates `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0
  OR LicenseRef-Slint-Software-3.0`, the rest MIT or Apache-2.0 variants,
  27 Unicode-3.0, some BSD, Zlib, ISC, CC0. The 12 are the question in
  "The licence question" below.
- **iced**: MIT or Apache-2.0 variants, BSD, Zlib, ISC, one
  `Apache-2.0 OR GPL-2.0-only` (taken as Apache-2.0), one
  `(MIT OR Apache-2.0) AND Unicode-3.0`. Nothing our `about.toml`
  `accepted` list (`about.toml:44`) lacks.

### The M9 path, at the desk

Both built for `xtensa-esp32s3-none-elf` with the esp toolchain (rustc
1.97.0-nightly, 2026-07-08, `source /home/lew/export-esp.sh`), esp-hal
1.2.1, esp-alloc 0.11.0, `opt-level = "s"`, fat LTO. No board was
flashed. The display is a fake: Slint renders by line into a 320-pixel
Rgb565 line buffer through a fake `Platform`, mousefood draws into a
fake 320x240 Rgb565 `DrawTarget`. Both only checksum the pixels.

**Slint** (`renderer-software`, `unsafe-single-threaded`, `libm`,
`compat-1-2`, `EmbedForSoftwareRenderer`), the desktop's `.slint` file
unchanged:

- The Slint runtime compiles for the target.
- The generated code for the spike's screen does **not** compile at any
  optimisation level above 0. LLVM's Xtensa backend aborts with
  `Cannot select: ... XtensaISD::PCREL_WRAPPER TargetConstantPool ...
  internal constant [2 x float] [float 0.000000e+00, float
  1.000000e+00]` in `InnerApp::init_chunk_0`. Tried: `opt-level` `s`,
  `z`, 1, 2, 3, with and without LTO.
- Bisected: a screen with only a `Text` compiles at `s` with LTO. Adding
  either `LineEdit` or `ListView` from `std-widgets.slint` brings the
  error back, so it is the standard widgets' generated code, not ours.
- It compiles when the generated crate alone is built at `opt-level = 0`
  and Slint's own crates at `s`, without LTO. That is a workaround, not
  a fix. Whether a newer esp toolchain fixes the backend is open.

**mousefood 0.5.2 with ratatui 0.30.2** (`default-features = false` on
ratatui, `fonts` on mousefood), the same screen as a ratatui view:
compiled on the first attempt at `s` with LTO.

| ELF section | esp-hal alone | Slint, Text only, `s` + LTO | Slint, the screen, generated crate at 0 | mousefood, the screen, `s` + LTO |
|---|---|---|---|---|
| `.text` | 11,517 | 221,697 | 978,201 | 105,829 |
| `.rodata` | 1,204 | 74,568 | 154,740 | 49,836 |
| `.data` | 1,356 | 1,732 | 5,224 | 1,624 |
| `.rwtext` | 5,264 | 5,268 | 13,548 | 5,268 |
| `.bss` without the heap | 92 | 112 | 176 | 160 |
| heap reserved by the spike | none | 128 KiB | 128 KiB | 200 KiB |

Bytes from `xtensa-esp32s3-elf-size -A`. "esp-hal alone" is the
toolchain smoke crate of 2026-09-15 under `/home/lew/scratch`. The
Slint screen's `.text` is inflated by the unoptimised generated crate
and is an upper bound.

Heap, measured on the host with a counting allocator around the same
screen code (64-bit pointers, so an upper bound for the 32-bit target):

| | after construction | peak over 10 frames |
|---|---|---|
| Slint, `MinimalSoftwareWindow`, line rendering | 34.6 KB | 55.0 KB |
| mousefood with its `framebuffer` feature (on by default) | 262.8 KB | 285.2 KB |
| mousefood without `framebuffer` | 109.2 KB | 131.8 KB |

mousefood's `framebuffer` holds a full 320x240 Rgb565 frame (153.6 KB)
on the heap. The 200 KiB the spike reserves would likely not hold it on
the target either. The M9's 8 MB PSRAM makes that a placement question,
not a capacity question.

**What lands on a 320x240 panel.** Both host probes wrote their pixels
to an image:

- mousefood's default 6x10 atlas gives 53x24 cells. German and Polish
  render; `✓` and `◇` render as blanks. The delivery glyphs need a
  different font or the ASCII fallback set
  ([Loquax: the user interface](loquax-ui.md), section 2).
- Slint's embedded glyphs render anti-aliased, but only for the
  characters the compiler saw at build time: printable ASCII, `●`, `…`
  and the characters in the `.slint` file's string literals
  (i-slint-compiler 1.18.1, `passes/embed_glyphs.rs`, the
  `characters_seen` set). `✓` occurs in the `.slint` file and rendered;
  `◇`, `⌂` and the `ć` of a runtime message did not. A received message
  can contain any character, so the M9 build has to declare its whole
  character set at build time, and what that costs in flash is not
  measured.
- The palette, laid out in fixed pixels for 640x400, did not fit
  320x240. A Slint file shared between the two screens needs a layout
  that adapts; that is our work, not the toolkit's limit.

## The two architectures

**A: one toolkit for both, Slint.** One declaration of every screen,
one software renderer, one focus model on the desktop and on the M9.

- Leaves open: the M9 is not a supported board, so the `Platform`, the
  ST7789 driver and the keyboard input are ours to write; the standard
  widgets do not compile with optimisation on today's esp toolchain;
  the character set has to be fixed at build time; the licence question
  below.
- The terminal frontend stays a separate renderer anyway, so "one
  toolkit" is two renderers, not three.

**B: two frontends over one core.** iced on the desktop; ratatui on the
M9 through mousefood, sharing the terminal frontend's view code.

- The M9 and the terminal are the same renderer and the same widget
  model. ratatui 0.30.2 is what `lnomad` already uses
  (`lnomad/Cargo.toml:149`). Only the desktop GUI is a renderer of its
  own.
- One interaction model then lives where [Loquax: the user
  interface](loquax-ui.md) already put it: the one keymap table and the
  one glyph table, read by every frontend. It does not come from a
  shared widget toolkit.
- Leaves open: two widget models (ratatui and iced) to keep in step; a
  monospace font on 320x240 with the delivery glyphs; iced has no
  accessibility today; keyboard input on the M9 is ours in either
  architecture.

## The licence question

*Moot since the decision for B (2026-10-25, Lew), kept as the record.
If Slint ever came back, this would need a human legal read first.*

Our work is `AGPL-3.0-or-later` (`Cargo.toml:89`). Slint offers three
licences ([Slint's LICENSE.md, v1.18.1](https://github.com/slint-ui/slint/blob/v1.18.1/LICENSE.md)):

> 1. Royalty-free License - Permits use in **proprietary** desktop,
>    mobile, and web applications **at no cost**. Use in embedded systems
>    is excluded.
> 2. GNU GPLv3 - Permits use in **open source software** under
>    GPL-compatible terms, **at no cost**, for desktop, mobile, and web
>    applications, as well as for embedded systems.

The Royalty-free licence 2.0, section 3, says:

> The License does not permit the use of the Software within Embedded
> Systems. An **Embedded System** is a computer system designed to
> perform a specific task within a larger mechanical or electrical
> system.

So for the M9 only the GPLv3 option is open, and it is GPL-3.0-**only**.
GPLv3, section 13:

> Notwithstanding any other provision of this License, you have
> permission to link or combine any covered work with a work licensed
> under version 3 of the GNU Affero General Public License into a single
> combined work, and to convey the resulting work. The terms of this
> License will continue to apply to the part which is the covered work,
> but the special requirements of the GNU Affero General Public License,
> section 13, concerning interaction through a network will apply to the
> combination as such.

AGPLv3, section 13, second paragraph (our `LICENSE`):

> Notwithstanding any other provision of this License, you have
> permission to link or combine any covered work with a work licensed
> under version 3 of the GNU General Public License into a single
> combined work, and to convey the resulting work. The terms of this
> License will continue to apply to the part which is the covered work,
> but the work with which it is combined will remain governed by version
> 3 of the GNU General Public License.

The questions for the legal read:

1. Both permissions name **version 3** of the other licence. Does a
   binary that combines our `AGPL-3.0-or-later` code with GPL-3.0-only
   Slint stay distributable if our side is ever taken under a later AGPL,
   or does the combination pin AGPLv3?
2. Is the M9 an "Embedded System" in the Royalty-free licence's sense,
   and is the desktop messenger a "Desktop Application"? If the desktop
   used the Royalty-free option and the M9 the GPLv3 option, the two
   builds would ship under different Slint licences.
3. The notices: `GPL-3.0-only` is not in `about.toml`'s `accepted` list
   (`about.toml:44`), so `just notices-guard` would refuse the graph
   until it is added, and every package would carry the GPLv3 text
   beside our AGPL.

iced and the ratatui path raise none of these: their licences are all
permissive.

## Decision

**Decided 2026-10-25 (Lew): option B, iced on the desktop, ratatui through
mousefood on the M9, over the one core, with the keymap and glyph tables
in the core (`leviculum-ui`) as the one interaction model.** No further
toolkit spike; the egui spike is dropped. Accessibility is not a
requirement. The safeguard: the desktop renderer is a frontend swap, so a
later change of toolkit does not touch the core. The comparison above
stays as the record of why. The decision is also recorded on [A messenger
whose core runs anywhere](handheld-messenger.md), "The core/frontend
cut".
lnflash is the third user of the shared widgets, after `loquax` and
`lnomad` (decided 2026-10-26, Lew, Codeberg #476; [Flashing an
LNode](lnode-flashing.md), "Product goal").

Why, as the proposal of 2026-10-25 put it: the M9 frontend is then the
terminal frontend's view code on a different backend, which is the most
code shared at the least cost: it compiled for the target at full
optimisation on the first try, in 106 KB of `.text`, and the desktop GUI
is a single extra renderer that is less than half Slint's size, build
time and dependency count, under MIT. The risks carried with it: two
widget models where Lew asked for one interaction model, which the shared
tables have to carry; the M9's font, where neither the delivery glyphs
nor anti-aliasing come for free; iced calls itself experimental; and the
M9's keyboard input and display driver are ours to write.

## Not yet measured

- Frame time and SPI throughput on the real panel, for either path.
- Slint's flash cost for a full Latin Extended character set at two
  sizes.
- A newer esp toolchain against the Slint backend error.
- Windows and macOS builds of either desktop spike.

## See also

- [A messenger whose core runs anywhere](handheld-messenger.md), whose
  "The core/frontend cut" records this decision
- [The ThinkNode M9](thinknode-m9.md)
- [Loquax: the user interface](loquax-ui.md)
- [Loquax: architecture](loquax-architecture.md)
- [Licensing and third-party notices](licensing-and-notices.md)
- [One button](one-button.md)
