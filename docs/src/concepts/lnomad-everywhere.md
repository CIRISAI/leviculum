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
5,600 lines of tests (its test module starts at `tui.rs:1494` of 7,309).

| Role | Files (production lines) | Production | Platform free |
|---|---|---:|---|
| Core: model, `update`, effects, chrome geometry | `app/event.rs` 1,963, `app/model.rs` 978, `app/chrome.rs` 537, `app/mod.rs` 32, `app/geometry.rs` 31, `app/input.rs` 5 | 3,546 | yes, no terminal crate; but in character cells |
| Fetch and navigation | `fetch.rs` 620, `url.rs` 191, `browser.rs` 182, `page_cache.rs` 113 | 1,106 | `url`, `page_cache` yes; `fetch` and `browser` need `leviculum-std` and tokio |
| Micron layout | `render.rs` 1,004 (and the parser crate `leviculum-micron`, 1,194) | 1,004 | yes |
| Terminal frontend | `tui.rs` 1,709, `main.rs` 289, `cli.rs` 37 | 2,035 | no (`tui.rs`, `main.rs`) |
| Discovery | `discovery.rs` 329 | 329 | yes, but `SystemTime` (`lnomad/src/discovery.rs:29`) |
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
lnomad-core-no-tui` (`Justfile:647`) and stated in the module doc of
`lnomad/src/app/mod.rs:1`. Two qualifications, both measured:

- **The core speaks cells.** Events arrive as raw keys and cell-addressed
  mouse events (`AppEvent`, `lnomad/src/app/event.rs:36`). The key handlers
  were hand-written `match`es per mode (`update_browse_key`,
  `update_address_key`, `update_places_key`), and only the scroll keys came
  from the shared table. Slice 1b made them one table (`ROWS`,
  `lnomad/src/app/keys.rs:150`; `resolve`, `lnomad/src/app/keys.rs:386`). The top
  bar is a cell layout (`TopBar`, `lnomad/src/app/chrome.rs:29`), and the
  click and hover tests run against it (`handle_click`,
  `lnomad/src/app/event.rs:1915`). There is no intent and no scene.
- **The core was `std`.** `PathBuf` for `bookmarks_path`, `SystemTime`
  in discovery, and the parser crate used `std::collections::BTreeMap`
  and `std::borrow::Cow`, all of which `alloc` has. The crate as a whole
  linked `leviculum-std` and tokio for `fetch`, so a firmware could not
  take it as it was
  ([A messenger whose core runs anywhere](handheld-messenger.md) says the
  same). Slice 4a made the core `no_std` behind a `std` feature (see the
  slice's paragraph in section 5).

The user-visible words were literals: 37 `set_toast(` calls (23 in
`app/event.rs`, 1 in `app/model.rs`, 13 in `tui.rs`), the start page and
the help groups. Nothing was translated. Slice 1 moved them into the
catalogue; the start page is now `start_page`
(`lnomad/src/app/event.rs:368`, a hosted browser's own since 1337) and the help groups `help_groups_in`
(`lnomad/src/app/chrome.rs:363`), both in the reader's language.

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
`loquax/src/ui.rs:172`): `scene.rs` 1,231 lines (1,091 production),
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
| Top bar: title, address, star, identity, cache bolt, hops (`topbar_layout`, `lnomad/src/app/chrome.rs:101`) | `TopBar` (`leviculum-ui/src/scene.rs:206`) with `Lead::Back` while history has a past, title, and `actions` (star, identify, reload) | `Back`, `Bookmark{on}`, `Identify{on}`, `Reload` | `browser_bookmark`, `browser_identify`, `browser_reload`, `browser_hops` | shared, moved; the actions browser |
| Address field (the `Address` arm of `Mode`, `lnomad/src/app/model.rs:102`) | a field node: the platform's text field, the scene names its hint and its go, as `Compose` does (`loquax/src/ui/scene.rs:658`) | `Go` (the URL is the platform's text, as Loquax's `Search` is) | `browser_address_hint` | browser node, shared shape |
| Page view (`model.page`, laid out by `layout`, `lnomad/src/render.rs:187`) | `Page`: a window of `PageLine`s, each a list of runs (see 2.2) | `Scroll(ScrollCmd)` (exists) | none: page text is `Text::Raw` | browser |
| Link focus, hint labels, follow (`focus`, `visible_links`, `lnomad/src/app/model.rs:958`) | a run carries its link index; `Page.focus` names the focused link | `FocusNext(dir)`, `Activate(NodeId::Link(n))`, `Hint` | `browser_hint` | `Activate` exists (`Intent`, `loquax/src/ui/intent.rs:168`); the link node browser |
| Form fields (`RenderedField`, `lnomad/src/render.rs:114`) | a run carries its field index; the edited field is a platform text field like the address | `Edit`, `Toggle(field)`, `Submit` | `browser_submit` | browser |
| Back and forward (`go_back`, `lnomad/src/app/event.rs:1608`; `go_forward`, `lnomad/src/app/event.rs:1618`) | `Lead::Back`; forward as a `TopBar` action | `Back` (exists), `Forward` | `back` (exists), `browser_forward` | `Back` exists |
| History (`History`, `lnomad/src/app/model.rs:304`) | a `Sheet` of rows | `Activate(row)` | `browser_history` | sheet exists, rows browser |
| Places: bookmarks and discovered nodes (`Place`, `lnomad/src/app/event.rs:889`; `places`, `lnomad/src/app/event.rs:929`) | `Sheet` (`leviculum-ui/src/scene.rs:293`) of list rows | `Activate(row)`, `Menu(row)` → `Rename`, `Forget` | `browser_places`, `browser_nodes` | sheet exists; `Rename` and `Forget` exist |
| The name search of 1227 (not in this tree: no `1227` in `lnomad/` or `docs/src/` at `af8fa5c1`) | the address field's results: the places `Sheet`, rows from 1227's announce search | `Search` (exists), `Bookmark{on}` for its pinning | `browser_find_name` | lands with 1227 into the shapes above, no new node |
| In-page search (`Mode::Search`, matches as `Match`, `lnomad/src/app/model.rs:174`) | the shared search field; matches as a run flag | `Search`, `Match(next)` (both exist) | `search` | exists |
| Images (`SlotState`, `lnomad/src/image.rs:229`) | `Image` node in the page window: name, state, the decoded picture by reference (pixels never enter the scene, as for the map) | `Activate` opens, `Menu` saves | `browser_image_loading`, `_failed`, `_too_many` | browser |
| Downloads (`Effect::Download`, `Effect`, `lnomad/src/app/model.rs:402`) | `Chip` (`leviculum-ui/src/scene.rs:303`) while one runs, a toast when done | `Download` | `browser_downloaded` (the toast of `lnomad/src/app/event.rs:144`) | chip exists |
| Identify (`IdentifyStore`, `lnomad/src/identify.rs:28`) | the marker as a `TopBar` action whose state is on or off | `Identify{on}` | `browser_identify_on`, `_off` | browser |
| Status line: focused link target, footer (`footer_items`, `lnomad/src/app/chrome.rs:310`) | no node of its own: the footer is the renderer's way to show `Action`s with chords (study, table 3.5); the link target is the focused link's text | none | the actions' labels | exists as a rule |
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
today is a 64 KiB placeholder (`HEAP_SIZE`, `leviculum-esp/src/lib.rs:423`).
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
  `lnomad/src/render.rs:67`) are the ones the terminal has today, and the
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
| List row | `ListItem` (`leviculum-ui/src/scene.rs:273`, since 1240); Loquax's `Row` (`loquax/src/ui/scene.rs:409`) is a `ListItem` with a conversation's `ListRow` beside it; iced `list_row` (`loquax/src/gui/chat.rs:2972`) | **parametrised**: the generic part moved to `leviculum-ui` as `ListItem` in slice 0 (id, `selected`, `activate`, `menu`, `A11y`); Loquax's `Row` keeps its `ListRow` beside it. Title, subtitle and trailing words join the item in slice 1, with the first list whose words are the item's own (Loquax's are its `ListRow`'s). Places, history and name results are list items |
| Top bar | `TopBar`, `Lead` (`leviculum-ui/src/scene.rs:187`) | **reused**, moved to `leviculum-ui` in slice 0; `Lead::Back` for history; the address is a field node below or inside it per renderer |
| Tab bar | `Tabs` (`leviculum-ui/src/scene.rs:251`), `TabId` (`loquax/src/ui/scene.rs:78`), Chats and Map | **parametrised** where the browser is hosted: `TabId::Browse` beside Chats and Map ("Calls later" in [Loquax on Android: navigation and views](loquax-android-navigation.md) already plans a third). The standalone lnomad has no tab bar |
| Sheet | `Sheet` (`leviculum-ui/src/scene.rs:293`) | **reused** unchanged: places, history, a link's long press |
| Search field | `Intent::Search` with the platform's text | **reused**: in-page search and the name search |
| Toast | `leviculum_ui::toast` | **reused**, already shared |
| Chip | `Chip` (`leviculum-ui/src/scene.rs:303`) | **reused**: download running, cached view |
| Accessibility | `A11y` (`leviculum-ui/src/scene.rs:119`) and `Role` (`leviculum-ui/src/scene.rs:69`) | **parametrised**: moved to `leviculum-ui` in slice 0, `Role` gains `Page`, `Link`, `Field`, `Image` in slice 1 |
| Strings | `Text`, `Lang`, `format`, the `catalogue!` macro (`leviculum-ui/src/strings.rs:180`, `leviculum-ui/src/strings.rs:27`, `leviculum-ui/src/strings.rs:144`, `leviculum-ui/src/strings.rs:80`) | **parametrised**: the machinery moved to `leviculum-ui` in slice 0; each program declares its own `StrId` with the macro, keys prefixed `browser_` so the Android files do not collide |
| Key bindings | the mechanism `leviculum_ui::keymap`; Loquax's rows `ROWS` (`loquax/src/keys.rs:225`); `Intent::from_action` (`loquax/src/ui/intent.rs:519`) | **adds** its own rows table (browse, address, hint, search, field, places) with the vi, Emacs and arrow spellings, replacing the per-mode `match`es; the help and footer derive from it as Loquax's do. Chords that mean the same keep the same key (`q`, `?`, `:`, `/`, [Loquax: keys, mouse and layout](loquax-keys-and-layout.md)) |
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
  (`connect_to_shared_instance`, `lnomad/src/fetch.rs:309`), so a second
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
(`loquax/src/engine/shell.rs:902`, `loquax/src/engine/shell.rs:941`): it
is "a destination on the network, not a transport node"
(`loquax/src/engine/shell.rs:928`), so a leaf on its screens' listener
(`loquax/src/engine/shell.rs:931`) has no route through it to a NomadNet
node. So the browser on the phone
fetches through the mailbox's node itself: `fetch::Session` (`Session`,
`lnomad/src/fetch.rs:245`) gains a constructor over a node the host hands
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
| 0 | **The contract's vocabulary moves into `leviculum-ui`**: `A11y`, `Role`, `Action<I>`, `TopBar`, `Lead`, `Tabs`, `Sheet`, `Chip`, a list item; `Text<K>`, `Lang`, `Arg`, `format`, the `catalogue!` macro, `android_xml`. Loquax re-exports, its `StrId` and node types stay its own. No behaviour change | `leviculum-ui/src/scene.rs`, `leviculum-ui/src/strings.rs` (new), `loquax/src/ui/scene.rs`, `loquax/src/ui/strings.rs`, `loquax/src/ui/intent.rs` | `loquax/tests/scene_golden.rs` unchanged; `the_phone_reads_the_catalogue` (`loquax/src/ui/strings.rs:993`) | the three `loquax/tests/golden/scene-*.json` and the generated `strings_scene.xml` byte-identical; `just ui-no-terminal`, `just loquax-core-no-std` green | Loquax slice one (`bb0e9f77`..`af8fa5c1`, here) |
| 1 | **The proof: lnomad's TUI draws through a scene** (2 passes). `lnomad/src/app/scene.rs`: `Model::scene(class)` builds `TopBar`, `Page` (window of runs), the places `Sheet`, chips, toast; `lnomad::Intent`; `lnomad/src/app/strings.rs`, English and German, for the 37 toasts, the start page and the help groups. `tui::view` (`lnomad/src/tui.rs:99`) renders the scene; the cell mouse arms stay as they are, as in the Loquax study | `lnomad/src/app/{scene,strings}.rs` (new), `app/event.rs`, `app/chrome.rs`, `tui.rs` | the 241 `#[test]`s of `tui.rs` unchanged; `render_golden.rs`; new `lnomad/tests/scene_golden.rs` with `scene-page-{53,80}.json` of the README; positive control: shift a link by one cell and the golden fails | every `TestBackend` frame unchanged; the README window at 53×22 holds at most 67 runs (asserted, from 2.2); the build time of a README scene measured and written into this page | slice 0 |
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
  type, any `strings::Key` (`leviculum-ui/src/strings.rs:58`), which
  `catalogue!` implements for the enum it declares. `Action<I>` takes the
  intent, any `scene::Labelled` (`leviculum-ui/src/scene.rs:30`), whose
  label is a key. The nodes that name more than words and verbs (`TopBar`,
  `Lead`, `Tab`, `Tabs`, `ListItem`, `Sheet`, `Chip`) take one parameter,
  a `scene::Contract` (`leviculum-ui/src/scene.rs:52`): the program's key,
  intent, node id, tab, avatar and the row a sheet lists. Loquax sets them
  once on the marker `Loquax` (`loquax/src/ui/scene.rs:241`) and names the
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

**Slice 1, pass one built 2026-11-01 (1255).** Three commits on this
branch; pass two starts where the last list below ends.

- **The scene.** `Model::scene()` (`lnomad/src/app/scene.rs:290`) builds a
  `Scene` (`lnomad/src/app/scene.rs:273`): the shared `TopBar` with
  `Lead::Back` while history has a past and forward, reload, the star and
  identify as actions; an `AddressField` (`lnomad/src/app/scene.rs:212`)
  with its value, hint, caret and go; the top bar's state as chips
  (identity, cache, hops); the `Page` (`lnomad/src/app/scene.rs:252`), a
  window of `PageLine`s of `Run`s (`lnomad/src/app/scene.rs:233`), one
  style, link, field and search state per run; the status chip; the
  places panel and the 1227 name results as `Sheet`s of lnomad's `Row`
  (`lnomad/src/app/scene.rs:169`: a heading, a placeholder, a list item,
  a note); the toast in the catalogue's words. The contract's marker is
  `Lnomad` (`lnomad/src/app/scene.rs:132`); the standalone browser has no
  tab, so its `Tab` is `()`. `Role` gained `Page`, `Link`, `Field` and
  `Image`.
- **The intents.** `lnomad::Intent` (`lnomad/src/app/intent.rs:69`) has
  22 arms; its module doc holds the table from today's keys to them, and
  `apply` (`lnomad/src/app/intent.rs:245`) folds one through the
  functions the keys reach. `an_intent_does_what_its_key_does` holds 14
  of them to their key: the same effects, the same scene after.
- **The catalogue.** `lnomad/src/app/strings.rs`, 87 strings in English
  and German, every key prefixed `browser_` and checked against Loquax's:
  all the toasts of the core and the shell, the top bar's words, the
  footer labels, the panel and the result list's chrome. The English
  texts are the old literals, so the frames hold them.
- **The terminal draws the scene** for the page window, the status chip,
  the places panel, the result list (a scene list now, not a hand-drawn
  block) and the toast. The 248 tests of `tui.rs` pass unchanged.
  Mutations show the frames read the scene: a link run's colour fails
  three frames, a run's text eight, six catalogue words ten.
- **Measured.** The goldens `lnomad/tests/golden/scene-page-{53,80}.json`
  pin the README's scene at 53 and 80 columns by 24; a run's colour and a
  link moved by one cell both fail them. A README window laid out on 53
  cells holds at most **67 runs**, as 2.2 measured; the terminal 53
  columns wide lays out 52 (one cell for its scrollbar) and peaks at 68,
  both asserted. One README scene at 53 by 24 builds in **54 µs** on the
  host in the unoptimised test profile (`a_readme_scene_is_built_in_well_under_a_frame`,
  200 builds).

**Slice 1, pass two built 2026-11-01 (1255b).** Five commits on this
branch close the slice.

- **The top bar from the scene.** `topbar_layout`
  (`lnomad/src/app/chrome.rs`) lays out the scene's top bar, address field
  and state chips: the title, the address or its hint, the star from the
  bookmark action, the identity, cache and hops chips. The hit tests read the
  same layout, so a click and the drawing agree. The one-row bar draws the
  identity and cache chips as glyphs and the hops chip in its words.
- **The overlays.** The search highlight is painted from the runs' match
  state. The focus and the field boxes take *which* node is focused from
  `Page.focus` and keep the cell spans the clicks land on: a link's span is
  its first line only, and drawing its runs would also light a wrapped second
  line, which changes frames. The hover (pointer state, which a window and a
  phone keep themselves) and the hint labels (a keyboard affordance of the
  terminal) stay the terminal's own.
- **The catalogue is complete for the shell.** The help (group titles,
  descriptions, and the key column's words such as "click"), the start page,
  the image status lines, the result rows' words of `lnomad/src/names.rs`
  and the fetch's errors joined it: 152 strings in English and German. The
  shared scroll rows keep their English in `leviculum-ui`, and lnomad maps
  each one to its own key, with a test that none is left untranslated.
  `run_tui` now sets the language from `LC_ALL`, `LC_MESSAGES` or `LANG`, the
  first one set, as the Loquax window does. A German frame test renders the
  start page and the help and finds no English sentence.
- **Unchanged frames.** The 248 tests of `tui.rs` pass without a change.
  Two tests were added: German, and italic. Mutating the scene's title, a
  run's match state, the hops chip, the focus or the star fails frames
  (1, 3, 1, 2 and 2). The new italic frame test closes the gap pass one's
  control found: dropping the runs' italic flag fails it and nothing else.
- **The names list as a scene list, pinned.**
  `lnomad/tests/golden/scene-names.json` holds the 1227 result list for
  "marcel" over three sites at a fixed clock: catalogue keys per row, the
  selection, the key hint. Swapping two rows or changing one row's words
  fails it. The README goldens did not move.

**Slice 1b, built 2026-11-01 (1285).** The per-mode `match`es are gone.

- **The table.** `lnomad/src/app/keys.rs` holds lnomad's rows
  (`ROWS`, `lnomad/src/app/keys.rs:150`) over `leviculum_ui::keymap`,
  beside the shared `SCROLL_ROWS`, the way Loquax holds its own
  (`loquax/src/keys.rs:225`). A mode is a pane: browse, address, search,
  field, hint, and the overlays that own the keys while open (help,
  places, the name list, and the name list while it asks). Two contexts:
  a fetch in flight (`Esc` and `Ctrl-g` cancel it) and a checkbox or radio
  focused (`Space` toggles it rather than typing). `resolve`
  (`lnomad/src/app/keys.rs:386`) is Loquax's rule: the most specific chord,
  lnomad's rows first among equals. A key's `Action` becomes a
  `lnomad::Intent` through `Intent::of_key`
  (`lnomad/src/app/intent.rs:155`), which reads what it acts on (the
  focused link, the selection, the bookmark), and `update_key`
  (`lnomad/src/app/event.rs:578`) folds it with `apply`, as a tap does.
  What a table cannot hold stays with the handler: the text the editors
  take, hint labels, and the "no" any key but `y` gives the pin question.
- **Size per mode**, chords in force over both tables: browse 47, address
  4, search 4, field 6, hint 3, help 25, places 28, names 24, ask 3
  (`the_tables_size_per_mode`).
- **The intents** grew from 22 to 28 arms for what the keys did that no
  intent named: `Cancel`, `Find` (open the search field), `Answer { yes }`
  (the pin question), `OpenImage`, `Theme`, `Redraw`. `Dismiss` now also
  leaves the address editor and a field, `Scroll` scrolls what is in
  front (the help, the places selection, the name list, else the page),
  `Toggle(n)` toggles without moving the focus, as `Space` did, and a tap
  on a name pinned to another key asks first, as `Enter` did.
- **Help and footer derived.** The help's key column is each row's keys,
  derived from its chords; the mouse spelling moved into the row's words
  ("enter a URL, or click the URL"). The footer is each mode's buttons
  whose action has a live chord there, showing that chord. Two
  consequences: the help lists every chord (cancel, redraw, the places
  keys, the pin answer), so it needs 50 rows where it fit 40, and a footer
  no longer offers a key its mode does not honour: `?` is a letter in an
  editor, so `F1` opens the help there (and from browsing), as in Loquax,
  and a text field's footer drops `Space toggle`. The search footer's
  `n`/`N` are gone too; that footer is never drawn.
- **Acceptance.** `every_old_chord_resolves_to_the_same_action`
  (`lnomad/src/app/keys.rs:818`) transcribes the old `match`es and
  compares them with the table for every printable ASCII key, `ä` and
  every special key under every mix of Shift, Ctrl and Alt, in 9 panes and
  4 settings; the only key the table adds is `F1`.
  `no_chord_resolves_to_two_actions_in_one_pane`
  (`lnomad/src/app/keys.rs:602`) resolves each chord's own key in every
  pane and setting it is in force. Positive controls: `m` made
  `Mods::Any` fails the first (`Ctrl-m` reached Mark), a second action on
  `y` fails the second. `the_help_lists_exactly_the_tables_chords`
  (`lnomad/src/app/keys.rs:881`) holds the help to the table.

**Slice 2, built 2026-11-02 (1286).** Seven commits on this branch; the
window draws, clicks and is measured. What it does not do yet is listed
last, and is where a second pass starts.

- **`leviculum-ui-iced`, extracted, not written beside.** Out of
  `loquax/src/gui/chat.rs` into the crate, generic over the program's
  message and coloured by a `Palette` each program fills from its own
  tokens (Loquax's `palette`, `loquax/src/gui/chat.rs:82`): the drawn
  icons with the compose bar's and the recording bar's (`icon`), the
  waveform (`wave`, the fold handed in), the bare, link and icon-action
  buttons (`control`), the avatar and its dot (`avatar`), the top bar,
  the count badge, a tab and the tabs' rail or bar, the list item, the
  search field (`chrome`), an iced key as the keymap's (`key`, from
  `loquax/src/gui.rs`), and a view without a window (`headless`: render
  to RGB, and a click that returns the published messages; Loquax's
  goldens now render through it). The mark's glyph data moved to
  `leviculum_ui::glyph`, so the icon draws a shape without knowing a
  delivery mark. Loquax keeps its nodes and their composition (bubbles,
  the compose and recording bars) and draws them through the crate.
  Loquax's window goldens are byte identical: `just loquax-gui` green
  (709 and 41 tests); one padding pixel more in the shared list item
  fails the chat pane and recording bar goldens.
- **The crate is empty on musl.** iced's window crates link libdl
  (x11-dl, libloading), which the workspace's musl-static default cannot
  link, so `cargo test --workspace` failed on the crate's test binary.
  A window is a gnu-target build, as Loquax's `gui` feature already
  said; `just loquax-gui` now lints on the host target too, and `just
  lnomad-gui` lints and tests lnomad's window and the crate there. The
  order's `cargo test -p leviculum-ui-iced -p lnomad --features gui` is
  that recipe's line with `--target` the host's.
- **The transfers left the terminal.** The fetch, the download, the
  discovery drain and closing the session moved unchanged from `tui.rs`
  into `lnomad/src/transfer.rs`; the terminal and the window run them.
- **`lnomad --gui`** (`lnomad/src/gui.rs`, feature `gui`). The view
  (`lnomad/src/gui.rs:671`) draws `Model::scene`: the shared top bar
  with back while history has a past and the bar's actions as words, a
  disabled one grey; the address as the window's own text field, its
  Enter the field's go; the state chips; the page as the scene's window
  of lines on a cell grid, one rich text per line and one span per run
  in Fira Mono (embedded, OFL, `lnomad/fonts/`), a click on a link run
  `Activate(Link(n))`; the places panel or the name results as a sheet
  of the shared list items; the status line and the toast. Keys go
  through the rows table as in the terminal, the wheel through the
  model's mouse arm. The model's size in cells is what the window's page
  holds (`cells`), so a resize lays the page out again as a terminal's
  does. The transfers run on the session's runtime and answer as
  outcomes the window folds as the terminal does (`fold`,
  `lnomad/src/gui.rs:554`).
- **Tests** (`just lnomad-gui`, 465 tests): the README at 1280 by 800
  pixel for pixel in both themes (`lnomad/tests/golden/gui-page-{light,dark}.png`,
  `lnomad/src/gui.rs:1519`; the link underline dropped moves 1,950
  bytes); a headless click on the README's first link raises
  `Activate(Link(n))` and folded starts its load, a click beside it
  nothing (`lnomad/src/gui.rs:1556`; the link index off by one fails
  it); the three name results for "marcel" are rows a click opens
  (`lnomad/src/gui.rs:1644`; a row raising another intent fails it);
  the page laid out on the window's cells (`lnomad/src/gui.rs:1582`).
- **Measured.** One frame of the README at 1280 by 800 (view, layout,
  paint and the readback of the software renderer) takes **15.8 ms** in
  the release profile and 358 ms in the test profile, on the Ryzen 9
  7950X host, the mean of 10 (`lnomad/src/gui.rs:1614`).
- **Not yet; pass two.** What pass one left is listed under pass two
  below, built.

**Slice 2, pass two built 2026-11-02 (1286b).** Eight commits on this
branch; the window now draws what the terminal draws, apart from a real
desktop's colour scheme on Linux and a screenshot over a live node.

- **Bold.** Fira Mono Bold 3.206 is embedded beside the Medium (OFL,
  the same version and the same 600 unit advance;
  `lnomad/src/gui.rs:80`), so a bold run is set bold and stays on the
  grid. Italic stays upright: the family has no italic, and a face it
  lacks falls back to the proportional sans, off the grid.
- **Form fields.** A field run is a box in the terminal's chrome
  colours, reversed while focused, with a block caret on the editor's
  cell placed as the terminal places its cursor (`caret`,
  `lnomad/src/gui.rs:877`; `line_spans`, `lnomad/src/gui.rs:960`). A
  click on a field is `Activate(Field(n))` through the same rich-text
  hit as a link, and an uncaptured key types into it through the rows
  table.
- **The help.** The overlay is a shared widget,
  `leviculum_ui_iced::help::overlay` (`leviculum-ui-iced/src/help.rs:32`),
  the twin of the terminal's: the same lines wrapped into the same rows
  over the model's cells and scrolled by the same bounds. The two
  renderers style one walk, the segments of a line and their cut to a
  drawn row (`segments`, `leviculum-ui/src/help.rs:89`; `cut`,
  `leviculum-ui/src/help.rs:110`). The wheel scrolls it and a click
  closes it (`help_layer`, `lnomad/src/gui.rs:722`).
- **One chrome for the suite.** The window's grounds, words, accent,
  panel, selection and badge are one table in `leviculum_ui::pixel`,
  `Chrome` (`leviculum-ui/src/pixel.rs:14`), with Loquax's values; Loquax's
  `LIGHT` and `DARK` take those fields from it, and lnomad's palette is
  `Palette::of` the same table (`lnomad/src/gui.rs:131`). A page's inks
  and NomadNet's link colour stay lnomad's. Loquax's window goldens and
  the phone's `colors.xml` test are unchanged;
  `the_chrome_is_the_suites` (`loquax/src/ui/chat.rs:1524`) holds the
  sharing.
- **`--theme auto` follows the desktop.** Asked at the start and on
  each change (`lnomad/src/gui.rs:524`), as Loquax's window does, until
  the reader flips the theme away from what the desktop last set. On
  Linux iced reads the freedesktop colour scheme only with its
  `linux-theme-detection` feature (mundy, over D-Bus), which neither
  window enables; until that is decided the answer there is winit's,
  and may be none, which keeps the window dark. Decided in 1310, below.
- **Inline pictures.** The terminal's image queue, a picture's save and
  the two toasts of a save and an open moved into `lnomad::transfer`
  (`spawn_image_queue`, `lnomad/src/transfer.rs:285`; `save_picture`,
  `lnomad/src/transfer.rs:328`), and both frontends run them. The window
  decodes with the same `image` crate, holds the picture as an iced
  RGBA image (iced's `image-without-codecs`, as Loquax has it) and draws
  it in the rows the layout reserves under its link, at its natural
  size on the cell grid, never enlarged, under the picture row ceiling
  (`picture_rows`, `lnomad/src/image.rs:83`; `accept`,
  `lnomad/src/gui.rs:331`; `picture_layer`, `lnomad/src/gui.rs:1038`).
  Until then, or when it cannot be drawn, that row says why, as the
  terminal's does. Payloads go into the same byte-bounded cache, so a
  revisit draws at once; a resize fits every picture again (`refit`,
  `lnomad/src/gui.rs:372`).
- **Hover.** The pointer's cell goes to the model as the terminal's
  mouse moves do (`lnomad/src/gui.rs:514`), so the status line says
  where a hovered link goes.
- **Tests** (`just lnomad-gui`, 472 tests). New goldens:
  `gui-form-light.png` (the form, its text field focused;
  `lnomad/src/gui.rs:1825`), `gui-help-dark.png`
  (`lnomad/src/gui.rs:1835`), `gui-picture-light.png` (a 240 by 96
  picture under its link is 30 by 6 cells; `lnomad/src/gui.rs:1949`);
  the README goldens moved for the bold runs and the suite's chrome. A
  click on a field focuses it and a key types into it
  (`lnomad/src/gui.rs:1734`); the reversed field around the block caret
  (`lnomad/src/gui.rs:1760`); a bold run in the bold face
  (`lnomad/src/gui.rs:1797`); an undecodable payload and a failed
  transfer as lines (`lnomad/src/gui.rs:1965`); a revisit from the cache
  (`lnomad/src/gui.rs:1984`); a resize fitting a wide picture to a
  narrow page (`lnomad/src/gui.rs:2002`); following the desktop until
  the reader flips (`lnomad/src/gui.rs:1860`); the hover
  (`lnomad/src/gui.rs:2020`). Each has its control: the bold face, the
  field's hit, its reverse, the help's bold keys (22,042 bytes), its
  close, the flip check, the picture (69,120 bytes, its 240 by 96 RGB)
  and the hover's top bar row each removed fail their test.
- **Measured.** The README frame at 1280 by 800 takes **16.2 ms** in the
  release profile now (15.8 ms before), the mean of 10 on the same host.
- **Not yet.** A screenshot of the real window over a live node (the
  goldens are the headless renders; no `lnsd` was started for this
  pass); the desktop's colour scheme on Linux (above); a picture saved
  or opened in the window is not under a test, since both write to the
  reader's download directory.

**Pass three, built 2026-11-02 (1310).** What pass two left open.

- **The desktop's tone on Linux.** Both windows follow it through
  `leviculum_ui_iced::desktop` (`theme`, `leviculum-ui-iced/src/desktop.rs:20`;
  `theme_changes`, `leviculum-ui-iced/src/desktop.rs:34`): mundy's
  colour-scheme stream as a subscription, not iced's
  `linux-theme-detection`, which blocks the event loop up to 200 ms
  before the first window. On the coder host, whose portal never
  answers, iced's feature took the first visible window from 39.3 ms
  to 241.8 ms (mean of 10); the subscription leaves it at 41 to 43 ms,
  the same as before. Without a session bus the stream yields none and
  ends (`leviculum-ui-iced/tests/desktop_theme.rs`). mundy is a Linux,
  non-musl dependency only; Android and the static builds do not have it.
- **The frame test bounds CPU time.** Under a 25 % CPU quota the README
  frame took 1.279 s of wall clock and 319 ms of the drawing thread's
  CPU; the bound is on the latter (`lnomad/src/gui.rs:1614`), which is
  the work a slow frame would add.
- **A picture sits flush left** in its cells, as the terminal draws it
  (`picture_layer`, `lnomad/src/gui.rs:1038`): the widget takes the
  picture's fitted size, not its cell box, in which iced centred it.

**Slice 4a, built 2026-11-02 (1303).** The core is `no_std` plus `alloc`;
`just lnomad-core-no-std` (`Justfile:2669`) holds it.

- **`leviculum-micron` is `no_std` always** (`leviculum-micron/src/lib.rs:25`):
  a parser has no use for a file or a clock, and its `std` paths were
  `BTreeMap`, `Cow` and `mem::take`.
- **lnomad has a default `std` feature** (`lnomad/Cargo.toml:74`;
  `lnomad/src/lib.rs:36`), and `ble`, `serial`, `tui` and `gui` imply it.
  Behind it: the modules `fetch`, `browser`, `transfer`, `cli` and
  `identify`; the file halves of `download` (the metadata name, the
  output path, the save), `identity` (the config dir, the identity
  file), `bookmarks` and `names` (load, save, the default path);
  `probe` and `decode` in `image`; `now_unix_secs` in discovery; the
  model's two store paths; and the crates `leviculum-std`, tokio, rmpv,
  toml, clap, futures, tui-input, open and image. thiserror, serde and
  serde_json stay, without their own `std`. Everything else is the core:
  `app/` (model, `update`, keys, chrome, scene, intents, catalogue),
  `render`, `url`, the page and image caches, discovery, `color`,
  `theme`, and the pure halves above (`basename`, `choose_name`,
  `fingerprint_hex`, the lists of bookmarks and pins, the image
  arithmetic and its status lines).
- **Three moves, not rewrites.** `resolve_link` moved from `browser`
  into `url` (`lnomad/src/url.rs:294`; `browser` re-exports it). A text
  field is `input::Field` (`lnomad/src/app/input.rs:16`): tui-input with
  `std`, and without it `leviculum_ui::input::Line`
  (`leviculum-ui/src/input.rs:291`), a field whose platform brings its
  own editor and hands the text in, as the address field of section 2.1
  says the platform's text field does. The core reads and edits a field
  through `InputBuffer` only, its caret column through `visual_cursor`
  (`leviculum-ui/src/input.rs:260`) and its scroll through
  `visual_scroll` (`leviculum-ui/src/input.rs:272`), both computed as
  tui-input computes them (a test holds them equal over wide
  characters), and a key edits through `InputBuffer::edit`, the
  shared line table, so Alt-< and Alt-> now go to a field's start and
  end as in Loquax's compose. `fit_cells` truncates where it called
  `f64::floor`, which core lacks; for its non-negative products the
  values are the same (`lnomad/src/image.rs:221`). Without a system
  clock the model's wall clock is `Model::clock` or the epoch
  (`lnomad/src/app/model.rs:703`); a board sets it.
- **The check.** A `std::fs` call injected into a temp copy of
  `lnomad/src/app/model.rs` is refused first; then `cargo check -p
  leviculum-micron -p lnomad --lib --no-default-features --target
  thumbv7em-none-eabihf`, and the same through the esp fork with `-Z
  build-std=core,alloc` for `xtensa-esp32s3-none-elf`, the M9's
  ESP32-S3. A host without the fork says SKIP on its own line. 7.4 s
  standing alone after touching `leviculum-core`, so it is in `guards`.
  `just lnomad-core-no-tui` now checks the `std` build without the
  terminal, which is what it always meant.
- **Unchanged.** `cargo test -p lnomad -p leviculum-micron`: lnomad's
  419 library tests and every integration target green, the terminal
  goldens among them; `just lnomad-gui` (465 tests) with the window's
  goldens byte identical.

**The Browse tab, built 2026-11-02 (1337).** Lew decided section 4's
first question as recommended: on the phone and the M9 the browser is a
tab inside Loquax; on a PC lnomad stays its own program, and the Loquax
terminal and window get no tab (the second question, answered no for now).

- **The core hosts lnomad's core.** `loquax` links lnomad without its
  `std` feature, so `just loquax-core-no-std` and `just
  lnomad-core-no-std` still build both for the firmware triples. The tab
  is `Browse` (`loquax/src/ui/browse.rs:38`): whether the host has the tab
  (`enabled`, set by the phone's session from the config, on unless it
  says off, at `loquax-android/src/session.rs:2332`), whether it is in front, and
  lnomad's `Model`, marked `hosted`. `TabId::Browse`
  (`loquax/src/ui/scene.rs:82`) joins Chats and Map where enabled, and the
  scene carries lnomad's scene as `browse` (`loquax/src/ui/scene.rs:1066`;
  `browse_scene`, `loquax/src/ui/scene.rs:1195`). Loquax's `Intent` gains
  one arm, `Browse(lnomad::Intent)`, carried, never a browser verb of its
  own; the model folds `UiEvent::Tab` and `UiEvent::Browse`
  (`BrowseEvent`, `loquax/src/ui/browse.rs:271`): a verb, the page a fetch
  brought as micron (the core parses it, no platform does), an announced
  node, the cell size, the tick.
- **Discovery.** The tab opens on lnomad's start screen with the places
  panel over it, the bookmarks and the nodes the announces named, the
  terminal's list; the 1227 name search and its pinning are lnomad's
  address field as they are. An announce is kept while the tab is off.
- **Chat and page hand over.** A NomadNet page link in a chat
  (`<hash>:/page/...`) opens the tab at that page where the host has the
  tab (`loquax/src/ui.rs:5448`) and is copied for lnomad where not. A
  page's `lxmf@` link raises lnomad's new `Effect::Write`
  (`lnomad/src/app/model.rs:456`) when hosted
  (`lnomad/src/app/event.rs:1658`), and Loquax opens that chat for
  writing (`effects_of`, `loquax/src/ui/browse.rs:477`); the browser's
  quit leaves the tab, not the app. A `nomadnet://` URL in a chat is not
  a link shape `leviculum_ui::link::find_links` knows yet.
- **The phone.** Request `browse` (a verb and a node's resource) and
  query `browse` (`screen`, `loquax-android/src/browser.rs:44`; `event`,
  `loquax-android/src/browser.rs:273`) cross the scene flat for
  `MsgPack.java` in the phone's words: the bar, the address, the page's
  lines of runs with their inks and the node a tap raises, the sheet's
  rows. `BrowsePane`
  (`loquax-android/android/src/network/leviculum/loquax/BrowsePane.java:66`)
  draws the runs as spans in a monospace `TextView`. Java parses no
  micron. Since 1650 the phone takes the page whole, up to 4000 lines
  (lnomad's `Model::window`, set in `session::known`), and its own
  `ScrollView` scrolls it under the finger; the line the finger leaves
  it at goes back as the verb `at`, so a fresh copy of the same page,
  Back and the paging arrows start there. Only a longer page comes as a
  window, and only then are the arrows shown. The bar's `actions`
  (forward, reload, bookmark, identify on and off) are a menu Java draws
  from the list as the library words and enables it, and the `chips` a
  line under the bar, the key before the identity while it identifies;
  the first switch-on says "The page learns your address while this is
  on". The `fields` carry their kind, whether they mask, their value and
  label: a text field opens the platform's editor with its value (a
  password editor when masked), handed in whole as lnomad's
  `Intent::Fill` (`lnomad/src/app/intent.rs:98`); a checkbox or a radio
  is toggled by `activate` alone; the focused field is drawn in the
  accent; a submit carries them all.
- **The fetch** came with pass two, below. The facade test here hands
  the page in as micron, as the fetch does.
- **The M9.** There is no M9 renderer in this tree (Loquax study slice
  11), so the tab ran on no board. Its heap was counted on the host
  (`the_browse_tab_heap_on_the_m9_cells`,
  `loquax-android/tests/browse_census.rs:84`; 64-bit, an upper bound for
  the 32-bit board): the unopened tab 0 B, open on twenty announced nodes
  25.8 KB, the README laid out on 53 by 24 cells 1.29 MB, one scene over
  it 19.3 KB held and 21.2 KB at its peak with 30 runs. The page does not
  fit the 64 KiB placeholder heap of `leviculum-esp`; it needs PSRAM, or
  the layout stores runs instead of cells (slice 4b decides).
- **Tests.** Goldens `loquax/tests/golden/scene-browse-{list,page,form}.json`
  (`the_browse_tab_is_the_golden_scene`, `loquax/tests/scene_golden.rs:405`);
  a core test per verb in `loquax/src/ui/browse.rs` with the host without
  the tab as the control; the facade test
  `the_phone_reads_a_page_through_the_scene`
  (`loquax-android/tests/browse.rs:64`) in Java's bytes; the APK.

**The fetch through the phone's own node, built 2026-11-02 (1337 pass
two).** Slice 3's network seam: one node, lent, not a second one.

- **The seam is a trait in lnomad.** `NodeLend`
  (`lnomad/src/fetch.rs:151`) is what a fetch session needs of a node:
  the node for one call, its next event, and what closing the session
  does to it. `Session<N: NodeLend = OwnNode>` is the one fetch, with
  `Session::over` (`lnomad/src/fetch.rs:326`) for a node a host lends.
  The standalone browser's `OwnNode` (`lnomad/src/fetch.rs:184`) is its
  shared-instance client as before, stopped on close; a host's
  `LentNode` (`lnomad/src/fetch.rs:209`) holds the host's node weakly and
  takes the events the host forwards, and closing the session closes only
  its link. `fetch_page` (`lnomad/src/transfer.rs:103`) and
  `fetch_file` (`lnomad/src/transfer.rs:122`) are the fetch every frontend
  runs, the terminal's tasks included; the phone takes the micron, which
  its core parses.
- **Loquax lends from the processor tap.** `Attached::lend`
  (`loquax/src/engine/shell.rs:580`) hands out the engine's node weakly
  and opens a tap beside the home requests' (`forward_lent`,
  `loquax/src/engine/shell.rs:209`), which passes on what a fetch reads
  and nothing else (`lends`, `loquax/src/engine/shell.rs:101`): its own
  links coming up and closing, the answers and their timeouts, and the
  `nomadnetwork.node` announces. A lend ends when the node is lent again
  or stops; a stop waits up to two seconds for the fetch to let go, since
  stopping takes the node to itself.
- **Which node.** With the mailbox on the phone, its node: the home node
  lends it once it runs (`DaemonOptions::lends`,
  `loquax/src/home/daemon.rs:247`), at every start, a restart included,
  since the screens' leaf reaches nothing past the mailbox, which is no
  transport node. With the mailbox on a computer, the screens' leaf,
  whose rendezvous carries its links on. The fetch identifies with the
  user's identity where the phone holds it (the mailbox here), else with
  the device's.
- **The phone's turn.** `loquax-android/src/browse_fetch.rs` is the task
  the terminal's loop is on a desktop: one fetch in flight, a newer one or
  a cancel superseding it, lnomad's discovery drain beside it. The
  session's pump (`browse`, `loquax-android/src/session.rs:2461`) hands it
  the owed effects and folds back what it says: `BrowseEvent::FailedText`,
  `Identifying`, `Downloaded` and `DownloadFailed` are new
  (`loquax/src/ui/browse.rs:270`). A download is kept under the app's
  files as a bubble's file is, `files/browse/` (`keep_download`,
  `loquax-android/src/session.rs:2523`), and the clipboard text and the
  file go to Java once with the next screen; `BrowsePane` writes the
  clipboard and opens the file, or the share sheet takes it. Announces
  reach the list from the session's start, the tab shown or not.
- **The phone's start page** is one sentence over the places list, no
  key named (`lnomad/src/app/event.rs:370`, English and German).
- **On by default since 1618.** The browser is Explore's way in: on
  unless the config's `browse_tab` says off
  (`loquax-android/src/session.rs:2332`). It was held back, off in the
  APK, until the fetch had been seen on a device; the walk of 17024c3a
  found Explore listing every page and opening none.
- **Measured.** From the tap on a node's row to the first screen that
  shows the page, the Reticulum README (23,490 bytes, a response
  Resource) on the host: 302, 286 and 291 ms through the phone's mailbox
  node, 282 ms through the leaf over a shared instance's transport node
  (`loquax-android/tests/browse_fetch.rs:266` and
  `loquax-android/tests/browse_fetch.rs:322`). The heap census
  again: the unopened tab 0 B, open on twenty nodes 13.4 KB (the shorter
  start page), the README laid out 1.30 MB, one scene 19.3 KB held and
  21.2 KB at its peak.
- **Still not on the phone** (1650): inline pictures, an external link
  opened, in-page search and the history list. Built since: the whole
  page under the finger, form fields of every kind with the platform's
  editor, identify on and off with its state, bookmark, forward and
  reload in the bar's menu.
- **Tests.** `a_lent_node_fetches_as_its_own_does_and_stays_its_hosts`
  (`lnomad/tests/fetch_integ.rs:302`, the control a node let go of);
  `a_lent_fetch_hears_its_own_links_and_its_answers_alone`
  (`loquax/src/engine/shell.rs:1021`, the controls a link a peer opened
  and an event no fetch reads); `the_fetchs_words_reach_the_browser`
  (`loquax/src/ui/browse.rs:936`) and
  `the_tab_opens_on_the_phones_start_page`
  (`loquax/src/ui/browse.rs:1026`); the facade end to end, both
  nodes, with the node absent before its announce as the control. With
  the tap silenced the leaf's run goes red in 60 s with no node listed.

**The tab on the desktop, built 2026-11-03 (1369).** The all-targets
rule (Lew, 2026-11-02) reopened section 4's second question: where the
tab is technically possible it moves, and on a PC it is. The standalone
`lnomad` stays as it is (its suite is the control); the window and the
terminal of Loquax get the tab beside it, behind the same `browse_tab`
as the phone, on by default since 1618.

- **One fetch, three hosts.** The phone's task moved from the Android
  crate into Loquax (`loquax/src/browse_fetch.rs`, feature `browse`,
  which `tui`, `gui` and the phone's build carry). It gained a page's
  pictures (`Ask::Images`, `loquax/src/browse_fetch.rs:266`, lnomad's
  `spawn_image_queue`; `Shot`, `loquax/src/browse_fetch.rs:79`). The
  desktop shell lends the node it attached to its daemon
  (`Browsing::start`, `loquax/src/browsing.rs:120`), the identity a
  node is shown on identify being the one that node runs on, as on the
  phone. With the tab off nothing is lent.
- **The effects.** `Browsing::effect` (`loquax/src/browsing.rs:142`):
  the network's to the fetch, the clipboard to the terminal's OSC 52 and
  the window's clipboard, a link out of NomadNet to the system's handler,
  a download into `$XDG_DOWNLOAD_DIR` or `~/Downloads`
  (`lnomad::download::download_dir`, the standalone's; the 1341 portal is
  a chooser and offers no download folder, so the tab saves where the
  desktop's own download folder is, `keep`, `loquax/src/browsing.rs:288`),
  the bookmarks and pins to their files. A write and a quit stay the
  core's (`effects_of`).
- **The keys.** While the tab is in front the core hands keys and the
  mouse to the browser (`loquax/src/ui.rs:3932`, `loquax/src/ui.rs:3987`),
  so lnomad's whole key table works, its `q` leaves the tab, and the
  terminal's screen size is the page's (`loquax/src/ui.rs:3985`). `B` and
  the palette's `browse` bring the tab to the front
  (`loquax/src/keys.rs:643`, `browse::open`, `loquax/src/ui/browse.rs:453`);
  with the tab off they say how to switch it on.
- **The terminal** draws the tab with lnomad's terminal view over the
  whole screen (`draw_screen`, `loquax/src/tui.rs:1714`), in the
  messenger's theme and colour depth (`loquax/src/tui.rs:2259`). It asks
  for no pictures (`no_images`): the map owns the terminal's picture
  protocol, and two pickers on one terminal is a later question.
- **The window** draws it with lnomad's window view beside the rail
  (`browse_view`, `loquax/src/gui.rs:3158`; `lnomad::gui::view`,
  `lnomad/src/gui.rs:681`). The pictures are lnomad's window code too:
  `Pictures` (`lnomad/src/gui.rs:272`), moved out of lnomad's `Window`
  so a host keeps it beside a model it hosts; the shell holds it and the
  window draws from it. What lnomad's view says is folded as lnomad's
  window folds it (`App::browse`, `loquax/src/gui.rs:2219`, with
  `wheel_mice` and `pointer_mouse`, `lnomad/src/gui.rs:433` and
  `lnomad/src/gui.rs:433`, and `type_address`,
  `lnomad/src/app/event.rs:546`, which lnomad's own window now calls as
  well). The window tells the browser its light or dark and the page's
  cells (`browse_sync`, `loquax/src/gui.rs:2105`). The tab's icon
  borrows the search glass; the shared icons have no globe yet.
- **What the standalone has that the tab lacked (1337's list), and what
  the desktop tab has now.** Inline pictures: the window, yes; the
  terminal, no (above); the phone, no. An external link opened: the
  window and the terminal, yes. Bookmarks and pins kept past the
  session: the window and the terminal, yes, in
  `browse-bookmarks.toml` and `browse-pins.toml` under Loquax's home
  (`set_up`, `loquax/src/browsing.rs:44`), the tab's own and not the
  standalone's; files beside the store rather than tables in it, since
  lnomad's own load and save are the code that keeps them. The help
  overlay: the window and the terminal, yes, lnomad's own, drawn by
  lnomad's views. In-page search: the window and the terminal, yes, its
  `/` reaches the browser (`the_tab_in_front_has_the_keys_and_the_mouse`).
  The history list: as the standalone. The phone keeps 1337's list.
- **Still open.** The hosted start page is the phone's one sentence ("Tap
  a node..."), since `hosted` both picks it and makes an `lxmf@` link
  write; on a desktop it should name keys. The toast's age counts the
  shell's 250 ms tick, not lnomad's 100 ms one, so a toast stays longer.
  The window's narrow class takes the tab bar as 72 pixels high for the
  page's cells.
- **Tests.** The core: keys, mouse, search and quit in front with the
  tab behind as the control, the terminal's size with a host without
  the tab as the control, the palette's `browse` both ways, the window's
  field and theme (`loquax/src/ui/browse.rs`). The shell's effects and
  what the fetch brings, with the tab off and no download folder as the
  controls (`loquax/src/browsing.rs`). The goldens: the terminal's list
  and page (`loquax/tests/golden/tui-browse-{list,page}.txt`,
  `the_browse_tab_is_lnomads_terminal_screen`) and the window's list and
  a page with a picture (`loquax/tests/golden/gui-browse-{list,page}.png`,
  `the_browse_tab_is_lnomads_window_beside_the_rail`). lnomad's suite
  and `just lnomad-gui` unchanged, its window goldens byte identical.

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

