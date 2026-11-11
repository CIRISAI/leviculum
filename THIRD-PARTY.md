# Third-party code in this repository

Leviculum is AGPL-3.0-or-later (see [LICENSE](LICENSE)). This file names
every piece of someone else's code the repository carries, under which
licence, and whether any of it reaches a binary we build or ship.

The Rust crates the binaries link from crates.io are listed, with their
licence texts, in [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES), which is
generated from `Cargo.lock` and travels inside every artifact. This file
covers what that one does not: the git submodules, the crates vendored
into `vendor/`, and the one binary blob.

`scripts/check-third-party.py` (in `just guards`) refuses a tree with a
`.gitmodules` entry or a `[patch.crates-io]` path that is not named below.

## Submodules under `reference/`: read, never built, never shipped

`reference/` holds the Python and C++ stacks we interoperate with, pinned
so that `file:line` citations into them mean one thing. Nothing under it
is compiled into any Leviculum binary or copied into any package:

- `scripts/check-plain-clone.sh` (in `just guards`) refuses a compile-time
  `include_str!`/`include_bytes!` into `reference/`, so every crate builds
  from a clone without submodules.
- The forge pipelines clone without submodules (`scripts/ci-gate.sh`), and
  the published source tarball is `git archive`, which carries a submodule
  as a gitlink and none of its content.
- The packaging scripts (`scripts/build-deb.sh`, `packaging/`, the lnflash
  bundle) read no path under `reference/`.

What reads them is the test suite at runtime (the Python interop tests run
`python3` against `reference/Reticulum` and `reference/LXMF`) and the
citation checker (`leviculum-std/tests/doc_citations.rs`). A plain clone
initialises none of them; README asks for `reference/Reticulum` only, for
the interop tests.

| Path | Upstream | Pin | Licence | Role |
|---|---|---|---|---|
| `reference/Reticulum` | [reallew/Reticulum](https://github.com/reallew/Reticulum), branch `libreticulum-patches`, a fork of markqvist/Reticulum | `d5e62d4e` (1.3.5 plus one test-harness patch) | Reticulum License (Mark Qvist) | Reference and interop peer at test time |
| `reference/LXMF` | [markqvist/LXMF](https://github.com/markqvist/LXMF) | `795fdaa2` (1.1.0) | Reticulum License (Mark Qvist) | Reference and interop peer at test time |
| `reference/LXST` | [markqvist/LXST](https://github.com/markqvist/LXST) | `1194c901` (0.4.4-6) | CC BY-NC-ND 4.0 | Citation target only; nothing runs it |
| `reference/RNode_Firmware` | [markqvist/RNode_Firmware](https://github.com/markqvist/RNode_Firmware) | `d39339f8` (1.86-1) | GPL-3.0 | Citation target only; never built here |

The pin column is what the gitlink names at the time of writing;
`git submodule status` is authoritative, and `just check-submodules`
refuses a checkout that disagrees with it.

`reference/LXST` is behind the release Sideband 2.2.0 requires
(`lxst>=0.5.4`), and no git ref can bring it level. The upstream repository
is a mirror whose code stops at `_version.py` 0.4.5: its tip and its `0.5.3`
tag differ from the pin only in `README.md`, and no `0.5.4` tag exists.
LXST 0.5.4 was published only as a PyPI wheel
(`lxst-0.5.4-py3-none-any.whl`, sha256
`feadeb244d12378c27c35dbe48a251ff4db03faf3c8f55b769f9d7656d0adeb7`, with prebuilt
`filterlib` binaries and no sdist). The telephony study cites it as
`lxst-0.5.4:` paths into that download rather than into this tree. Its
licence is CC BY-NC-ND 4.0, the same as the pin's: we read LXST for the
wire protocol and never port its code into Leviculum.

## Crates vendored under `vendor/`: linked into binaries

Each is a published crate with a small local patch, substituted for the
crates.io release through `[patch.crates-io]`. Their licences permit
modification and redistribution; each directory keeps the upstream
licence file, and each crate appears in THIRD-PARTY-NOTICES like any
other dependency.

| Path | Upstream | Version | Licence | Patch | Linked into |
|---|---|---|---|---|---|
| `vendor/crossterm` | [crossterm-rs/crossterm](https://github.com/crossterm-rs/crossterm) | 0.29.0 | MIT | Mouse side buttons 8/9 surface as `MouseButton::Back` / `Forward` instead of being dropped in `parse_cb` | lnomad (host) |
| `vendor/termbg` | [dalance/termbg](https://github.com/dalance/termbg) | 0.6.2 | MIT OR Apache-2.0 | One line: the crossterm requirement raised from 0.28 to 0.29, so lnomad links one crossterm | lnomad (host) |
| `vendor/iced_debug` | [iced-rs/iced](https://github.com/iced-rs/iced) | 0.14.0 | MIT | `set_sink`: the finished view, layout, interact, draw and present spans handed to a function the window sets, for its frame meter (1441) | loquax (host, `gui`) |
| `vendor/iced_tiny_skia` | [iced-rs/iced](https://github.com/iced-rs/iced) | 0.14.1 | MIT | `whole_when_fragmented`: a frame's damage of more than four rectangles or an eighth of the window is drawn as the whole window, where iced redraws every layer once per rectangle through a clip mask (1456) | loquax, lnomad (host, `gui`) |
| `vendor/nmea0183` | [nsforth/nmea0183](https://github.com/nsforth/nmea0183) | 0.6.0 | BSD-3-Clause | The float parse replaced by a fixed-point one (Codeberg #386) | nRF firmware |

The patches are described where they are wired in: the workspace
`Cargo.toml` (crossterm, termbg, iced_debug, iced_tiny_skia) and `leviculum-nrf/Cargo.toml`
(nmea0183). The vendored `Cargo.toml` files keep upstream's name, version
and repository URL; this table is where the difference is recorded.

## Binary blob

| Path | Origin | Licence | Role |
|---|---|---|---|
| `lnflash/payload/t114/s140_nrf52_7.3.0_softdevice.hex` | Nordic Semiconductor S140 SoftDevice 7.3.0 | Nordic licence, `s140_nrf52_7.3.0_license-agreement.txt` beside it | Shipped in the lnflash bundle, with its licence, for the board that needs it |

## Data

| Path | Origin | Licence | Role |
|---|---|---|---|
| `leviculum-mapcut/fixtures/oltrarno.pmtiles` | OpenStreetMap, © OpenStreetMap contributors, via the Protomaps Florence sample archive (six tiles, z14 and z15, as the sample holds them) | ODbL 1.0, attribution in `leviculum-mapcut/fixtures/README.md` | The map source the autoload's tests serve on a loopback port (loquax, leviculum-mapcut); never shipped |
| `leviculum-map/fixtures/firenze.lmr` | OpenStreetMap, © OpenStreetMap contributors, via the Protomaps Florence sample archive (six tiles, z11 to z15) | ODbL 1.0, attribution in `leviculum-map/fixtures/README.md` and inside the file | Test fixture of the map core's region reader and draw |
| `leviculum-map/data/ne_50m_land.lwo` | Natural Earth 5.1.2, 1:50m physical vectors, `ne_50m_land` (1,422 rings, 59,239 points, 159,357 bytes), baked by `scripts/bake-world-outline.py` | Public domain (Natural Earth terms of use, naturalearthdata.com/about/terms-of-use); source and checksum in `leviculum-map/fixtures/README.md` | The world outline `leviculum-map` draws where no region covers the view; built into every binary that draws a map (loquax with `tui`) |
| `leviculum-map/data/ne_50m_admin_0_boundary_lines_land.lwl` | Natural Earth 5.1.2, 1:50m cultural vectors, `ne_50m_admin_0_boundary_lines_land` (393 lines, 19,760 points, 49,512 bytes), baked by `scripts/bake-world-outline.py` | Public domain (Natural Earth terms of use, naturalearthdata.com/about/terms-of-use); source and checksum in `leviculum-map/fixtures/README.md` | The country borders `leviculum-map` draws over the world outline; built into every binary that draws a map (loquax with `tui`) |
| `leviculum-mapfill/fixtures/firenze-z15.lmf` | OpenStreetMap, © OpenStreetMap contributors, via the Protomaps Florence sample archive (four z15 tiles) | ODbL 1.0, attribution in `leviculum-mapfill/fixtures/README.md` | Test fixture of the map filler, and the geometry the M9 measurement binary (`leviculum-mapfill/m9`, never shipped) carries in flash |
| `loquax/fonts/FiraSans-Regular.ttf` | Fira Sans Regular 4.203, the Mozilla Foundation and Telefonica S.A., copied byte for byte (sha256 `3dabf3d4...97cd97`) from the crate `iced_graphics` 0.14, which embeds the same file | SIL Open Font License 1.1, its text with the font's copyright line in `loquax/fonts/OFL.txt` | The face `loquax::mapnames` sets the map's names and the pins' labels in (1128), so the window and the Android app draw one picture without iced on the phone; shipped in the Android APK, which carries `OFL.txt` beside it (`assets/FiraSans-OFL.txt`); since 1565 also embedded in the ThinkNode M9 image, which sets the map's names in it through `loquax-m9/src/glyphs.rs`, and a package that ships that image must carry the OFL text |
| `lnomad/fonts/FiraMono-Medium.ttf` | Fira Mono Medium, the Mozilla Foundation and Telefonica S.A., copied byte for byte (sha256 `5f9173ce...7f848292`) from the crate `cosmic-text` 0.15, which carries the same file | SIL Open Font License 1.1, its text with the font's copyright line in `lnomad/fonts/OFL.txt` (the crate's `FiraMono-LICENSE`) | The face lnomad's window (`gui`, #475) sets a page's cells in, so the window and its goldens agree on every host; in lnomad built with `gui` only, which no package ships today. A package that ships it must carry the OFL text |
| `lnomad/fonts/FiraMono-Bold.ttf` | Fira Mono Bold 3.206, the Mozilla Foundation and Telefonica S.A., copied byte for byte (sha256 `61f0ca3a...95cab305`) from the Google Fonts repository (`ofl/firamono/FiraMono-Bold.ttf`), the same version and copyright as the Medium above | SIL Open Font License 1.1, the same text as the Medium's in `lnomad/fonts/OFL.txt` | The face lnomad's window sets a page's bold runs in, on the Medium's grid (the same 600 unit advance); in lnomad built with `gui` only, which no package ships today. A package that ships it must carry the OFL text |
| `loquax/emoji/emoji.tsv` | Generated by `scripts/gen-emoji-table.py` from Unicode's `emoji-test.txt` 15.1 (the emoji, their order, groups and English names) and CLDR 44's German annotations (`common/annotations/de.xml`, `common/annotationsDerived/de.xml`); sources and checksums in the script | Unicode Licence V3, its text in `loquax/emoji/LICENSE-UNICODE.txt` | The emoji picker's table (1137): 1,898 emoji in nine categories with their skin tone variants, 95,227 bytes; shipped in the Android APK, which carries the licence beside it (`assets/emoji-LICENSE-UNICODE.txt`) |
| `loquax-android/tests/golden/map-florence-z16.png` | A picture drawn from `leviculum-map/fixtures/firenze.lmr`: OpenStreetMap, © OpenStreetMap contributors | ODbL 1.0 (a produced work), attribution in `loquax-android/tests/golden/README.md` | The golden of the Android map screen's frame (1128); never shipped |
| `loquax/tests/golden/gui-names-florence-z16.png` | A picture drawn from `leviculum-map/fixtures/firenze.lmr`: OpenStreetMap, © OpenStreetMap contributors | ODbL 1.0 (a produced work), attribution in `loquax/tests/golden/README.md` | The golden of the window's map names (1100); never shipped |

## A font embedded through a crate feature

| What | Origin | Licence | Role |
|---|---|---|---|
| Fira Sans Regular | `FiraSans-Regular.ttf` inside the crate `iced_graphics` 0.14, embedded by iced's `fira-sans` feature, which loquax's `gui` feature turns on (1100) | SIL Open Font License 1.1 (Mozilla Foundation, Carrois Corporate); the crate itself is MIT, so THIRD-PARTY-NOTICES names the crate's licence and not the font's | The face the window sets its own text in, the same on every host; in loquax built with `gui` only, which no package ships today. A package that ships it must carry the OFL text. The map's names are set from loquax's own copy of the same file (above, 1128) |
