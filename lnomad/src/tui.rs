//! The ratatui/crossterm terminal UI: the testable core and the thin IO shell.
//!
//! The design is TEA (The Elm Architecture) so the interesting logic is pure
//! and driven from headless tests:
//!
//! - [`Model`] is the whole UI state (the laid-out page, its links, the title,
//!   the browsing history, the address editor, the terminal size, a quit flag).
//! - [`update`] folds an [`AppEvent`] into the model and returns a list of
//!   [`Effect`]s for the IO shell to run. It does no IO, so a test feeds it
//!   events and inspects the model and the returned effects directly. Navigation
//!   is expressed purely: an address entry or a history motion yields
//!   [`Effect::Navigate`]; a cancel yields [`Effect::Cancel`]. A completed fetch
//!   arrives back as [`AppEvent::PageLoaded`] / [`AppEvent::LoadFailed`], also
//!   folded by `update`.
//! - [`view`] draws the model into a ratatui [`Frame`]: a two-row top-bar (page
//!   title + back/forward/reload/address controls), the scrollable content, and
//!   a one-row status bar. A test renders it into a
//!   [`ratatui::backend::TestBackend`] and asserts on the resulting buffer.
//! - [`run_tui`] is the only part that touches a real terminal and the network.
//!   It owns the [`Session`], runs the initial fetch and every subsequent
//!   navigation as a background task whose result rejoins the `tokio::select!`
//!   loop, so the UI stays live (spinner animates, keys and the address input
//!   work) while a page loads. A slow or failed fetch never blocks the UI; a
//!   cancel drops the in-flight task and returns to the current page.
//!
//! Phase 3 adds the top-bar, the history stack, async navigation with a loading
//! spinner and `Esc`/`Ctrl-g` cancel, and a `tui-input` address editor.
//!
//! Phase 4 adds the navigation layer over the rendered page: [`visible_links`]
//! maps each on-screen link to a screen rectangle, reused by Tab focus (with
//! auto-scroll), `f` hint mode (vimium-style labels over links and controls),
//! left-click hit-testing, and mouse-hover. The focused or hovered link's target
//! shows in the status bar. Links no longer carry a visible `[N]` marker.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as RtLine, Span as RtSpan, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::{Frame, Terminal};
use tokio::sync::{mpsc, Mutex};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use leviculum_micron::MicronDocument;

use crate::bookmarks::{Bookmark, Bookmarks};
use crate::browser::{self, BrowserOptions};
use crate::discovery::{DiscoveredNode, NomadNodeRegistry};
use crate::fetch::Session;
use crate::page_cache::{CacheEntry, PageCache};
use crate::render::{layout_blocks, FieldValue, RLine, RStyle, RenderedField, RenderedLink};
use crate::theme::{resolve_theme, Bg, Theme, ThemeFlag};
use crate::url::{classify_link, parse_url, LinkKind, Target, DEFAULT_PATH};
use leviculum_micron::FieldKind;

/// The number of columns reserved on the right for the scrollbar.
const SCROLLBAR_COLS: u16 = 1;
/// How many lines one mouse-wheel notch scrolls.
const WHEEL_STEP: usize = 3;
/// Rows in the fixed top-bar (title row + controls row).
const TOPBAR_ROWS: u16 = 2;
/// Rows in the fixed status bar.
const STATUS_ROWS: u16 = 1;
/// Total chrome rows: top-bar plus status bar. The content viewport is the
/// terminal height minus this.
const CHROME_ROWS: u16 = TOPBAR_ROWS + STATUS_ROWS;
/// Columns of blank gap between top-bar controls.
const CTRL_GAP: u16 = 2;
/// Spinner animation cadence while a fetch is in flight, in milliseconds. Also
/// the cadence at which an idle toast is aged towards its expiry.
const SPINNER_TICK_MS: u64 = 120;
/// How long a toast stays up before auto-dismissing, in milliseconds.
const TOAST_LIFETIME_MS: u64 = 4000;
/// Toast lifetime expressed in animation ticks (`TOAST_LIFETIME_MS` over the
/// tick cadence), so expiry is a pure count of ticks.
const TOAST_TICKS: u64 = TOAST_LIFETIME_MS / SPINNER_TICK_MS;

/// The label for each of the three fixed top-bar controls.
const BACK_LABEL: &str = "‹ back";
const FORWARD_LABEL: &str = "forward ›";
const RELOAD_LABEL: &str = "⟳ reload";

/// The subtle right-aligned top-bar marker shown when the current page was
/// served from the in-RAM page cache rather than a fresh fetch.
const CACHED_LABEL: &str = "⚡ cached ";

/// Browse-mode status hints. A curated subset that fits an 80-column status bar;
/// the full keybinding list lives in the `?` help overlay.
const BROWSE_HINTS: &str =
    "j/k scroll  f hint  / search  d places  m mark  y copy  : addr  ? help  q quit";
/// Address-mode status hints.
const ADDRESS_HINTS: &str = "Enter: go   Esc: cancel";
/// Hint-mode status hints.
const HINT_HINTS: &str = "type a hint label or link text   Esc: cancel";
/// Search-mode status hints (shown when the query line has no room of its own).
const SEARCH_HINTS: &str = "Enter: search   n/N: next/prev   Esc: cancel";
/// Field-edit-mode status hints.
const FIELD_HINTS: &str = "type to edit   Space: toggle   Tab: next field/link   Esc: done";

/// The home-row alphabet hint labels are drawn from (vimium-style).
const HINT_ALPHABET: [char; 9] = ['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l'];

/// A ratatui [`Color`] from a theme RGB triple.
fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// The style the fixed top-bar and status bar are filled with, full width, so
/// they stand out from the content. With colour, the theme's chrome background
/// and foreground (a muted slate on dark, a light bar on light); under
/// `no_color`, the REVERSE modifier so the bars still delineate without any
/// colour. Shared by both bars for a consistent look.
fn chrome_style(no_color: bool, theme: Theme) -> Style {
    if no_color {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
            .fg(rgb(theme.chrome_fg()))
            .bg(rgb(theme.chrome_bg()))
    }
}

/// The foreground for the bright chrome text drawn over the bars: the status-bar
/// key-hints and the AVAILABLE top-bar controls. Uses the theme's bright chrome
/// foreground (~4.5:1 on the chrome background) instead of `Modifier::DIM`,
/// which halved the foreground and read as too low contrast. Under `no_color`
/// it returns the plain style so the reverse-video chrome fill shows through.
fn chrome_text_style(no_color: bool, theme: Theme) -> Style {
    if no_color {
        Style::default()
    } else {
        Style::default().fg(rgb(theme.chrome_fg()))
    }
}

/// The foreground for UNAVAILABLE top-bar controls: the theme's muted chrome
/// foreground, a distinct-but-readable grey (~3:1 on the chrome background) that
/// reads as "disabled" without the illegibility of `Modifier::DIM`. Under
/// `no_color` it returns the plain style so the reverse-video fill shows
/// through (no DIM under reverse).
fn chrome_muted_style(no_color: bool, theme: Theme) -> Style {
    if no_color {
        Style::default()
    } else {
        Style::default().fg(rgb(theme.chrome_muted_fg()))
    }
}

/// The content layout width for a terminal `cols` wide: full width minus the
/// scrollbar column, never below 1 so wrapping stays well defined.
fn content_width(cols: u16) -> usize {
    (cols.saturating_sub(SCROLLBAR_COLS) as usize).max(1)
}

/// The content viewport height for a terminal `rows` tall: the height left after
/// the fixed top-bar and status bar.
fn content_height(rows: u16) -> usize {
    rows.saturating_sub(CHROME_ROWS) as usize
}

/// Truncate `s` to at most `cols` display columns, unicode-width-aware, ending
/// with a `…` when anything is cut. The returned string's display width never
/// exceeds `cols`, so it fits an overlay of `cols` inner columns without
/// spilling over the border. A zero budget yields an empty string.
fn truncate_to_cols(s: &str, cols: usize) -> String {
    if cols == 0 {
        return String::new();
    }
    let total: usize = s
        .chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum();
    if total <= cols {
        return s.to_string();
    }
    // Reserve one column for the ellipsis, then take as many full characters as
    // fit. A wide char that would straddle the budget is dropped whole, so the
    // result never exceeds `cols`.
    let budget = cols - 1;
    let mut out = String::new();
    let mut width = 0usize;
    for c in s.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if width + w > budget {
            break;
        }
        out.push(c);
        width += w;
    }
    out.push('…');
    out
}

/// Split the frame into the three fixed regions: top-bar, content, status.
fn regions(area: Rect) -> [Rect; 3] {
    let parts = Layout::vertical([
        Constraint::Length(TOPBAR_ROWS),
        Constraint::Min(0),
        Constraint::Length(STATUS_ROWS),
    ])
    .split(area);
    [parts[0], parts[1], parts[2]]
}

/// A vertical scroll motion, resolved against the current viewport height in
/// [`Model::apply_scroll`]. Bound to both vi and emacs keys plus the wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollCmd {
    /// Up one line.
    LineUp,
    /// Down one line.
    LineDown,
    /// Up half a viewport.
    HalfPageUp,
    /// Down half a viewport.
    HalfPageDown,
    /// Up one full viewport.
    PageUp,
    /// Down one full viewport.
    PageDown,
    /// To the very top.
    Top,
    /// To the very bottom.
    Bottom,
}

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
    pub input: Input,
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
fn chars_eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// One of the clickable top-bar controls, recorded with its rect so a later
/// phase can hit-test mouse clicks against it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// Go back in history.
    Back,
    /// Go forward in history.
    Forward,
    /// Reload the current page.
    Reload,
    /// The address / breadcrumb slot.
    Address,
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

/// What a hint label jumps to: a page link or a top-bar control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintTarget {
    /// The page link with this 1-based index.
    Link(usize),
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
#[derive(Clone, Debug, Default)]
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
    /// Open this external URL in the user's default handler (the IO shell runs
    /// `open::that`). Only whitelisted safe schemes ever reach this effect.
    OpenExternal(String),
    /// Cancel the in-flight fetch and stay on the current page.
    Cancel,
    /// Copy this text to the system clipboard (the IO shell writes it as an
    /// OSC 52 sequence to the terminal).
    Copy(String),
    /// Persist the model's bookmarks to disk (the IO shell writes the TOML).
    SaveBookmarks,
    /// Quit the UI.
    Quit,
}

/// Whether a transient toast carries an error or a neutral/success note. Drives
/// the toast's accent colour and glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    /// A failure the reader should notice (a bad link, a failed fetch): drawn in
    /// an attention colour with a `⚠` prefix.
    Error,
    /// A neutral confirmation (copied, bookmarked, cancelled): drawn in a calm
    /// colour with a `✓` prefix.
    Info,
}

/// A transient, auto-dismissing notification floated over the content. Replaces
/// the old sticky status-bar messages, leaving the status bar for the key-hints
/// (or the loading spinner during a fetch).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    /// Whether this is an error or a neutral note.
    pub kind: ToastKind,
    /// The message text; carries the offending target for error toasts so it is
    /// self-describing.
    pub text: String,
    /// The [`Model::now_tick`] value when the toast was shown, so its age (and
    /// thus expiry) is a pure function of the monotonic tick counter and needs no
    /// wall clock to unit-test.
    pub shown_at: u64,
}

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
    /// The `#anchor` the in-flight navigation should scroll to once its page
    /// loads, or `None`. Kept beside `pending` so the anchor survives the async
    /// round-trip and is resolved against the freshly-loaded page's lines.
    pub pending_anchor: Option<String>,
    /// The current interaction mode (browse, address entry, hint, or search).
    pub mode: Mode,
    /// The address-bar editor.
    pub input: Input,
    /// The in-page search query editor (`/`).
    pub search_input: Input,
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
    /// relayout from the parsed document and the current [`field_state`].
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
    /// The characters typed so far in hint mode, narrowing the visible labels.
    pub hint_input: String,
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
    /// rainbow hue. See [`spinner_span`].
    pub spin: usize,
    /// Whether the keybinding help overlay is shown.
    pub show_help: bool,
    /// Whether colour is suppressed (NO_COLOR / non-tty): the chrome bars fall
    /// back to reverse video instead of a coloured background.
    pub no_color: bool,
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
    pub bookmarks_path: Option<PathBuf>,
    /// Whether the places panel (bookmarks + discovered nodes) overlay is shown.
    pub show_places: bool,
    /// The selected row in the places panel, an index into [`places`].
    pub places_sel: usize,
    /// The in-RAM LRU cache of recently viewed pages, keyed by fetch target. A
    /// revisit (including back/forward) renders instantly from here; a reload
    /// bypasses it and form submits are never stored. See [`PageCache`].
    pub page_cache: PageCache,
    /// Whether the page currently shown was served from [`page_cache`] rather
    /// than a fresh fetch. Drives the subtle "cached" top-bar marker; cleared on
    /// the next fresh load.
    pub cached_view: bool,
    /// Set once the user has asked to quit; the IO loop breaks on it.
    pub quit: bool,
}

impl Model {
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
                input: Input::new(f.value.clone()),
                checked: f.checked,
                value: f.value.clone(),
            })
            .collect();
    }

    /// The current field values, for feeding a relayout so the laid-out widgets
    /// reflect what the reader has typed / toggled. Positionally indexed by a
    /// field's 1-based index minus one.
    fn field_values(&self) -> Vec<FieldValue> {
        self.field_state
            .iter()
            .map(|fe| FieldValue {
                text: fe.input.value().to_string(),
                checked: fe.checked,
            })
            .collect()
    }

    /// Re-wrap the stored document to `width` under the current theme, replacing
    /// `page`/`links`. The caller is responsible for re-clamping `scroll`
    /// afterwards. Also used to re-lay-out in place after a theme toggle.
    pub fn relayout(&mut self, width: usize) {
        self.width = width;
        let values = self.field_values();
        let (page, links, block_lines, field_defs) =
            layout_blocks(&self.doc, width, self.theme, &values);
        self.page = page;
        self.links = links;
        self.block_lines = block_lines;
        self.field_defs = field_defs;
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

    /// Whether a fetch is currently in flight.
    pub fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    /// Show a transient toast, stamped with the current tick so it can expire.
    /// Every transient message routes through here instead of the status bar.
    pub fn set_toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toast = Some(Toast {
            kind,
            text: text.into(),
            shown_at: self.now_tick,
        });
    }

    /// Dismiss any active toast (a navigation or key event clears it early).
    pub fn dismiss_toast(&mut self) {
        self.toast = None;
    }

    /// Advance the toast towards expiry: clear it once it has been up for at
    /// least [`TOAST_TICKS`] ticks. Called on every animation tick.
    fn expire_toast(&mut self) {
        if let Some(toast) = &self.toast {
            if self.now_tick.saturating_sub(toast.shown_at) >= TOAST_TICKS {
                self.toast = None;
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
        let vp = viewport.max(1);
        let half = (vp / 2).max(1);
        self.scroll = match cmd {
            ScrollCmd::LineUp => self.scroll.saturating_sub(1),
            ScrollCmd::LineDown => self.scroll.saturating_add(1),
            ScrollCmd::HalfPageUp => self.scroll.saturating_sub(half),
            ScrollCmd::HalfPageDown => self.scroll.saturating_add(half),
            ScrollCmd::PageUp => self.scroll.saturating_sub(vp),
            ScrollCmd::PageDown => self.scroll.saturating_add(vp),
            ScrollCmd::Top => 0,
            ScrollCmd::Bottom => max,
        }
        .min(max);
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
    fn frame_area(&self) -> Rect {
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
/// order) then one per top-bar control, each wearing a unique label.
pub fn hints(model: &Model) -> Vec<Hint> {
    let [topbar, _content, _status] = regions(model.frame_area());
    let mut slots: Vec<(HintTarget, u16, u16)> = Vec::new();
    for vl in visible_links(model) {
        slots.push((HintTarget::Link(vl.index), vl.row, vl.col_start));
    }
    for cr in top_bar_controls(topbar) {
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
fn hint_matches(model: &Model, hint: &Hint, input: &str) -> bool {
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

/// A UI input event, decoupled from crossterm so [`update`] is trivially
/// testable and the event source can be swapped later.
#[derive(Clone, Debug)]
pub enum AppEvent {
    /// A key was pressed.
    Key(KeyEvent),
    /// A mouse event occurred (wheel scrolling, left-click to follow a link or
    /// activate a top-bar control, and move-to-hover).
    Mouse(MouseEvent),
    /// The terminal was resized to `(cols, rows)`.
    Resize(u16, u16),
    /// An explicit request to quit.
    Quit,
    /// A navigation completed: the parsed document and its resolved title.
    PageLoaded {
        /// The freshly parsed page.
        doc: MicronDocument,
        /// The resolved top-bar title for it.
        title: String,
    },
    /// A navigation failed with a human-readable message.
    LoadFailed(String),
    /// A NomadNet node announce was seen on the session's event stream; fold it
    /// into the model's discovered-node registry.
    NodeDiscovered(DiscoveredNode),
    /// The spinner animation tick, delivered while a fetch is in flight.
    Tick,
}

/// Fold an event into the model, returning any [`Effect`]s for the IO shell.
/// Pure and IO-free.
pub fn update(model: &mut Model, event: AppEvent) -> Vec<Effect> {
    match event {
        AppEvent::Quit => {
            model.quit = true;
            vec![Effect::Quit]
        }
        AppEvent::Tick => {
            model.spin = model.spin.wrapping_add(1);
            model.now_tick = model.now_tick.wrapping_add(1);
            model.expire_toast();
            Vec::new()
        }
        AppEvent::PageLoaded { doc, title } => {
            apply_loaded(model, doc, title);
            Vec::new()
        }
        AppEvent::LoadFailed(msg) => {
            model.pending = None;
            model.set_toast(ToastKind::Error, msg);
            Vec::new()
        }
        AppEvent::NodeDiscovered(node) => {
            model.node_registry.upsert_node(&node);
            Vec::new()
        }
        AppEvent::Resize(cols, rows) => {
            model.size = (cols, rows);
            model.relayout(content_width(cols));
            let vp = model.viewport();
            model.clamp_scroll(vp);
            Vec::new()
        }
        AppEvent::Mouse(mouse) => {
            let vp = model.viewport();
            match mouse.kind {
                MouseEventKind::ScrollDown => {
                    model.scroll_lines_down(WHEEL_STEP, vp);
                    Vec::new()
                }
                MouseEventKind::ScrollUp => {
                    model.scroll_lines_up(WHEEL_STEP);
                    Vec::new()
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    handle_click(model, mouse.column, mouse.row)
                }
                // Mouse side buttons: Back (button 8) and Forward (button 9)
                // drive the same history navigation as Alt-Left / Alt-Right,
                // and are a no-op when there is no history in that direction.
                MouseEventKind::Down(MouseButton::Back) => go_back(model),
                MouseEventKind::Down(MouseButton::Forward) => go_forward(model),
                MouseEventKind::Moved => {
                    update_hover(model, mouse.column, mouse.row);
                    Vec::new()
                }
                _ => Vec::new(),
            }
        }
        AppEvent::Key(key) => update_key(model, key),
    }
}

/// Fold a completed page load: replace the document, relayout, reset scroll, set
/// the title, and record the navigation in history per its pending action.
fn apply_loaded(model: &mut Model, doc: MicronDocument, title: String) {
    let pending = model.pending.take();
    let anchor = model.pending_anchor.take();
    // A fresh fetch, not a cache hit: clear the "cached" marker.
    model.cached_view = false;
    model.doc = doc;
    // A fresh document replaces the old fields: clear the store so the relayout
    // lays them out from the new prefill, then rebuild the editable store from it.
    model.field_state.clear();
    model.relayout(content_width(model.size.0));
    model.rebuild_field_state();
    model.scroll = 0;
    model.title = title;
    model.dismiss_toast();
    // A fresh page invalidates any focus/hover cursor into the old link set, and
    // any search match highlights against the old page.
    model.focus = None;
    model.field_focus = None;
    model.mode = Mode::Browse;
    model.hover = None;
    model.matches.clear();
    model.current_match = None;
    let loaded_target = pending.map(|pending| {
        model.current_dest = Some(pending.target.dest_hash);
        match pending.action {
            HistoryAction::Push => model.history.visit(pending.target.clone()),
            HistoryAction::Goto(idx) => model.history.goto(idx),
        }
        pending.target
    });
    // A followed `#anchor` scrolls its block's first line to the top; an unknown
    // anchor falls back to the top of the page with a toast note.
    if let Some(name) = anchor {
        match anchor_line(model, &name) {
            Some(line) => {
                let vp = model.viewport();
                model.scroll = line.min(model.max_scroll(vp));
            }
            None => model.set_toast(ToastKind::Error, format!("anchor #{name} not found")),
        }
    }
    // Cache the freshly loaded page (overwriting any prior entry for this target,
    // as a reload does) so a later revisit renders instantly. A non-idempotent
    // form submit is never cached.
    if let Some(target) = loaded_target {
        if !is_form_submit(&target) {
            model.page_cache.insert(
                target,
                CacheEntry {
                    doc: model.doc.clone(),
                    title: model.title.clone(),
                    scroll: model.scroll,
                },
            );
        }
    }
}

/// Split a trailing `#anchor` off a target's path, returning the anchor-free
/// target and the anchor name (when non-empty). The initial URL keeps its
/// `#anchor` inside `path` after [`parse_url`](crate::url::parse_url); stripping
/// it here keeps the fetched path clean and lets the load handler scroll to it.
fn split_path_anchor(mut target: Target) -> (Target, Option<String>) {
    let split = target
        .path
        .split_once('#')
        .map(|(base, anchor)| (base.to_string(), anchor.to_string()));
    match split {
        Some((base, anchor)) if !anchor.is_empty() => {
            target.path = base;
            (target, Some(anchor))
        }
        _ => (target, None),
    }
}

/// Fold a key press, routed by mode. Returns any effects.
fn update_key(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // Any key dismisses the current toast; a handler below may raise a fresh one
    // for this very key (e.g. `m` -> "bookmarked"), which then wins.
    model.dismiss_toast();

    // Ctrl-C quits from any mode.
    if ctrl && key.code == KeyCode::Char('c') {
        model.quit = true;
        return vec![Effect::Quit];
    }

    // The help overlay swallows keys until dismissed.
    if model.show_help {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
        ) {
            model.show_help = false;
        }
        return Vec::new();
    }

    // The places panel is modal like the help overlay: it owns keys until closed.
    if model.show_places {
        return update_places_key(model, key);
    }

    match model.mode {
        Mode::Address => update_address_key(model, key),
        Mode::Hint => update_hint_key(model, key),
        Mode::Search => update_search_key(model, key),
        Mode::Field => update_field_key(model, key),
        Mode::Browse => update_browse_key(model, key, ctrl),
    }
}

/// Fold a key while a form field has focus. Tab/Shift-Tab move to the next/
/// previous interactive element; Esc leaves editing back to browse. A text field
/// takes the usual editing keys (typing, Backspace/Delete, Left/Right, Home/End)
/// and re-lays the page out so its widget reflects the new value; a checkbox/
/// radio toggles on Space (a radio deselects its group siblings). Any other key
/// is ignored, so field editing never leaks into browse hotkeys.
fn update_field_key(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Tab => {
            focus_next(model);
            return Vec::new();
        }
        KeyCode::BackTab => {
            focus_prev(model);
            return Vec::new();
        }
        KeyCode::Esc => {
            model.field_focus = None;
            model.mode = Mode::Browse;
            return Vec::new();
        }
        _ => {}
    }

    let Some(fi) = model.field_focus else {
        model.mode = Mode::Browse;
        return Vec::new();
    };
    let Some(fe) = model.field_state.get(fi - 1) else {
        return Vec::new();
    };

    match fe.kind {
        FieldKind::Text => {
            if matches!(
                key.code,
                KeyCode::Char(_)
                    | KeyCode::Backspace
                    | KeyCode::Delete
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Home
                    | KeyCode::End
            ) {
                if let Some(fe) = model.field_state.get_mut(fi - 1) {
                    fe.input.handle_event(&Event::Key(key));
                }
                // The widget text changed width: re-lay the page out so the box
                // and every downstream position stay correct, keeping the scroll.
                let (w, vp) = (model.width, model.viewport());
                model.relayout(w);
                model.clamp_scroll(vp);
            }
        }
        FieldKind::Checkbox => {
            if key.code == KeyCode::Char(' ') {
                if let Some(fe) = model.field_state.get_mut(fi - 1) {
                    fe.checked = !fe.checked;
                }
                let (w, vp) = (model.width, model.viewport());
                model.relayout(w);
                model.clamp_scroll(vp);
            }
        }
        FieldKind::Radio => {
            if key.code == KeyCode::Char(' ') {
                select_radio(model, fi);
                let (w, vp) = (model.width, model.viewport());
                model.relayout(w);
                model.clamp_scroll(vp);
            }
        }
    }
    Vec::new()
}

/// Select radio field `fi` (1-based), deselecting every other radio sharing its
/// name (one selection per group), matching the reference radio-group behaviour.
fn select_radio(model: &mut Model, fi: usize) {
    let Some(name) = model.field_state.get(fi - 1).map(|fe| fe.name.clone()) else {
        return;
    };
    for fe in model.field_state.iter_mut() {
        if fe.kind == FieldKind::Radio && fe.name == name {
            fe.checked = false;
        }
    }
    if let Some(fe) = model.field_state.get_mut(fi - 1) {
        fe.checked = true;
    }
}

/// Fold a key while the in-page search editor has focus. `Enter` commits the
/// query (compute matches, jump to the first); `Esc` cancels and clears all
/// highlights; any other key edits the query.
fn update_search_key(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Esc => {
            exit_search(model);
            Vec::new()
        }
        KeyCode::Enter => {
            let query = model.search_input.value().to_string();
            model.mode = Mode::Browse;
            commit_search(model, &query);
            Vec::new()
        }
        _ => {
            model.search_input.handle_event(&Event::Key(key));
            Vec::new()
        }
    }
}

/// Fold a key while hint mode is active. A character narrows the visible labels
/// (by label prefix or link-text substring); a unique match follows the link or
/// activates the control. `Esc` exits; a non-matching key is ignored.
fn update_hint_key(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Esc => {
            exit_hint(model);
            Vec::new()
        }
        KeyCode::Backspace => {
            model.hint_input.pop();
            Vec::new()
        }
        KeyCode::Char(c) => {
            let mut candidate = model.hint_input.clone();
            candidate.push(c.to_ascii_lowercase());
            let all = hints(model);
            // An exact label match activates immediately (handles single-char
            // labels, where the label is also its own prefix).
            if let Some(hint) = all.iter().find(|h| h.label == candidate) {
                let target = hint.target;
                exit_hint(model);
                return activate_hint_target(model, target);
            }
            let matches: Vec<&Hint> = all
                .iter()
                .filter(|h| hint_matches(model, h, &candidate))
                .collect();
            match matches.as_slice() {
                // A key that matches nothing is ignored (not accepted).
                [] => Vec::new(),
                [only] => {
                    let target = only.target;
                    exit_hint(model);
                    activate_hint_target(model, target)
                }
                _ => {
                    model.hint_input = candidate;
                    Vec::new()
                }
            }
        }
        _ => Vec::new(),
    }
}

/// Fold a key while the address editor has focus.
fn update_address_key(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Esc => {
            model.mode = Mode::Browse;
            model.input.reset();
            model.dismiss_toast();
            Vec::new()
        }
        KeyCode::Enter => {
            let raw = model.input.value().trim().to_string();
            match parse_url(&raw, model.current_dest) {
                Ok(target) => {
                    model.mode = Mode::Browse;
                    model.input.reset();
                    begin_navigation(model, target, HistoryAction::Push, None)
                }
                Err(err) => {
                    model.set_toast(ToastKind::Error, format!("bad URL: {raw} ({err})"));
                    Vec::new()
                }
            }
        }
        _ => {
            model.input.handle_event(&Event::Key(key));
            Vec::new()
        }
    }
}

/// Fold a key while browsing (scrolling, navigation, mode switches).
fn update_browse_key(model: &mut Model, key: KeyEvent, ctrl: bool) -> Vec<Effect> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if key.code == KeyCode::Char('q') {
        model.quit = true;
        return vec![Effect::Quit];
    }

    // Cancel an in-flight fetch (Esc or Ctrl-g), returning to the current page.
    if model.is_loading() && (key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('g')))
    {
        model.pending = None;
        model.set_toast(ToastKind::Info, "cancelled");
        return vec![Effect::Cancel];
    }

    // Ctrl-L: recompute the layout and redraw, keeping the (re-clamped) offset.
    if ctrl && key.code == KeyCode::Char('l') {
        let (w, vp) = (model.width, model.viewport());
        model.relayout(w);
        model.clamp_scroll(vp);
        return Vec::new();
    }

    // Enter address mode.
    if key.code == KeyCode::Char(':') {
        enter_address(model);
        return Vec::new();
    }

    // Enter hint mode (`f`).
    if key.code == KeyCode::Char('f') && !ctrl && !alt {
        enter_hint(model);
        return Vec::new();
    }

    // Enter in-page search mode (`/`).
    if key.code == KeyCode::Char('/') && !ctrl && !alt {
        enter_search(model);
        return Vec::new();
    }

    // Cycle the current search match: `n` next, `N` previous (both wrap), each
    // scrolling the current match into view. A no-op with no active matches.
    if key.code == KeyCode::Char('n') && !ctrl && !alt {
        next_match(model);
        return Vec::new();
    }
    if key.code == KeyCode::Char('N') && !ctrl && !alt {
        prev_match(model);
        return Vec::new();
    }

    // Toggle the light/dark theme (`t`), correcting a wrong auto-detection.
    if key.code == KeyCode::Char('t') && !ctrl && !alt {
        model.toggle_theme();
        return Vec::new();
    }

    // Toggle the places panel (`d`): bookmarks + discovered nodes.
    if key.code == KeyCode::Char('d') && !ctrl && !alt {
        toggle_places(model);
        return Vec::new();
    }

    // Bookmark (or un-bookmark) the current page (`m`).
    if key.code == KeyCode::Char('m') && !ctrl && !alt {
        return toggle_bookmark_current(model);
    }

    // Yank the current page (or focused link) URL to the clipboard (`y`).
    if key.code == KeyCode::Char('y') && !ctrl && !alt {
        return yank_url(model);
    }

    // Tab / Shift-Tab move the focus cursor across links; Enter follows it.
    if key.code == KeyCode::Tab {
        focus_next(model);
        return Vec::new();
    }
    if key.code == KeyCode::BackTab {
        focus_prev(model);
        return Vec::new();
    }
    if key.code == KeyCode::Enter {
        return match model.focus {
            Some(idx) => follow_link(model, idx),
            None => Vec::new(),
        };
    }

    // Toggle the help overlay.
    if key.code == KeyCode::Char('?') {
        model.show_help = true;
        return Vec::new();
    }

    // Reload the current page (no history change).
    if key.code == KeyCode::Char('R') {
        return reload_current(model);
    }

    // Back / forward (Alt-Left / Alt-Right): peek the target and re-fetch it,
    // moving the cursor only once it loads.
    if alt && key.code == KeyCode::Left {
        return go_back(model);
    }
    if alt && key.code == KeyCode::Right {
        return go_forward(model);
    }

    if let Some(cmd) = key_to_scroll(&key) {
        let vp = model.viewport();
        model.apply_scroll(cmd, vp);
    }
    Vec::new()
}

/// Enter address-entry mode with a cleared editor.
fn enter_address(model: &mut Model) {
    model.mode = Mode::Address;
    model.input.reset();
    model.dismiss_toast();
}

/// Enter hint mode with a cleared filter buffer.
fn enter_hint(model: &mut Model) {
    model.mode = Mode::Hint;
    model.hint_input.clear();
    model.dismiss_toast();
}

/// Leave hint mode, discarding the filter buffer.
fn exit_hint(model: &mut Model) {
    model.mode = Mode::Browse;
    model.hint_input.clear();
}

/// Enter in-page search mode with a cleared query editor, dropping any prior
/// match highlights so the reader starts from a clean slate.
fn enter_search(model: &mut Model) {
    model.mode = Mode::Search;
    model.search_input.reset();
    model.matches.clear();
    model.current_match = None;
    model.dismiss_toast();
}

/// Leave search mode (Esc): back to Browse, clearing the query and all match
/// highlights plus the current-match marker.
fn exit_search(model: &mut Model) {
    model.mode = Mode::Browse;
    model.search_input.reset();
    model.matches.clear();
    model.current_match = None;
    model.dismiss_toast();
}

/// Commit a search query: recompute matches over the current page, mark the
/// first as current and scroll it into view. An empty result clears the current
/// match and notes it in a toast.
fn commit_search(model: &mut Model, query: &str) {
    model.matches = find_matches(&model.page, query);
    if model.matches.is_empty() {
        model.current_match = None;
        model.set_toast(ToastKind::Info, format!("no matches for \"{query}\""));
    } else {
        model.current_match = Some(0);
        model.dismiss_toast();
        scroll_to_current_match(model);
    }
}

/// Advance the current match to the next one (wrapping) and scroll it into view.
/// A no-op when there are no matches.
fn next_match(model: &mut Model) {
    let n = model.matches.len();
    if n == 0 {
        return;
    }
    let cur = model.current_match.unwrap_or(0);
    model.current_match = Some((cur + 1) % n);
    scroll_to_current_match(model);
}

/// Move the current match to the previous one (wrapping) and scroll it into
/// view. A no-op when there are no matches.
fn prev_match(model: &mut Model) {
    let n = model.matches.len();
    if n == 0 {
        return;
    }
    let cur = model.current_match.unwrap_or(0);
    model.current_match = Some((cur + n - 1) % n);
    scroll_to_current_match(model);
}

/// Scroll so the current match's line is inside the viewport (minimal motion,
/// like Tab focus), clamped to the page. A no-op when there is no current match.
fn scroll_to_current_match(model: &mut Model) {
    let Some(ci) = model.current_match else {
        return;
    };
    let Some(m) = model.matches.get(ci) else {
        return;
    };
    let line = m.line_idx;
    let vp = model.viewport();
    if line < model.scroll {
        model.scroll = line;
    } else if vp > 0 && line >= model.scroll + vp {
        model.scroll = line + 1 - vp;
    }
    model.clamp_scroll(vp);
}

/// One row in the places panel: a saved bookmark or a discovered node. Built by
/// [`places`] as a flat, ordered list (bookmarks first, then discovered nodes)
/// that [`Model::places_sel`] indexes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A saved bookmark: the URL to reopen and its captured title.
    Bookmark {
        /// The full page URL to reopen.
        url: String,
        /// The page title captured when it was bookmarked.
        title: String,
    },
    /// A discovered NomadNet node: its default page is opened on activation.
    Node {
        /// The node's destination hash.
        dest_hash: [u8; 16],
        /// The node's display name, if its announce carried one.
        name: Option<String>,
        /// The hop count from the most recent announce, if known.
        hops: Option<u32>,
        /// Unix seconds of the most recent announce.
        last_seen: u64,
    },
}

/// The flat, ordered list of places: every bookmark (in insertion order), then
/// every discovered node (in discovery order). The single source of truth for
/// both the panel's rendering and its selection/activation, so a row index means
/// the same thing to each.
pub fn places(model: &Model) -> Vec<Place> {
    let mut out = Vec::new();
    for b in model.bookmarks.list() {
        out.push(Place::Bookmark {
            url: b.url.clone(),
            title: b.title.clone(),
        });
    }
    for n in model.node_registry.nodes() {
        out.push(Place::Node {
            dest_hash: n.dest_hash,
            name: n.name.clone(),
            hops: n.hops,
            last_seen: n.last_seen,
        });
    }
    out
}

/// Toggle the places panel. Opening it resets the selection to the first row and
/// clears any transient status.
fn toggle_places(model: &mut Model) {
    model.show_places = !model.show_places;
    if model.show_places {
        model.places_sel = 0;
        model.dismiss_toast();
    }
}

/// Close the places panel.
fn close_places(model: &mut Model) {
    model.show_places = false;
}

/// Fold a key while the places panel is open. The up/down motions are the SAME
/// as the content scroll: [`key_to_scroll`] maps `j`/`k`/`Ctrl-n`/`Ctrl-p`/arrows
/// (line), `Ctrl-f`/`Ctrl-b`/`Ctrl-d`/`Ctrl-u` (page/half-page, clamped to the
/// ends), and `g`/`G`/Home/End (first/last), applied to the panel SELECTION
/// instead of the page. `Enter` opens the selected place, `x` deletes the
/// selected bookmark, `Esc`/`d` close the panel.
fn update_places_key(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    // Movement shares the content keymap, so the panel and the page scroll the
    // same way. This is checked first: `Ctrl-d` is a half-page motion here, not
    // the `d` that closes the panel.
    if let Some(cmd) = key_to_scroll(&key) {
        apply_places_scroll(model, cmd);
        return Vec::new();
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('d') => {
            close_places(model);
            Vec::new()
        }
        KeyCode::Enter => {
            let idx = model.places_sel;
            activate_place(model, idx)
        }
        KeyCode::Char('x') => delete_selected_place(model),
        _ => Vec::new(),
    }
}

/// Apply a [`ScrollCmd`] to the places-panel selection, reusing the content
/// keymap so the panel and the page share one set of up/down motions. Line
/// motions step one entry, page/half-page motions jump several (clamped to the
/// list ends), and top/bottom select the first/last entry. A no-op when the
/// panel is empty.
fn apply_places_scroll(model: &mut Model, cmd: ScrollCmd) {
    let len = places(model).len();
    if len == 0 {
        model.places_sel = 0;
        return;
    }
    let max = (len - 1) as isize;
    let vp = model.viewport().max(1) as isize;
    let half = (vp / 2).max(1);
    let sel = model.places_sel as isize;
    let next = match cmd {
        ScrollCmd::LineUp => sel - 1,
        ScrollCmd::LineDown => sel + 1,
        ScrollCmd::HalfPageUp => sel - half,
        ScrollCmd::HalfPageDown => sel + half,
        ScrollCmd::PageUp => sel - vp,
        ScrollCmd::PageDown => sel + vp,
        ScrollCmd::Top => 0,
        ScrollCmd::Bottom => max,
    };
    model.places_sel = next.clamp(0, max) as usize;
}

/// Open the selected place: a bookmark's URL, or a discovered node's default
/// page. Closes the panel and starts a fresh navigation. A malformed bookmark
/// URL surfaces an error toast instead.
fn activate_place(model: &mut Model, idx: usize) -> Vec<Effect> {
    let Some(place) = places(model).into_iter().nth(idx) else {
        return Vec::new();
    };
    let target = match place {
        Place::Bookmark { url, .. } => match parse_url(&url, model.current_dest) {
            Ok(target) => target,
            Err(err) => {
                model.set_toast(ToastKind::Error, format!("bad bookmark URL: {url} ({err})"));
                return Vec::new();
            }
        },
        Place::Node { dest_hash, .. } => Target {
            dest_hash,
            path: DEFAULT_PATH.to_string(),
            fields: Vec::new(),
            is_file: false,
        },
    };
    close_places(model);
    begin_navigation(model, target, HistoryAction::Push, None)
}

/// Delete the selected bookmark and persist, keeping the selection in range. A
/// no-op (no save) when the selected row is a discovered node, not a bookmark.
fn delete_selected_place(model: &mut Model) -> Vec<Effect> {
    let Some(Place::Bookmark { url, .. }) = places(model).into_iter().nth(model.places_sel) else {
        return Vec::new();
    };
    model.bookmarks.remove(&url);
    let len = places(model).len();
    model.places_sel = model.places_sel.min(len.saturating_sub(1));
    model.set_toast(ToastKind::Info, format!("removed bookmark {url}"));
    vec![Effect::SaveBookmarks]
}

/// Toggle a bookmark for the current page: remove it when the page is already
/// bookmarked, else add it under the current title. Persists on change. A no-op
/// with a toast note when nothing is loaded.
fn toggle_bookmark_current(model: &mut Model) -> Vec<Effect> {
    let Some(url) = current_url(model) else {
        model.set_toast(ToastKind::Info, "nothing to bookmark");
        return Vec::new();
    };
    if model.bookmarks.contains(&url) {
        model.bookmarks.remove(&url);
        model.set_toast(ToastKind::Info, format!("removed bookmark {url}"));
    } else {
        let title = model.title.trim().to_string();
        model.bookmarks.add(Bookmark {
            url: url.clone(),
            title,
        });
        model.set_toast(ToastKind::Info, format!("bookmarked {url}"));
    }
    vec![Effect::SaveBookmarks]
}

/// Yank the focused link's target URL, or (with nothing focused) the current
/// page URL, to the clipboard. A no-op with a toast note when there is nothing
/// to copy.
fn yank_url(model: &mut Model) -> Vec<Effect> {
    let url = match focused_link_url(model).or_else(|| current_url(model)) {
        Some(url) => url,
        None => {
            model.set_toast(ToastKind::Info, "nothing to copy");
            return Vec::new();
        }
    };
    model.set_toast(ToastKind::Info, format!("copied {url}"));
    vec![Effect::Copy(url)]
}

/// The full URL of the focused link's resolved target, or `None` when no link is
/// focused (or it fails to resolve).
fn focused_link_url(model: &Model) -> Option<String> {
    let idx = model.focus?;
    let link = model.links.iter().find(|l| l.index == idx)?;
    let (target, _anchor) = browser::resolve_link(link, model.current_dest).ok()?;
    Some(target_url(&target))
}

/// The full URL of the current page (`<dest_hex>:<path>`), or `None` when no
/// page is loaded.
fn current_url(model: &Model) -> Option<String> {
    model.history.current().map(target_url)
}

/// A full, reopenable URL for a target: `<dest_hex>:<path>`, with any query
/// fields reattached as the backtick blob [`parse_url`] understands (the stored
/// `var_` key prefix stripped back off).
fn target_url(target: &Target) -> String {
    let base = format!("{}:{}", full_hex(&target.dest_hash), target.path);
    if target.fields.is_empty() {
        return base;
    }
    let blob = target
        .fields
        .iter()
        .map(|(k, v)| format!("{}={}", k.strip_prefix("var_").unwrap_or(k), v))
        .collect::<Vec<_>>()
        .join("|");
    format!("{base}`{blob}")
}

/// The full 32-character lowercase hex of a destination hash.
fn full_hex(dest: &[u8; 16]) -> String {
    let mut s = String::with_capacity(dest.len() * 2);
    for byte in dest {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Encode `text` as an OSC 52 clipboard-set sequence: `ESC ] 52 ; c ;
/// <base64(text)> ST` (ST = `ESC \`). The IO shell writes this straight to the
/// terminal, which copies the payload to the system clipboard. Pure, so the
/// encoding is unit-testable without a terminal.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x1b\\", base64_encode(text.as_bytes()))
}

/// Standard base64 (RFC 4648, with `=` padding) of `bytes`. Small and local so
/// the OSC 52 helper needs no extra dependency.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(n >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

/// Whether `target` is a non-idempotent form submit: it carries at least one
/// `field_<name>` value collected from an on-page form. Such requests are never
/// served from (or stored in) the page cache. Preset-only (`var_`) targets are
/// idempotent and cacheable.
fn is_form_submit(target: &Target) -> bool {
    target.fields.iter().any(|(k, _)| k.starts_with("field_"))
}

/// Start a navigation to `target`, recording it in history per `action` and
/// scrolling to `anchor` (if any) once shown.
///
/// A cache hit for an idempotent target renders instantly from
/// [`Model::page_cache`] with its stored scroll restored and emits NO fetch and
/// NO spinner. A miss, or a non-idempotent form submit (never cached), sets the
/// pending fetch and emits [`Effect::Navigate`]. The single entry point every
/// navigation (address bar, link, bookmark, back/forward) routes through so the
/// cache is checked uniformly; a reload deliberately bypasses it.
fn begin_navigation(
    model: &mut Model,
    target: Target,
    action: HistoryAction,
    anchor: Option<String>,
) -> Vec<Effect> {
    // Remember where the reader was on the page being left, so a later revisit
    // of it restores the scroll position.
    model.stash_scroll();
    model.dismiss_toast();

    if !is_form_submit(&target) {
        if let Some(entry) = model.page_cache.get(&target).cloned() {
            return load_from_cache(model, target, action, anchor, entry);
        }
    }

    model.pending = Some(Pending {
        target: target.clone(),
        action,
    });
    model.pending_anchor = anchor;
    vec![Effect::Navigate(target)]
}

/// Show `target` from its cached parsed document: relayout at the current width/
/// theme, restore the stored scroll (or scroll to `anchor`), fold the navigation
/// into history exactly as a fresh load would, and flag the view as cached.
///
/// Emits no fetch. When a fetch was in flight it emits [`Effect::Cancel`] so a
/// late result cannot clobber the page we just restored.
fn load_from_cache(
    model: &mut Model,
    target: Target,
    action: HistoryAction,
    anchor: Option<String>,
    entry: CacheEntry,
) -> Vec<Effect> {
    let was_loading = model.is_loading();
    model.pending = None;
    model.pending_anchor = None;

    model.doc = entry.doc;
    model.title = entry.title;
    // The cached document brings its own fields: rebuild the editable store from
    // the freshly laid-out prefill, exactly as a fresh load does.
    model.field_state.clear();
    model.relayout(content_width(model.size.0));
    model.rebuild_field_state();

    // A different page invalidates any focus/hover cursor and search matches
    // held against the old one.
    model.focus = None;
    model.field_focus = None;
    model.mode = Mode::Browse;
    model.hover = None;
    model.matches.clear();
    model.current_match = None;

    model.current_dest = Some(target.dest_hash);
    match action {
        HistoryAction::Push => model.history.visit(target),
        HistoryAction::Goto(idx) => model.history.goto(idx),
    }

    // Restore the reader's last scroll, or scroll to a followed `#anchor`.
    let vp = model.viewport();
    match anchor {
        Some(name) => match anchor_line(model, &name) {
            Some(line) => model.scroll = line.min(model.max_scroll(vp)),
            None => {
                model.scroll = 0;
                model.set_toast(ToastKind::Error, format!("anchor #{name} not found"));
            }
        },
        None => model.scroll = entry.scroll.min(model.max_scroll(vp)),
    }

    model.cached_view = true;

    if was_loading {
        vec![Effect::Cancel]
    } else {
        Vec::new()
    }
}

/// Reload the page under the history cursor without changing history.
fn reload_current(model: &mut Model) -> Vec<Effect> {
    let Some(target) = model.history.current().cloned() else {
        return Vec::new();
    };
    let idx = model.history.idx;
    model.pending = Some(Pending {
        target: target.clone(),
        action: HistoryAction::Goto(idx),
    });
    model.pending_anchor = None;
    model.dismiss_toast();
    vec![Effect::Navigate(target)]
}

/// Navigate back one history entry (re-fetching it), if possible.
fn go_back(model: &mut Model) -> Vec<Effect> {
    if !model.history.can_back() {
        return Vec::new();
    }
    let idx = model.history.idx - 1;
    let target = model.history.stack[idx].clone();
    begin_navigation(model, target, HistoryAction::Goto(idx), None)
}

/// Navigate forward one history entry (re-fetching it), if possible.
fn go_forward(model: &mut Model) -> Vec<Effect> {
    if !model.history.can_forward() {
        return Vec::new();
    }
    let idx = model.history.idx + 1;
    let target = model.history.stack[idx].clone();
    begin_navigation(model, target, HistoryAction::Goto(idx), None)
}

/// Follow the link with 1-based `index`: resolve its target against the current
/// destination and start a fresh navigation, or raise an error toast.
fn follow_link(model: &mut Model, index: usize) -> Vec<Effect> {
    let Some(link) = model.links.iter().find(|l| l.index == index).cloned() else {
        return Vec::new();
    };
    // An external URL is opened in the user's default handler; an unsafe scheme
    // is refused outright. Only an RNS target is fetched in-mesh.
    match classify_link(&link.target) {
        LinkKind::External(url) => {
            model.dismiss_toast();
            vec![Effect::OpenExternal(url)]
        }
        LinkKind::Unsafe(scheme) => {
            model.set_toast(ToastKind::Error, format!("won't open {scheme}: link"));
            Vec::new()
        }
        LinkKind::Rns => match browser::resolve_link(&link, model.current_dest) {
            Ok((mut target, anchor)) => {
                // A submit link (one that references form fields, or `*` for all)
                // carries the CURRENT field values, packaged as NomadNet expects:
                // `field_<name>` entries alongside the `var_<key>` presets that
                // resolve_link already put on the target. Preset `var_` and
                // collected `field_` keys never collide, so we just append.
                target.fields.extend(collect_submit_fields(model, &link));
                // A submit target is routed through the same entry point but is
                // never served from cache (see [`is_form_submit`]); any `#anchor`
                // is resolved against the shown page.
                begin_navigation(model, target, HistoryAction::Push, anchor)
            }
            Err(err) => {
                model.set_toast(
                    ToastKind::Error,
                    format!("bad link: {} ({err})", link.target),
                );
                Vec::new()
            }
        },
    }
}

/// Collect the current values of the form fields a link references, packaged as
/// the NomadNet request map expects: each becomes a `field_<name>` entry.
///
/// Mirrors the reference `Browser.handle_link` (NomadNet `Browser.py`): a link's
/// field components that carry no `=` are field-NAME references (a `*` component
/// means "all fields"); their referenced fields' current widget values are added
/// under a `field_` prefix. A text field is always included; a checkbox/radio is
/// included only when checked (its submit value), and several checked checkboxes
/// sharing a name are comma-joined. The `k=v` preset components are handled by
/// [`browser::resolve_link`] as `var_<k>`, so they are ignored here.
fn collect_submit_fields(model: &Model, link: &RenderedLink) -> Vec<(String, String)> {
    let mut all = false;
    let mut referenced: Vec<&str> = Vec::new();
    for (k, v) in &link.fields {
        if !v.is_empty() {
            continue; // a `k=v` preset -> handled as var_ by resolve_link.
        }
        if k == "*" {
            all = true;
        } else {
            referenced.push(k.as_str());
        }
    }
    if !all && referenced.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<(String, String)> = Vec::new();
    for fe in &model.field_state {
        if !all && !referenced.iter().any(|n| *n == fe.name) {
            continue;
        }
        let key = format!("field_{}", fe.name);
        match fe.kind {
            FieldKind::Text => out.push((key, fe.input.value().to_string())),
            FieldKind::Checkbox => {
                if fe.checked {
                    if let Some(existing) = out.iter_mut().find(|(k, _)| *k == key) {
                        existing.1 = format!("{},{}", existing.1, fe.value);
                    } else {
                        out.push((key, fe.value.clone()));
                    }
                }
            }
            FieldKind::Radio => {
                if fe.checked {
                    if let Some(existing) = out.iter_mut().find(|(k, _)| *k == key) {
                        existing.1 = fe.value.clone();
                    } else {
                        out.push((key, fe.value.clone()));
                    }
                }
            }
        }
    }
    out
}

/// Activate a hint target: follow the link, or drive the top-bar control.
fn activate_hint_target(model: &mut Model, target: HintTarget) -> Vec<Effect> {
    match target {
        HintTarget::Link(idx) => follow_link(model, idx),
        HintTarget::Control(Control::Back) => go_back(model),
        HintTarget::Control(Control::Forward) => go_forward(model),
        HintTarget::Control(Control::Reload) => reload_current(model),
        HintTarget::Control(Control::Address) => {
            enter_address(model);
            Vec::new()
        }
    }
}

/// The interactive elements (links and form fields) in document order: sorted by
/// laid-out `(line, col_start)`, so the Tab cycle walks them exactly as they read
/// down the page. The single source of truth for focus traversal.
fn interactive_order(model: &Model) -> Vec<Focus> {
    let mut items: Vec<(usize, usize, Focus)> = Vec::new();
    for l in &model.links {
        items.push((l.line, l.col_start, Focus::Link(l.index)));
    }
    for f in &model.field_defs {
        items.push((f.line, f.col_start, Focus::Field(f.index)));
    }
    items.sort_by_key(|(line, col, _)| (*line, *col));
    items.into_iter().map(|(_, _, focus)| focus).collect()
}

/// The currently focused interactive element, as a [`Focus`], or `None`.
fn current_focus(model: &Model) -> Option<Focus> {
    if let Some(i) = model.field_focus {
        Some(Focus::Field(i))
    } else {
        model.focus.map(Focus::Link)
    }
}

/// Move the focus cursor to the next interactive element (link or field) in
/// reading order (wrapping), scrolling it into view. A no-op when the page has no
/// interactive elements.
fn focus_next(model: &mut Model) {
    let order = interactive_order(model);
    if order.is_empty() {
        return;
    }
    let next = match current_focus(model).and_then(|c| order.iter().position(|x| *x == c)) {
        Some(i) => order[(i + 1) % order.len()],
        None => order[0],
    };
    apply_focus(model, next);
}

/// Move the focus cursor to the previous interactive element (wrapping),
/// scrolling it into view. A no-op when the page has no interactive elements.
fn focus_prev(model: &mut Model) {
    let order = interactive_order(model);
    if order.is_empty() {
        return;
    }
    let last = order.len() - 1;
    let prev = match current_focus(model).and_then(|c| order.iter().position(|x| *x == c)) {
        Some(0) | None => order[last],
        Some(i) => order[i - 1],
    };
    apply_focus(model, prev);
}

/// Focus an interactive element: set the link/field focus (they are mutually
/// exclusive), switch mode (a field enters [`Mode::Field`] editing; a link
/// returns to [`Mode::Browse`]), and auto-scroll it into view.
fn apply_focus(model: &mut Model, focus: Focus) {
    match focus {
        Focus::Link(index) => {
            model.focus = Some(index);
            model.field_focus = None;
            model.mode = Mode::Browse;
            if let Some(line) = model
                .links
                .iter()
                .find(|l| l.index == index)
                .map(|l| l.line)
            {
                scroll_line_into_view(model, line);
            }
        }
        Focus::Field(index) => {
            model.field_focus = Some(index);
            model.focus = None;
            model.mode = Mode::Field;
            if let Some(line) = model
                .field_defs
                .iter()
                .find(|f| f.index == index)
                .map(|f| f.line)
            {
                scroll_line_into_view(model, line);
            }
        }
    }
}

/// Scroll the minimal amount so page `line` is inside the viewport.
fn scroll_line_into_view(model: &mut Model, line: usize) {
    let vp = model.viewport();
    if line < model.scroll {
        model.scroll = line;
    } else if vp > 0 && line >= model.scroll + vp {
        model.scroll = line + 1 - vp;
    }
    model.clamp_scroll(vp);
}

/// Hit-test a mouse click at `(col, row)`: follow the link under it, else
/// activate the top-bar control under it, else do nothing.
fn handle_click(model: &mut Model, col: u16, row: u16) -> Vec<Effect> {
    for vl in visible_links(model) {
        if row == vl.row && col >= vl.col_start && col < vl.col_end {
            return follow_link(model, vl.index);
        }
    }
    // A click on a form field focuses it (entering field-edit mode).
    for vf in visible_fields(model) {
        if row == vf.row && col >= vf.col_start && col < vf.col_end {
            apply_focus(model, Focus::Field(vf.index));
            return Vec::new();
        }
    }
    let [topbar, _content, _status] = regions(model.frame_area());
    for cr in top_bar_controls(topbar) {
        if rect_contains(cr.rect, col, row) {
            return activate_hint_target(model, HintTarget::Control(cr.control));
        }
    }
    Vec::new()
}

/// Set the hovered link to whichever visible link is under `(col, row)`, or
/// `None` when the cursor is over no link.
fn update_hover(model: &mut Model, col: u16, row: u16) {
    model.hover = visible_links(model)
        .into_iter()
        .find(|vl| row == vl.row && col >= vl.col_start && col < vl.col_end)
        .map(|vl| vl.index);
}

/// Whether `rect` contains the point `(x, y)`.
fn rect_contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

/// Map a key press to a [`ScrollCmd`], honouring both vi and emacs idioms.
/// Returns `None` for keys that are not scroll motions.
fn key_to_scroll(key: &KeyEvent) -> Option<ScrollCmd> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let plain = !ctrl && !alt;
    match key.code {
        // Line down: j, Ctrl-n, Down.
        KeyCode::Char('j') if plain => Some(ScrollCmd::LineDown),
        KeyCode::Char('n') if ctrl => Some(ScrollCmd::LineDown),
        KeyCode::Down => Some(ScrollCmd::LineDown),
        // Line up: k, Ctrl-p, Up.
        KeyCode::Char('k') if plain => Some(ScrollCmd::LineUp),
        KeyCode::Char('p') if ctrl => Some(ScrollCmd::LineUp),
        KeyCode::Up => Some(ScrollCmd::LineUp),
        // Page down: Ctrl-f, Ctrl-v, Space, PageDown.
        KeyCode::Char('f') if ctrl => Some(ScrollCmd::PageDown),
        KeyCode::Char('v') if ctrl => Some(ScrollCmd::PageDown),
        KeyCode::Char(' ') if plain => Some(ScrollCmd::PageDown),
        KeyCode::PageDown => Some(ScrollCmd::PageDown),
        // Page up: Ctrl-b, Alt-v, PageUp.
        KeyCode::Char('b') if ctrl => Some(ScrollCmd::PageUp),
        KeyCode::Char('v') if alt => Some(ScrollCmd::PageUp),
        KeyCode::PageUp => Some(ScrollCmd::PageUp),
        // Half page: Ctrl-d / Ctrl-u.
        KeyCode::Char('d') if ctrl => Some(ScrollCmd::HalfPageDown),
        KeyCode::Char('u') if ctrl => Some(ScrollCmd::HalfPageUp),
        // Top: g, Alt-< (Alt+Shift+,), Home.
        KeyCode::Char('g') if plain => Some(ScrollCmd::Top),
        KeyCode::Char('<') if alt => Some(ScrollCmd::Top),
        KeyCode::Home => Some(ScrollCmd::Top),
        // Bottom: G, Alt-> (Alt+Shift+.), End.
        KeyCode::Char('G') if plain => Some(ScrollCmd::Bottom),
        KeyCode::Char('>') if alt => Some(ScrollCmd::Bottom),
        KeyCode::End => Some(ScrollCmd::Bottom),
        _ => None,
    }
}

/// The geometry of the three fixed top-bar controls plus the address slot, laid
/// out left to right on the controls row (the top-bar's second row). Exposed so
/// a later phase can hit-test mouse clicks against the returned rectangles.
pub fn top_bar_controls(topbar: Rect) -> Vec<ControlRect> {
    let y = topbar.y.saturating_add(1);
    let mut x = topbar.x;
    let mut out = Vec::new();
    for (control, label) in [
        (Control::Back, BACK_LABEL),
        (Control::Forward, FORWARD_LABEL),
        (Control::Reload, RELOAD_LABEL),
    ] {
        let w = UnicodeWidthStr::width(label) as u16;
        out.push(ControlRect {
            control,
            rect: Rect {
                x,
                y,
                width: w.min(topbar.right().saturating_sub(x)),
                height: 1,
            },
        });
        x = x.saturating_add(w).saturating_add(CTRL_GAP);
    }
    let addr_w = topbar.right().saturating_sub(x);
    out.push(ControlRect {
        control: Control::Address,
        rect: Rect {
            x,
            y,
            width: addr_w,
            height: 1,
        },
    });
    out
}

/// Draw the whole UI: the top-bar, the scrollable content, and the status bar,
/// plus the help overlay when active.
pub fn view(model: &Model, frame: &mut Frame) {
    let [topbar, content, status] = regions(frame.area());
    render_topbar(model, frame, topbar);
    render_content(model, frame, content);
    render_status(model, frame, status);
    // Overlays drawn on top of the laid-out page: the form-field input boxes, the
    // search-match highlights, the focus highlight, the mouse-hover highlight, and
    // the hint badges while hint mode is active.
    render_fields(model, frame);
    render_search_matches(model, frame);
    render_focus(model, frame);
    render_hover(model, frame);
    if model.mode == Mode::Hint {
        render_hints(model, frame);
    }
    if model.show_places {
        render_places(model, frame, frame.area());
    }
    if model.show_help {
        render_help(frame, frame.area());
    }
    // The toast floats on top of everything so it is always visible.
    render_toast(model, frame, frame.area());
}

/// Draw the form-field input boxes over the laid-out page: every field's cells
/// get an input style (a distinct chrome-slate box, or an underline box under
/// `no_color`); the focused field is reversed on top so it stands out. A focused
/// TEXT field also places the hardware cursor at its edit position.
fn render_fields(model: &Model, frame: &mut Frame) {
    let visible = visible_fields(model);
    if visible.is_empty() {
        return;
    }
    let area = frame.area();
    let base = if model.no_color {
        Style::default().add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default()
            .fg(rgb(model.theme.chrome_fg()))
            .bg(rgb(model.theme.chrome_bg()))
    };
    let focused = base.add_modifier(Modifier::REVERSED);
    // The hardware cursor position for a focused text field, resolved after the
    // buffer paint so it lands on the correct cell.
    let mut cursor: Option<(u16, u16)> = None;
    {
        let buf = frame.buffer_mut();
        for vf in &visible {
            let is_focused = model.field_focus == Some(vf.index);
            let style = if is_focused { focused } else { base };
            for x in vf.col_start..vf.col_end {
                if x >= area.width || vf.row >= area.height {
                    continue;
                }
                if let Some(c) = buf.cell_mut((x, vf.row)) {
                    c.set_style(style);
                }
            }
            if is_focused && vf.kind == FieldKind::Text {
                if let Some(fe) = model.field_state.get(vf.index - 1) {
                    // Past the opening `[`, offset by the editor cursor, clamped
                    // inside the box (before the closing `]`).
                    let cx = vf.col_start + 1 + fe.input.visual_cursor() as u16;
                    let clamped = cx.min(vf.col_end.saturating_sub(1));
                    cursor = Some((clamped, vf.row));
                }
            }
        }
    }
    if let Some(pos) = cursor {
        frame.set_cursor_position(pos);
    }
}

/// Highlight the focused link's cells (reverse video) so Tab navigation is
/// visible. A no-op when nothing is focused or the focus is off-screen.
fn render_focus(model: &Model, frame: &mut Frame) {
    let Some(idx) = model.focus else {
        return;
    };
    let area = frame.area();
    let buf = frame.buffer_mut();
    let highlight = Style::default().add_modifier(Modifier::REVERSED);
    for vl in visible_links(model) {
        if vl.index != idx {
            continue;
        }
        for x in vl.col_start..vl.col_end {
            if x >= area.width || vl.row >= area.height {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, vl.row)) {
                cell.set_style(highlight);
            }
        }
    }
}

/// Highlight the link the mouse is hovering over, so the pointer's target is
/// visible in the content as well as the status bar. Kept distinct from the Tab
/// focus (reverse video): hover patches bold + underline onto the link's own
/// colour, so the two can coexist on different links. A no-op when nothing is
/// hovered or the hovered link is off-screen.
fn render_hover(model: &Model, frame: &mut Frame) {
    let Some(idx) = model.hover else {
        return;
    };
    let area = frame.area();
    let buf = frame.buffer_mut();
    let hover = Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    for vl in visible_links(model) {
        if vl.index != idx {
            continue;
        }
        for x in vl.col_start..vl.col_end {
            if x >= area.width || vl.row >= area.height {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, vl.row)) {
                // Patch (not replace) so the link colour and any focus reverse
                // underneath are preserved and the hover modifiers add on top.
                let style = cell.style().patch(hover);
                cell.set_style(style);
            }
        }
    }
}

/// Highlight the on-screen search matches: every match wears a theme-aware
/// background tint, and the current match a stronger reversed highlight on top.
/// Off-viewport matches are skipped. A no-op when there are no matches.
fn render_search_matches(model: &Model, frame: &mut Frame) {
    if model.matches.is_empty() {
        return;
    }
    let area = frame.area();
    let viewport = model.viewport();
    let scroll = model.scroll;
    let match_style = Style::default()
        .fg(rgb(model.theme.search_match_fg()))
        .bg(rgb(model.theme.search_match_bg()));
    let current_style = Style::default().add_modifier(Modifier::REVERSED);
    let buf = frame.buffer_mut();
    for (i, m) in model.matches.iter().enumerate() {
        if m.line_idx < scroll || m.line_idx >= scroll + viewport {
            continue;
        }
        let row = TOPBAR_ROWS + (m.line_idx - scroll) as u16;
        let style = if model.current_match == Some(i) {
            current_style
        } else {
            match_style
        };
        for col in m.col_start..m.col_end {
            let x = col as u16;
            if x >= area.width || row >= area.height {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, row)) {
                cell.set_style(style);
            }
        }
    }
}

/// Draw the hint badges: each matching hint's label over the first cells of its
/// link or control, on a distinct background. Only hints still matching the
/// typed prefix are shown, so typing narrows the field.
fn render_hints(model: &Model, frame: &mut Frame) {
    let area = frame.area();
    let input = model.hint_input.clone();
    let badge = Style::default()
        .fg(rgb(model.theme.hint_badge_fg()))
        .bg(rgb(model.theme.hint_badge_bg()))
        .add_modifier(Modifier::BOLD);
    let buf = frame.buffer_mut();
    for hint in hints(model) {
        if !hint.label.starts_with(&input) {
            continue;
        }
        for (i, ch) in hint.label.chars().enumerate() {
            let x = hint.col + i as u16;
            if x >= area.width || hint.row >= area.height {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, hint.row)) {
                cell.set_char(ch);
                cell.set_style(badge);
            }
        }
    }
}

/// Draw the visible slice of the page and the scrollbar into the content region.
fn render_content(model: &Model, frame: &mut Frame, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let viewport = area.height as usize;
    let body = Rect {
        x: area.x,
        y: area.y,
        width: area.width.saturating_sub(SCROLLBAR_COLS),
        height: area.height,
    };
    let end = model.scroll.saturating_add(viewport).min(model.page.len());
    let start = model.scroll.min(end);
    let text = to_ratatui_text(&model.page[start..end]);
    frame.render_widget(Paragraph::new(text), body);

    // Map the scrollbar over the SCROLL POSITIONS, not the line count: content
    // length is `max_scroll` so `position` spans the full `[0, max_scroll]` range
    // across the track. This makes the thumb top hit the track top at `scroll==0`
    // and the thumb bottom hit the track bottom at `scroll==max_scroll`. Setting
    // `content_length` to `page.len()` with `viewport_content_length` instead left
    // the thumb short of the bottom for pages only a little taller than the
    // viewport (the position denominator over-counted by the viewport height).
    let mut state = ScrollbarState::new(model.max_scroll(viewport).max(1)).position(model.scroll);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None);
    frame.render_stateful_widget(scrollbar, area, &mut state);
}

/// Draw the two-row top-bar: title, then the back/forward/reload controls and
/// the address slot (breadcrumb, or the live editor in address mode).
fn render_topbar(model: &Model, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    // Fill the whole bar (both rows, full width) with the chrome style first; the
    // title and controls render on top and inherit its background.
    frame.render_widget(
        Block::default().style(chrome_style(model.no_color, model.theme)),
        area,
    );
    let title_row = Rect { height: 1, ..area };
    let title = RtLine::from(RtSpan::styled(
        model.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(title), title_row);

    // A subtle, right-aligned marker when the shown page came from the in-RAM
    // cache rather than a fresh fetch. Drawn in the muted chrome fg (never DIM,
    // and reverse-video-safe under NO_COLOR), so it reads as unobtrusive chrome.
    if model.cached_view {
        let w = UnicodeWidthStr::width(CACHED_LABEL) as u16;
        if title_row.width > w {
            let marker = Rect {
                x: title_row.x + title_row.width - w,
                y: title_row.y,
                width: w,
                height: 1,
            };
            let style = chrome_muted_style(model.no_color, model.theme);
            frame.render_widget(Paragraph::new(RtSpan::styled(CACHED_LABEL, style)), marker);
        }
    }

    if area.height < TOPBAR_ROWS {
        return;
    }
    // Available controls read in the bright chrome fg; unavailable ones in the
    // muted (readable) chrome fg, never `Modifier::DIM`.
    let available = chrome_text_style(model.no_color, model.theme);
    let unavailable = chrome_muted_style(model.no_color, model.theme);
    for cr in top_bar_controls(area) {
        match cr.control {
            Control::Back => {
                let style = if model.history.can_back() {
                    available
                } else {
                    unavailable
                };
                frame.render_widget(Paragraph::new(RtSpan::styled(BACK_LABEL, style)), cr.rect);
            }
            Control::Forward => {
                let style = if model.history.can_forward() {
                    available
                } else {
                    unavailable
                };
                frame.render_widget(
                    Paragraph::new(RtSpan::styled(FORWARD_LABEL, style)),
                    cr.rect,
                );
            }
            Control::Reload => {
                let style = if model.history.current().is_some() {
                    available
                } else {
                    unavailable
                };
                frame.render_widget(Paragraph::new(RtSpan::styled(RELOAD_LABEL, style)), cr.rect);
            }
            Control::Address => render_address_slot(model, frame, cr.rect),
        }
    }
}

/// Draw the address slot: the live editor (with a `:` prompt and cursor) in
/// address mode, otherwise a dimmed breadcrumb of the current URL.
fn render_address_slot(model: &Model, frame: &mut Frame, area: Rect) {
    if area.width == 0 {
        return;
    }
    if model.mode == Mode::Address {
        frame.render_widget(Paragraph::new(RtSpan::raw(":")), Rect { width: 1, ..area });
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y,
            width: area.width.saturating_sub(1),
            height: 1,
        };
        if inner.width == 0 {
            return;
        }
        let w = inner.width as usize;
        let scroll = model.input.visual_scroll(w.saturating_sub(1));
        frame.render_widget(
            Paragraph::new(model.input.value().to_string()).scroll((0, scroll as u16)),
            inner,
        );
        let cx = inner.x + (model.input.visual_cursor().saturating_sub(scroll)) as u16;
        frame.set_cursor_position((cx.min(inner.right().saturating_sub(1)), inner.y));
    } else {
        let dim = Style::default().add_modifier(Modifier::DIM);
        frame.render_widget(Paragraph::new(RtSpan::styled(breadcrumb(model), dim)), area);
    }
}

/// The address-slot breadcrumb for the current page, or a hint when nothing is
/// loaded yet.
fn breadcrumb(model: &Model) -> String {
    match model.history.current() {
        Some(target) => format!("{}:{}", short_hex(&target.dest_hash), target.path),
        None => "press : to enter an address".to_string(),
    }
}

/// A short, glanceable form of a destination hash: the first 8 hex characters
/// followed by an ellipsis.
fn short_hex(dest: &[u8; 16]) -> String {
    let mut s = String::with_capacity(9);
    for byte in &dest[..4] {
        s.push_str(&format!("{byte:02x}"));
    }
    s.push('…');
    s
}

/// The six braille glyphs of the loading spinner: a single dot circling once per
/// full cycle. `spin % 6` picks the current frame.
const SPIN_FRAMES: [&str; 6] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴"];

/// Degrees of hue the spinner colour advances per animation tick. The colour
/// glides continuously through the HSV wheel rather than snapping between a few
/// fixed rainbow stops. At the [`SPINNER_TICK_MS`] (120 ms) redraw cadence a full
/// 360 degree rainbow takes `360 / HUE_STEP` ticks = 40 ticks = 4.8 s, and each
/// tick shifts the hue by only 9 degrees, so the change reads as a smooth glide
/// with no flicker.
const HUE_STEP: f32 = 9.0;

/// Convert an HSV colour to 8-bit RGB. Pure function. `h_deg` is the hue in
/// degrees (wrapped into `[0, 360)`), `s` and `v` are the saturation and value in
/// `[0, 1]`. Uses the standard sextant formulation; the spinner calls it with
/// `s = v = 1.0` for maximally vivid, full-brightness colours.
fn hsv_to_rgb(h_deg: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h = h_deg.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let c = v * s;
    let sextant = h / 60.0;
    let x = c * (1.0 - (sextant.rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r1, g1, b1) = match sextant as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to_u8 = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to_u8(r1), to_u8(g1), to_u8(b1))
}

/// Render one frame of the loading spinner: the current braille glyph as a bold,
/// full-brightness span whose colour rotates slowly and continuously through the
/// HSV hue wheel. The glyph advances one step per `spin` (the dot circles) while
/// the hue advances a small [`HUE_STEP`] per tick, so the colour glides smoothly
/// and much more slowly than the dot position.
fn spinner_span(spin: usize) -> RtSpan<'static> {
    let glyph = SPIN_FRAMES[spin % SPIN_FRAMES.len()];
    let hue = (spin as f32 * HUE_STEP).rem_euclid(360.0);
    let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);
    RtSpan::styled(
        glyph,
        Style::default()
            .fg(Color::Rgb(r, g, b))
            .add_modifier(Modifier::BOLD),
    )
}

/// Draw the status bar: the loading spinner while a fetch is in flight, else the
/// focused/hovered link's target, else the context key-hints. Transient messages
/// no longer live here; they float as a [`render_toast`] overlay instead.
fn render_status(model: &Model, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    // Fill the status bar full width with the chrome style; the spinner / status /
    // hints render on top and inherit its background.
    frame.render_widget(
        Block::default().style(chrome_style(model.no_color, model.theme)),
        area,
    );
    // In search mode the status bar becomes the `/<query>` editor.
    if model.mode == Mode::Search {
        render_search_bar(model, frame, area);
        return;
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    if let Some(pending) = &model.pending {
        let label = format!(" loading {}", pending.target.path);
        let line = RtLine::from(vec![spinner_span(model.spin), RtSpan::styled(label, dim)]);
        frame.render_widget(Paragraph::new(line), area);
    } else if let Some(target) = focused_link_target(model) {
        // A focused (Tab) or hovered (mouse) link shows its target here, taking
        // the place of the key-hints until focus/hover clears.
        frame.render_widget(Paragraph::new(RtSpan::styled(target, dim)), area);
    } else {
        let hints = match model.mode {
            Mode::Address => ADDRESS_HINTS,
            Mode::Hint => HINT_HINTS,
            Mode::Search => SEARCH_HINTS,
            Mode::Field => FIELD_HINTS,
            Mode::Browse => BROWSE_HINTS,
        };
        // The key-hints are the most-read chrome text: render them in the bright
        // chrome fg (no DIM) so they stay legible on the chrome bar.
        let hint_style = chrome_text_style(model.no_color, model.theme);
        frame.render_widget(Paragraph::new(RtSpan::styled(hints, hint_style)), area);
    }
}

/// Draw the in-page search editor into the status bar: a `/` prompt, then the
/// live query with its cursor, styled to read on the chrome bar.
fn render_search_bar(model: &Model, frame: &mut Frame, area: Rect) {
    if area.width == 0 {
        return;
    }
    let text_style = chrome_text_style(model.no_color, model.theme);
    frame.render_widget(
        Paragraph::new(RtSpan::styled("/", text_style)),
        Rect { width: 1, ..area },
    );
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y,
        width: area.width.saturating_sub(1),
        height: 1,
    };
    if inner.width == 0 {
        return;
    }
    let w = inner.width as usize;
    let scroll = model.search_input.visual_scroll(w.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(RtSpan::styled(
            model.search_input.value().to_string(),
            text_style,
        ))
        .scroll((0, scroll as u16)),
        inner,
    );
    let cx = inner.x + (model.search_input.visual_cursor().saturating_sub(scroll)) as u16;
    frame.set_cursor_position((cx.min(inner.right().saturating_sub(1)), inner.y));
}

/// The status-bar text for the focused-or-hovered link's target, if any: the
/// current node's short hash and the link's path (`node · :/page/x.mu`).
fn focused_link_target(model: &Model) -> Option<String> {
    let idx = model.focus.or(model.hover)?;
    let link = model.links.iter().find(|l| l.index == idx)?;
    // A same-destination link's target begins with ':'; prefix the current node
    // so the reader sees which node it resolves against.
    match model.current_dest {
        Some(dest) if link.target.starts_with(':') => {
            Some(format!("{} · {}", short_hex(&dest), link.target))
        }
        _ => Some(link.target.clone()),
    }
}

/// The keybindings listed in the help overlay.
const HELP_LINES: &[&str] = &[
    "lnomad — keys",
    "",
    "  j / k  ↓ / ↑        scroll a line",
    "  Ctrl-f / Ctrl-b     page down / up",
    "  Ctrl-d / Ctrl-u     half page down / up",
    "  g / G  Home / End   top / bottom",
    "  Tab / Shift-Tab     focus link / field",
    "  Enter               follow the link",
    "  (field) type        edit a text field",
    "  (field) Space       toggle a checkbox / radio",
    "  (field) Esc         leave field editing",
    "  f                   hint a link",
    "  / n N               search / next / prev",
    "  click               follow link",
    "  :                   enter an address",
    "  d                   places panel",
    "  m                   bookmark this page",
    "  y                   copy link / page URL",
    "  R                   reload the page (always refetches)",
    "  t                   toggle light / dark theme",
    "  M-← / M-→           back / forward",
    "  mouse back / fwd     back / forward",
    "  Esc / Ctrl-g        cancel a load",
    "  ?                   toggle this help",
    "  q / Ctrl-c          quit",
];

/// Draw the centered places panel: a chrome-styled overlay with a "Bookmarks"
/// section and a "Discovered nodes" section, the selected row reverse-video
/// highlighted. Both the fill and the border use the theme's chrome colours, so
/// the panel tracks the light/dark theme.
fn render_places(model: &Model, frame: &mut Frame, area: Rect) {
    let entries = places(model);
    let bm_count = model.bookmarks.len();
    let text_style = chrome_text_style(model.no_color, model.theme);
    let muted = chrome_muted_style(model.no_color, model.theme);
    let header = text_style.add_modifier(Modifier::BOLD);
    let selected = Style::default().add_modifier(Modifier::REVERSED);

    // Fix the panel width up front so rows can be truncated to the inner display
    // width (the two border columns removed). A long bookmark row that reached
    // the border let the selected row's reversed bar spill past it; truncating
    // to the inner width keeps every row strictly inside the border.
    let width = 72u16.min(area.width);
    let inner = width.saturating_sub(2) as usize;

    let mut lines: Vec<RtLine<'static>> = Vec::new();
    lines.push(RtLine::from(RtSpan::styled("Bookmarks", header)));
    if bm_count == 0 {
        lines.push(RtLine::from(RtSpan::styled("  (none)", muted)));
    } else {
        for (i, place) in entries.iter().enumerate().take(bm_count) {
            lines.push(place_line(
                place,
                i,
                model.places_sel,
                text_style,
                selected,
                inner,
            ));
        }
    }
    lines.push(RtLine::from(""));
    lines.push(RtLine::from(RtSpan::styled("Discovered nodes", header)));
    if entries.len() == bm_count {
        lines.push(RtLine::from(RtSpan::styled("  (none)", muted)));
    } else {
        for (i, place) in entries.iter().enumerate().skip(bm_count) {
            lines.push(place_line(
                place,
                i,
                model.places_sel,
                text_style,
                selected,
                inner,
            ));
        }
    }

    let height = (lines.len() as u16 + 2).min(area.height);
    let overlay = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" places ")
        .style(chrome_style(model.no_color, model.theme));
    frame.render_widget(Clear, overlay);
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), overlay);
}

/// One places-panel row: its label, reverse-video highlighted when it is the
/// selected row.
fn place_line(
    place: &Place,
    idx: usize,
    sel: usize,
    normal: Style,
    selected: Style,
    inner: usize,
) -> RtLine<'static> {
    let style = if idx == sel { selected } else { normal };
    let label = truncate_to_cols(&place_label(place), inner);
    RtLine::from(RtSpan::styled(label, style))
}

/// The one-line label for a place: a bookmark's title and URL, or a node's name,
/// short hash and hop count.
fn place_label(place: &Place) -> String {
    match place {
        Place::Bookmark { url, title } => {
            if title.is_empty() {
                format!("  {url}")
            } else {
                format!("  {title}  ·  {url}")
            }
        }
        Place::Node {
            dest_hash,
            name,
            hops,
            ..
        } => {
            let label = name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| short_hex(dest_hash));
            let hop = hops
                .map(|h| format!("{h} hops"))
                .unwrap_or_else(|| "? hops".to_string());
            format!("  {label}  ·  {}  ·  {hop}", short_hex(dest_hash))
        }
    }
}

/// Draw the transient toast: a small bordered box floated at the bottom-right of
/// the content (never over the status bar), coloured by kind. An error is an
/// attention red with a `⚠`; an info note is a calm green with a `✓`. Under
/// `no_color` both fall back to reverse video. A no-op when no toast is up.
fn render_toast(model: &Model, frame: &mut Frame, area: Rect) {
    let Some(toast) = &model.toast else {
        return;
    };
    let [_topbar, content, _status] = regions(area);
    // Too little room to float a bordered box: skip rather than draw garbage.
    if content.width < 8 || content.height < 3 {
        return;
    }
    let (glyph, accent) = match toast.kind {
        ToastKind::Error => ('⚠', Color::Rgb(255, 95, 95)),
        ToastKind::Info => ('✓', Color::Rgb(120, 220, 120)),
    };
    let style = if model.no_color {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().fg(accent)
    };
    // A one-column margin from the content's right edge, room for both borders,
    // and the message truncated to whatever inner width is left.
    let margin = 1u16;
    let max_inner = content.width.saturating_sub(2 + 2 * margin).max(1) as usize;
    let body = truncate_to_cols(&format!("{glyph} {}", toast.text), max_inner);
    let inner_w = UnicodeWidthStr::width(body.as_str()) as u16;
    let box_w = inner_w + 2;
    let box_h = 3u16;
    // Bottom-right of the content, sitting just above the status bar.
    let overlay = Rect {
        x: content.right().saturating_sub(box_w + margin),
        y: content.bottom().saturating_sub(box_h),
        width: box_w,
        height: box_h,
    };
    let block = Block::default().borders(Borders::ALL).style(style);
    frame.render_widget(Clear, overlay);
    frame.render_widget(
        Paragraph::new(RtSpan::styled(body, style)).block(block),
        overlay,
    );
}

/// Draw the centered help overlay listing the keybindings.
fn render_help(frame: &mut Frame, area: Rect) {
    // Size the overlay to the widest help line plus its two borders, so no line
    // wraps in a normal-width terminal. `Wrap` below stays as a safety net for
    // terminals too narrow to hold the widest line.
    let inner = HELP_LINES
        .iter()
        .map(|l| UnicodeWidthStr::width(*l))
        .max()
        .unwrap_or(0) as u16;
    let width = (inner + 2).min(area.width);
    let height = (HELP_LINES.len() as u16 + 2).min(area.height);
    let overlay = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let text = Text::from(
        HELP_LINES
            .iter()
            .map(|l| RtLine::from(*l))
            .collect::<Vec<_>>(),
    );
    let block = Block::default().borders(Borders::ALL).title(" help ");
    frame.render_widget(Clear, overlay);
    frame.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
        overlay,
    );
}

/// Map laid-out styled lines to a ratatui [`Text`], grouping runs of equal
/// style into one span each (the same run-grouping the ANSI sink uses).
pub fn to_ratatui_text(lines: &[RLine]) -> Text<'static> {
    let mut out_lines: Vec<RtLine<'static>> = Vec::with_capacity(lines.len());
    for line in lines {
        let mut spans: Vec<RtSpan<'static>> = Vec::new();
        let mut i = 0;
        while i < line.cells.len() {
            let st = line.cells[i].st;
            let mut j = i;
            let mut text = String::new();
            while j < line.cells.len() && line.cells[j].st == st {
                text.push(line.cells[j].ch);
                j += 1;
            }
            spans.push(RtSpan::styled(text, rstyle_to_ratatui(st)));
            i = j;
        }
        out_lines.push(RtLine::from(spans));
    }
    Text::from(out_lines)
}

/// Translate a resolved [`RStyle`] into a ratatui [`Style`]: RGB colours map
/// directly, and bold/underline/italic map to the matching modifiers.
fn rstyle_to_ratatui(st: RStyle) -> Style {
    let mut style = Style::default();
    if let Some((r, g, b)) = st.fg {
        style = style.fg(Color::Rgb(r, g, b));
    }
    if let Some((r, g, b)) = st.bg {
        style = style.bg(Color::Rgb(r, g, b));
    }
    if st.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if st.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if st.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    style
}

/// The terminal side-effects the IO shell needs, abstracted so a test can
/// substitute a mock and assert that `restore` runs, with no real terminal.
pub trait TerminalOps {
    /// Enter the UI: alternate screen, raw mode, mouse capture.
    fn enter(&mut self) -> io::Result<()>;
    /// Restore the terminal to how it was before [`enter`](TerminalOps::enter).
    fn restore(&mut self) -> io::Result<()>;
}

/// The real crossterm terminal: alt-screen + raw mode + mouse capture on
/// [`enter`](TerminalOps::enter), reversed on [`restore`](TerminalOps::restore).
pub struct CrosstermTerminal {
    out: Stdout,
}

impl CrosstermTerminal {
    /// A terminal driver writing to stdout.
    pub fn new() -> Self {
        Self { out: io::stdout() }
    }
}

impl Default for CrosstermTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalOps for CrosstermTerminal {
    fn enter(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(self.out, EnterAlternateScreen, EnableMouseCapture)
    }

    fn restore(&mut self) -> io::Result<()> {
        execute!(self.out, LeaveAlternateScreen, DisableMouseCapture)?;
        disable_raw_mode()
    }
}

/// A RAII guard that restores the terminal on drop. Constructing it enters the
/// UI; dropping it (normally, on `?`, or during a panic unwind) restores it, so
/// no code path can leave the terminal in raw/alt-screen mode.
pub struct TerminalGuard<T: TerminalOps> {
    ops: T,
    restored: bool,
}

impl<T: TerminalOps> TerminalGuard<T> {
    /// Enter the UI and return a guard that will restore it.
    pub fn new(mut ops: T) -> io::Result<Self> {
        ops.enter()?;
        Ok(Self {
            ops,
            restored: false,
        })
    }

    /// Restore the terminal now (idempotent); the drop becomes a no-op.
    pub fn restore_now(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        self.ops.restore()
    }
}

impl<T: TerminalOps> Drop for TerminalGuard<T> {
    fn drop(&mut self) {
        let _ = self.restore_now();
    }
}

/// Install a panic hook that restores the terminal BEFORE the previous hook
/// runs, so a panic's backtrace prints to a sane (cooked) terminal instead of
/// the raw alternate screen.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        let _ = execute!(out, LeaveAlternateScreen, DisableMouseCapture);
        let _ = disable_raw_mode();
        previous(info);
    }));
}

/// The result of a background fetch, tagged with the generation that issued it
/// so a superseded or cancelled fetch's late result can be ignored.
struct FetchOutcome {
    generation: u64,
    result: LoadResult,
}

/// A fetch's outcome: a parsed page and its title, or an error message.
enum LoadResult {
    Ok { doc: MicronDocument, title: String },
    Err(String),
}

/// Start (or restart) a background fetch for `target`. Aborts any in-flight
/// task, bumps the generation, and spawns a task that locks the session, fetches
/// and parses the page, resolves the title, and reports the tagged outcome.
fn spawn_fetch(
    inflight: &mut Option<tokio::task::JoinHandle<()>>,
    generation: &mut u64,
    session: &Arc<Mutex<Session>>,
    tx: &mpsc::UnboundedSender<FetchOutcome>,
    timeout: Duration,
    target: Target,
) {
    if let Some(handle) = inflight.take() {
        handle.abort();
    }
    *generation = generation.wrapping_add(1);
    let generation_now = *generation;
    let session = session.clone();
    let tx = tx.clone();
    let handle = tokio::spawn(async move {
        let mut guard = session.lock().await;
        let result = match browser::fetch_document(&mut guard, &target, timeout).await {
            Ok(doc) => {
                let name = guard.node_name(&target.dest_hash);
                let title = browser::page_title(name.as_deref(), &target.dest_hash, &target.path);
                LoadResult::Ok { doc, title }
            }
            Err(err) => LoadResult::Err(err.to_string()),
        };
        let _ = tx.send(FetchOutcome {
            generation: generation_now,
            result,
        });
    });
    *inflight = Some(handle);
}

/// Run the effects [`update`] returned: start navigations, cancel the in-flight
/// fetch, or record a quit.
fn run_effects(
    effects: Vec<Effect>,
    model: &mut Model,
    inflight: &mut Option<tokio::task::JoinHandle<()>>,
    generation: &mut u64,
    session: &Arc<Mutex<Session>>,
    tx: &mpsc::UnboundedSender<FetchOutcome>,
    timeout: Duration,
) {
    for effect in effects {
        match effect {
            Effect::Navigate(target) => {
                spawn_fetch(inflight, generation, session, tx, timeout, target);
            }
            Effect::OpenExternal(url) => {
                // Hand the URL to the platform default handler (`xdg-open` on
                // Linux). `open::that` launches the handler and returns; a
                // failure only raises a toast, never takes the UI down.
                match open::that(&url) {
                    Ok(_) => model.set_toast(ToastKind::Info, format!("opened externally: {url}")),
                    Err(_) => model.set_toast(ToastKind::Error, format!("failed to open: {url}")),
                }
            }
            Effect::Cancel => {
                if let Some(handle) = inflight.take() {
                    handle.abort();
                }
                // Invalidate any late result from the cancelled task.
                *generation = generation.wrapping_add(1);
            }
            Effect::Copy(text) => {
                // Write the OSC 52 clipboard-set sequence straight to the
                // terminal; the update side has already raised the "copied" toast.
                let _ =
                    crossterm::execute!(std::io::stdout(), crossterm::style::Print(osc52(&text)));
            }
            Effect::SaveBookmarks => {
                // Persist the bookmarks, ignoring IO errors: a failed write must
                // not take down the browser (no config dir → nothing to do).
                if let Some(path) = &model.bookmarks_path {
                    let _ = model.bookmarks.save(path);
                }
            }
            Effect::Quit => model.quit = true,
        }
    }
}

/// How long to wait for the terminal's OSC 11 background reply before giving up
/// and falling back to the dark theme. Kept short so a terminal that never
/// answers does not stall startup.
const BG_QUERY_TIMEOUT: Duration = Duration::from_millis(100);

/// Query the terminal background for `--theme auto`, returning `None` (→ dark)
/// when the flag is explicit, the query fails, or the terminal does not answer.
///
/// The `termbg` crate sends the OSC 11 query (falling back to `COLORFGBG`),
/// parses the reply, and handles the timeout, tmux/screen wrapping and raw-mode
/// save/restore itself. Its 16-bit-per-channel result is reduced to 8-bit; the
/// foreground, when `COLORFGBG` provides one, is a luminance tiebreaker only.
fn detect_background(flag: ThemeFlag) -> Option<Bg> {
    if !matches!(flag, ThemeFlag::Auto) {
        return None;
    }
    let bg = termbg::rgb(BG_QUERY_TIMEOUT).ok()?;
    Some(Bg {
        bg: rgb16_to_rgb8(bg),
        fg: colorfgbg_foreground(),
    })
}

/// Reduce a `termbg` 16-bit-per-channel colour to 8-bit by taking the high byte.
fn rgb16_to_rgb8(c: termbg::Rgb) -> (u8, u8, u8) {
    ((c.r >> 8) as u8, (c.g >> 8) as u8, (c.b >> 8) as u8)
}

/// The foreground colour implied by `COLORFGBG` (`"<fg>;<bg>"` of palette
/// indices), as a coarse grey used only as a mid-tone tiebreaker. `None` when
/// the variable is absent or unparsable.
fn colorfgbg_foreground() -> Option<(u8, u8, u8)> {
    let raw = std::env::var("COLORFGBG").ok()?;
    let fg_index: u8 = raw.split(';').next()?.trim().parse().ok()?;
    Some(ansi_index_grey(fg_index))
}

/// A grey whose luminance approximates a 16-colour palette index: the normal
/// set (0..8) reads dark, the bright set (8..16) light. Only the relative
/// luminance matters, so an exact palette is unnecessary.
fn ansi_index_grey(index: u8) -> (u8, u8, u8) {
    let v = match index {
        0 => 0,
        7 => 192,
        8 => 96,
        15 => 255,
        i if i < 8 => 128,
        _ => 224,
    };
    (v, v, v)
}

/// Run the interactive TUI, owning the [`Session`] and driving navigation.
///
/// Does the initial fetch of `initial` and every subsequent navigation as a
/// background task; the task's result rejoins the `tokio::select!` loop as a
/// [`AppEvent::PageLoaded`] / [`AppEvent::LoadFailed`], so the UI stays live
/// (spinner animates, keys and the address editor work) while a page loads. A
/// cancel drops the in-flight task; a slow or failed fetch never blocks the UI.
pub async fn run_tui(
    session: Session,
    initial: Target,
    opts: BrowserOptions,
    theme_flag: ThemeFlag,
) -> io::Result<()> {
    install_panic_hook();

    // Resolve the theme BEFORE entering the alt-screen/raw event loop: the OSC 11
    // background query (via `termbg`) briefly toggles raw mode and reads a reply
    // on stdin, and doing it now keeps that reply from leaking into key input.
    // `termbg` restores the terminal state and drains stdin itself.
    let theme = resolve_theme(detect_background(theme_flag), theme_flag);

    let mut guard = TerminalGuard::new(CrosstermTerminal::new())?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut events = EventStream::new();

    // Load persisted bookmarks (best-effort): resolve the store path and read it,
    // tolerating a missing or corrupt file. `None` when no config dir is
    // resolvable, in which case bookmarks stay in-memory only and
    // `Effect::SaveBookmarks` becomes a no-op.
    let bookmarks_path = crate::bookmarks::default_path();
    let bookmarks = bookmarks_path
        .as_deref()
        .map(Bookmarks::load)
        .unwrap_or_default();

    let mut model = Model {
        no_color: opts.no_color,
        theme,
        bookmarks,
        bookmarks_path,
        ..Model::default()
    };
    let size = terminal.size()?;
    model.size = (size.width, size.height);
    model.relayout(content_width(size.width));

    let session = Arc::new(Mutex::new(session));
    let (tx, mut rx) = mpsc::unbounded_channel::<FetchOutcome>();
    let mut inflight: Option<tokio::task::JoinHandle<()>> = None;
    let mut generation: u64 = 0;

    // Kick off the initial navigation, honouring a `#anchor` on the initial URL
    // (the parser folds it into the path; split it back off so the fetched path
    // is clean and the load handler can scroll to it).
    let (initial, initial_anchor) = split_path_anchor(initial);
    model.pending = Some(Pending {
        target: initial.clone(),
        action: HistoryAction::Push,
    });
    model.pending_anchor = initial_anchor;
    spawn_fetch(
        &mut inflight,
        &mut generation,
        &session,
        &tx,
        opts.timeout,
        initial,
    );

    let mut ticker = tokio::time::interval(Duration::from_millis(SPINNER_TICK_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let result = loop {
        if let Err(err) = terminal.draw(|frame| view(&model, frame)) {
            break Err(err);
        }
        if model.quit {
            break Ok(());
        }
        // Tick while a fetch spinner animates or a toast is aging towards its
        // auto-dismiss, so an idle toast still expires.
        let animate = model.needs_tick();
        tokio::select! {
            maybe_event = events.next() => match step_event(maybe_event) {
                EventStep::Apply(app) => {
                    let effects = update(&mut model, app);
                    run_effects(
                        effects,
                        &mut model,
                        &mut inflight,
                        &mut generation,
                        &session,
                        &tx,
                        opts.timeout,
                    );
                }
                // A recognised-but-inert event (focus/paste, which map to
                // nothing): keep looping.
                EventStep::Ignore => {}
                // A closed stream (Ok) or a genuine IO error (Err) both leave
                // the loop; the error propagates so a dead terminal cannot
                // busy-loop.
                EventStep::End(result) => break result,
            },
            Some(outcome) = rx.recv() => {
                if outcome.generation == generation {
                    inflight = None;
                    match outcome.result {
                        LoadResult::Ok { doc, title } => {
                            update(&mut model, AppEvent::PageLoaded { doc, title });
                        }
                        LoadResult::Err(msg) => {
                            update(&mut model, AppEvent::LoadFailed(msg));
                        }
                    }
                }
            },
            _ = ticker.tick(), if animate => {
                update(&mut model, AppEvent::Tick);
            }
        }
    };

    // Tear down: abort and drain the in-flight task, then best-effort close the
    // session once no task clone of it survives.
    if let Some(handle) = inflight.take() {
        handle.abort();
        let _ = handle.await;
    }
    drop(tx);
    if let Ok(mutex) = Arc::try_unwrap(session) {
        let _ = mutex.into_inner().close().await;
    }

    guard.restore_now()?;
    result
}

/// Translate a crossterm event into an [`AppEvent`], dropping the ones the UI
/// does not act on (focus, paste).
fn map_event(event: Event) -> Option<AppEvent> {
    match event {
        Event::Key(key) => Some(AppEvent::Key(key)),
        Event::Mouse(mouse) => Some(AppEvent::Mouse(mouse)),
        Event::Resize(cols, rows) => Some(AppEvent::Resize(cols, rows)),
        _ => None,
    }
}

/// What the event loop should do with one item pulled from the terminal
/// [`EventStream`].
enum EventStep {
    /// Apply this app-event to the model (a mapped, actionable event).
    Apply(AppEvent),
    /// A recognised-but-inert event that maps to nothing: do nothing and keep
    /// looping. Crossterm discards unparseable byte sequences internally (its
    /// `parse_event` contract clears the buffer on error), so those never reach
    /// us as a `Some(Err)` — the only event we ignore is a mapped-to-`None` one
    /// (focus gained/lost, paste).
    Ignore,
    /// Leave the loop with this result: `Ok(())` on a clean close (the stream
    /// ended), `Err` on a genuine IO error from the event stream (a stdin/poll
    /// failure). Ignoring the latter would busy-loop on a dead terminal.
    End(io::Result<()>),
}

/// Classify one `EventStream` item without touching the model, so the loop's
/// exit behaviour is unit-testable. A `Some(Err)` is a real IO error (stdin or
/// poll failure) — crossterm never surfaces unparseable sequences as errors, it
/// discards them internally — so it is an exit, NOT ignore-and-continue:
/// ignoring it risks a busy-loop on a dead terminal. `None` (stream closed) is a
/// clean exit.
fn step_event(item: Option<Result<Event, io::Error>>) -> EventStep {
    match item {
        Some(Ok(event)) => match map_event(event) {
            Some(app) => EventStep::Apply(app),
            None => EventStep::Ignore,
        },
        Some(Err(err)) => EventStep::End(Err(err)),
        None => EventStep::End(Ok(())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};
    use leviculum_micron::parse;
    use ratatui::backend::TestBackend;
    use std::cell::Cell;
    use std::rc::Rc;

    const SAMPLE: &str = include_str!("../tests/fixtures/sample.mu");

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn press(model: &mut Model, code: KeyCode, mods: KeyModifiers) -> Vec<Effect> {
        update(model, AppEvent::Key(key(code, mods)))
    }

    fn type_str(model: &mut Model, s: &str) {
        for ch in s.chars() {
            press(model, KeyCode::Char(ch), KeyModifiers::NONE);
        }
    }

    fn tgt(n: u8) -> Target {
        Target {
            dest_hash: [n; 16],
            path: format!("/page/{n}.mu"),
            fields: Vec::new(),
            is_file: false,
        }
    }

    fn model_from_sample(width: usize, size: (u16, u16)) -> Model {
        Model::from_document(&parse(SAMPLE), width, "Sample", size)
    }

    /// The full buffer flattened to a newline-joined string.
    fn flat(buffer: &ratatui::buffer::Buffer) -> String {
        let mut s = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                s.push_str(buffer[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    /// The text of buffer row `y` across columns `0..width`.
    fn row_text(buffer: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
        let mut s = String::new();
        for x in 0..width {
            s.push_str(buffer[(x, y)].symbol());
        }
        s
    }

    /// The plain text of a laid-out line (its cells' characters).
    fn line_text(line: &RLine) -> String {
        line.cells.iter().map(|c| c.ch).collect()
    }

    #[test]
    fn quit_key_sets_quit_flag() {
        let mut model = model_from_sample(80, (80, 24));
        assert!(!model.quit);
        let effects = press(&mut model, KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(model.quit);
        assert_eq!(effects, vec![Effect::Quit]);
    }

    #[test]
    fn ctrl_c_sets_quit_flag() {
        let mut model = model_from_sample(80, (80, 24));
        let effects = press(&mut model, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(model.quit);
        assert_eq!(effects, vec![Effect::Quit]);
    }

    #[test]
    fn plain_c_does_not_quit() {
        let mut model = model_from_sample(80, (80, 24));
        press(&mut model, KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(!model.quit);
    }

    #[test]
    fn resize_updates_size() {
        let mut model = model_from_sample(80, (80, 24));
        update(&mut model, AppEvent::Resize(40, 10));
        assert_eq!(model.size, (40, 10));
    }

    #[test]
    fn content_region_is_total_minus_three() {
        assert_eq!(content_height(24), 21);
        let [topbar, content, status] = regions(Rect::new(0, 0, 80, 24));
        assert_eq!(topbar.height, TOPBAR_ROWS);
        assert_eq!(status.height, STATUS_ROWS);
        assert_eq!(content.height, 24 - 3);
        assert_eq!(model_from_sample(80, (80, 24)).viewport(), 21);
    }

    // --- History ---------------------------------------------------------

    #[test]
    fn history_visit_back_forward_and_truncation() {
        let mut h = History::default();
        assert!(!h.can_back());
        assert!(!h.can_forward());
        assert_eq!(h.current(), None);

        let (a, b, c) = (tgt(1), tgt(2), tgt(3));
        h.visit(a.clone());
        assert_eq!(h.current(), Some(&a));
        assert!(!h.can_back());

        h.visit(b.clone());
        assert!(h.can_back());
        assert!(!h.can_forward());
        assert_eq!(h.current(), Some(&b));

        assert_eq!(h.back(), Some(&a));
        assert_eq!(h.current(), Some(&a));
        assert!(h.can_forward());
        assert_eq!(h.forward(), Some(&b));

        // From the middle, a fresh visit truncates the forward branch.
        assert_eq!(h.back(), Some(&a));
        h.visit(c.clone());
        assert_eq!(h.current(), Some(&c));
        assert!(!h.can_forward());
        assert_eq!(h.stack.len(), 2);

        // back then forward lands on c, not the discarded b.
        assert_eq!(h.back(), Some(&a));
        assert_eq!(h.forward(), Some(&c));
    }

    #[test]
    fn history_edges_return_none() {
        let mut h = History::default();
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), None);
        h.visit(tgt(1));
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), None);
        assert_eq!(h.idx, 0);
    }

    // --- update / Effect -------------------------------------------------

    #[test]
    fn colon_enters_address_mode() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        let effects = press(&mut m, KeyCode::Char(':'), KeyModifiers::NONE);
        assert!(effects.is_empty());
        assert_eq!(m.mode, Mode::Address);
    }

    #[test]
    fn typing_then_enter_yields_navigate() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        press(&mut m, KeyCode::Char(':'), KeyModifiers::NONE);
        let hash = "0123456789abcdef0123456789abcdef";
        type_str(&mut m, &format!("{hash}:/page/about.mu"));
        let effects = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        let expected = parse_url(&format!("{hash}:/page/about.mu"), None).unwrap();
        assert_eq!(effects, vec![Effect::Navigate(expected.clone())]);
        assert_eq!(m.mode, Mode::Browse);
        assert_eq!(m.pending.as_ref().map(|p| &p.target), Some(&expected));
        assert!(m.input.value().is_empty());
    }

    #[test]
    fn bad_url_sets_error_and_no_navigate() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        press(&mut m, KeyCode::Char(':'), KeyModifiers::NONE);
        type_str(&mut m, "not-a-hash");
        let effects = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert!(effects.is_empty());
        let toast = m.toast.as_ref().expect("an error toast is raised");
        assert_eq!(toast.kind, ToastKind::Error);
        assert!(toast.text.contains("not-a-hash"), "toast: {}", toast.text);
        // Still in address mode so the user can fix the input.
        assert_eq!(m.mode, Mode::Address);
    }

    #[test]
    fn alt_left_on_history_yields_navigate_prev() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        let (a, b) = (tgt(1), tgt(2));
        m.history.visit(a.clone());
        m.history.visit(b.clone());
        m.current_dest = Some(b.dest_hash);

        let effects = press(&mut m, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(effects, vec![Effect::Navigate(a.clone())]);
        // The cursor only moves once the page loads.
        assert_eq!(m.history.idx, 1);
        assert_eq!(
            m.pending.as_ref().map(|p| p.action.clone()),
            Some(HistoryAction::Goto(0))
        );
    }

    #[test]
    fn mouse_back_button_on_history_yields_navigate_prev() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        let (a, b) = (tgt(1), tgt(2));
        m.history.visit(a.clone());
        m.history.visit(b.clone());
        m.current_dest = Some(b.dest_hash);

        let effects = update(
            &mut m,
            AppEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Back))),
        );
        assert_eq!(effects, vec![Effect::Navigate(a.clone())]);
        // The cursor only moves once the page loads (same path as Alt-Left).
        assert_eq!(m.history.idx, 1);
        assert_eq!(
            m.pending.as_ref().map(|p| p.action.clone()),
            Some(HistoryAction::Goto(0))
        );
    }

    #[test]
    fn mouse_forward_button_on_history_yields_navigate_next() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        let (a, b) = (tgt(1), tgt(2));
        m.history.visit(a.clone());
        m.history.visit(b);
        // Step back so a forward step exists.
        m.history.goto(0);
        m.current_dest = Some(a.dest_hash);

        let effects = update(
            &mut m,
            AppEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Forward))),
        );
        assert_eq!(effects, vec![Effect::Navigate(tgt(2))]);
        assert_eq!(m.history.idx, 0);
        assert_eq!(
            m.pending.as_ref().map(|p| p.action.clone()),
            Some(HistoryAction::Goto(1))
        );
    }

    #[test]
    fn mouse_side_buttons_are_noop_without_history() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        // No prior navigation: nothing behind or ahead.
        let back = update(
            &mut m,
            AppEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Back))),
        );
        let forward = update(
            &mut m,
            AppEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Forward))),
        );
        assert!(back.is_empty());
        assert!(forward.is_empty());
        assert!(m.pending.is_none());
    }

    #[test]
    fn esc_during_loading_yields_cancel() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.pending = Some(Pending {
            target: tgt(1),
            action: HistoryAction::Push,
        });
        let effects = press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(effects, vec![Effect::Cancel]);
        assert!(m.pending.is_none());
        let toast = m.toast.as_ref().expect("a cancel toast is raised");
        assert_eq!(toast.kind, ToastKind::Info);
        assert_eq!(toast.text, "cancelled");
    }

    #[test]
    fn ctrl_g_during_loading_yields_cancel() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.pending = Some(Pending {
            target: tgt(1),
            action: HistoryAction::Push,
        });
        let effects = press(&mut m, KeyCode::Char('g'), KeyModifiers::CONTROL);
        assert_eq!(effects, vec![Effect::Cancel]);
        assert!(m.pending.is_none());
    }

    #[test]
    fn page_loaded_updates_doc_pushes_history_resets_scroll() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.scroll = 5;
        m.pending = Some(Pending {
            target: tgt(7),
            action: HistoryAction::Push,
        });
        let effects = update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse("Fresh page body"),
                title: "Fresh".to_string(),
            },
        );
        assert!(effects.is_empty());
        assert_eq!(m.scroll, 0);
        assert_eq!(m.title, "Fresh");
        assert_eq!(m.history.stack.len(), 1);
        assert_eq!(m.history.stack[0], tgt(7));
        assert!(m.pending.is_none());
        assert_eq!(m.current_dest, Some([7; 16]));
        assert!(flat_page(&m).contains("Fresh"), "page: {:?}", flat_page(&m));
    }

    /// The current page's flattened text.
    fn flat_page(m: &Model) -> String {
        m.page.iter().map(line_text).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn load_failed_keeps_page_and_sets_status() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.pending = Some(Pending {
            target: tgt(1),
            action: HistoryAction::Push,
        });
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse("Kept page body"),
                title: "Kept".to_string(),
            },
        );
        let lines_before = m.page.len();

        m.pending = Some(Pending {
            target: tgt(2),
            action: HistoryAction::Push,
        });
        let effects = update(&mut m, AppEvent::LoadFailed("no path".to_string()));
        assert!(effects.is_empty());
        let toast = m.toast.as_ref().expect("a failure toast is raised");
        assert_eq!(toast.kind, ToastKind::Error);
        assert_eq!(toast.text, "no path");
        assert!(m.pending.is_none());
        // The page is unchanged: still the previously loaded document.
        assert_eq!(m.title, "Kept");
        assert_eq!(m.page.len(), lines_before);
        assert_eq!(m.history.stack.len(), 1);
    }

    #[test]
    fn reload_navigates_current_without_history_change() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.history.visit(tgt(3));
        let effects = press(&mut m, KeyCode::Char('R'), KeyModifiers::NONE);
        assert_eq!(effects, vec![Effect::Navigate(tgt(3))]);
        assert_eq!(
            m.pending.as_ref().map(|p| p.action.clone()),
            Some(HistoryAction::Goto(0))
        );
    }

    // --- page cache ------------------------------------------------------

    #[test]
    fn is_form_submit_flags_field_targets_only() {
        let submit = Target {
            dest_hash: [3; 16],
            path: "/page/s.mu".to_string(),
            fields: vec![("field_name".to_string(), "value".to_string())],
            is_file: false,
        };
        assert!(is_form_submit(&submit));
        // A `var_` preset is idempotent and cacheable.
        let preset = Target {
            dest_hash: [3; 16],
            path: "/page/s.mu".to_string(),
            fields: vec![("var_a".to_string(), "1".to_string())],
            is_file: false,
        };
        assert!(!is_form_submit(&preset));
        assert!(!is_form_submit(&tgt(1)));
    }

    #[test]
    fn page_loaded_inserts_into_the_cache() {
        let mut m = tall_model((80, 24));
        m.pending = Some(Pending {
            target: tgt(1),
            action: HistoryAction::Push,
        });
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: long_doc(),
                title: "A".to_string(),
            },
        );
        assert!(m.page_cache.contains(&tgt(1)), "fresh load is cached");
        assert!(!m.cached_view, "a fresh load is not a cached view");
    }

    #[test]
    fn fresh_navigation_to_cached_target_loads_from_cache_without_fetch() {
        let mut m = tall_model((80, 24));
        m.history.visit(tgt(1));
        m.current_dest = Some([1; 16]);
        m.page_cache.insert(
            tgt(2),
            CacheEntry {
                doc: long_doc(),
                title: "Two".to_string(),
                scroll: 9,
            },
        );
        let fx = begin_navigation(&mut m, tgt(2), HistoryAction::Push, None);
        assert!(fx.is_empty(), "a cache hit emits no fetch: {fx:?}");
        assert!(m.pending.is_none(), "no fetch is pending");
        assert_eq!(m.title, "Two");
        assert_eq!(m.scroll, 9, "the stored scroll is restored");
        assert!(m.cached_view, "the cached marker is set");
        // History is folded immediately (no async round-trip).
        assert_eq!(m.history.stack, vec![tgt(1), tgt(2)]);
        assert_eq!(m.history.idx, 1);
    }

    #[test]
    fn back_and_forward_hit_the_cache_and_restore_scroll() {
        // A tall, narrow viewport makes the sample document long enough that a
        // scroll offset of 12 is well within range (not clamped on restore).
        let mut m = tall_model((40, 13));
        // Load A through the fetch path so it caches.
        m.pending = Some(Pending {
            target: tgt(1),
            action: HistoryAction::Push,
        });
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: long_doc(),
                title: "A".to_string(),
            },
        );
        m.scroll = 12;

        // Navigate to B (a fresh fetch): leaving A stashes its scroll.
        let fx = begin_navigation(&mut m, tgt(2), HistoryAction::Push, None);
        assert_eq!(fx, vec![Effect::Navigate(tgt(2))]);
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: long_doc(),
                title: "B".to_string(),
            },
        );
        m.scroll = 3;

        // Back to A: instant from cache, scroll 12 restored, no fetch.
        let fx = press(&mut m, KeyCode::Left, KeyModifiers::ALT);
        assert!(fx.is_empty(), "back hit the cache: {fx:?}");
        assert!(m.pending.is_none());
        assert_eq!(m.history.idx, 0, "history moved immediately");
        assert_eq!(m.title, "A");
        assert_eq!(m.scroll, 12, "A's stashed scroll restored");
        assert!(m.cached_view);

        // Forward to B: instant, scroll 3 restored (stashed on leaving B).
        let fx = press(&mut m, KeyCode::Right, KeyModifiers::ALT);
        assert!(fx.is_empty(), "forward hit the cache: {fx:?}");
        assert_eq!(m.history.idx, 1);
        assert_eq!(m.title, "B");
        assert_eq!(m.scroll, 3);
        assert!(m.cached_view);
    }

    #[test]
    fn reload_bypasses_cache_and_overwrites_the_entry() {
        let mut m = tall_model((80, 24));
        m.history.visit(tgt(1));
        m.current_dest = Some([1; 16]);
        m.page_cache.insert(
            tgt(1),
            CacheEntry {
                doc: parse("old body"),
                title: "Old".to_string(),
                scroll: 3,
            },
        );
        // R refetches even though the page is cached.
        let fx = press(&mut m, KeyCode::Char('R'), KeyModifiers::NONE);
        assert_eq!(fx, vec![Effect::Navigate(tgt(1))]);
        assert_eq!(
            m.pending.as_ref().map(|p| p.action.clone()),
            Some(HistoryAction::Goto(0))
        );
        // On load the entry is overwritten with the fresh page.
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse("new body"),
                title: "New".to_string(),
            },
        );
        assert!(!m.cached_view, "a fresh load clears the cached marker");
        let hit = m.page_cache.get(&tgt(1)).expect("still cached");
        assert_eq!(hit.title, "New", "the entry was overwritten");
    }

    #[test]
    fn form_submit_is_never_cached_and_always_fetches() {
        let mut m = tall_model((80, 24));
        let submit = Target {
            dest_hash: [3; 16],
            path: "/page/s.mu".to_string(),
            fields: vec![("field_name".to_string(), "value".to_string())],
            is_file: false,
        };
        let fx = begin_navigation(&mut m, submit.clone(), HistoryAction::Push, None);
        assert_eq!(fx, vec![Effect::Navigate(submit.clone())], "always fetches");
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse("submitted"),
                title: "S".to_string(),
            },
        );
        assert!(
            !m.page_cache.contains(&submit),
            "a form submit result must not be cached"
        );
    }

    #[test]
    fn cache_hit_while_loading_cancels_the_inflight_fetch() {
        let mut m = tall_model((80, 24));
        m.history.visit(tgt(1));
        m.current_dest = Some([1; 16]);
        m.page_cache.insert(
            tgt(2),
            CacheEntry {
                doc: long_doc(),
                title: "Two".to_string(),
                scroll: 0,
            },
        );
        m.pending = Some(Pending {
            target: tgt(9),
            action: HistoryAction::Push,
        });
        let fx = begin_navigation(&mut m, tgt(2), HistoryAction::Push, None);
        assert_eq!(fx, vec![Effect::Cancel], "a stale fetch is cancelled");
        assert!(m.pending.is_none());
        assert_eq!(m.title, "Two");
    }

    #[test]
    fn cached_marker_shown_only_when_view_is_cached() {
        let mut m = loaded_model((80, 24));
        let fresh = flat(&render(&m, 80, 24));
        assert!(!fresh.contains("cached"), "no marker on a fresh page");
        m.cached_view = true;
        let cached = flat(&render(&m, 80, 24));
        assert!(
            cached.contains("cached"),
            "marker shown when cached:\n{cached}"
        );
    }

    // --- scrolling (retained from phase 2) -------------------------------

    #[test]
    fn apply_scroll_math() {
        let mut m = Model {
            page: vec![RLine::default(); 100],
            ..Model::default()
        };
        let vp = 10;
        m.apply_scroll(ScrollCmd::LineDown, vp);
        assert_eq!(m.scroll, 1, "LineDown increments");

        m.scroll = 0;
        m.apply_scroll(ScrollCmd::PageDown, vp);
        assert_eq!(m.scroll, 10, "PageDown moves by viewport");

        m.scroll = 0;
        m.apply_scroll(ScrollCmd::HalfPageDown, vp);
        assert_eq!(m.scroll, 5, "HalfPage moves by viewport/2");

        m.scroll = 42;
        m.apply_scroll(ScrollCmd::Top, vp);
        assert_eq!(m.scroll, 0, "Top is 0");

        m.apply_scroll(ScrollCmd::Bottom, vp);
        assert_eq!(m.scroll, 90, "Bottom is max = len - viewport");

        m.apply_scroll(ScrollCmd::LineDown, vp);
        assert_eq!(m.scroll, 90, "LineDown clamps at bottom");
        m.apply_scroll(ScrollCmd::PageDown, vp);
        assert_eq!(m.scroll, 90, "PageDown clamps at bottom");

        m.scroll = 0;
        m.apply_scroll(ScrollCmd::LineUp, vp);
        assert_eq!(m.scroll, 0, "LineUp clamps at top");
        m.apply_scroll(ScrollCmd::PageUp, vp);
        assert_eq!(m.scroll, 0, "PageUp clamps at top");
    }

    #[test]
    fn apply_scroll_no_overflow_when_page_shorter_than_viewport() {
        let mut m = Model {
            page: vec![RLine::default(); 3],
            ..Model::default()
        };
        let vp = 10;
        for cmd in [
            ScrollCmd::LineDown,
            ScrollCmd::PageDown,
            ScrollCmd::HalfPageDown,
            ScrollCmd::Bottom,
        ] {
            m.scroll = 0;
            m.apply_scroll(cmd, vp);
            assert_eq!(m.scroll, 0, "{cmd:?} must clamp to 0 for a short page");
        }
        let mut e = Model::default();
        e.apply_scroll(ScrollCmd::PageDown, 0);
        e.apply_scroll(ScrollCmd::Bottom, 0);
        assert_eq!(e.scroll, 0);
    }

    fn long_doc() -> leviculum_micron::MicronDocument {
        let words: Vec<String> = (0..300).map(|i| format!("word{i:03}")).collect();
        parse(&words.join(" "))
    }

    fn tall_model(size: (u16, u16)) -> Model {
        Model::from_document(&long_doc(), content_width(size.0), "Long", size)
    }

    #[test]
    fn update_scroll_keys() {
        let mut m = tall_model((40, 13)); // viewport 13 - 3 = 10
        let vp = m.viewport();
        assert_eq!(vp, 10);
        assert_eq!(m.scroll, 0);

        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(m.scroll, 1, "j scrolls down one line");

        press(&mut m, KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert_eq!(m.scroll, 1 + vp, "Ctrl-f pages down by the viewport");

        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        let bottom = m.max_scroll(vp);
        assert!(bottom > 0);
        assert_eq!(m.scroll, bottom, "G jumps to the bottom");

        press(&mut m, KeyCode::Char('v'), KeyModifiers::ALT);
        assert_eq!(m.scroll, bottom - vp, "Alt-v pages up by the viewport");

        let before = m.scroll;
        update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollDown)));
        assert_eq!(
            m.scroll,
            (before + WHEEL_STEP).min(bottom),
            "wheel down scrolls by WHEEL_STEP"
        );
    }

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn vi_and_emacs_map_same_command() {
        let cases = [
            (
                key(KeyCode::Char('j'), KeyModifiers::NONE),
                key(KeyCode::Char('n'), KeyModifiers::CONTROL),
            ),
            (
                key(KeyCode::Char('k'), KeyModifiers::NONE),
                key(KeyCode::Char('p'), KeyModifiers::CONTROL),
            ),
            (
                key(KeyCode::Char('f'), KeyModifiers::CONTROL),
                key(KeyCode::Char(' '), KeyModifiers::NONE),
            ),
            (
                key(KeyCode::Char('b'), KeyModifiers::CONTROL),
                key(KeyCode::Char('v'), KeyModifiers::ALT),
            ),
            (
                key(KeyCode::Char('g'), KeyModifiers::NONE),
                key(KeyCode::Home, KeyModifiers::NONE),
            ),
            (
                key(KeyCode::Char('G'), KeyModifiers::NONE),
                key(KeyCode::End, KeyModifiers::NONE),
            ),
        ];
        for (vi, emacs) in cases {
            let cmd = key_to_scroll(&vi);
            assert!(cmd.is_some(), "{vi:?} should map to a scroll command");
            assert_eq!(cmd, key_to_scroll(&emacs), "{vi:?} vs {emacs:?}");
        }
    }

    #[test]
    fn resize_rewraps_and_clamps() {
        let mut m = Model::from_document(&long_doc(), content_width(100), "Long", (100, 20));
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);

        update(&mut m, AppEvent::Resize(40, 20));
        let narrow = m.page.len();
        assert!(
            m.scroll <= m.max_scroll(m.viewport()),
            "scroll must stay clamped after shrinking"
        );

        update(&mut m, AppEvent::Resize(100, 20));
        let wide = m.page.len();
        assert!(
            m.scroll <= m.max_scroll(m.viewport()),
            "scroll must stay clamped after growing"
        );
        assert!(
            narrow > wide,
            "a narrower width must re-wrap into more lines: {narrow} vs {wide}"
        );
    }

    // --- view (TestBackend) ---------------------------------------------

    /// A loaded model with a seeded history entry, ready to render.
    fn loaded_model(size: (u16, u16)) -> Model {
        let mut m = Model::from_document(
            &parse(SAMPLE),
            content_width(size.0),
            " Test Node · :/page/index.mu ",
            size,
        );
        m.history.visit(tgt(0xab));
        m.current_dest = Some([0xab; 16]);
        m
    }

    fn render(model: &Model, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| view(model, frame)).expect("draw");
        terminal.backend().buffer().clone()
    }

    #[test]
    fn topbar_renders_title_and_controls() {
        let m = loaded_model((80, 24));
        let buffer = render(&m, 80, 24);
        let text = flat(&buffer);
        assert!(text.contains("Test Node"), "title missing:\n{text}");
        assert!(text.contains(":/page/index.mu"), "path missing:\n{text}");
        assert!(text.contains("back"), "back control missing:\n{text}");
        assert!(text.contains("forward"), "forward control missing:\n{text}");
        assert!(text.contains("reload"), "reload control missing:\n{text}");
    }

    #[test]
    fn status_bar_renders_hints() {
        let m = loaded_model((80, 24));
        let buffer = render(&m, 80, 24);
        let status = row_text(&buffer, 23, 80);
        assert!(status.contains("quit"), "hints missing: {status:?}");
    }

    #[test]
    fn loading_state_renders_spinner_and_text() {
        let mut m = loaded_model((80, 24));
        m.pending = Some(Pending {
            target: tgt(9),
            action: HistoryAction::Push,
        });
        let buffer = render(&m, 80, 24);
        let status = row_text(&buffer, 23, 80);
        assert!(status.contains("loading"), "loading missing: {status:?}");
    }

    #[test]
    fn hsv_to_rgb_known_conversions() {
        // The primary and secondary hues at full saturation and value.
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), (255, 0, 0), "h=0 -> red");
        assert_eq!(hsv_to_rgb(60.0, 1.0, 1.0), (255, 255, 0), "h=60 -> yellow");
        assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), (0, 255, 0), "h=120 -> green");
        assert_eq!(hsv_to_rgb(180.0, 1.0, 1.0), (0, 255, 255), "h=180 -> cyan");
        assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), (0, 0, 255), "h=240 -> blue");
        assert_eq!(
            hsv_to_rgb(300.0, 1.0, 1.0),
            (255, 0, 255),
            "h=300 -> magenta"
        );
        // Hue wraps: 360 == 0, and negatives fold back into range.
        assert_eq!(
            hsv_to_rgb(360.0, 1.0, 1.0),
            (255, 0, 0),
            "h=360 wraps to red"
        );
        assert_eq!(
            hsv_to_rgb(-60.0, 1.0, 1.0),
            (255, 0, 255),
            "h=-60 -> magenta"
        );
        // s=1, v=1 always yields a fully saturated, full-brightness colour: one
        // channel at 255 and at least one at 0.
        for deg in (0..360).step_by(7) {
            let (r, g, b) = hsv_to_rgb(deg as f32, 1.0, 1.0);
            assert_eq!(r.max(g).max(b), 255, "not full brightness at h={deg}");
            assert_eq!(r.min(g).min(b), 0, "not fully saturated at h={deg}");
        }
    }

    #[test]
    fn spinner_span_glides_smoothly_through_the_spectrum() {
        // The glyph steps through the six braille frames as `spin` advances.
        for spin in 0..12 {
            assert_eq!(
                &*spinner_span(spin).content,
                SPIN_FRAMES[spin % SPIN_FRAMES.len()],
                "glyph out of cycle at spin={spin}"
            );
        }
        // Every frame is bold and carries the HSV colour for its hue.
        let cycle = (360.0 / HUE_STEP).ceil() as usize;
        for spin in 0..cycle {
            let style = spinner_span(spin).style;
            assert!(
                style.add_modifier.contains(Modifier::BOLD),
                "spinner not bold at spin={spin}"
            );
            let hue = (spin as f32 * HUE_STEP).rem_euclid(360.0);
            let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);
            assert_eq!(
                style.fg,
                Some(Color::Rgb(r, g, b)),
                "wrong hsv fg at spin={spin}"
            );
        }
        // The colour glides smoothly: consecutive ticks shift the hue by only the
        // small HUE_STEP, so each RGB channel moves by a bounded amount (no hard
        // jumps). A single HUE_STEP spans at most one 60-degree sextant, over which
        // a channel changes by at most 255.
        let max_delta = (HUE_STEP / 60.0 * 255.0).ceil() as i32 + 1;
        for spin in 0..cycle {
            let a = spinner_span(spin).style.fg;
            let b = spinner_span(spin + 1).style.fg;
            let (Some(Color::Rgb(ar, ag, ab)), Some(Color::Rgb(br, bg, bb))) = (a, b) else {
                panic!("spinner fg not an Rgb colour at spin={spin}");
            };
            assert_ne!(a, b, "hue did not change between spin={spin} and next");
            for (x, y, ch) in [(ar, br, "r"), (ag, bg, "g"), (ab, bb, "b")] {
                let d = (x as i32 - y as i32).abs();
                assert!(
                    d <= max_delta,
                    "channel {ch} jumped by {d} (> {max_delta}) at spin={spin}"
                );
            }
        }
        // A full cycle spans the whole spectrum: the hue reaches both ends.
        let hues: Vec<f32> = (0..cycle)
            .map(|s| (s as f32 * HUE_STEP).rem_euclid(360.0))
            .collect();
        let min = hues.iter().cloned().fold(f32::MAX, f32::min);
        let max = hues.iter().cloned().fold(f32::MIN, f32::max);
        assert!(min < 30.0, "cycle does not start near hue 0 (min={min})");
        assert!(max > 330.0, "cycle does not reach the far end (max={max})");
    }

    #[test]
    fn loading_status_bar_has_rainbow_bold_spinner_cell() {
        let mut m = loaded_model((80, 24));
        m.pending = Some(Pending {
            target: tgt(9),
            action: HistoryAction::Push,
        });
        m.spin = 3;
        let hue = (m.spin as f32 * HUE_STEP).rem_euclid(360.0);
        let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);
        let buffer = render(&m, 80, 24);
        let mut found = false;
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, 23)];
            if cell.modifier.contains(Modifier::BOLD) && cell.fg == Color::Rgb(r, g, b) {
                found = true;
            }
        }
        assert!(
            found,
            "no bold rainbow spinner cell in the loading status bar"
        );
    }

    #[test]
    fn error_toast_renders_over_content_and_status_keeps_hints() {
        let mut m = loaded_model((80, 24));
        m.set_toast(ToastKind::Error, "no path to destination");
        let buffer = render(&m, 80, 24);

        // The toast text and its warning glyph render somewhere in the content.
        let all = flat(&buffer);
        assert!(all.contains("no path"), "toast text missing:\n{all}");
        assert!(all.contains('⚠'), "error glyph missing:\n{all}");

        // The attention red is painted on the toast cells.
        let mut found_error_style = false;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if buffer[(x, y)].fg == Color::Rgb(255, 95, 95) {
                    found_error_style = true;
                }
            }
        }
        assert!(found_error_style, "error style not painted");

        // The status bar (row 23) still shows the key-hints, not the message.
        let status = row_text(&buffer, 23, 80);
        assert!(status.contains("quit"), "hints missing: {status:?}");
        assert!(
            !status.contains("no path"),
            "message leaked into status bar"
        );
    }

    #[test]
    fn set_toast_stores_kind_and_text() {
        let mut m = Model::default();
        m.set_toast(ToastKind::Info, "copied aa11:/page/x.mu");
        let toast = m.toast.as_ref().expect("toast stored");
        assert_eq!(toast.kind, ToastKind::Info);
        assert_eq!(toast.text, "copied aa11:/page/x.mu");
        assert_eq!(toast.shown_at, m.now_tick);
    }

    #[test]
    fn toast_expires_after_the_timeout() {
        let mut m = Model::default();
        m.set_toast(ToastKind::Info, "copied");
        // Just short of the lifetime it is still up.
        for _ in 0..TOAST_TICKS - 1 {
            update(&mut m, AppEvent::Tick);
        }
        assert!(m.toast.is_some(), "toast dismissed too early");
        // One more tick crosses the lifetime and clears it.
        update(&mut m, AppEvent::Tick);
        assert!(m.toast.is_none(), "toast should have expired");
    }

    #[test]
    fn any_key_dismisses_the_toast() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.set_toast(ToastKind::Info, "bookmarked");
        // A plain scroll key is a navigation event: it clears the toast.
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(m.toast.is_none(), "a key event should dismiss the toast");
    }

    #[test]
    fn needs_tick_tracks_loading_and_toast() {
        let mut m = Model::default();
        assert!(!m.needs_tick(), "idle model needs no tick");
        m.set_toast(ToastKind::Info, "up");
        assert!(m.needs_tick(), "an active toast must drive the tick");
    }

    #[test]
    fn bad_link_follow_sets_error_toast_with_url_not_status_bar() {
        // A same-destination link with no current destination fails to resolve.
        let mut m = loaded_model((80, 24));
        m.current_dest = None;
        m.links = vec![RenderedLink {
            index: 1,
            label: "Broken".to_string(),
            target: ":/page/broken.mu".to_string(),
            ..RenderedLink::default()
        }];
        let effects = follow_link(&mut m, 1);
        assert!(effects.is_empty(), "a broken link must not navigate");
        let toast = m.toast.as_ref().expect("a bad-link toast is raised");
        assert_eq!(toast.kind, ToastKind::Error);
        assert!(
            toast.text.contains(":/page/broken.mu"),
            "toast must carry the offending url: {}",
            toast.text
        );

        // The offending url shows in the toast overlay, never in the status bar.
        let buffer = render(&m, 80, 24);
        let status = row_text(&buffer, 23, 80);
        assert!(
            !status.contains(":/page/broken.mu"),
            "the url leaked into the status bar: {status:?}"
        );
        assert!(
            status.contains("quit"),
            "status bar lost its hints: {status:?}"
        );
    }

    #[test]
    fn view_renders_heading_and_underlined_link_label() {
        let model = model_from_sample(content_width(80), (80, 24));
        let buffer = render(&model, 80, 24);
        let text = flat(&buffer);
        assert!(text.contains("Sample Page"), "heading missing:\n{text}");
        assert!(text.contains("Alpha"), "link label missing:\n{text}");

        let link_fg = Color::Rgb(0, 175, 255);
        let mut found_styled_link = false;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "A"
                    && cell.fg == link_fg
                    && cell.modifier.contains(Modifier::UNDERLINED)
                {
                    found_styled_link = true;
                }
            }
        }
        assert!(
            found_styled_link,
            "no underlined LINK_FG 'A' cell found:\n{text}"
        );
    }

    #[test]
    fn light_theme_view_uses_light_link_and_heading_band() {
        let mut model = model_from_sample(content_width(80), (80, 24));
        model.toggle_theme();
        assert_eq!(model.theme, Theme::Light);
        let buffer = render(&model, 80, 24);

        // The link label 'A' now carries the light (deep-blue) link colour.
        let light_link = Color::Rgb(0, 90, 170);
        let mut found_light_link = false;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "A"
                    && cell.fg == light_link
                    && cell.modifier.contains(Modifier::UNDERLINED)
                {
                    found_light_link = true;
                }
            }
        }
        assert!(found_light_link, "no light-blue underlined link cell found");

        // The depth-1 "Sample Page" heading carries the light band bg #777777
        // (the dark theme would band it #bbbbbb instead).
        let light_band_bg = Color::Rgb(0x77, 0x77, 0x77);
        let has_light_band = (0..buffer.area.height)
            .any(|y| (0..buffer.area.width).any(|x| buffer[(x, y)].bg == light_band_bg));
        assert!(has_light_band, "light heading band not found");
        // The dark depth-1 band must not appear under the light theme.
        let dark_band_bg = Color::Rgb(0xbb, 0xbb, 0xbb);
        let has_dark_band = (0..buffer.area.height)
            .any(|y| (0..buffer.area.width).any(|x| buffer[(x, y)].bg == dark_band_bg));
        assert!(!has_dark_band, "dark heading band leaked into light theme");
    }

    #[test]
    fn t_toggles_theme_and_view_reflects_it() {
        let mut m = loaded_model((80, 24));
        assert_eq!(m.theme, Theme::Dark);

        // `t` flips the model to the light theme...
        let effects = press(&mut m, KeyCode::Char('t'), KeyModifiers::NONE);
        assert!(effects.is_empty());
        assert_eq!(m.theme, Theme::Light);
        let light_bg = rgb(Theme::Light.chrome_bg());
        let buffer = render(&m, 80, 24);
        assert_eq!(
            buffer[(0, 0)].bg,
            light_bg,
            "view must reflect the light chrome after toggling"
        );

        // ...and `t` again flips back to dark.
        press(&mut m, KeyCode::Char('t'), KeyModifiers::NONE);
        assert_eq!(m.theme, Theme::Dark);
        let dark_bg = rgb(Theme::Dark.chrome_bg());
        let buffer = render(&m, 80, 24);
        assert_eq!(
            buffer[(0, 0)].bg,
            dark_bg,
            "view must return to dark chrome"
        );
    }

    #[test]
    fn view_scrolls_slice_below_topbar_and_draws_scrollbar() {
        let (w, h) = (40u16, 13u16);
        let mut m = Model::from_document(&long_doc(), content_width(w), "Long", (w, h));
        assert!(
            m.page.len() > m.viewport(),
            "fixture must exceed the viewport"
        );
        let content_top = TOPBAR_ROWS; // content begins after the two-row top-bar

        let buffer = render(&m, w, h);
        let top0 = row_text(&buffer, content_top, w - SCROLLBAR_COLS);
        assert_eq!(top0.trim_end(), line_text(&m.page[0]).trim_end());

        let vp = m.viewport();
        press(&mut m, KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert_eq!(m.scroll, vp);
        let buffer = render(&m, w, h);
        let top1 = row_text(&buffer, content_top, w - SCROLLBAR_COLS);
        assert_eq!(top1.trim_end(), line_text(&m.page[vp]).trim_end());

        // The scrollbar occupies the reserved right-hand column of the content.
        let mut scrollbar_cell = false;
        for y in content_top..h {
            if buffer[(w - SCROLLBAR_COLS, y)].symbol() != " " {
                scrollbar_cell = true;
            }
        }
        assert!(scrollbar_cell, "scrollbar column should not be empty");
    }

    #[test]
    fn scrollbar_thumb_reaches_top_and_bottom() {
        // A page much taller than the viewport, so the thumb is a small handle
        // that must travel the whole track as `scroll` runs `0..=max_scroll`.
        let (w, h) = (40u16, 13u16);
        let mut m = Model::from_document(&long_doc(), content_width(w), "Long", (w, h));
        assert!(
            m.page.len() > m.viewport() * 2,
            "fixture must be much taller than the viewport"
        );

        let col = w - SCROLLBAR_COLS; // rightmost column carries the scrollbar
        let content_top = TOPBAR_ROWS; // first content row (below the two-row top-bar)
        let content_bottom = h - STATUS_ROWS - 1; // last content row (above the status bar)

        // The rows in the scrollbar column occupied by the thumb glyph (block).
        let thumb_rows = |buf: &ratatui::buffer::Buffer| -> Vec<u16> {
            (content_top..=content_bottom)
                .filter(|&y| buf[(col, y)].symbol() == "█")
                .collect()
        };

        // scroll = 0: the thumb sits at the TOP of the track, not the bottom.
        m.scroll = 0;
        let buf = render(&m, w, h);
        let top = thumb_rows(&buf);
        assert!(!top.is_empty(), "thumb should be visible at scroll 0");
        assert_eq!(
            *top.first().unwrap(),
            content_top,
            "thumb top must hit the track top at scroll 0"
        );
        assert!(
            *top.last().unwrap() < content_bottom,
            "thumb must not fill to the bottom at scroll 0"
        );

        // scroll = max_scroll: the thumb BOTTOM reaches the bottom row of the track.
        let vp = m.viewport();
        m.scroll = m.max_scroll(vp);
        assert!(m.scroll > 0, "max_scroll must be positive for a tall page");
        let buf = render(&m, w, h);
        let bottom = thumb_rows(&buf);
        assert!(!bottom.is_empty(), "thumb should be visible at max scroll");
        assert_eq!(
            *bottom.last().unwrap(),
            content_bottom,
            "thumb bottom must reach the track bottom at max scroll"
        );
        assert!(
            *bottom.first().unwrap() > content_top,
            "thumb must have left the top at max scroll"
        );

        // A mid value puts the thumb strictly between the two extremes.
        m.scroll = m.max_scroll(vp) / 2;
        let buf = render(&m, w, h);
        let mid = thumb_rows(&buf);
        assert!(!mid.is_empty(), "thumb should be visible mid-scroll");
        assert!(
            *mid.first().unwrap() > content_top && *mid.last().unwrap() < content_bottom,
            "mid-scroll thumb must sit between the track ends: {mid:?}"
        );
    }

    #[test]
    fn chrome_bars_carry_background_full_width() {
        let m = loaded_model((80, 24));
        assert!(!m.no_color, "colour is on by default");
        assert_eq!(m.theme, Theme::Dark, "dark is the default theme");
        let dark_bg = rgb(Theme::Dark.chrome_bg());
        let buffer = render(&m, 80, 24);
        // The two top-bar rows (0, 1) and the status row (23) carry the chrome
        // background across the full width, including the rightmost column.
        for &y in &[0u16, 1, 23] {
            for &x in &[0u16, 40, 79] {
                assert_eq!(
                    buffer[(x, y)].bg,
                    dark_bg,
                    "cell ({x},{y}) is missing the chrome background"
                );
            }
        }
    }

    #[test]
    fn chrome_bars_carry_light_background_under_light_theme() {
        let mut m = loaded_model((80, 24));
        m.toggle_theme();
        assert_eq!(m.theme, Theme::Light);
        let light_bg = rgb(Theme::Light.chrome_bg());
        let buffer = render(&m, 80, 24);
        for &y in &[0u16, 1, 23] {
            for &x in &[0u16, 40, 79] {
                assert_eq!(
                    buffer[(x, y)].bg,
                    light_bg,
                    "cell ({x},{y}) is missing the light chrome background"
                );
            }
        }
    }

    #[test]
    fn chrome_bars_use_reverse_under_no_color() {
        let dark_bg = rgb(Theme::Dark.chrome_bg());
        for theme in [Theme::Dark, Theme::Light] {
            let mut m = loaded_model((80, 24));
            m.no_color = true;
            m.theme = theme;
            m.relayout(m.width);
            let buffer = render(&m, 80, 24);
            for &y in &[0u16, 1, 23] {
                for &x in &[0u16, 40, 79] {
                    let cell = &buffer[(x, y)];
                    assert!(
                        cell.modifier.contains(Modifier::REVERSED),
                        "cell ({x},{y}) must be reversed under no_color ({theme:?})"
                    );
                    assert_ne!(
                        cell.bg, dark_bg,
                        "no_color must not paint the colour background ({theme:?})"
                    );
                }
            }
        }
    }

    #[test]
    fn status_hints_use_bright_chrome_fg_not_dim() {
        // The status-bar key-hints render in the theme's bright chrome fg with no
        // DIM modifier, under both themes. loaded_model has no focus/hover/status
        // pending, so the hints branch is what draws row 23.
        for theme in [Theme::Dark, Theme::Light] {
            let mut m = loaded_model((80, 24));
            if theme == Theme::Light {
                m.toggle_theme();
            }
            assert_eq!(m.theme, theme);
            let buffer = render(&m, 80, 24);
            // (0,23) is the first hint glyph ('j' of "j/k scroll ...").
            let cell = &buffer[(0u16, 23u16)];
            assert_eq!(
                cell.fg,
                rgb(theme.chrome_fg()),
                "status hint must use the bright chrome fg ({theme:?})"
            );
            assert!(
                !cell.modifier.contains(Modifier::DIM),
                "status hint must not be DIM ({theme:?})"
            );
        }
    }

    #[test]
    fn unavailable_control_uses_muted_fg_not_dim() {
        // loaded_model has a single history entry, so back/forward are both
        // unavailable: they must render in the readable muted chrome fg, not DIM.
        for theme in [Theme::Dark, Theme::Light] {
            let mut m = loaded_model((80, 24));
            if theme == Theme::Light {
                m.toggle_theme();
            }
            assert!(
                !m.history.can_back(),
                "back must be unavailable in this model"
            );
            let buffer = render(&m, 80, 24);
            // (0,1) is the first glyph of the back control on the top-bar's
            // second row.
            let cell = &buffer[(0u16, 1u16)];
            assert_eq!(
                cell.fg,
                rgb(theme.chrome_muted_fg()),
                "unavailable control must use the muted chrome fg ({theme:?})"
            );
            assert!(
                !cell.modifier.contains(Modifier::DIM),
                "unavailable control must not be DIM ({theme:?})"
            );
        }
    }

    #[test]
    fn step_event_exits_on_stream_error_and_close() {
        // A Some(Err) from the event stream is a genuine IO error (stdin/poll
        // failure); crossterm discards unparseable sequences internally and
        // never surfaces them as errors. So it EXITS the loop, carrying the
        // error, rather than being ignored (which would busy-loop a dead tty).
        let err = io::Error::other("poll failure");
        assert!(matches!(step_event(Some(Err(err))), EventStep::End(Err(_))));
        // A closed stream (None) is a clean exit.
        assert!(matches!(step_event(None), EventStep::End(Ok(()))));
        // A real key still maps to an applied app-event.
        let ev = Event::Key(key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(
            step_event(Some(Ok(ev))),
            EventStep::Apply(AppEvent::Key(_))
        ));
    }

    #[test]
    fn help_overlay_renders_keybindings() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(m.show_help);
        let buffer = render(&m, 80, 24);
        let text = flat(&buffer);
        assert!(text.contains("help"), "help title missing:\n{text}");
        assert!(text.contains("quit"), "help body missing:\n{text}");
        // Esc dismisses it.
        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.show_help);
    }

    #[test]
    fn help_overlay_widens_to_the_longest_line() {
        let mut m = loaded_model((100, 30));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        let buffer = render(&m, 100, 30);
        let text = flat(&buffer);
        // The widest description renders contiguously on one row: at the old fixed
        // width of 44 it wrapped ("toggle light / dark" | "theme").
        assert!(
            text.lines()
                .any(|row| row.contains("toggle light / dark theme")),
            "widest help line wrapped:\n{text}"
        );
        // The overlay is at least the widest line plus its two borders wide.
        let inner = HELP_LINES
            .iter()
            .map(|l| UnicodeWidthStr::width(*l))
            .max()
            .unwrap();
        let border = text
            .lines()
            .find(|row| row.contains('┌'))
            .expect("top border row");
        let chars: Vec<char> = border.chars().collect();
        let start = chars.iter().position(|&c| c == '┌').unwrap();
        let end = chars.iter().position(|&c| c == '┐').unwrap();
        let width = end - start + 1;
        assert!(
            width >= inner + 2,
            "overlay width {width} < longest line {inner} + 2",
        );
    }

    #[test]
    fn address_mode_renders_editor() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Char(':'), KeyModifiers::NONE);
        type_str(&mut m, "abc");
        let buffer = render(&m, 80, 24);
        let text = flat(&buffer);
        assert!(text.contains("abc"), "editor content missing:\n{text}");
    }

    // --- phase 4: visible_links, hints, focus, mouse --------------------

    /// A left-button mouse-down event at `(col, row)`.
    fn click(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// A mouse-move event at `(col, row)`.
    fn moved(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// The laid-out link with 1-based `index`.
    fn link_of(m: &Model, index: usize) -> &RenderedLink {
        m.links.iter().find(|l| l.index == index).expect("link")
    }

    #[test]
    fn visible_links_map_content_lines_to_screen_rects() {
        let m = model_from_sample(content_width(80), (80, 24));
        let vls = visible_links(&m);
        assert_eq!(vls.len(), 2, "sample has two links");
        for vl in &vls {
            let link = link_of(&m, vl.index);
            // row = top-bar rows + (content line - scroll); scroll is 0 here.
            assert_eq!(vl.row, TOPBAR_ROWS + link.line as u16);
            assert_eq!(vl.col_start, link.col_start as u16);
            assert_eq!(vl.col_end, link.col_end as u16);
        }
    }

    #[test]
    fn visible_links_exclude_off_viewport() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        // Scroll past both links (they sit near the top of the page).
        m.scroll = link_of(&m, 2).line + 1;
        assert!(
            visible_links(&m).is_empty(),
            "links above the scroll must be excluded"
        );
    }

    #[test]
    fn visible_links_partial_scroll_keeps_only_lower_link() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        let beta_line = link_of(&m, 2).line;
        m.scroll = beta_line;
        let vls = visible_links(&m);
        assert_eq!(vls.len(), 1, "only the lower link stays visible");
        assert_eq!(vls[0].index, 2);
        // The now-topmost visible link sits at the content top row.
        assert_eq!(vls[0].row, TOPBAR_ROWS);
    }

    #[test]
    fn hint_labels_are_single_char_when_few() {
        let labels = hint_labels(3);
        assert_eq!(labels, vec!["a", "s", "d"]);
        // All nine of the alphabet fit as single characters.
        assert!(hint_labels(9).iter().all(|l| l.len() == 1));
        let nine = hint_labels(9);
        assert_eq!(nine.len(), 9);
        assert_eq!(
            nine.iter().collect::<std::collections::HashSet<_>>().len(),
            9
        );
    }

    #[test]
    fn hint_labels_are_two_char_when_many() {
        let labels = hint_labels(10);
        assert_eq!(labels.len(), 10);
        assert!(
            labels.iter().all(|l| l.chars().count() == 2),
            "got: {labels:?}"
        );
        assert_eq!(labels[0], "aa");
        // Unique across the whole set.
        let uniq: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(uniq.len(), 10);
    }

    #[test]
    fn hints_cover_visible_links_then_controls_with_unique_labels() {
        let m = loaded_model((80, 24));
        let hs = hints(&m);
        // Two links + four top-bar controls.
        assert_eq!(hs.len(), 6);
        assert!(matches!(hs[0].target, HintTarget::Link(1)));
        assert!(matches!(hs[1].target, HintTarget::Link(2)));
        assert!(matches!(hs[2].target, HintTarget::Control(Control::Back)));
        assert!(matches!(
            hs[5].target,
            HintTarget::Control(Control::Address)
        ));
        let labels: std::collections::HashSet<_> = hs.iter().map(|h| &h.label).collect();
        assert_eq!(labels.len(), 6, "labels must be unique");
    }

    #[test]
    fn f_enters_hint_mode() {
        let mut m = loaded_model((80, 24));
        let effects = press(&mut m, KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(effects.is_empty());
        assert_eq!(m.mode, Mode::Hint);
    }

    #[test]
    fn typing_a_unique_label_follows_that_link() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Char('f'), KeyModifiers::NONE);
        // The first link's label is 'a'.
        let effects = press(&mut m, KeyCode::Char('a'), KeyModifiers::NONE);
        let expected = browser::resolve_link(link_of(&m, 1), m.current_dest)
            .unwrap()
            .0;
        assert_eq!(effects, vec![Effect::Navigate(expected.clone())]);
        assert_eq!(m.mode, Mode::Browse, "hint mode exits after a match");
        assert_eq!(m.pending.as_ref().map(|p| &p.target), Some(&expected));
    }

    #[test]
    fn typing_link_text_follows_that_link() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Char('f'), KeyModifiers::NONE);
        // 'b' matches no label but uniquely matches the "• Beta" link text.
        let effects = press(&mut m, KeyCode::Char('b'), KeyModifiers::NONE);
        let expected = browser::resolve_link(link_of(&m, 2), m.current_dest)
            .unwrap()
            .0;
        assert_eq!(effects, vec![Effect::Navigate(expected)]);
        assert_eq!(m.mode, Mode::Browse);
    }

    #[test]
    fn non_matching_hint_key_is_ignored() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Char('f'), KeyModifiers::NONE);
        // 'z' is neither a label prefix nor in any link's text.
        let effects = press(&mut m, KeyCode::Char('z'), KeyModifiers::NONE);
        assert!(effects.is_empty());
        assert_eq!(m.mode, Mode::Hint, "still hinting");
        assert!(m.hint_input.is_empty(), "invalid key not accepted");
    }

    #[test]
    fn esc_exits_hint_mode() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Char('f'), KeyModifiers::NONE);
        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(m.mode, Mode::Browse);
    }

    #[test]
    fn tab_cycles_focus_and_wraps() {
        // The sample page has two links then one text field, in document order.
        let mut m = loaded_model((80, 24));
        assert_eq!(m.focus, None);
        assert_eq!(m.field_focus, None);
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, Some(1));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, Some(2));
        // The third element is the text field: link focus clears, field focus
        // takes over, and the mode switches to field editing.
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, None);
        assert_eq!(m.field_focus, Some(1));
        assert_eq!(m.mode, Mode::Field);
        // Wrap back to the first link (and back to browse mode).
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, Some(1));
        assert_eq!(m.field_focus, None);
        assert_eq!(m.mode, Mode::Browse);
        // Shift-Tab goes backward from the first link, wrapping to the field.
        press(&mut m, KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(m.field_focus, Some(1));
        assert_eq!(m.mode, Mode::Field);
    }

    #[test]
    fn tab_sets_status_target() {
        let m = loaded_model((80, 24));
        let mut m2 = m.clone();
        press(&mut m2, KeyCode::Tab, KeyModifiers::NONE);
        let target = focused_link_target(&m2).expect("focused target");
        assert!(
            target.contains(&link_of(&m, 1).target),
            "status target missing the link path: {target:?}"
        );
    }

    #[test]
    fn enter_follows_focused_link() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        let effects = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        let expected = browser::resolve_link(link_of(&m, 1), m.current_dest)
            .unwrap()
            .0;
        assert_eq!(effects, vec![Effect::Navigate(expected)]);
    }

    #[test]
    fn following_external_https_link_opens_externally() {
        let mut m = Model {
            links: vec![RenderedLink {
                index: 1,
                label: "Site".to_string(),
                target: "https://example.org/x".to_string(),
                ..RenderedLink::default()
            }],
            ..Model::default()
        };
        let effects = follow_link(&mut m, 1);
        assert_eq!(
            effects,
            vec![Effect::OpenExternal("https://example.org/x".to_string())]
        );
        assert!(
            m.pending.is_none(),
            "an external link must not start a fetch"
        );
    }

    #[test]
    fn following_unsafe_scheme_link_refuses_with_toast() {
        let mut m = Model {
            links: vec![RenderedLink {
                index: 1,
                label: "Bad".to_string(),
                target: "file:///etc/passwd".to_string(),
                ..RenderedLink::default()
            }],
            ..Model::default()
        };
        let effects = follow_link(&mut m, 1);
        assert!(effects.is_empty(), "an unsafe link must yield no effect");
        assert!(m.pending.is_none(), "an unsafe link must not navigate");
        let toast = m.toast.as_ref().expect("a refusal toast is raised");
        assert_eq!(toast.kind, ToastKind::Error);
        assert!(
            toast.text.contains("file"),
            "toast names the refused scheme: {}",
            toast.text
        );
    }

    #[test]
    fn following_rns_link_still_navigates() {
        let mut m = loaded_model((80, 24));
        let effects = follow_link(&mut m, 1);
        let expected = browser::resolve_link(link_of(&m, 1), m.current_dest)
            .unwrap()
            .0;
        assert_eq!(effects, vec![Effect::Navigate(expected)]);
    }

    #[test]
    fn focus_auto_scrolls_to_off_screen_link() {
        // A link on content line 50 with a 21-row viewport must scroll into view.
        let mut m = Model {
            page: vec![RLine::default(); 60],
            links: vec![RenderedLink {
                index: 1,
                label: "Deep".to_string(),
                target: ":/page/deep.mu".to_string(),
                line: 50,
                col_start: 0,
                col_end: 4,
                ..RenderedLink::default()
            }],
            size: (80, 24),
            ..Model::default()
        };
        assert_eq!(m.viewport(), 21);
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, Some(1));
        // scroll = line + 1 - viewport = 50 + 1 - 21 = 30.
        assert_eq!(m.scroll, 30);
    }

    #[test]
    fn mouse_click_on_link_navigates() {
        let mut m = loaded_model((80, 24));
        let vl = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 1)
            .expect("alpha visible");
        let effects = update(&mut m, AppEvent::Mouse(click(vl.col_start, vl.row)));
        let expected = browser::resolve_link(link_of(&m, 1), m.current_dest)
            .unwrap()
            .0;
        assert_eq!(effects, vec![Effect::Navigate(expected)]);
    }

    #[test]
    fn mouse_click_off_a_link_does_nothing() {
        let mut m = loaded_model((80, 24));
        // Row well below any link, in the content body.
        let effects = update(&mut m, AppEvent::Mouse(click(0, 20)));
        assert!(effects.is_empty());
        assert!(m.pending.is_none());
    }

    #[test]
    fn mouse_click_on_reload_control_reloads() {
        let mut m = loaded_model((80, 24));
        let cr = top_bar_controls(regions(m.frame_area())[0])
            .into_iter()
            .find(|c| c.control == Control::Reload)
            .expect("reload control");
        let effects = update(&mut m, AppEvent::Mouse(click(cr.rect.x, cr.rect.y)));
        // Reload re-navigates the current history entry.
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Navigate(_)));
        assert_eq!(
            m.pending.as_ref().map(|p| p.action.clone()),
            Some(HistoryAction::Goto(0))
        );
    }

    #[test]
    fn rendered_buffer_has_no_marker_or_legend() {
        // A link-bearing page with no "Links:" content of its own.
        let m = Model::from_document(
            &parse("Intro paragraph\n\n`[Alpha`:/page/alpha.mu]"),
            content_width(80),
            "T",
            (80, 24),
        );
        let buffer = render(&m, 80, 24);
        let text = flat(&buffer);
        assert!(text.contains("Alpha"), "link label missing:\n{text}");
        assert!(!text.contains("[1]"), "visible [N] marker leaked:\n{text}");
        assert!(!text.contains("Links:"), "legend leaked:\n{text}");
    }

    #[test]
    fn hint_badges_render_over_links() {
        let mut m = loaded_model((80, 24));
        let vl = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 1)
            .expect("alpha visible");
        press(&mut m, KeyCode::Char('f'), KeyModifiers::NONE);
        let buffer = render(&m, 80, 24);
        // The first link wears the 'a' badge over its first cell.
        let cell = &buffer[(vl.col_start, vl.row)];
        assert_eq!(cell.symbol(), "a", "hint badge not drawn over the link");
        assert_eq!(cell.bg, Color::Rgb(255, 215, 0), "badge background missing");
    }

    #[test]
    fn mouse_move_over_link_sets_and_clears_hover() {
        let mut m = loaded_model((80, 24));
        let vl = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 1)
            .expect("alpha visible");
        update(&mut m, AppEvent::Mouse(moved(vl.col_start, vl.row)));
        assert_eq!(m.hover, Some(1), "moving over a link sets the hover");
        // A move off any link clears it.
        update(&mut m, AppEvent::Mouse(moved(vl.col_start, vl.row + 3)));
        assert_eq!(m.hover, None, "moving off a link clears the hover");
    }

    #[test]
    fn hovered_link_renders_highlighted_and_others_do_not() {
        let mut m = loaded_model((80, 24));
        let hovered = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 1)
            .expect("alpha visible");
        let other = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 2)
            .expect("beta visible");
        update(
            &mut m,
            AppEvent::Mouse(moved(hovered.col_start, hovered.row)),
        );
        let buffer = render(&m, 80, 24);
        let cell = &buffer[(hovered.col_start, hovered.row)];
        assert!(
            cell.modifier.contains(Modifier::BOLD) && cell.modifier.contains(Modifier::UNDERLINED),
            "hovered link not highlighted: {:?}",
            cell.modifier
        );
        // The non-hovered link keeps its plain link styling (no hover bold).
        let plain = &buffer[(other.col_start, other.row)];
        assert!(
            !plain.modifier.contains(Modifier::BOLD),
            "non-hovered link wrongly bolded: {:?}",
            plain.modifier
        );
    }

    #[test]
    fn focused_link_renders_highlighted() {
        let mut m = loaded_model((80, 24));
        let vl = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 1)
            .expect("alpha visible");
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        let buffer = render(&m, 80, 24);
        let cell = &buffer[(vl.col_start, vl.row)];
        assert!(
            cell.modifier.contains(Modifier::REVERSED),
            "focused link not highlighted"
        );
    }

    #[test]
    fn status_bar_shows_focused_link_target() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        let buffer = render(&m, 80, 24);
        let status = row_text(&buffer, 23, 80);
        assert!(
            status.contains(&link_of(&m, 1).target),
            "status bar missing the focused target: {status:?}"
        );
    }

    // --- phase 5: in-page search + #anchor scroll-to --------------------

    /// One laid-out line from plain text, every cell in the default style.
    fn rline(text: &str) -> RLine {
        RLine {
            cells: text
                .chars()
                .map(|ch| crate::render::StyledChar {
                    ch,
                    st: RStyle::default(),
                    link: None,
                    field: None,
                })
                .collect(),
        }
    }

    #[test]
    fn find_matches_multiple_across_lines_and_case_insensitive() {
        let page = vec![rline("Foo bar foo"), rline("nothing here"), rline("FOObar")];
        let ms = find_matches(&page, "foo");
        assert_eq!(
            ms,
            vec![
                Match {
                    line_idx: 0,
                    col_start: 0,
                    col_end: 3
                },
                Match {
                    line_idx: 0,
                    col_start: 8,
                    col_end: 11
                },
                Match {
                    line_idx: 2,
                    col_start: 0,
                    col_end: 3
                },
            ]
        );
    }

    #[test]
    fn find_matches_empty_query_and_no_match_are_empty() {
        let page = vec![rline("abcdef")];
        assert!(
            find_matches(&page, "").is_empty(),
            "empty query -> no matches"
        );
        assert!(
            find_matches(&page, "xyz").is_empty(),
            "no hit -> no matches"
        );
        // A query longer than the line never matches.
        assert!(find_matches(&page, "abcdefg").is_empty());
    }

    #[test]
    fn find_matches_adjacent_hits_are_non_overlapping() {
        // "aa" over "aaaa" yields two adjacent, non-overlapping matches (not the
        // three an overlapping scan would report).
        let ms = find_matches(&[rline("aaaa")], "aa");
        assert_eq!(
            ms,
            vec![
                Match {
                    line_idx: 0,
                    col_start: 0,
                    col_end: 2
                },
                Match {
                    line_idx: 0,
                    col_start: 2,
                    col_end: 4
                },
            ]
        );
    }

    #[test]
    fn find_matches_positions_are_display_columns_after_wide_chars() {
        // Two width-2 emojis (4 display columns) precede "term", which starts at
        // char index 2 but display column 4; col_end is 4 + width("term") = 8.
        let ms = find_matches(&[rline("😀😀term")], "term");
        assert_eq!(
            ms,
            vec![Match {
                line_idx: 0,
                col_start: 4,
                col_end: 8,
            }]
        );
    }

    #[test]
    fn search_highlight_lands_on_the_term_after_wide_chars() {
        // "😀😀 " is 5 display columns; the term occupies the next six.
        let mut m = Model::from_document(&parse("😀😀 needle"), content_width(80), "S", (80, 24));
        commit_search(&mut m, "needle");
        let mch = m.matches.first().copied().expect("one match");
        assert_eq!((mch.col_start, mch.col_end), (5, 11));

        let buffer = render(&m, 80, 24);
        let row = TOPBAR_ROWS + (mch.line_idx - m.scroll) as u16;
        // The highlighted cells spell the term, not a wide-char-shifted slice.
        let mut got = String::new();
        for x in mch.col_start as u16..mch.col_end as u16 {
            got.push_str(buffer[(x, row)].symbol());
        }
        assert_eq!(got, "needle");
        assert!(
            buffer[(mch.col_start as u16, row)]
                .modifier
                .contains(Modifier::REVERSED),
            "the current match should be highlighted",
        );
    }

    #[test]
    fn link_after_wide_chars_hit_tests_at_its_display_column() {
        let doc = "😀😀 `[Alpha`:/page/alpha.mu]";
        let mut m = Model::from_document(&parse(doc), content_width(80), "S", (80, 24));
        // A current destination so the relative link resolves on activation.
        m.current_dest = Some([0x11; 16]);
        let vl = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 1)
            .expect("alpha visible");
        // The label starts at display column 5 (after 😀😀 + space), not the char
        // index 3 the old cell-index positioning reported.
        assert_eq!(vl.col_start, 5);

        // A click on the label's display column follows it ...
        let effects = update(&mut m, AppEvent::Mouse(click(vl.col_start, vl.row)));
        assert!(matches!(effects.as_slice(), [Effect::Navigate(_)]));

        // ... while the old char-index column (3) sits inside the second emoji and
        // hits nothing.
        let mut m2 = Model::from_document(&parse(doc), content_width(80), "S", (80, 24));
        m2.current_dest = Some([0x11; 16]);
        let effects = update(&mut m2, AppEvent::Mouse(click(3, vl.row)));
        assert!(
            effects.is_empty(),
            "char-index column must not hit the link"
        );
    }

    /// A controlled document with a known anchor deep in a tall page: 30 intro
    /// paragraphs, then `` `:deep `` before a heading, then 30 trailing ones, so
    /// the anchor sits well below the first viewport and above the page bottom.
    fn long_anchor_doc() -> leviculum_micron::MicronDocument {
        let mut s = String::new();
        for i in 0..30 {
            s.push_str(&format!("intro{i}\n\n"));
        }
        s.push_str("`:deep\n>Deep Heading\n\n");
        for i in 0..30 {
            s.push_str(&format!("tail{i}\n\n"));
        }
        parse(&s)
    }

    #[test]
    fn anchor_line_resolves_known_and_rejects_unknown() {
        // `:target binds to its own (blank) block; the heading slug binds to the
        // heading block. Both resolve to a page line; an unknown anchor does not.
        let m = Model::from_document(
            &parse("Intro para\n`:target\n>Marked Section\nbody text"),
            content_width(80),
            "T",
            (80, 24),
        );
        let target_line = anchor_line(&m, "target").expect("known anchor resolves");
        let heading_line = anchor_line(&m, "marked-section").expect("heading slug resolves");
        assert!(
            line_text(&m.page[heading_line]).contains("Marked Section"),
            "heading anchor must land on the heading line: {:?}",
            line_text(&m.page[heading_line])
        );
        assert!(
            target_line < heading_line,
            "the anchor precedes the heading"
        );
        assert_eq!(anchor_line(&m, "nope"), None);
    }

    #[test]
    fn navigation_with_anchor_scrolls_to_anchor_line() {
        let mut m = model_from_sample(content_width(80), (80, 10));
        m.pending = Some(Pending {
            target: tgt(7),
            action: HistoryAction::Push,
        });
        m.pending_anchor = Some("deep".to_string());
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: long_anchor_doc(),
                title: "Deep".to_string(),
            },
        );
        let vp = m.viewport();
        let line = anchor_line(&m, "deep").expect("anchor resolves on the loaded page");
        assert!(
            line > vp,
            "anchor must sit below the first viewport to test a jump"
        );
        assert_eq!(m.scroll, line.min(m.max_scroll(vp)));
        assert!(m.scroll > 0, "scroll must have moved to the anchor");
        assert!(m.pending_anchor.is_none(), "anchor consumed on load");
    }

    #[test]
    fn navigation_with_unknown_anchor_stays_top_and_notes() {
        let mut m = model_from_sample(content_width(80), (80, 10));
        m.pending = Some(Pending {
            target: tgt(7),
            action: HistoryAction::Push,
        });
        m.pending_anchor = Some("missing".to_string());
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: long_anchor_doc(),
                title: "Deep".to_string(),
            },
        );
        assert_eq!(m.scroll, 0, "unknown anchor falls back to the top");
        let toast = m.toast.as_ref().expect("unknown anchor raises a toast");
        assert!(
            toast.text.contains("not found"),
            "unknown anchor should note it: {}",
            toast.text
        );
    }

    #[test]
    fn split_path_anchor_strips_the_fragment() {
        let (base, anchor) = split_path_anchor(Target {
            dest_hash: [1; 16],
            path: "/page/x.mu#sec".to_string(),
            fields: Vec::new(),
            is_file: false,
        });
        assert_eq!(base.path, "/page/x.mu");
        assert_eq!(anchor.as_deref(), Some("sec"));
        // No fragment -> no anchor, path untouched.
        let (base2, anchor2) = split_path_anchor(Target {
            dest_hash: [1; 16],
            path: "/page/y.mu".to_string(),
            fields: Vec::new(),
            is_file: false,
        });
        assert_eq!(base2.path, "/page/y.mu");
        assert!(anchor2.is_none());
    }

    /// A tall page whose only "needle" sits below the first viewport, so a search
    /// commit must scroll to reveal it.
    fn deep_match_model(size: (u16, u16)) -> Model {
        let mut s = String::new();
        for i in 0..40 {
            s.push_str(&format!("filler{i}\n\n"));
        }
        s.push_str("needle deep here\n");
        Model::from_document(&parse(&s), content_width(size.0), "S", size)
    }

    /// A short page with three "needle" occurrences on distinct lines, all inside
    /// a full-height viewport, for cycling tests.
    fn triple_match_model() -> Model {
        let s = "needle one\n\nfiller\n\nneedle two\n\nneedle three";
        Model::from_document(&parse(s), content_width(80), "S", (80, 24))
    }

    #[test]
    fn slash_enters_search_and_enter_commits_matches_and_scrolls() {
        let mut m = deep_match_model((80, 10));
        let eff = press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(eff.is_empty());
        assert_eq!(m.mode, Mode::Search);
        type_str(&mut m, "needle");
        let eff = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert!(eff.is_empty());
        assert_eq!(m.mode, Mode::Browse, "Enter commits and leaves search");
        assert!(!m.matches.is_empty(), "commit populates matches");
        assert_eq!(m.current_match, Some(0));
        // The (single, deep) match is scrolled into the viewport.
        let first = m.matches[0];
        let vp = m.viewport();
        assert!(
            first.line_idx >= vp,
            "match must start below the first viewport"
        );
        assert!(
            first.line_idx >= m.scroll && first.line_idx < m.scroll + vp,
            "current match must be visible after commit"
        );
    }

    #[test]
    fn n_and_shift_n_cycle_matches_and_wrap() {
        let mut m = triple_match_model();
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        type_str(&mut m, "needle");
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(m.matches.len(), 3, "three needle lines");
        assert_eq!(m.current_match, Some(0));
        press(&mut m, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(m.current_match, Some(1));
        press(&mut m, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(m.current_match, Some(2));
        // n wraps past the end back to the first.
        press(&mut m, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(m.current_match, Some(0));
        // N wraps before the start to the last.
        press(&mut m, KeyCode::Char('N'), KeyModifiers::NONE);
        assert_eq!(m.current_match, Some(2));
    }

    #[test]
    fn n_with_no_matches_is_a_noop() {
        let mut m = triple_match_model();
        assert!(m.matches.is_empty());
        press(&mut m, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(m.current_match, None);
        assert_eq!(m.scroll, 0);
    }

    #[test]
    fn reentering_search_and_esc_clears_highlights() {
        let mut m = triple_match_model();
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        type_str(&mut m, "needle");
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!m.matches.is_empty(), "committed matches exist");
        // Re-entering search drops the prior highlights immediately.
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(
            m.matches.is_empty(),
            "entering search clears old highlights"
        );
        // Esc returns to browse with nothing highlighted.
        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(m.mode, Mode::Browse);
        assert!(m.matches.is_empty());
        assert!(m.current_match.is_none());
    }

    #[test]
    fn commit_with_no_match_notes_it() {
        let mut m = triple_match_model();
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        type_str(&mut m, "zzz");
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert!(m.matches.is_empty());
        assert_eq!(m.current_match, None);
        let toast = m.toast.as_ref().expect("a fruitless search raises a toast");
        assert!(
            toast.text.contains("no matches"),
            "a fruitless search should say so: {}",
            toast.text
        );
    }

    #[test]
    fn search_highlights_matches_and_current_is_stronger() {
        let mut m = triple_match_model();
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        type_str(&mut m, "needle");
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        let buffer = render(&m, 80, 24);
        // The current (first) match is drawn reversed.
        let cur = m.matches[0];
        let cur_row = TOPBAR_ROWS + cur.line_idx as u16;
        let cur_cell = &buffer[(cur.col_start as u16, cur_row)];
        assert!(
            cur_cell.modifier.contains(Modifier::REVERSED),
            "current match must use the stronger reversed highlight"
        );
        // A non-current match carries the theme's match tint, not the reverse.
        let other = m.matches[1];
        let other_row = TOPBAR_ROWS + other.line_idx as u16;
        let other_cell = &buffer[(other.col_start as u16, other_row)];
        assert_eq!(
            other_cell.bg,
            rgb(Theme::Dark.search_match_bg()),
            "non-current match must carry the tint background"
        );
        assert!(
            !other_cell.modifier.contains(Modifier::REVERSED),
            "non-current match must not use the stronger highlight"
        );
    }

    #[test]
    fn search_commit_scrolls_current_match_into_the_rendered_viewport() {
        let mut m = deep_match_model((80, 10));
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        type_str(&mut m, "needle");
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        let mline = m.matches[0].line_idx;
        let vp = m.viewport();
        assert!(
            mline >= m.scroll && mline < m.scroll + vp,
            "match in viewport"
        );
        let buffer = render(&m, 80, 10);
        let row = TOPBAR_ROWS + (mline - m.scroll) as u16;
        let cell = &buffer[(m.matches[0].col_start as u16, row)];
        assert!(
            cell.modifier.contains(Modifier::REVERSED),
            "the current match must render (reversed) inside the viewport"
        );
    }

    #[test]
    fn anchor_jump_renders_the_anchor_line_at_the_content_top() {
        let mut m = model_from_sample(content_width(80), (80, 10));
        m.pending = Some(Pending {
            target: tgt(7),
            action: HistoryAction::Push,
        });
        m.pending_anchor = Some("deep".to_string());
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: long_anchor_doc(),
                title: "Deep".to_string(),
            },
        );
        let buffer = render(&m, 80, 10);
        let top = row_text(&buffer, TOPBAR_ROWS, 80 - SCROLLBAR_COLS);
        assert_eq!(
            top.trim_end(),
            line_text(&m.page[m.scroll]).trim_end(),
            "the anchor's line must sit at the top of the content viewport"
        );
    }

    #[test]
    fn search_bar_renders_query_line() {
        let mut m = triple_match_model();
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        type_str(&mut m, "abc");
        let buffer = render(&m, 80, 24);
        let status = row_text(&buffer, 23, 80);
        assert!(status.contains('/'), "search prompt missing: {status:?}");
        assert!(status.contains("abc"), "query text missing: {status:?}");
    }

    // --- terminal guard (retained from phase 2) --------------------------

    struct MockOps {
        entered: Rc<Cell<bool>>,
        restored: Rc<Cell<bool>>,
    }

    impl TerminalOps for MockOps {
        fn enter(&mut self) -> io::Result<()> {
            self.entered.set(true);
            Ok(())
        }
        fn restore(&mut self) -> io::Result<()> {
            self.restored.set(true);
            Ok(())
        }
    }

    #[test]
    fn guard_restores_on_drop() {
        let entered = Rc::new(Cell::new(false));
        let restored = Rc::new(Cell::new(false));
        {
            let guard = TerminalGuard::new(MockOps {
                entered: entered.clone(),
                restored: restored.clone(),
            })
            .expect("enter");
            assert!(entered.get(), "enter should have run");
            assert!(!restored.get(), "restore must not run before drop");
            drop(guard);
        }
        assert!(restored.get(), "restore must run on drop");
    }

    #[test]
    fn guard_restore_now_is_idempotent() {
        let restored = Rc::new(Cell::new(false));
        let mut guard = TerminalGuard::new(MockOps {
            entered: Rc::new(Cell::new(false)),
            restored: restored.clone(),
        })
        .expect("enter");
        guard.restore_now().expect("restore");
        assert!(restored.get());
        restored.set(false);
        guard.restore_now().expect("restore idempotent");
        assert!(!restored.get(), "restore ran twice");
    }

    // --- phase 6: bookmarks, copy, places panel -------------------------

    fn disc_node(hash: u8, name: &str) -> DiscoveredNode {
        DiscoveredNode {
            dest_hash: [hash; 16],
            name: Some(name.to_string()),
            first_seen: 0,
            last_seen: 0,
            hops: Some(1),
        }
    }

    #[test]
    fn osc52_wraps_base64_payload() {
        // ESC ] 52 ; c ; base64("hi") ST, with ST = ESC backslash.
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x1b\\");
    }

    #[test]
    fn status_bar_hints_fit_eighty_columns() {
        // The curated browse hints must fit an 80-column bar so "quit" stays
        // visible (see status_bar_renders_hints).
        assert!(
            UnicodeWidthStr::width(BROWSE_HINTS) <= 80,
            "browse hints overflow 80 cols: {}",
            UnicodeWidthStr::width(BROWSE_HINTS)
        );
    }

    #[test]
    fn bookmark_toggle_adds_then_removes_and_persists() {
        let mut m = loaded_model((80, 24));
        let url = current_url(&m).expect("current url");
        // First `m` bookmarks the page and asks the shell to persist.
        let fx = press(&mut m, KeyCode::Char('m'), KeyModifiers::NONE);
        assert!(m.bookmarks.contains(&url));
        assert_eq!(fx, vec![Effect::SaveBookmarks]);
        // Second `m` un-bookmarks it, again persisting.
        let fx = press(&mut m, KeyCode::Char('m'), KeyModifiers::NONE);
        assert!(!m.bookmarks.contains(&url));
        assert_eq!(fx, vec![Effect::SaveBookmarks]);
    }

    #[test]
    fn yank_copies_current_page_url() {
        let mut m = loaded_model((80, 24));
        let url = current_url(&m).expect("current url");
        let fx = press(&mut m, KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(fx, vec![Effect::Copy(url)]);
        let toast = m.toast.as_ref().expect("a copy toast is raised");
        assert_eq!(toast.kind, ToastKind::Info);
        assert!(toast.text.contains("copied"), "toast: {}", toast.text);
    }

    #[test]
    fn places_lists_bookmarks_then_discovered_nodes() {
        let mut m = loaded_model((80, 24));
        m.bookmarks.add(Bookmark {
            url: "aa11:/page/index.mu".to_string(),
            title: "Home".to_string(),
        });
        m.node_registry.upsert_node(&disc_node(0xcd, "Node-C"));
        let ps = places(&m);
        assert_eq!(ps.len(), 2, "one bookmark then one node");
        assert!(matches!(ps[0], Place::Bookmark { .. }));
        assert!(matches!(ps[1], Place::Node { .. }));
    }

    #[test]
    fn places_panel_opens_navigates_and_activates() {
        let mut m = loaded_model((80, 24));
        m.bookmarks.add(Bookmark {
            url: "aa11:/page/index.mu".to_string(),
            title: "Home".to_string(),
        });
        m.node_registry.upsert_node(&disc_node(0xcd, "Node-C"));

        // `d` opens the panel; it then owns keys.
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(m.show_places);
        assert_eq!(m.places_sel, 0);

        // `j` moves onto the discovered node; Enter opens its default page.
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, 1);
        let fx = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!m.show_places, "activating closes the panel");
        assert_eq!(
            fx,
            vec![Effect::Navigate(Target {
                dest_hash: [0xcd; 16],
                path: DEFAULT_PATH.to_string(),
                fields: Vec::new(),
                is_file: false,
            })]
        );
    }

    #[test]
    fn places_panel_shares_the_content_scroll_keymap() {
        let mut m = loaded_model((80, 24));
        for i in 0..5u8 {
            m.bookmarks.add(Bookmark {
                url: format!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa{i:02}:/page/index.mu"),
                title: format!("B{i}"),
            });
        }
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(m.show_places);
        assert_eq!(m.places_sel, 0);

        // Line down: j, Ctrl-n, Down all step one entry.
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, 1);
        press(&mut m, KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert_eq!(m.places_sel, 2);
        press(&mut m, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(m.places_sel, 3);

        // Line up: k, Ctrl-p, Up step back one entry.
        press(&mut m, KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, 2);
        press(&mut m, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(m.places_sel, 1);
        press(&mut m, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(m.places_sel, 0);

        // g / G / Home / End jump to the first / last entry.
        let last = places(&m).len() - 1;
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, last);
        press(&mut m, KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, 0);
        press(&mut m, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(m.places_sel, last);
        press(&mut m, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(m.places_sel, 0);

        // Ctrl-d is a half-page motion here, not the `d` that closes the panel.
        press(&mut m, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(m.show_places, "Ctrl-d must not close the panel");
        assert_eq!(
            m.places_sel, last,
            "half-page jump clamps to the last entry"
        );

        // Plain `d` still closes the panel.
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(!m.show_places);
    }

    #[test]
    fn places_panel_deletes_selected_bookmark_and_persists() {
        let mut m = loaded_model((80, 24));
        m.bookmarks.add(Bookmark {
            url: "aa11:/page/index.mu".to_string(),
            title: "Home".to_string(),
        });
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        // `x` removes the selected bookmark and asks to persist.
        let fx = press(&mut m, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(m.bookmarks.is_empty());
        assert_eq!(fx, vec![Effect::SaveBookmarks]);
    }

    #[test]
    fn discovery_event_feeds_the_places_panel() {
        let mut m = loaded_model((80, 24));
        update(&mut m, AppEvent::NodeDiscovered(disc_node(0xcd, "Node-C")));
        let ps = places(&m);
        assert_eq!(ps.len(), 1);
        assert!(matches!(ps[0], Place::Node { .. }));
    }

    #[test]
    fn truncate_to_cols_is_width_aware() {
        assert_eq!(truncate_to_cols("hello", 10), "hello");
        assert_eq!(truncate_to_cols("hello", 5), "hello");
        // Cut to a budget: the ellipsis takes the last column.
        assert_eq!(truncate_to_cols("hello world", 5), "hell…");
        assert_eq!(
            UnicodeWidthStr::width(truncate_to_cols("hello world", 5).as_str()),
            5
        );
        // A wide char that would straddle the budget is dropped whole, so the
        // result never exceeds the requested width.
        let cut = truncate_to_cols("🚀ab", 2);
        assert!(
            UnicodeWidthStr::width(cut.as_str()) <= 2,
            "over budget: {cut:?}"
        );
        assert_eq!(truncate_to_cols("anything", 0), "");
    }

    #[test]
    fn places_row_truncates_and_keeps_border_intact() {
        let mut m = loaded_model((80, 24));
        // A bookmark whose label (title + url + emoji) far exceeds the panel's
        // inner width, so the selected row must be truncated with an ellipsis.
        m.bookmarks.add(Bookmark {
            url: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:/page/a-very-long-page-path.mu".to_string(),
            title: "🚀 an extremely long bookmark title that cannot fit inside the panel"
                .to_string(),
        });
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, 0, "the long bookmark is the selected row");
        let buffer = render(&m, 80, 24);

        // Panel geometry: width 72 centred in an 80-column area → x=4, so the
        // right border sits at column 75.
        let width = 72u16;
        let x = (80 - width) / 2;
        let right_border = x + width - 1;

        // The bookmark row is the one carrying the truncation ellipsis.
        let mut row = None;
        for y in 0..buffer.area.height {
            if row_text(&buffer, y, 80).contains('…') {
                row = Some(y);
            }
        }
        let row = row.expect("a truncated bookmark row with an ellipsis");

        // The right border cell is intact and no reversed selected-bar cell
        // reaches or passes it (the overflow the truncation fixes).
        assert_eq!(
            buffer[(right_border, row)].symbol(),
            "│",
            "right border overwritten"
        );
        for col in right_border..buffer.area.width {
            assert!(
                !buffer[(col, row)].modifier.contains(Modifier::REVERSED),
                "selected bar spilled to column {col} on row {row}"
            );
        }
    }

    #[test]
    fn places_panel_renders_both_sections() {
        let mut m = loaded_model((80, 24));
        m.bookmarks.add(Bookmark {
            url: "aa11:/page/index.mu".to_string(),
            title: "Home".to_string(),
        });
        m.node_registry.upsert_node(&disc_node(0xcd, "Node-C"));
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        let buffer = render(&m, 80, 24);
        let text = flat(&buffer);
        assert!(text.contains("Bookmarks"), "missing header:\n{text}");
        assert!(text.contains("Discovered nodes"), "missing header:\n{text}");
        assert!(text.contains("Home"), "missing bookmark:\n{text}");
        assert!(text.contains("Node-C"), "missing node:\n{text}");
    }

    // --- Phase 7: editable form fields + submit -------------------------

    const HASH_HEX: &str = "0123456789abcdef0123456789abcdef";

    /// A loaded model from a micron source with a current destination set, so
    /// same-destination links and field submission resolve.
    fn field_model(src: &str, size: (u16, u16)) -> Model {
        let mut m = Model::from_document(&parse(src), content_width(size.0), "T", size);
        m.history.visit(tgt(0xab));
        m.current_dest = Some([0xab; 16]);
        m
    }

    #[test]
    fn field_store_inits_from_prefill() {
        let src = "Name: `<name`bob>\nAgree: `<?|agree`Agree>";
        let m = field_model(src, (80, 24));
        assert_eq!(m.field_state.len(), 2);
        assert_eq!(m.field_state[0].name, "name");
        assert_eq!(m.field_state[0].kind, FieldKind::Text);
        assert_eq!(m.field_state[0].input.value(), "bob");
        assert_eq!(m.field_state[1].name, "agree");
        assert_eq!(m.field_state[1].kind, FieldKind::Checkbox);
        assert!(!m.field_state[1].checked, "checkbox starts unchecked");
    }

    #[test]
    fn typing_edits_focused_text_field() {
        let src = "Name: `<name`bo>";
        let mut m = field_model(src, (80, 24));
        // Tab focuses the only interactive element, the text field.
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.field_focus, Some(1));
        assert_eq!(m.mode, Mode::Field);
        // The editor starts with the cursor at the end of "bo"; typing appends.
        type_str(&mut m, "b");
        assert_eq!(m.field_state[0].input.value(), "bob");
        // The re-layout reflects the new value in the laid-out widget.
        let field_line: String = m.page[m.field_defs[0].line]
            .cells
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            field_line.contains("[bob]"),
            "widget not updated: {field_line:?}"
        );
    }

    #[test]
    fn space_toggles_focused_checkbox() {
        let src = "Agree: `<?|agree`Agree>";
        let mut m = field_model(src, (80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.field_focus, Some(1));
        assert!(!m.field_state[0].checked);
        press(&mut m, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(m.field_state[0].checked, "Space should check the box");
        press(&mut m, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!m.field_state[0].checked, "Space should uncheck it again");
    }

    #[test]
    fn space_selects_radio_and_deselects_group_siblings() {
        // Two radios sharing the name "col": selecting one deselects the other.
        let src = "`<^|col`Red>`<^|col`Blue>";
        let mut m = field_model(src, (80, 24));
        assert_eq!(m.field_state.len(), 2);
        // Focus + select the first radio.
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut m, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(m.field_state[0].checked);
        assert!(!m.field_state[1].checked);
        // Move to the second and select it: the first must clear.
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut m, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!m.field_state[0].checked, "sibling radio not cleared");
        assert!(m.field_state[1].checked);
    }

    #[test]
    fn esc_leaves_field_editing() {
        let src = "Name: `<name`>";
        let mut m = field_model(src, (80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.mode, Mode::Field);
        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(m.mode, Mode::Browse);
        assert_eq!(m.field_focus, None);
    }

    #[test]
    fn visible_fields_positions_after_wide_char() {
        // A width-2 emoji then a space, then the field: the widget starts at
        // display column 3 (2 + 1), not character index 2.
        let src = "\u{1f389} `<name`>";
        let m = field_model(src, (80, 24));
        let vf = visible_fields(&m);
        assert_eq!(vf.len(), 1);
        assert_eq!(vf[0].row, TOPBAR_ROWS);
        assert_eq!(vf[0].col_start, 3, "field mispositioned after wide char");
        // Empty text field renders as "[]" -> two columns wide.
        assert_eq!(vf[0].col_end, 5);
    }

    #[test]
    fn submit_link_carries_collected_field_values() {
        // A page with a text field "name" (prefill "bob") and a submit link that
        // references it plus a `k=v` preset.
        let src = format!("`<name`bob>\n`[Go`{HASH_HEX}:/page/s.mu`name|a=1]");
        let mut m = field_model(&src, (80, 24));
        let effects = follow_link(&mut m, 1);
        let target = match effects.as_slice() {
            [Effect::Navigate(t)] => t.clone(),
            other => panic!("expected a single Navigate, got {other:?}"),
        };
        assert_eq!(target.path, "/page/s.mu");
        // The preset lands as var_a; the collected field value as field_name.
        assert!(
            target
                .fields
                .contains(&("var_a".to_string(), "1".to_string())),
            "preset missing: {:?}",
            target.fields
        );
        assert!(
            target
                .fields
                .contains(&("field_name".to_string(), "bob".to_string())),
            "collected field missing: {:?}",
            target.fields
        );
    }

    #[test]
    fn submit_link_reflects_edited_value_and_ignores_unreferenced_fields() {
        // Two text fields; the link references only "q". Editing "q" must flow
        // through, and the unreferenced "other" field must not be submitted.
        let src = format!("`<q`hi>`<other`x>\n`[Go`{HASH_HEX}:/page/s.mu`q]");
        let mut m = field_model(&src, (80, 24));
        // Focus the first field (q) and append "!".
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        type_str(&mut m, "!");
        assert_eq!(m.field_state[0].input.value(), "hi!");
        let link = m.links.iter().find(|l| l.index == 1).cloned().unwrap();
        let collected = collect_submit_fields(&m, &link);
        assert_eq!(
            collected,
            vec![("field_q".to_string(), "hi!".to_string())],
            "only the referenced, edited field should be collected"
        );
    }

    #[test]
    fn submit_link_star_collects_all_fields() {
        let src = format!("`<a`1>`<b`2>\n`[Go`{HASH_HEX}:/page/s.mu`*]");
        let m = field_model(&src, (80, 24));
        let link = m.links.iter().find(|l| l.index == 1).cloned().unwrap();
        let collected = collect_submit_fields(&m, &link);
        assert_eq!(
            collected,
            vec![
                ("field_a".to_string(), "1".to_string()),
                ("field_b".to_string(), "2".to_string()),
            ],
            "`*` should collect every field"
        );
    }

    #[test]
    fn unchecked_checkbox_is_not_submitted() {
        let src = format!("`<?|agree`Agree>\n`[Go`{HASH_HEX}:/page/s.mu`agree]");
        let mut m = field_model(&src, (80, 24));
        let link = m.links.iter().find(|l| l.index == 1).cloned().unwrap();
        // Unchecked: nothing collected.
        assert!(collect_submit_fields(&m, &link).is_empty());
        // Check it (focus the checkbox, Space), then it submits its value.
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut m, KeyCode::Char(' '), KeyModifiers::NONE);
        let collected = collect_submit_fields(&m, &link);
        assert_eq!(
            collected,
            vec![("field_agree".to_string(), "Agree".to_string())],
            "a checked box submits its value (the label when none given)"
        );
    }

    #[test]
    fn mouse_click_on_field_focuses_it() {
        let src = "Name: `<name`>";
        let mut m = field_model(src, (80, 24));
        let vf = visible_fields(&m)[0].clone();
        let effects = update(&mut m, AppEvent::Mouse(click(vf.col_start, vf.row)));
        assert!(effects.is_empty(), "focusing a field yields no effect");
        assert_eq!(m.field_focus, Some(1));
        assert_eq!(m.mode, Mode::Field);
    }

    #[test]
    fn field_state_survives_resize() {
        let src = "Name: `<name`>";
        let mut m = field_model(src, (80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        type_str(&mut m, "kept");
        assert_eq!(m.field_state[0].input.value(), "kept");
        update(&mut m, AppEvent::Resize(60, 20));
        assert_eq!(
            m.field_state[0].input.value(),
            "kept",
            "a resize must not reset the field store"
        );
    }

    // --- Phase 7: TestBackend rendering ---------------------------------

    #[test]
    fn input_field_renders_its_value_in_a_box() {
        let src = "Name: `<name`bob>";
        let m = field_model(src, (80, 24));
        let buffer = render(&m, 80, 24);
        assert!(flat(&buffer).contains("[bob]"), "field value not drawn");
        // The field cells carry the input-box background (theme chrome bg).
        let vf = visible_fields(&m)[0].clone();
        let cell = &buffer[(vf.col_start, vf.row)];
        assert_eq!(
            cell.bg,
            Color::Rgb(
                Theme::Dark.chrome_bg().0,
                Theme::Dark.chrome_bg().1,
                Theme::Dark.chrome_bg().2
            ),
            "input field should render in an input-box style"
        );
    }

    #[test]
    fn focused_field_is_highlighted() {
        let src = "Name: `<name`bob>";
        let mut m = field_model(src, (80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        let vf = visible_fields(&m)[0].clone();
        let buffer = render(&m, 80, 24);
        let cell = &buffer[(vf.col_start, vf.row)];
        assert!(
            cell.modifier.contains(Modifier::REVERSED),
            "a focused field should be highlighted"
        );
    }

    #[test]
    fn checkbox_renders_its_state() {
        // Prechecked checkbox renders "[x]"; an unchecked one "[ ]".
        let checked = field_model("`<?|a|Yes|*`On>", (80, 24));
        assert!(flat(&render(&checked, 80, 24)).contains("[x] On"));
        let unchecked = field_model("`<?|a`Off>", (80, 24));
        assert!(flat(&render(&unchecked, 80, 24)).contains("[ ] Off"));
    }
}
