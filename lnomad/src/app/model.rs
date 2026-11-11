//! The browser state: [`Model`], its parts, the effects [`update`] hands the
//! shell, and the pure queries over them (which links are on screen, where the
//! hints go).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[cfg(feature = "std")]
use std::path::PathBuf;

use unicode_width::UnicodeWidthChar;

use leviculum_micron::{FieldKind, MicronDocument};

use crate::bookmarks::Bookmarks;
use crate::color::{ColorDepth, Tier};
use crate::discovery::NomadNodeRegistry;
use crate::image::{ImageSlot, SlotState};
use crate::names::{PinStore, SearchHit};
use crate::page_cache::PageCache;
use crate::render::{
    insert_image_rows, layout_blocks, layout_blocks_wrapping, FieldValue, RLine, RenderedField,
    RenderedLink,
};
use crate::theme::Theme;
use crate::url::Target;
pub(crate) use leviculum_ui::text::truncate_to_cols;
use leviculum_ui::tokens::{tokens, Tokens};

use super::chrome::*;
use super::event::*;
use super::geometry::{regions, Rect};
use super::input::{field_with, visual_scroll, Field, InputBuffer};
use super::strings::{Lang, Text};

/// The number of columns reserved on the right for the scrollbar.
pub(crate) const SCROLLBAR_COLS: u16 = 1;
/// How many lines one mouse-wheel notch scrolls.
pub(crate) const WHEEL_STEP: usize = 3;
/// Rows in the fixed top-bar. A single identity row: the page title, the
/// address, and a right-aligned status cluster.
pub(crate) const TOPBAR_ROWS: u16 = 1;
/// Rows in the fixed status bar.
pub(crate) const STATUS_ROWS: u16 = 1;
/// Total chrome rows: top-bar plus status bar. The content viewport is the
/// terminal height minus this.
pub(crate) const CHROME_ROWS: u16 = TOPBAR_ROWS + STATUS_ROWS;
/// Columns of blank gap between the top-bar's left identity (title / address)
/// and its right-aligned status cluster.
pub(crate) const CTRL_GAP: u16 = 2;
/// Spinner animation cadence while a fetch is in flight, in milliseconds. Also
/// the cadence at which an idle toast is aged towards its expiry.
pub(crate) const SPINNER_TICK_MS: u64 = 120;
/// How long a toast stays up before auto-dismissing, in milliseconds.
pub(crate) const TOAST_LIFETIME_MS: u64 = 4000;
/// Toast lifetime expressed in animation ticks (`TOAST_LIFETIME_MS` over the
/// tick cadence), so expiry is a pure count of ticks.
pub(crate) const TOAST_TICKS: u64 = TOAST_LIFETIME_MS / SPINNER_TICK_MS;

/// The status-cluster glyph shown when the current page is bookmarked, and when
/// it is not. The star is a click target that toggles the bookmark.
pub(crate) const STAR_ON: &str = "★";
pub(crate) const STAR_OFF: &str = "☆";

/// The subtle right-aligned status-cluster glyph shown when the current page was
/// served from the in-RAM page cache rather than a fresh fetch.
pub(crate) const CACHED_GLYPH: &str = "⚡";

/// The status-cluster glyph shown (with the fingerprint prefix) while the
/// session identifies to the current page's node. An ordinary wide emoji (no
/// VS16), so `UnicodeWidthStr` scores it 2 columns like the terminal draws it.
pub(crate) const IDENTIFY_GLYPH: &str = "\u{1F511}";

/// The home-row alphabet hint labels are drawn from (vimium-style).
pub(crate) const HINT_ALPHABET: [char; 9] = ['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l'];

/// The content layout width for a terminal `cols` wide: full width minus the
/// scrollbar column, never below 1 so wrapping stays well defined.
pub(crate) fn content_width(cols: u16) -> usize {
    (cols.saturating_sub(SCROLLBAR_COLS) as usize).max(1)
}

/// The content viewport height for a terminal `rows` tall: the height left after
/// the fixed top-bar and status bar.
pub(crate) fn content_height(rows: u16) -> usize {
    rows.saturating_sub(CHROME_ROWS) as usize
}

/// A vertical scroll motion, resolved against the current viewport height in
/// [`Model::apply_scroll`]. Bound to both vi and emacs keys plus the wheel; the
/// keys are the shared scroll rows of [`leviculum_ui::keymap::SCROLL_ROWS`].
pub use leviculum_ui::keymap::ScrollCmd;

/// The single scroll rule shared by every scrollable window — the content body
/// and both overlays (help, places), and any future overlay; it lives in
/// `leviculum-ui` because loquax's overlays scroll by it too.
pub use leviculum_ui::keymap::scrolled;

/// The interactive UI mode: normal browsing or entering an address.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Scrolling and navigation keys are live.
    #[default]
    Browse,
    /// The top-bar address editor has focus.
    Address,
    /// Hint mode (`f`): each visible link and top-bar control wears a label the
    /// reader types to jump to it.
    Hint,
    /// In-page search (`/`): a one-line query editor; committing highlights all
    /// matches and jumps to the first.
    Search,
    /// A form field has focus and is being edited: typing edits a text field,
    /// Space toggles a checkbox/radio, Tab/Shift-Tab move to the next/previous
    /// interactive element, Esc returns to browsing.
    Field,
}

/// A focusable interactive element on the page: a link or a form field, keyed by
/// its 1-based index in its own kind's space. The Tab focus cycle walks links and
/// fields together in document order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// The page link with this 1-based index.
    Link(usize),
    /// The form field with this 1-based index.
    Field(usize),
}

/// A form field currently on screen, with its screen rectangle. Produced by
/// [`visible_fields`] and consumed by hit-testing, focus and the input overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleField {
    /// The field's 1-based [`RenderedField::index`].
    pub index: usize,
    /// The field name (form key).
    pub name: String,
    /// The field kind (text, checkbox, radio).
    pub kind: FieldKind,
    /// The screen row (absolute, below the top-bar) the field sits on.
    pub row: u16,
    /// The first screen column of the field widget.
    pub col_start: u16,
    /// One past the last screen column of the field widget.
    pub col_end: u16,
}

/// The editable state of one form field: the source of truth for what the reader
/// has typed or toggled. Persists across a re-layout (resize / theme toggle);
/// rebuilt only when a new page loads.
#[derive(Clone, Debug)]
pub struct FieldEdit {
    /// The field name (submitted as `field_<name>`).
    pub name: String,
    /// The field kind.
    pub kind: FieldKind,
    /// The text editor for a text field (unused for checkbox/radio).
    pub input: Field,
    /// The current checked state of a checkbox/radio (unused for text).
    pub checked: bool,
    /// The submit value of a checkbox/radio: its explicit value, or its label
    /// when none was given (unused for text; a text field submits its editor
    /// contents).
    pub value: String,
}

/// One in-page search hit: a column span on a single laid-out page line. Column
/// indices are 0-based DISPLAY columns (the screen column the renderer draws the
/// cell at), not character indices: a wide char (emoji, CJK) advances two
/// columns, so `col_start`/`col_end` line up with the cells ratatui actually
/// paints in the content body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    /// 0-based index of the laid-out [`RLine`] the match sits on.
    pub line_idx: usize,
    /// 0-based first matched display column on that line.
    pub col_start: usize,
    /// 0-based display column one past the last matched column
    /// (`col_start..col_end`).
    pub col_end: usize,
}

/// Every case-insensitive substring match of `query` over the rendered page
/// text (the [`RLine`] cells, i.e. exactly what the reader sees, post-wrap).
///
/// Matching is per line and non-overlapping: after a hit the scan resumes at
/// its end, so adjacent hits (`aa` in `aaaa` -> two matches) are found but
/// overlapping ones (`aa` starting one column apart) are not. An empty query
/// yields no matches. Comparison is case-insensitive per character while keeping
/// a strict one-cell-per-column mapping, so the returned rects line up with the
/// laid-out cells regardless of case.
pub fn find_matches(page: &[RLine], query: &str) -> Vec<Match> {
    let needle: Vec<char> = query.chars().collect();
    let n = needle.len();
    if n == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (line_idx, line) in page.iter().enumerate() {
        let hay: Vec<char> = line.cells.iter().map(|c| c.ch).collect();
        if hay.len() < n {
            continue;
        }
        // `col_at[i]` is the DISPLAY column where char `i` starts; `col_at[len]`
        // is the column one past the line. Matching stays char-based, but the
        // emitted spans are display columns so they align with the renderer.
        let mut col_at = Vec::with_capacity(hay.len() + 1);
        let mut col = 0usize;
        for ch in &hay {
            col_at.push(col);
            col += UnicodeWidthChar::width(*ch).unwrap_or(0);
        }
        col_at.push(col);
        let mut i = 0;
        while i + n <= hay.len() {
            if (0..n).all(|k| chars_eq_ci(hay[i + k], needle[k])) {
                out.push(Match {
                    line_idx,
                    col_start: col_at[i],
                    col_end: col_at[i + n],
                });
                i += n;
            } else {
                i += 1;
            }
        }
    }
    out
}

/// Case-insensitive equality of two characters, without changing the column
/// count (each side stays one character), so match columns keep aligning to
/// laid-out cells.
pub(crate) fn chars_eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// One of the clickable top-bar controls, recorded with its rect so mouse
/// clicks and hint labels can be hit-tested against it. The navigation trio
/// (back / forward / reload) lives in the footer, not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// The address slot: a click opens the address editor.
    Address,
    /// The bookmark star: a click toggles the current page's bookmark.
    Bookmark,
}

/// A top-bar control's screen rectangle, for mouse hit-testing in a later phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlRect {
    /// Which control this rectangle belongs to.
    pub control: Control,
    /// Where it sits on screen.
    pub rect: Rect,
}

/// A link that is currently on screen, with its screen rectangle. Produced by
/// [`visible_links`] and consumed by hit-testing, hint labelling and focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibleLink {
    /// The link's 1-based [`RenderedLink::index`](crate::render::RenderedLink).
    pub index: usize,
    /// The screen row (absolute, below the top-bar) the link sits on.
    pub row: u16,
    /// The first screen column of the clickable span.
    pub col_start: u16,
    /// One past the last screen column of the clickable span.
    pub col_end: u16,
}

/// What a hint label jumps to: a page link, a form field or a top-bar control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintTarget {
    /// The page link with this 1-based index.
    Link(usize),
    /// The form field with this 1-based index.
    Field(usize),
    /// A top-bar control.
    Control(Control),
}

/// A hint badge: its typed label, what it activates, and where it renders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    /// The label the reader types to activate this hint.
    pub label: String,
    /// What activating the hint does.
    pub target: HintTarget,
    /// The screen row the badge renders on.
    pub row: u16,
    /// The first screen column the badge renders at.
    pub col: u16,
}

/// The browsing history: a linear stack of visited targets and a cursor into it.
///
/// [`visit`](History::visit) truncates any forward entries (a new navigation
/// from the middle discards the forward branch) and pushes. [`back`](History::back)
/// and [`forward`](History::forward) move the cursor and return the target now
/// under it, so the shell can re-fetch it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    /// Visited targets in order.
    pub stack: Vec<Target>,
    /// Index of the current entry in `stack` (meaningful only when non-empty).
    pub idx: usize,
}

impl History {
    /// Record a fresh navigation: drop any forward entries, then push `target`
    /// and point the cursor at it.
    pub fn visit(&mut self, target: Target) {
        if self.stack.is_empty() {
            self.stack.push(target);
            self.idx = 0;
        } else {
            self.stack.truncate(self.idx + 1);
            self.stack.push(target);
            self.idx = self.stack.len() - 1;
        }
    }

    /// Move the cursor to an existing index (a back/forward navigation that has
    /// now loaded). Clamped to the stack; a no-op on an empty stack.
    pub fn goto(&mut self, idx: usize) {
        if !self.stack.is_empty() {
            self.idx = idx.min(self.stack.len() - 1);
        }
    }

    /// The target currently under the cursor, if any.
    pub fn current(&self) -> Option<&Target> {
        self.stack.get(self.idx)
    }

    /// Whether a back step is possible.
    pub fn can_back(&self) -> bool {
        self.idx > 0
    }

    /// Whether a forward step is possible.
    pub fn can_forward(&self) -> bool {
        self.idx + 1 < self.stack.len()
    }

    /// Step back and return the target now under the cursor, or `None` at the
    /// start.
    pub fn back(&mut self) -> Option<&Target> {
        if self.can_back() {
            self.idx -= 1;
            self.stack.get(self.idx)
        } else {
            None
        }
    }

    /// Step forward and return the target now under the cursor, or `None` at the
    /// end.
    pub fn forward(&mut self) -> Option<&Target> {
        if self.can_forward() {
            self.idx += 1;
            self.stack.get(self.idx)
        } else {
            None
        }
    }
}

/// How a completed navigation should update the history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryAction {
    /// A fresh navigation: [`History::visit`] the target on success.
    Push,
    /// A back/forward/reload navigation: [`History::goto`] this index on success.
    Goto(usize),
}

/// One inline image to fetch: which link it belongs to and where it lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRequest {
    /// The 1-based link index, which every image event refers back to.
    pub link: usize,
    /// The file target to download.
    pub target: Target,
}

/// A navigation in flight: the target being fetched and how history records it
/// once the page loads.
#[derive(Clone, Debug)]
pub struct Pending {
    /// The target being fetched.
    pub target: Target,
    /// How to fold it into history on success.
    pub action: HistoryAction,
}

/// A side effect [`update`] asks the IO shell to perform. Keeping these out of
/// `update` lets navigation logic be unit-tested without any IO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Start (or restart) a fetch for this target.
    Navigate(Target),
    /// Download this `/file/` target: the IO shell fetches the raw bytes,
    /// saves them into the download directory, and folds the outcome back as
    /// [`AppEvent::DownloadDone`] / [`AppEvent::DownloadFailed`]. A download is
    /// not a navigation: the page and history stay put.
    Download(Target),
    /// Open this external URL in the user's default handler (the IO shell runs
    /// `open::that`). Only whitelisted safe schemes ever reach this effect.
    OpenExternal(String),
    /// Fetch the current page's inline images, in this order. The IO shell
    /// works the list one at a time — a link carries one transfer at a time
    /// anyway — and folds each outcome back as an image event. The page is
    /// already on screen when this starts, so nothing waits for a picture.
    /// Starting one drops whatever image queue was running, as do
    /// [`Navigate`](Effect::Navigate) and [`Cancel`](Effect::Cancel): the
    /// transfers still to come belong to a page nobody is looking at.
    FetchImages(Vec<ImageRequest>),
    /// Save an already-fetched inline image to the download directory, with no
    /// second transfer.
    SaveImage {
        /// The link index whose picture to save.
        link: usize,
    },
    /// Save an already-fetched inline image and hand the file to the platform
    /// handler (`xdg-open` on Linux).
    OpenImage {
        /// The link index whose picture to open.
        link: usize,
    },
    /// Cancel the in-flight fetch and stay on the current page.
    Cancel,
    /// Copy this text to the system clipboard (the IO shell writes it as an
    /// OSC 52 sequence to the terminal).
    Copy(String),
    /// Persist the model's bookmarks to disk (the IO shell writes the TOML).
    SaveBookmarks,
    /// Persist the model's pins to disk (the IO shell writes the TOML).
    SavePins,
    /// Turn identifying to `dest` on or off (the IO shell calls
    /// [`Session::set_identify`](crate::fetch::Session::set_identify), which
    /// persists the decision and closes the reused link). Always emitted
    /// BEFORE the accompanying reload's [`Navigate`](Effect::Navigate), so the
    /// fresh link is built under the new decision.
    SetIdentify {
        /// The destination the decision applies to.
        dest: [u8; 16],
        /// `true` to identify, `false` to go anonymous.
        on: bool,
    },
    /// Write to this `lxmf@` address (32 lowercase hex): a page's link to
    /// its operator, followed where a messenger hosts the browser
    /// ([`Model::hosted`]). The standalone browser copies it instead.
    Write(String),
    /// Quit the UI.
    Quit,
}

pub use leviculum_ui::toast::{Toast, ToastKind};

/// The complete UI state. Pure data: [`update`] is the only thing that mutates
/// it, and it never performs IO.
///
/// The model owns the parsed `doc` and its current layout `width` so a resize
/// can re-wrap: [`relayout`](Model::relayout) recomputes `page`/`links` from the
/// stored document at a new width.
#[derive(Clone, Debug, Default)]
pub struct Model {
    /// The parsed document, kept so a resize can re-wrap it to a new width.
    pub doc: MicronDocument,
    /// The width the page is currently laid out at.
    pub width: usize,
    /// The page laid out into target-agnostic styled lines.
    pub page: Vec<RLine>,
    /// The page's links, with their laid-out positions for hit-testing.
    pub links: Vec<RenderedLink>,
    /// The title shown in the top-bar (node name or short hash, plus path).
    pub title: String,
    /// The last known terminal size, as `(cols, rows)`.
    pub size: (u16, u16),
    /// Index of the top visible line in `page`. Always clamped to the page.
    pub scroll: usize,
    /// The browsing history and its cursor.
    pub history: History,
    /// The destination currently displayed, for the same-destination URL form.
    pub current_dest: Option<[u8; 16]>,
    /// A navigation in flight, or `None` when idle. `Some` means "loading".
    pub pending: Option<Pending>,
    /// A file download in flight: the filename being fetched, or `None` when
    /// idle. Drives the loading indicator like [`pending`](Model::pending)
    /// does, but its completion raises a toast instead of replacing the page.
    pub download: Option<String>,
    /// The `#anchor` the in-flight navigation should scroll to once its page
    /// loads, or `None`. Kept beside `pending` so the anchor survives the async
    /// round-trip and is resolved against the freshly-loaded page's lines.
    pub pending_anchor: Option<String>,
    /// The current interaction mode (browse, address entry, hint, or search).
    pub mode: Mode,
    /// The address-bar editor.
    pub input: Field,
    /// The in-page search query editor (`/`).
    pub search_input: Field,
    /// The current query's matches over the laid-out page, highlighted while set.
    pub matches: Vec<Match>,
    /// Index into `matches` of the current match (the one scrolled to and drawn
    /// with the stronger highlight), or `None` when there is no current match.
    pub current_match: Option<usize>,
    /// Block index -> first laid-out line, so a `#anchor` (stored as a block
    /// index) resolves to a page line. Recomputed on every relayout.
    pub block_lines: Vec<usize>,
    /// The link the focus cursor is on (Tab navigation), 1-based, or `None`.
    /// Mutually exclusive with [`field_focus`](Model::field_focus): at most one
    /// interactive element is focused at a time.
    pub focus: Option<usize>,
    /// The form field's laid-out positions and metadata, refreshed on every
    /// relayout from the parsed document and the current `field_state`.
    pub field_defs: Vec<RenderedField>,
    /// The editable field store (typed text / toggled state), the source of
    /// truth for rendering and submission. Indexed by a field's 1-based index
    /// minus one; preserved across a relayout, rebuilt on a page load.
    pub field_state: Vec<FieldEdit>,
    /// The form field the focus cursor is on (Tab navigation / a click), 1-based,
    /// or `None`. Set exactly when [`mode`](Model::mode) is [`Mode::Field`].
    pub field_focus: Option<usize>,
    /// The link the mouse is hovering over, 1-based, or `None`.
    pub hover: Option<usize>,
    /// The footer button the mouse is hovering over, as an index into the current
    /// mode's footer item list, or `None`. Highlights that button so it reads as
    /// clickable. Mutually exclusive with a content-link [`hover`](Model::hover).
    pub footer_hover: Option<usize>,
    /// The characters typed so far in hint mode, narrowing the visible labels.
    pub hint_input: String,
    /// Whether a messenger hosts the browser (the Browse tab of Loquax on
    /// the phone and the M9): an `lxmf@` link then asks the host to write
    /// ([`Effect::Write`]) rather than copying the address.
    pub hosted: bool,
    /// A host's name for the page the browser shows or loads (1621, the
    /// name Loquax's Explore lists it by): set, the bar's title, the
    /// address field while it is not edited and the loading line say it
    /// instead of the page's own title, its hash and its path. `None` in
    /// lnomad's own windows.
    pub label: Option<String>,
    /// The language the browser speaks: its catalogue's
    /// ([`super::strings`]) English unless the shell names another.
    pub lang: Lang,
    /// What the active toast says, as the catalogue's words: the scene's
    /// toast. [`toast`](Model::toast) holds the same words resolved in
    /// [`lang`](Model::lang), which is what the terminal draws.
    pub toast_text: Option<Text>,
    /// The active transient toast (fetch error, "copied", "bookmarked", ...), or
    /// `None`. Rendered as a floating overlay, not in the status bar; auto-expires
    /// and is cleared on the next key event. See [`Model::set_toast`].
    pub toast: Option<Toast>,
    /// A monotonic tick counter, advanced once per animation [`AppEvent::Tick`].
    /// The toast's age is measured against it, so expiry is testable by advancing
    /// this counter without any real time passing.
    pub now_tick: u64,
    /// The loading spinner's animation tick: advances once per redraw while a
    /// fetch is in flight, driving both the circling glyph and the shimmering
    /// rainbow hue. See `spinner_span`.
    pub spin: usize,
    /// Whether the keybinding help overlay is shown.
    pub show_help: bool,
    /// Whether colour is suppressed (NO_COLOR / non-tty): the chrome bars fall
    /// back to reverse video instead of a coloured background.
    pub no_color: bool,
    /// The terminal colour depth. Under [`ColorDepth::Ansi256`] the rendered
    /// buffer's 24-bit RGB colours are downgraded to the nearest xterm-256 index
    /// in a single post-pass at the end of [`view`](crate::tui::view); [`ColorDepth::Truecolor`]
    /// leaves them as authored. Subordinate to [`no_color`](Model::no_color),
    /// which suppresses the downgrade along with all colour.
    pub depth: ColorDepth,
    /// The active light/dark theme: the accents baked into `page` and the chrome
    /// colours the view draws. Toggled at runtime with `t`.
    pub theme: Theme,
    /// NomadNet nodes discovered from announces seen on the session's event
    /// stream, folded in from [`AppEvent::NodeDiscovered`]. Listed in the places
    /// panel's "Discovered nodes" section.
    pub node_registry: NomadNodeRegistry,
    /// The persisted bookmarks, loaded at startup and saved on change.
    pub bookmarks: Bookmarks,
    /// Where the bookmarks are persisted, or `None` when no config dir is
    /// resolvable (the browser then runs without persistence).
    #[cfg(feature = "std")]
    pub bookmarks_path: Option<PathBuf>,
    /// The pinned `name -> hash` pairs ([`crate::names`]), loaded at startup
    /// and saved on change.
    pub pins: PinStore,
    /// Where the pins are persisted, or `None` when no config dir is
    /// resolvable (pins then last for the session only).
    #[cfg(feature = "std")]
    pub pins_path: Option<PathBuf>,
    /// The open name-search result list, or `None`. Modal like the places
    /// panel: it owns the keys until closed.
    pub names: Option<NameResults>,
    /// A fixed wall clock in Unix seconds for tests and for a board's own
    /// clock; `None` reads the system clock ([`Model::wall_now`]). Only pins
    /// and the result list's "last seen" read it.
    pub clock: Option<u64>,
    /// Whether the places panel (bookmarks + discovered nodes) overlay is shown.
    pub show_places: bool,
    /// The selected row in the places panel, an index into [`places`].
    pub places_sel: usize,
    /// The places overlay's VIEW offset: the first entry-line shown when the
    /// panel is taller than the terminal. Distinct from `places_sel` (the
    /// selection); the view is scrolled the minimum needed to keep the selected
    /// row visible. Reset to 0 when the panel opens.
    pub places_scroll: usize,
    /// The places-panel entry the mouse is hovering over, an index into
    /// [`places`], or `None`. Highlights that row so it reads as clickable; kept
    /// distinct from the selection (`places_sel`) so the two highlights differ.
    pub places_hover: Option<usize>,
    /// The one-slot undo stash: the bookmark most recently deleted from the
    /// places panel, restored by `u` (`undo_delete_bookmark`). A new deletion
    /// overwrites it (only one level of undo).
    pub last_deleted_bookmark: Option<DeletedBookmark>,
    /// The help overlay's VIEW offset: the first help line shown when the help
    /// is taller than the terminal. Reset to 0 when the help opens.
    pub help_scroll: usize,
    /// The in-RAM LRU cache of recently viewed pages, keyed by fetch target. A
    /// revisit (including back/forward) renders instantly from here; a reload
    /// bypasses it and form submits are never stored. See [`PageCache`].
    pub page_cache: PageCache,
    /// Whether the page currently shown was served from `page_cache` rather
    /// than a fresh fetch. Drives the subtle "cached" top-bar marker; cleared on
    /// the next fresh load.
    pub cached_view: bool,
    /// lnomad's own identity fingerprint as lowercase hex. Stable for the whole
    /// session, so the IO shell sets it ONCE at model construction from
    /// [`Session::fingerprint_hex`](crate::fetch::Session::fingerprint_hex).
    /// Empty by default (unit tests set it directly), in which case the top-bar
    /// identity marker stays hidden.
    pub fingerprint: String,
    /// Whether the session identifies to [`current_dest`](Model::current_dest).
    /// The `i` toggle flips it optimistically so the UI updates immediately;
    /// the IO shell's refresh after each fresh load is authoritative, and a
    /// cache-served navigation restores it from `identify_known`
    /// (Model::identify_known). Drives the top-bar marker and the footer label.
    pub identifying: bool,
    /// The destinations the model has learned the session identifies to: every
    /// toggle and every fresh load's authoritative answer is folded in via
    /// [`note_identify`](Model::note_identify). A cache-served navigation (which
    /// asks the session nothing) reads [`identifying`](Model::identifying) from
    /// here; the page cache only holds pages fetched this run, so the answer was
    /// always learned first.
    pub identify_known: alloc::collections::BTreeSet<[u8; 16]>,
    /// A host that has said once what identifying means (1650): the first
    /// switch-on in a hosted browser says the sentence a reader without the
    /// help screen needs, every later one the usual toast.
    pub identify_explained: bool,
    /// The lines the scene carries for a host that scrolls the page itself
    /// (1650, the phone's native scroll): the whole page while it has no
    /// more lines than this, else a window this tall around
    /// [`scroll`](Model::scroll). `None`, the terminal's rule: the
    /// viewport's lines from `scroll`. [`scroll`](Model::scroll) keeps its
    /// meaning either way, the top line the reader sees.
    pub window: Option<usize>,
    /// A host that scrolls the page sideways as well (1667, the phone): a
    /// line of art without a space stays whole past the width instead of
    /// breaking into pieces. `false`, the terminal's rule: every line
    /// fits the width.
    pub sideways: bool,
    /// The current page's inline images, one slot per standalone image link,
    /// in link order. Rebuilt on every page load; drives both the rows the
    /// relayout reserves and what is drawn into them.
    pub images: Vec<ImageSlot>,
    /// Whether inline images are suppressed (`--images off`). Named like
    /// [`no_color`](Model::no_color), and for the same reason: the default of
    /// a `bool` field is the ordinary behaviour, not the opt-out. With it set
    /// an image link stays an ordinary link, which is exactly what NomadNet
    /// shows.
    pub no_images: bool,
    /// Set once the user has asked to quit; the IO loop breaks on it.
    pub quit: bool,
}

/// A name search's result list ([`crate::names::search`]): what was typed,
/// the hits in display order, the selected row, and whether the selected hit's
/// "same name, other key" question is being asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameResults {
    /// The query as typed.
    pub query: String,
    /// The hits, pinned first and conflicting last.
    pub hits: Vec<SearchHit>,
    /// The selected row, an index into `hits`.
    pub sel: usize,
    /// Whether the overlay is asking to confirm opening (and pinning) the
    /// selected hit, whose name is pinned to another hash.
    pub confirm: bool,
}

impl Model {
    /// The wall clock in Unix seconds: [`clock`](Model::clock) when set, else
    /// the system time. Without `std` there is no system time: a board sets
    /// the clock from its own, and until it does pins read the epoch.
    pub fn wall_now(&self) -> u64 {
        #[cfg(feature = "std")]
        let now = self.clock.unwrap_or_else(crate::discovery::now_unix_secs);
        #[cfg(not(feature = "std"))]
        let now = self.clock.unwrap_or(0);
        now
    }

    /// Lay a parsed document out at `width` and build a model from it, keeping
    /// the document so a later resize can re-wrap it.
    pub fn from_document(
        doc: &MicronDocument,
        width: usize,
        title: impl Into<String>,
        size: (u16, u16),
    ) -> Self {
        let theme = Theme::default();
        let (page, links, block_lines, field_defs) =
            layout_blocks(doc, width, theme, &[] as &[FieldValue]);
        let mut model = Self {
            doc: doc.clone(),
            width,
            page,
            links,
            block_lines,
            field_defs,
            title: title.into(),
            size,
            theme,
            ..Self::default()
        };
        model.rebuild_field_state();
        model
    }

    /// Rebuild the editable field store from the freshly laid-out field defs
    /// (their parsed prefill). Called on construction and on every page load;
    /// NOT on a resize/theme relayout, where the store must be preserved.
    pub fn rebuild_field_state(&mut self) {
        self.field_state = self
            .field_defs
            .iter()
            .map(|f| FieldEdit {
                name: f.name.clone(),
                kind: f.kind,
                input: field_with(&f.value),
                checked: f.checked,
                value: f.value.clone(),
            })
            .collect();
    }

    /// The current field values, for feeding a relayout so the laid-out widgets
    /// reflect what the reader has typed / toggled. Positionally indexed by a
    /// field's 1-based index minus one.
    pub(crate) fn field_values(&self) -> Vec<FieldValue> {
        self.field_state
            .iter()
            .enumerate()
            .map(|(i, fe)| {
                // Slide the text window so the caret stays inside the box: a
                // message longer than the field must still be typeable. The
                // widths come from the previous layout and never change between
                // relayouts, so reading them here is sound.
                let w = self.field_defs.get(i).map_or(1, |d| d.width.max(1));
                FieldValue {
                    text: InputBuffer::value(&fe.input).to_string(),
                    checked: fe.checked,
                    scroll: visual_scroll(&fe.input, w.saturating_sub(1)),
                }
            })
            .collect()
    }

    /// Re-wrap the stored document to `width` under the current theme, replacing
    /// `page`/`links`. The caller is responsible for re-clamping `scroll`
    /// afterwards. Also used to re-lay-out in place after a theme toggle.
    pub fn relayout(&mut self, width: usize) {
        self.width = width;
        let values = self.field_values();
        let (mut page, mut links, mut block_lines, mut field_defs) =
            layout_blocks_wrapping(&self.doc, width, self.theme, &values, self.sideways);
        // Inline pictures are reserved AFTER the text is laid out: their height
        // is not known until the image has arrived and been decoded, which is
        // long after the page first appeared.
        let reserved: Vec<(usize, u16)> = self
            .images
            .iter()
            .map(|slot| (slot.link, slot.state.rows()))
            .collect();
        insert_image_rows(
            &mut page,
            &mut links,
            &mut field_defs,
            &mut block_lines,
            &reserved,
        );
        self.page = page;
        self.links = links;
        self.block_lines = block_lines;
        self.field_defs = field_defs;
    }

    /// The slot for a link index, if that link is an inline picture.
    pub fn image_slot(&self, link: usize) -> Option<&ImageSlot> {
        self.images.iter().find(|slot| slot.link == link)
    }

    /// Fold an image state change in and re-lay the page out, since the rows a
    /// slot reserves depend on its state.
    pub(crate) fn set_image_state(&mut self, link: usize, state: SlotState) {
        let Some(slot) = self.images.iter_mut().find(|slot| slot.link == link) else {
            return;
        };
        if slot.state == state {
            return;
        }
        slot.state = state;
        let (w, vp) = (self.width, self.viewport());
        self.relayout(w);
        self.clamp_scroll(vp);
    }

    /// The colour tokens the chrome reads (`leviculum_ui::tokens`): the tier
    /// is the depth, or monochrome under [`no_color`](Model::no_color).
    pub fn tokens(&self) -> &'static Tokens {
        tokens(Tier::of(self.no_color, self.depth), self.theme)
    }

    /// Toggle between the dark and light theme, re-laying the page out so the
    /// new accents take effect and re-clamping the scroll offset.
    pub fn toggle_theme(&mut self) {
        self.theme = self.theme.toggle();
        let (w, vp) = (self.width, self.viewport());
        self.relayout(w);
        self.clamp_scroll(vp);
    }

    /// The number of page lines visible at once: the terminal height minus the
    /// fixed top-bar and status bar.
    pub fn viewport(&self) -> usize {
        content_height(self.size.1)
    }

    /// Whether a fetch or a file download is currently in flight.
    pub fn is_loading(&self) -> bool {
        self.pending.is_some() || self.download.is_some()
    }

    /// Show a transient toast, stamped with the current tick so it can expire.
    /// Every transient message routes through here instead of the status bar.
    pub fn set_toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.say(kind, Text::Raw(text.into()));
    }

    /// Show a toast in the catalogue's words, resolved in the model's
    /// language for the terminal and kept as keys for the scene.
    pub fn say(&mut self, kind: ToastKind, text: Text) {
        self.toast = Some(Toast {
            kind,
            text: text.resolve(self.lang),
            shown_at: self.now_tick,
        });
        self.toast_text = Some(text);
    }

    /// Dismiss any active toast (a navigation or key event clears it early).
    pub fn dismiss_toast(&mut self) {
        self.toast = None;
        self.toast_text = None;
    }

    /// Record whether the session identifies to `dest`: fold it into
    /// [`identify_known`](Model::identify_known) and, when `dest` is the current
    /// page's node, refresh the display flag. Called by the `i` toggle and by
    /// the IO shell with each fresh load's authoritative answer.
    pub fn note_identify(&mut self, dest: [u8; 16], on: bool) {
        if on {
            self.identify_known.insert(dest);
        } else {
            self.identify_known.remove(&dest);
        }
        if self.current_dest == Some(dest) {
            self.identifying = on;
        }
    }

    /// Advance the toast towards expiry: clear it once it has been up for at
    /// least [`TOAST_TICKS`] ticks. Called on every animation tick.
    pub(crate) fn expire_toast(&mut self) {
        if let Some(toast) = &self.toast {
            if self.now_tick.saturating_sub(toast.shown_at) >= TOAST_TICKS {
                self.dismiss_toast();
            }
        }
    }

    /// Whether the UI needs the periodic tick: a fetch spinner is animating, or a
    /// toast is up and must age towards its auto-dismiss.
    pub fn needs_tick(&self) -> bool {
        self.is_loading() || self.toast.is_some()
    }

    /// The largest valid `scroll` for a given viewport: the last position where
    /// the final page line still sits at the bottom of the viewport.
    pub fn max_scroll(&self, viewport: usize) -> usize {
        self.page.len().saturating_sub(viewport)
    }

    /// Clamp `scroll` into `[0, max_scroll(viewport)]` (e.g. after a re-wrap).
    pub fn clamp_scroll(&mut self, viewport: usize) {
        self.scroll = self.scroll.min(self.max_scroll(viewport));
    }

    /// Apply a scroll command against `viewport`, clamped to the page. Never
    /// under- or overflows, even when the page is shorter than the viewport.
    pub fn apply_scroll(&mut self, cmd: ScrollCmd, viewport: usize) {
        let max = self.max_scroll(viewport);
        self.scroll = scrolled(self.scroll, cmd, viewport, max);
    }

    /// Stash the current page's scroll offset into its cache entry (when the
    /// page is cached), so a later revisit restores where the reader was. A
    /// no-op when the current target is not cached (e.g. a form-submit result,
    /// or the very first page still loading).
    pub fn stash_scroll(&mut self) {
        if let Some(target) = self.history.current().cloned() {
            self.page_cache.set_scroll(&target, self.scroll);
        }
    }

    /// Scroll down by `n` lines (mouse wheel), clamped to the page.
    pub fn scroll_lines_down(&mut self, n: usize, viewport: usize) {
        self.scroll = self.scroll.saturating_add(n).min(self.max_scroll(viewport));
    }

    /// Scroll up by `n` lines (mouse wheel), clamped at the top.
    pub fn scroll_lines_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// The frame rectangle implied by the model's last known size, so the pure
    /// layout helpers ([`visible_links`], [`hints`]) can run without a live frame.
    pub(crate) fn frame_area(&self) -> Rect {
        Rect::new(0, 0, self.size.0, self.size.1)
    }
}

/// The links currently on screen, each with its absolute screen rectangle.
///
/// A link on content line `link.line` is visible when
/// `scroll <= link.line < scroll + viewport`; it then sits at screen row
/// `TOPBAR_ROWS + (link.line - scroll)`, columns `[col_start, col_end)` (the
/// content body starts at column 0). Off-viewport links are excluded. Pure, so
/// hit-testing, hints and focus all share one source of truth.
pub fn visible_links(model: &Model) -> Vec<VisibleLink> {
    let viewport = model.viewport();
    let scroll = model.scroll;
    let mut out = Vec::new();
    for link in &model.links {
        if link.line < scroll || link.line >= scroll + viewport {
            continue;
        }
        let row = TOPBAR_ROWS + (link.line - scroll) as u16;
        out.push(VisibleLink {
            index: link.index,
            row,
            col_start: link.col_start as u16,
            col_end: link.col_end as u16,
        });
    }
    out
}

/// The form fields currently on screen, each with its absolute screen rectangle,
/// mirroring [`visible_links`]. A field on content line `field.line` is visible
/// when `scroll <= field.line < scroll + viewport`; it then sits at screen row
/// `TOPBAR_ROWS + (field.line - scroll)`, columns `[col_start, col_end)`. Off-
/// viewport fields are excluded. Pure, so hit-testing, focus and the input
/// overlay share one source of truth.
pub fn visible_fields(model: &Model) -> Vec<VisibleField> {
    let viewport = model.viewport();
    let scroll = model.scroll;
    let mut out = Vec::new();
    for field in &model.field_defs {
        if field.line < scroll || field.line >= scroll + viewport {
            continue;
        }
        let row = TOPBAR_ROWS + (field.line - scroll) as u16;
        out.push(VisibleField {
            index: field.index,
            name: field.name.clone(),
            kind: field.kind,
            row,
            col_start: field.col_start as u16,
            col_end: field.col_end as u16,
        });
    }
    out
}

/// Resolve an anchor name to the page line to scroll to: the first laid-out
/// [`RLine`] of the block the anchor marks. The parser records anchors as block
/// indices (`doc.anchors`); [`Model::block_lines`] maps a block index to its
/// first line. `None` when the anchor is unknown or its block laid out no line.
pub fn anchor_line(model: &Model, anchor: &str) -> Option<usize> {
    let block = *model.doc.anchors.get(anchor)?;
    model.block_lines.get(block).copied()
}

/// Generate `n` unique hint labels from the home-row alphabet: single characters
/// while they suffice, two-character combinations once there are more targets
/// than the alphabet holds.
pub fn hint_labels(n: usize) -> Vec<String> {
    if n <= HINT_ALPHABET.len() {
        return HINT_ALPHABET
            .iter()
            .take(n)
            .map(|c| c.to_string())
            .collect();
    }
    let mut out = Vec::with_capacity(n);
    'outer: for &a in &HINT_ALPHABET {
        for &b in &HINT_ALPHABET {
            out.push(format!("{a}{b}"));
            if out.len() == n {
                break 'outer;
            }
        }
    }
    out
}

/// The hint badges for the current frame: one per visible link (in reading
/// order), then one per visible form field, then one per top-bar control, each
/// wearing a unique label.
pub fn hints(model: &Model) -> Vec<Hint> {
    let [topbar, _content, _status] = regions(model.frame_area());
    let mut slots: Vec<(HintTarget, u16, u16)> = Vec::new();
    for vl in visible_links(model) {
        slots.push((HintTarget::Link(vl.index), vl.row, vl.col_start));
    }
    for vf in visible_fields(model) {
        slots.push((HintTarget::Field(vf.index), vf.row, vf.col_start));
    }
    for cr in top_bar_controls(model, topbar) {
        slots.push((HintTarget::Control(cr.control), cr.rect.y, cr.rect.x));
    }
    hint_labels(slots.len())
        .into_iter()
        .zip(slots)
        .map(|(label, (target, row, col))| Hint {
            label,
            target,
            row,
            col,
        })
        .collect()
}

/// Whether `hint` matches the typed `input`: its label starts with the prefix,
/// or (for a link) its visible text contains the input as a case-insensitive
/// substring. `input` is assumed already lowercased.
pub(crate) fn hint_matches(model: &Model, hint: &Hint, input: &str) -> bool {
    if hint.label.starts_with(input) {
        return true;
    }
    if let HintTarget::Link(idx) = hint.target {
        if let Some(link) = model.links.iter().find(|l| l.index == idx) {
            return link.label.to_lowercase().contains(input);
        }
    }
    false
}
