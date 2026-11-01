# lnomad everywhere: the browser on the four targets

Status: plan, 2026-11-01 (order 1228, Lew's goal of 2026-10-31). Docs
only; no product code changed. `lnomad` runs on the four targets Loquax
runs on (the Linux terminal, a Linux window, Android, the ThinkNode M9)
from one code base, with the controls and the operating concepts of the
messenger, so that no piece of work is done twice. The contract it rides
on is Loquax's scene, built in the slice-one commits `bb0e9f77` and
`af8fa5c1` on this branch.

All numbers below were measured on branch `homenode` at `af8fa5c1`.
"Production lines" are the lines before a file's first `#[cfg(test)]` or
`mod tests`; "total" is `wc -l`.

## 1. Inventory

### 1.1 lnomad by file and by role

`lnomad/src` has 24 files, **16,723 lines in total, 9,003 production
lines**; the integration tests under `lnomad/tests` add 1,155. The
"13 k" of the order was below the measurement: `tui.rs` alone holds
5,600 lines of tests (its test module starts at `tui.rs:1784` of 7,309).

| Role | Files (production lines) | Production | Platform free |
|---|---|---:|---|
| Core: model, `update`, effects, chrome geometry | `app/event.rs` 1,963, `app/model.rs` 978, `app/chrome.rs` 537, `app/mod.rs` 32, `app/geometry.rs` 31, `app/input.rs` 5 | 3,546 | yes, no terminal crate; but in character cells |
| Fetch and navigation | `fetch.rs` 620, `url.rs` 191, `browser.rs` 182, `page_cache.rs` 113 | 1,106 | `url`, `page_cache` yes; `fetch` and `browser` need `leviculum-std` and tokio |
| Micron layout | `render.rs` 1,004 (and the parser crate `leviculum-micron`, 1,194) | 1,004 | yes |
| Terminal frontend | `tui.rs` 1,709, `main.rs` 289, `cli.rs` 37 | 2,035 | no (`tui.rs`, `main.rs`) |
| Discovery | `discovery.rs` 329 | 329 | yes, but `SystemTime` (`lnomad/src/discovery.rs:24`) |
| Bookmarks | `bookmarks.rs` 143 | 143 | the list yes, the file IO no |
| Images | `image.rs` 307, `image_cache.rs` 135 | 442 | the arithmetic yes; the decoder is `std` |
| Downloads | `download.rs` 157 | 157 | no (files) |
| Identify | `identify.rs` 104, `identity.rs` 65 | 169 | no (files) |
| Glue | `lib.rs` 64, `color.rs` 4, `theme.rs` 4 | 72 | yes |

**Free of the terminal today: 6,968 of 9,003 production lines (77 %)**,
everything but `tui.rs`, `main.rs` and `cli.rs`. Free of `std` as well,
which is what the M9 needs, is less: `fetch.rs`, `browser.rs`,
`download.rs`, `identify.rs` and `identity.rs` (1,128 lines) do file or
network IO and are the platform layer; the other 5,840 lines need only
`alloc` after the mechanical changes of slice 4a, but for the file halves
of `bookmarks.rs` and the decoder in `image.rs`. The line is held by `just
lnomad-core-no-tui` (`Justfile:628`) and stated in the module doc of
`lnomad/src/app/mod.rs:1`. Two qualifications, both measured:

- **The core speaks cells.** Events arrive as raw keys and cell-addressed
  mouse events (`AppEvent`, `lnomad/src/app/event.rs:28`). The key handlers
  are hand-written `match`es per mode (`update_browse_key`,
  `lnomad/src/app/event.rs:689`; `update_address_key`,
  `lnomad/src/app/event.rs:633`; `update_places_key`,
  `lnomad/src/app/event.rs:1021`), and only the scroll keys come from the
  shared table (`keymap::best` at `lnomad/src/app/event.rs:2116`). The top
  bar is a cell layout (`TopBar`, `lnomad/src/app/chrome.rs:20`), and the
  click and hover tests run against it (`handle_click`,
  `lnomad/src/app/event.rs:2015`). There is no intent and no scene.
- **The core is `std`.** `PathBuf` (`lnomad/src/app/model.rs:5`, for
  `bookmarks_path`), `SystemTime` in discovery, and the parser crate uses
  `std::collections::BTreeMap` (`leviculum-micron/src/model.rs:10`) and
  `std::borrow::Cow` (`leviculum-micron/src/parser.rs:100`), all of which
  `alloc` has. The crate as a whole links `leviculum-std` and tokio for
  `fetch`, so a firmware cannot take it as it is
  ([A messenger whose core runs anywhere](handheld-messenger.md) says the
  same).

The user-visible words are literals: 37 `set_toast(` calls (23 in
`app/event.rs`, 1 in `app/model.rs`, 13 in `tui.rs`), the start page
(`START_PAGE`, `lnomad/src/app/event.rs:362`) and the help groups
(`help_groups`, `lnomad/src/app/chrome.rs:406`). Nothing is translated.

### 1.2 What lnomad shares with Loquax today

The study's lines 30 and 31 (1217), refreshed:

| Crate | Files | Total | Production | Modules lnomad uses | Modules Loquax uses |
|---|---:|---:|---:|---|---|
| `leviculum-ui` | 17 | 4,387 | 3,368 | 14 of 16: all but `commands` and `delivery` | 14 of 16: all but `spinner` and `style` |
| `leviculum-ui-ratatui` | 12 | 1,936 | 1,404 | all 11 | 9 of 11: not `chip`, `spinner` |

Counted as the files under `lnomad/src`, `loquax/src` and
`loquax-android/src` that name a module through `leviculum_ui::` or
`leviculum_ui_ratatui::`. The study's table named lnflash as a user of
`leviculum-ui`; at `af8fa5c1` `lnflash/Cargo.toml` does not depend on it.

What both share is the widget vocabulary: styled text, colour tiers and
tokens, the keymap mechanism with the scroll rows, input and editing, the
footer, help and toast models, and the ratatui widgets over them. **None
of the contract is shared.** The scene, the intents and the string
catalogue live inside the `loquax` crate (`pub mod scene`,
`loquax/src/ui.rs:148`): `scene.rs` 1,231 lines (1,091 production),
`intent.rs` 543 (495), `strings.rs` 474 (350). lnomad depends on
`leviculum-ui`, not on `loquax`, so today it cannot name one node type of
the contract.

## 2. Mapping lnomad onto the contract

### 2.1 Concept by concept

"Exists" means the node or intent is in `loquax/src/ui/scene.rs` or
`loquax/src/ui/intent.rs` now; "shared, moved" means it exists and moves
into `leviculum-ui` (slice 0) so that both programs name it; "browser"
means only the browser has it. Catalogue keys named `browser_*` are new.

| lnomad concept, today | Scene node | Intent | Catalogue | Status |
|---|---|---|---|---|
| Top bar: title, address, star, identity, cache bolt, hops (`topbar_layout`, `lnomad/src/app/chrome.rs:79`) | `TopBar` (`leviculum-ui/src/scene.rs:191`) with `Lead::Back` while history has a past, title, and `actions` (star, identify, reload) | `Back`, `Bookmark{on}`, `Identify{on}`, `Reload` | `browser_bookmark`, `browser_identify`, `browser_reload`, `browser_hops` | shared, moved; the actions browser |
| Address field (the `Address` arm of `Mode`, `lnomad/src/app/model.rs:98`) | a field node: the platform's text field, the scene names its hint and its go, as `Compose` does (`loquax/src/ui/scene.rs:497`) | `Go` (the URL is the platform's text, as Loquax's `Search` is) | `browser_address_hint` | browser node, shared shape |
| Page view (`model.page`, laid out by `layout`, `lnomad/src/render.rs:182`) | `Page`: a window of `PageLine`s, each a list of runs (see 2.2) | `Scroll(ScrollCmd)` (exists) | none: page text is `Text::Raw` | browser |
| Link focus, hint labels, follow (`focus`, `visible_links`, `lnomad/src/app/model.rs:898`) | a run carries its link index; `Page.focus` names the focused link | `FocusNext(dir)`, `Activate(NodeId::Link(n))`, `Hint` | `browser_hint` | `Activate` exists (`Intent`, `loquax/src/ui/intent.rs:158`); the link node browser |
| Form fields (`RenderedField`, `lnomad/src/render.rs:109`) | a run carries its field index; the edited field is a platform text field like the address | `Edit`, `Toggle(field)`, `Submit` | `browser_submit` | browser |
| Back and forward (`go_back`, `lnomad/src/app/event.rs:1718`; `go_forward`, `lnomad/src/app/event.rs:1728`) | `Lead::Back`; forward as a `TopBar` action | `Back` (exists), `Forward` | `back` (exists), `browser_forward` | `Back` exists |
| History (`History`, `lnomad/src/app/model.rs:300`) | a `Sheet` of rows | `Activate(row)` | `browser_history` | sheet exists, rows browser |
| Places: bookmarks and discovered nodes (`Place`, `lnomad/src/app/event.rs:939`; `places`, `lnomad/src/app/event.rs:977`) | `Sheet` (`leviculum-ui/src/scene.rs:274`) of list rows | `Activate(row)`, `Menu(row)` → `Rename`, `Forget` | `browser_places`, `browser_nodes` | sheet exists; `Rename` and `Forget` exist |
| The name search of 1227 (not in this tree: no `1227` in `lnomad/` or `docs/src/` at `af8fa5c1`) | the address field's results: the places `Sheet`, rows from 1227's announce search | `Search` (exists), `Bookmark{on}` for its pinning | `browser_find_name` | lands with 1227 into the shapes above, no new node |
| In-page search (`Mode::Search`, matches as `Match`, `lnomad/src/app/model.rs:170`) | the shared search field; matches as a run flag | `Search`, `Match(next)` (both exist) | `search` | exists |
| Images (`SlotState`, `lnomad/src/image.rs:212`) | `Image` node in the page window: name, state, the decoded picture by reference (pixels never enter the scene, as for the map) | `Activate` opens, `Menu` saves | `browser_image_loading`, `_failed`, `_too_many` | browser |
| Downloads (`Effect::Download`, `Effect`, `lnomad/src/app/model.rs:398`) | `Chip` (`leviculum-ui/src/scene.rs:284`) while one runs, a toast when done | `Download` | `browser_downloaded` (the toast of `lnomad/src/app/event.rs:124`) | chip exists |
| Identify (`IdentifyStore`, `lnomad/src/identify.rs:28`) | the marker as a `TopBar` action whose state is on or off | `Identify{on}` | `browser_identify_on`, `_off` | browser |
| Status line: focused link target, footer (`footer_items`, `lnomad/src/app/chrome.rs:313`) | no node of its own: the footer is the renderer's way to show `Action`s with chords (study, table 3.5); the link target is the focused link's text | none | the actions' labels | exists as a rule |
| Toast (`Toast`, `leviculum-ui/src/toast.rs:23`) | the scene's toast | none | per message | shared already |
| Help overlay | the renderer draws the keymap rows | `Help` (exists) | row labels | shared mechanism |

The new intents are `Go`, `Forward`, `Reload`, `Bookmark`, `Identify`,
`Hint`, `Toggle`, `Submit`, `Download`, about ten; the navigation and
generic ones (`Back`, `Activate`, `Menu`, `Dismiss`, `Scroll`, `Search`,
`Match`, `Help`, `Rename`, `Forget`) exist. The browser's intents are an
enum of lnomad's (`lnomad::Intent`), not arms added to Loquax's `Intent`:
Loquax's carries `Browse(lnomad::Intent)` where it hosts the browser (on
the phone and the M9, section 4).

### 2.2 Micron: one layout and a scene subtree, not a renderer per target

Micron is a character-cell format. NomadNet draws it into a terminal;
tables align on columns and a divider fills the row, so a page has a
width in cells. lnomad already lays it out once, platform free, into rows
of styled cells (`RLine`, `leviculum-ui/src/style.rs:113`; `StyledChar`,
`leviculum-ui/src/style.rs:95`). The choice is whether the scene carries
that layout, or each renderer parses and lays out micron itself.

Measured with `lnomad::layout` over the two pages in the tree (the
Reticulum README is the largest page we have; a throwaway test, not
committed):

| Page | Bytes | Blocks | Spans | Links | Width | Lines | Cells | Runs | Cells × 48 B |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `lnomad/tests/fixtures/sample.mu` | 268 | 16 | 9 | 2 | 53 | 18 | 345 | 12 | 16.6 KB |
| `lnomad/tests_data/reticulum_readme.mu` | 23,490 | 261 | 415 | 57 | 53 | 554 | 19,822 | 719 | 951 KB |
| | | | | | 80 | 431 | 20,158 | 591 | 968 KB |
| | | | | | 120 | 345 | 20,752 | 505 | 996 KB |

A run is a stretch of cells with one style, one link and one field. In a
window of the screen's rows the README never exceeds **67 runs and 1,018
cells at 53×22** (the M9's 53×24 cells less the two chrome rows), 68 runs
at 80×22, 90 runs and 2,871 cells at 120×40; one line holds at most 11.

`StyledChar` is 48 bytes on the host. On the 32-bit targets it is 32 by
its fields (a 4-byte `char`, a 12-byte style, two 8-byte
`Option<usize>`), arithmetic and not measured on the board: 634 KB for
the README. The M9 has 512 KB of SRAM and 8 MB of PSRAM
([The ThinkNode M9](thinknode-m9.md), the hardware table); the ESP32 heap
today is a 64 KiB placeholder (`HEAP_SIZE`, `leviculum-esp/src/lib.rs:92`).
The nRF board's heap of `leviculum-nrf/heap-budget` (96 KiB, `HEAP_SIZE`,
`leviculum-nrf/heap-budget/src/lib.rs:49`) is full but for 8 bytes
([The board heap budget](../firmware/heap-budget.md)) and hosts no UI;
the M9's budget is the ESP32's, not that one.

**Decision: the rendered page is a scene subtree from the one layout.**
`Page { lines: Vec<PageLine>, first: usize, total: usize, focus }`, a
`PageLine` a `Vec<Run { text, style: RStyle, link, field, matched }>`,
built for the visible window plus a margin, never for the whole page.

- One layout means one set of bugs: the wrap, the table columns, the
  anchors and the hit-test ranges (`RenderedLink`,
  `lnomad/src/render.rs:62`) are the ones the terminal has today, and the
  goldens of `lnomad/tests/render_golden.rs` pin them for every target.
- The scene window is small: at most 67 to 90 runs and 1,018 to 2,871
  cells of text. A per-frame build is cheap, a msgpack crossing to Java is cheap.
- A renderer draws runs in a monospace grid: ratatui spans (as
  `to_ratatui_text` does now), iced `rich_text` spans in a monospace font,
  an Android `SpannableString` in a monospace `TextView`, mousefood cells.
  None of them parses micron.
- A renderer per target would mean four layouts of the same 1,004 lines,
  each drifting, and the M9's in a firmware nobody tests with the
  terminal's goldens. That is the duplication Lew's rule forbids.

The full cell page stays the model's state, as it is now. On the M9 it
goes to PSRAM (634 KB of 8 MB for the largest page we have) or the layout
stores runs instead of cells; the M9 slice measures and picks (slice 4).

## 3. Shared controls

| Control | Where it is | lnomad |
|---|---|---|
| List row | `ListItem` (`leviculum-ui/src/scene.rs:261`, since 1240); Loquax's `Row` (`loquax/src/ui/scene.rs:322`) is a `ListItem` with a conversation's `ListRow` beside it; iced `list_row` (`loquax/src/gui/chat.rs:1151`) | **parametrised**: the generic part moved to `leviculum-ui` as `ListItem` in slice 0 (id, `selected`, `activate`, `menu`, `A11y`); Loquax's `Row` keeps its `ListRow` beside it. Title, subtitle and trailing words join the item in slice 1, with the first list whose words are the item's own (Loquax's are its `ListRow`'s). Places, history and name results are list items |
| Top bar | `TopBar`, `Lead` (`leviculum-ui/src/scene.rs:175`) | **reused**, moved to `leviculum-ui` in slice 0; `Lead::Back` for history; the address is a field node below or inside it per renderer |
| Tab bar | `Tabs` (`leviculum-ui/src/scene.rs:232`), `TabId` (`loquax/src/ui/scene.rs:74`), Chats and Map | **parametrised** where the browser is hosted: `TabId::Browse` beside Chats and Map ("Calls later" in [Loquax on Android: navigation and views](loquax-android-navigation.md) already plans a third). The standalone lnomad has no tab bar |
| Sheet | `Sheet` (`leviculum-ui/src/scene.rs:274`) | **reused** unchanged: places, history, a link's long press |
| Search field | `Intent::Search` with the platform's text | **reused**: in-page search and the name search |
| Toast | `leviculum_ui::toast` | **reused**, already shared |
| Chip | `Chip` (`leviculum-ui/src/scene.rs:284`) | **reused**: download running, cached view |
| Accessibility | `A11y` (`leviculum-ui/src/scene.rs:107`) and `Role` (`leviculum-ui/src/scene.rs:69`) | **parametrised**: moved to `leviculum-ui` in slice 0, `Role` gains `Page`, `Link`, `Field`, `Image` in slice 1 |
| Strings | `Text`, `Lang`, `format`, the `catalogue!` macro (`leviculum-ui/src/strings.rs:179`, `leviculum-ui/src/strings.rs:27`, `leviculum-ui/src/strings.rs:143`, `leviculum-ui/src/strings.rs:79`) | **parametrised**: the machinery moved to `leviculum-ui` in slice 0; each program declares its own `StrId` with the macro, keys prefixed `browser_` so the Android files do not collide |
| Key bindings | the mechanism `leviculum_ui::keymap`; Loquax's rows `ROWS` (`loquax/src/keys.rs:187`); `Intent::from_action` (`loquax/src/ui/intent.rs:398`) | **adds** its own rows table (browse, address, hint, search, field, places) with the vi, Emacs and arrow spellings, replacing the per-mode `match`es; the help and footer derive from it as Loquax's do. Chords that mean the same keep the same key (`q`, `?`, `:`, `/`, [Loquax: keys, mouse and layout](loquax-keys-and-layout.md)) |
| Android host and renderers | Loquax's tab host (`ConversationsActivity`), node binders from slice 8 of the study | **reused** in the Loquax APK: one `PageBinder` added |
| iced window | `loquax/src/gui.rs`, `loquax/src/gui/chat.rs` | **extracted**: the top bar, sheet, toast, list item and search field widgets move into a crate `leviculum-ui-iced`, the iced twin of `leviculum-ui-ratatui`; both programs draw through it. Not a second layer: the same extraction #475 did for ratatui |
| Terminal widgets | `leviculum-ui-ratatui` | **reused**; the scene-node widgets (top bar, sheet, list item) are added there, so the M9's mousefood draws them too |

## 4. One app or two

**Recommendation: both, by target.**

- **PC: the `lnomad` binary stays its own program**, terminal and
  window (`lnomad --gui`), linking the shared crates. Lew decided on
  2026-10-25 that the two "stay two programs with shared widgets"
  ([A messenger whose core runs anywhere](handheld-messenger.md), "One
  interaction model"). On a PC both talk to the one `lnsd` as clients
  (`connect_to_shared_instance`, `lnomad/src/fetch.rs:167`), so a second
  program costs no second node. The Debian package is separate on
  purpose (`lnomad/Cargo.toml:20`: a browser-only user must not get the
  `lnsd` service pulled in), and that stays true.
- **Phone and M9: the browser is a tab inside Loquax** (`TabId::Browse`),
  the same `lnomad` core and scene builder linked into the Loquax APK and
  the M9 firmware.

The reasons for the tab:

1. **One node per device.** A phone has no `lnsd`; Loquax's mailbox runs
   a node of its own inside the app (`loquax-android/src/mailbox.rs:4`).
   A second APK would need a second node dialing the entry servers, a
   second foreground service and a second identity, on a battery. The M9
   runs one firmware with one node and one radio; two programs is not a
   choice there.
2. **The 90 % user installs one thing.** One APK, one onboarding, one
   icon (Lew, 2026-10-31: end-user apps as simple as WhatsApp). The
   browser comes with the messenger, and its `lxmf@` links open a chat in
   the same process, the handoff the handheld page promises "once both
   share a shell".
3. **The M9 has one screen and one firmware.** A tab is the only form a
   second program can take on it.
4. **The Android host exists.** The tab bar, the top bar, sheets and the
   binders are Loquax's; a browser APK would need its own host, which is
   the second layer the rule forbids.

What the network seam needs on the phone (slice 3): the mailbox's node is
built with transport off and without the application event stream
(`loquax/src/engine/shell.rs:641`, `loquax/src/engine/shell.rs:680`): it
is "a destination on the network, not a transport node"
(`loquax/src/engine/shell.rs:667`), so a leaf on its screens' listener
(`loquax/src/engine/shell.rs:670`) has no route through it to a NomadNet
node. So the browser on the phone
fetches through the mailbox's node itself: `fetch::Session` (`Session`,
`lnomad/src/fetch.rs:102`) gains a constructor over a node the host hands
it, beside today's shared-instance one. One node, no new transport role.

**Lew decides:**

1. Phone and M9: the browser as a tab inside Loquax, against the
   2026-10-25 "two programs" (which this keeps on the PC). This is the
   recommendation.
2. Whether the Loquax window and terminal also get the Browse tab. It
   costs one feature flag once the tab exists, and would give a PC user
   one program too; the recommendation is no for now, since `lnomad` is
   there.

## 5. Slices

Each slice is one coder pass (two where marked), leaves every target green
and ships on its own.

| # | Slice | Files | Tests | Accepted by | Depends on |
|---|---|---|---|---|---|
| 0 | **The contract's vocabulary moves into `leviculum-ui`**: `A11y`, `Role`, `Action<I>`, `TopBar`, `Lead`, `Tabs`, `Sheet`, `Chip`, a list item; `Text<K>`, `Lang`, `Arg`, `format`, the `catalogue!` macro, `android_xml`. Loquax re-exports, its `StrId` and node types stay its own. No behaviour change | `leviculum-ui/src/scene.rs`, `leviculum-ui/src/strings.rs` (new), `loquax/src/ui/scene.rs`, `loquax/src/ui/strings.rs`, `loquax/src/ui/intent.rs` | `loquax/tests/scene_golden.rs` unchanged; `the_phone_reads_the_catalogue` (`loquax/src/ui/strings.rs:330`) | the three `loquax/tests/golden/scene-*.json` and the generated `strings_scene.xml` byte-identical; `just ui-no-terminal`, `just loquax-core-no-std` green | Loquax slice one (`bb0e9f77`..`af8fa5c1`, here) |
| 1 | **The proof: lnomad's TUI draws through a scene** (2 passes). `lnomad/src/app/scene.rs`: `Model::scene(class)` builds `TopBar`, `Page` (window of runs), the places `Sheet`, chips, toast; `lnomad::Intent`; `lnomad/src/app/strings.rs`, English and German, for the 37 toasts, the start page and the help groups. `tui::view` (`lnomad/src/tui.rs:97`) renders the scene; the cell mouse arms stay as they are, as in the Loquax study | `lnomad/src/app/{scene,strings}.rs` (new), `app/event.rs`, `app/chrome.rs`, `tui.rs` | the 241 `#[test]`s of `tui.rs` unchanged; `render_golden.rs`; new `lnomad/tests/scene_golden.rs` with `scene-page-{53,80}.json` of the README; positive control: shift a link by one cell and the golden fails | every `TestBackend` frame unchanged; the README window at 53×22 holds at most 67 runs (asserted, from 2.2); the build time of a README scene measured and written into this page | slice 0 |
| 1b | **Keys become a table and intents**: lnomad's rows over `leviculum_ui::keymap`, vi, Emacs and arrows; `update_*_key` resolve to `lnomad::Intent`; help and footer derived | `lnomad/src/app/keys.rs` (new), `app/event.rs`, `app/chrome.rs` | a table test: each chord the old `match`es honoured resolves to the same intent; no chord resolves to two intents in one mode | the help overlay lists exactly the chords the handler honours | slice 1 |
| 2 | **The window** (2 passes): `leviculum-ui-iced` extracted from `loquax/src/gui*.rs` (top bar, sheet, toast, list item, search field); `lnomad --gui` behind a `gui` feature draws the scene, the page as monospace `rich_text` runs | `leviculum-ui-iced/` (new), `loquax/src/gui.rs`, `loquax/src/gui/chat.rs`, `lnomad/src/gui.rs` (new), `lnomad/Cargo.toml` | `loquax/tests/golden/gui-chat-{light,dark}.png` byte-identical; new `lnomad/tests/golden/gui-page-{light,dark}.png` of the README fixture | Loquax's goldens unchanged; a click on a link run raises `Activate(Link(n))` in a headless test; frame time for the README at 1280×800 measured | slice 1; Loquax study slices 2 and 3 (iced draws the scene, the top bar and tab bar in the model) |
| 3 | **Android: the Browse tab in Loquax** (2 passes). `fetch::Session` over a node the host hands it; the mailbox lends its node; the facade gets `Query::Scene{Browse}` and the browser's requests; Java adds a `PageBinder` (monospace `SpannableString`) and the address as a real `EditText` | `lnomad/src/fetch.rs`, `loquax-android/src/{session,schema,browser}.rs`, `loquax/src/ui/scene.rs` (`TabId::Browse`), Java: tab host, `PageBinder` | `lnomad/tests/fetch_integ.rs` unchanged; a facade test: the msgpack of the README window; the device walk opens Browse, loads a page from the walk's local node, follows a link, goes back | the msgpack size of a README window measured and the time of one scroll's crossing; the device walk green | slice 1; Lew's decision 1; Loquax study slice 8 (the host Activity and binders); without it the binder sits on today's tab host |
| 4a | **The core goes `no_std`**: `leviculum-micron` and lnomad's core build with `alloc` only; a `std` feature carries fetch, files, the image decoder and the terminal; `just lnomad-core-no-std` as `just loquax-core-no-std` does it | `leviculum-micron/src/*.rs`, `lnomad/src/{lib,app/model,discovery,bookmarks,identify,download,image}.rs`, `lnomad/Cargo.toml`, `scripts/`, `Justfile` | the whole lnomad suite with default features; the new check with a positive control (an injected `std::fs` fails it) | the check green on `thumbv7em-none-eabihf` and the ESP32 triple | slice 1 |
| 4b | **The M9 tab** (2 passes): the TUI's scene widgets on mousefood; the page in PSRAM or as runs; fetch through the board's node | `leviculum-ui-ratatui` (the `no_std` half), the M9 firmware's UI | an embedded-graphics simulator PNG of the README at 320×240 on the host, from the same fixture scene | on the board: the README loaded, heap peak and frame time measured and written here; scrolling the README end to end without an allocation failure | slice 4a; Loquax study slice 11 (the M9 shell and renderer) |

Passes: 0: 1, 1: 2, 1b: 1, 2: 2, 3: 2, 4a: 1, 4b: 2, together 11. The
order is the dependency order; slices 2 and 3 can swap if Loquax's
Android slice 8 lands before its iced slices.

**Slice 0, built 2026-11-01 (1240).** The vocabulary is in
`leviculum_ui::scene` and `leviculum_ui::strings`, `alloc` only like the
rest of the crate without its `std` feature (`leviculum-ui/src/lib.rs:29`),
so `just loquax-core-no-std` still builds it for the board.

- **Generics.** `Text<K>`, `Arg<K>` and `A11y<K>` take the catalogue's key
  type, any `strings::Key` (`leviculum-ui/src/strings.rs:57`), which
  `catalogue!` implements for the enum it declares. `Action<I>` takes the
  intent, any `scene::Labelled` (`leviculum-ui/src/scene.rs:30`), whose
  label is a key. The nodes that name more than words and verbs (`TopBar`,
  `Lead`, `Tab`, `Tabs`, `ListItem`, `Sheet`, `Chip`) take one parameter,
  a `scene::Contract` (`leviculum-ui/src/scene.rs:52`): the program's key,
  intent, node id, tab, avatar and the row a sheet lists. Loquax sets them
  once on the marker `Loquax` (`loquax/src/ui/scene.rs:154`) and names the
  shared nodes through aliases, so its builders and every renderer read
  as before.
- **What stays Loquax's.** `StrId` and `CATALOGUE` (declared with
  `leviculum_ui::catalogue!`), `NodeId`, `TabId`, `Avatar`, `Ring`, `Row`,
  `List`, the chat's nodes, `Card` and `Scene`. `format` and
  `android_xml` keep their Loquax signatures as one-line calls into the
  shared ones, the latter naming its source file and `just
  loquax-strings` in the header it writes.
- **No behaviour change.** The three `scene-*.json` goldens and both
  `strings_scene.xml` are byte identical; a role's word changed in the
  shared code fails the scene golden, a changed header fails
  `the_phone_reads_the_catalogue`.

## 6. What not to do

- **No micron renderer per target.** One layout, runs in the scene
  (section 2.2).
- **No browser intents in Loquax's `Intent`.** The browser's verbs are
  its enum; the host wraps it.
- **No lnomad APK beside Loquax.** Two nodes on one phone (section 4).
- **No second widget layer for iced.** `leviculum-ui-iced` is extracted
  from Loquax's widgets, not written beside them.
- **No full page in the scene.** A window of the screen's rows, so the
  M9 and the JNI crossing pay for 67 runs, not for 719.

