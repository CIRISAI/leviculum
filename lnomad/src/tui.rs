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
use crate::render::{layout_blocks, RLine, RStyle, RenderedLink};
use crate::theme::{resolve_theme, Bg, Theme, ThemeFlag};
use crate::url::{parse_url, Target, DEFAULT_PATH};

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
/// Spinner animation cadence while a fetch is in flight, in milliseconds.
const SPINNER_TICK_MS: u64 = 120;

/// The label for each of the three fixed top-bar controls.
const BACK_LABEL: &str = "‹ back";
const FORWARD_LABEL: &str = "forward ›";
const RELOAD_LABEL: &str = "⟳ reload";

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
    pub focus: Option<usize>,
    /// The link the mouse is hovering over, 1-based, or `None`.
    pub hover: Option<usize>,
    /// The characters typed so far in hint mode, narrowing the visible labels.
    pub hint_input: String,
    /// A transient status/error message shown in the status bar, or `None` for
    /// the default hints.
    pub status: Option<String>,
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
        let (page, links, block_lines) = layout_blocks(doc, width, theme);
        Self {
            doc: doc.clone(),
            width,
            page,
            links,
            block_lines,
            title: title.into(),
            size,
            theme,
            ..Self::default()
        }
    }

    /// Re-wrap the stored document to `width` under the current theme, replacing
    /// `page`/`links`. The caller is responsible for re-clamping `scroll`
    /// afterwards. Also used to re-lay-out in place after a theme toggle.
    pub fn relayout(&mut self, width: usize) {
        self.width = width;
        let (page, links, block_lines) = layout_blocks(&self.doc, width, self.theme);
        self.page = page;
        self.links = links;
        self.block_lines = block_lines;
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
            Vec::new()
        }
        AppEvent::PageLoaded { doc, title } => {
            apply_loaded(model, doc, title);
            Vec::new()
        }
        AppEvent::LoadFailed(msg) => {
            model.pending = None;
            model.status = Some(msg);
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
    model.doc = doc;
    model.relayout(content_width(model.size.0));
    model.scroll = 0;
    model.title = title;
    model.status = None;
    // A fresh page invalidates any focus/hover cursor into the old link set, and
    // any search match highlights against the old page.
    model.focus = None;
    model.hover = None;
    model.matches.clear();
    model.current_match = None;
    if let Some(pending) = pending {
        model.current_dest = Some(pending.target.dest_hash);
        match pending.action {
            HistoryAction::Push => model.history.visit(pending.target),
            HistoryAction::Goto(idx) => model.history.goto(idx),
        }
    }
    // A followed `#anchor` scrolls its block's first line to the top; an unknown
    // anchor falls back to the top of the page with a status note.
    if let Some(name) = anchor {
        match anchor_line(model, &name) {
            Some(line) => {
                let vp = model.viewport();
                model.scroll = line.min(model.max_scroll(vp));
            }
            None => model.status = Some(format!("anchor #{name} not found")),
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
        Mode::Browse => update_browse_key(model, key, ctrl),
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
            model.status = None;
            Vec::new()
        }
        KeyCode::Enter => {
            let raw = model.input.value().trim().to_string();
            match parse_url(&raw, model.current_dest) {
                Ok(target) => {
                    model.mode = Mode::Browse;
                    model.input.reset();
                    model.status = None;
                    model.pending = Some(Pending {
                        target: target.clone(),
                        action: HistoryAction::Push,
                    });
                    model.pending_anchor = None;
                    vec![Effect::Navigate(target)]
                }
                Err(err) => {
                    model.status = Some(format!("bad URL: {err}"));
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
        model.status = Some("cancelled".to_string());
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
    model.status = None;
}

/// Enter hint mode with a cleared filter buffer.
fn enter_hint(model: &mut Model) {
    model.mode = Mode::Hint;
    model.hint_input.clear();
    model.status = None;
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
    model.status = None;
}

/// Leave search mode (Esc): back to Browse, clearing the query and all match
/// highlights plus the current-match marker.
fn exit_search(model: &mut Model) {
    model.mode = Mode::Browse;
    model.search_input.reset();
    model.matches.clear();
    model.current_match = None;
    model.status = None;
}

/// Commit a search query: recompute matches over the current page, mark the
/// first as current and scroll it into view. An empty result clears the current
/// match and notes it in the status bar.
fn commit_search(model: &mut Model, query: &str) {
    model.matches = find_matches(&model.page, query);
    if model.matches.is_empty() {
        model.current_match = None;
        model.status = Some(format!("no matches for \"{query}\""));
    } else {
        model.current_match = Some(0);
        model.status = None;
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
        model.status = None;
    }
}

/// Close the places panel.
fn close_places(model: &mut Model) {
    model.show_places = false;
}

/// Fold a key while the places panel is open: `j/k`/arrows move the selection,
/// `Enter` opens it, `x` deletes the selected bookmark, `Esc`/`d` close.
fn update_places_key(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('d') => {
            close_places(model);
            Vec::new()
        }
        KeyCode::Char('j') | KeyCode::Down => {
            move_places_selection(model, 1);
            Vec::new()
        }
        KeyCode::Char('k') | KeyCode::Up => {
            move_places_selection(model, -1);
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

/// Move the places selection by `delta`, clamped to the current row count. A
/// no-op when the panel is empty.
fn move_places_selection(model: &mut Model, delta: isize) {
    let len = places(model).len();
    if len == 0 {
        model.places_sel = 0;
        return;
    }
    let max = len - 1;
    let next = model.places_sel as isize + delta;
    model.places_sel = next.clamp(0, max as isize) as usize;
}

/// Open the selected place: a bookmark's URL, or a discovered node's default
/// page. Closes the panel and starts a fresh navigation. A malformed bookmark
/// URL surfaces a status message instead.
fn activate_place(model: &mut Model, idx: usize) -> Vec<Effect> {
    let Some(place) = places(model).into_iter().nth(idx) else {
        return Vec::new();
    };
    let target = match place {
        Place::Bookmark { url, .. } => match parse_url(&url, model.current_dest) {
            Ok(target) => target,
            Err(err) => {
                model.status = Some(format!("bad bookmark URL: {err}"));
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
    model.status = None;
    model.pending = Some(Pending {
        target: target.clone(),
        action: HistoryAction::Push,
    });
    model.pending_anchor = None;
    vec![Effect::Navigate(target)]
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
    model.status = Some(format!("removed bookmark {url}"));
    vec![Effect::SaveBookmarks]
}

/// Toggle a bookmark for the current page: remove it when the page is already
/// bookmarked, else add it under the current title. Persists on change. A no-op
/// with a status note when nothing is loaded.
fn toggle_bookmark_current(model: &mut Model) -> Vec<Effect> {
    let Some(url) = current_url(model) else {
        model.status = Some("nothing to bookmark".to_string());
        return Vec::new();
    };
    if model.bookmarks.contains(&url) {
        model.bookmarks.remove(&url);
        model.status = Some(format!("removed bookmark {url}"));
    } else {
        let title = model.title.trim().to_string();
        model.bookmarks.add(Bookmark {
            url: url.clone(),
            title,
        });
        model.status = Some(format!("bookmarked {url}"));
    }
    vec![Effect::SaveBookmarks]
}

/// Yank the focused link's target URL, or (with nothing focused) the current
/// page URL, to the clipboard. A no-op with a status note when there is nothing
/// to copy.
fn yank_url(model: &mut Model) -> Vec<Effect> {
    let url = match focused_link_url(model).or_else(|| current_url(model)) {
        Some(url) => url,
        None => {
            model.status = Some("nothing to copy".to_string());
            return Vec::new();
        }
    };
    model.status = Some(format!("copied {url}"));
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
    model.status = None;
    vec![Effect::Navigate(target)]
}

/// Navigate back one history entry (re-fetching it), if possible.
fn go_back(model: &mut Model) -> Vec<Effect> {
    if !model.history.can_back() {
        return Vec::new();
    }
    let idx = model.history.idx - 1;
    let target = model.history.stack[idx].clone();
    model.pending = Some(Pending {
        target: target.clone(),
        action: HistoryAction::Goto(idx),
    });
    model.pending_anchor = None;
    model.status = None;
    vec![Effect::Navigate(target)]
}

/// Navigate forward one history entry (re-fetching it), if possible.
fn go_forward(model: &mut Model) -> Vec<Effect> {
    if !model.history.can_forward() {
        return Vec::new();
    }
    let idx = model.history.idx + 1;
    let target = model.history.stack[idx].clone();
    model.pending = Some(Pending {
        target: target.clone(),
        action: HistoryAction::Goto(idx),
    });
    model.pending_anchor = None;
    model.status = None;
    vec![Effect::Navigate(target)]
}

/// Follow the link with 1-based `index`: resolve its target against the current
/// destination and start a fresh navigation, or set an error status.
fn follow_link(model: &mut Model, index: usize) -> Vec<Effect> {
    let Some(link) = model.links.iter().find(|l| l.index == index).cloned() else {
        return Vec::new();
    };
    match browser::resolve_link(&link, model.current_dest) {
        Ok((target, anchor)) => {
            model.status = None;
            model.pending = Some(Pending {
                target: target.clone(),
                action: HistoryAction::Push,
            });
            // Remember any `#anchor` so the load handler scrolls to it once the
            // page arrives (resolved against the freshly-laid-out lines).
            model.pending_anchor = anchor;
            vec![Effect::Navigate(target)]
        }
        Err(err) => {
            model.status = Some(format!("bad link: {err}"));
            Vec::new()
        }
    }
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

/// Move the focus cursor to the next link in reading order (wrapping), scrolling
/// it into view. A no-op when the page has no links.
fn focus_next(model: &mut Model) {
    let n = model.links.len();
    if n == 0 {
        return;
    }
    let next = match model.focus {
        None => 1,
        Some(i) if i >= n => 1,
        Some(i) => i + 1,
    };
    set_focus(model, next);
}

/// Move the focus cursor to the previous link in reading order (wrapping),
/// scrolling it into view. A no-op when the page has no links.
fn focus_prev(model: &mut Model) {
    let n = model.links.len();
    if n == 0 {
        return;
    }
    let prev = match model.focus {
        None => n,
        Some(i) if i <= 1 => n,
        Some(i) => i - 1,
    };
    set_focus(model, prev);
}

/// Set the focus to link `index` and auto-scroll so it is inside the viewport.
fn set_focus(model: &mut Model, index: usize) {
    model.focus = Some(index);
    let Some(link) = model.links.iter().find(|l| l.index == index) else {
        return;
    };
    let line = link.line;
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
    // Overlays drawn on top of the laid-out page: the search-match highlights,
    // the focus highlight, and the hint badges while hint mode is active.
    render_search_matches(model, frame);
    render_focus(model, frame);
    if model.mode == Mode::Hint {
        render_hints(model, frame);
    }
    if model.show_places {
        render_places(model, frame, frame.area());
    }
    if model.show_help {
        render_help(frame, frame.area());
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

/// A garish, fully saturated true-colour rainbow the spinner glows through. The
/// colours are picked at full brightness so they pop on both the dark and light
/// chrome bar.
const RAINBOW: [(u8, u8, u8); 6] = [
    (255, 0, 0),   // red
    (255, 140, 0), // orange
    (255, 240, 0), // yellow
    (0, 230, 0),   // green
    (0, 220, 255), // cyan
    (255, 0, 200), // magenta
];

/// Render one frame of the loading spinner: the current braille glyph as a bold,
/// full-brightness rainbow span. The glyph advances one step per `spin` (the dot
/// circles), while the hue advances twice as fast (`spin * 2`) so the colour
/// shimmers faster than the dot position.
fn spinner_span(spin: usize) -> RtSpan<'static> {
    let glyph = SPIN_FRAMES[spin % SPIN_FRAMES.len()];
    let (r, g, b) = RAINBOW[(spin * 2) % RAINBOW.len()];
    RtSpan::styled(
        glyph,
        Style::default()
            .fg(Color::Rgb(r, g, b))
            .add_modifier(Modifier::BOLD),
    )
}

/// Draw the status bar: the loading spinner while a fetch is in flight, else a
/// status/error message, else the context key-hints.
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
    } else if let Some(msg) = &model.status {
        let err = Style::default().fg(Color::Rgb(255, 95, 95));
        frame.render_widget(Paragraph::new(RtSpan::styled(msg.clone(), err)), area);
    } else {
        let hints = match model.mode {
            Mode::Address => ADDRESS_HINTS,
            Mode::Hint => HINT_HINTS,
            Mode::Search => SEARCH_HINTS,
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
    "  Tab / Shift-Tab     focus prev/next",
    "  Enter               follow the link",
    "  f                   hint a link",
    "  / n N               search / next / prev",
    "  click               follow link",
    "  :                   enter an address",
    "  d                   places panel",
    "  m                   bookmark this page",
    "  y                   copy link / page URL",
    "  R                   reload the page",
    "  t                   toggle light / dark theme",
    "  M-← / M-→           back / forward",
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

    let mut lines: Vec<RtLine<'static>> = Vec::new();
    lines.push(RtLine::from(RtSpan::styled("Bookmarks", header)));
    if bm_count == 0 {
        lines.push(RtLine::from(RtSpan::styled("  (none)", muted)));
    } else {
        for (i, place) in entries.iter().enumerate().take(bm_count) {
            lines.push(place_line(place, i, model.places_sel, text_style, selected));
        }
    }
    lines.push(RtLine::from(""));
    lines.push(RtLine::from(RtSpan::styled("Discovered nodes", header)));
    if entries.len() == bm_count {
        lines.push(RtLine::from(RtSpan::styled("  (none)", muted)));
    } else {
        for (i, place) in entries.iter().enumerate().skip(bm_count) {
            lines.push(place_line(place, i, model.places_sel, text_style, selected));
        }
    }

    let width = 64u16.min(area.width);
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
) -> RtLine<'static> {
    let style = if idx == sel { selected } else { normal };
    RtLine::from(RtSpan::styled(place_label(place), style))
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
            Effect::Cancel => {
                if let Some(handle) = inflight.take() {
                    handle.abort();
                }
                // Invalidate any late result from the cancelled task.
                *generation = generation.wrapping_add(1);
            }
            Effect::Copy(text) => {
                // Write the OSC 52 clipboard-set sequence straight to the
                // terminal; the update side has already set the "copied" status.
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
        let loading = model.is_loading();
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
            _ = ticker.tick(), if loading => {
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
        assert!(m.status.is_some());
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
    fn esc_during_loading_yields_cancel() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.pending = Some(Pending {
            target: tgt(1),
            action: HistoryAction::Push,
        });
        let effects = press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(effects, vec![Effect::Cancel]);
        assert!(m.pending.is_none());
        assert_eq!(m.status.as_deref(), Some("cancelled"));
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
        assert_eq!(m.status.as_deref(), Some("no path"));
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
    fn spinner_span_shimmers_and_cycles() {
        // The glyph steps through the six braille frames as `spin` advances.
        for spin in 0..12 {
            assert_eq!(
                &*spinner_span(spin).content,
                SPIN_FRAMES[spin % SPIN_FRAMES.len()],
                "glyph out of cycle at spin={spin}"
            );
        }
        // Every frame is bold and a full-brightness rainbow true colour.
        for spin in 0..RAINBOW.len() {
            let style = spinner_span(spin).style;
            assert!(
                style.add_modifier.contains(Modifier::BOLD),
                "spinner not bold at spin={spin}"
            );
            let (r, g, b) = RAINBOW[(spin * 2) % RAINBOW.len()];
            assert_eq!(
                style.fg,
                Some(Color::Rgb(r, g, b)),
                "wrong rainbow fg at spin={spin}"
            );
        }
        // The hue shimmers: consecutive `spin` values glow in DIFFERENT rainbow
        // colours (the hue rotates faster than the glyph position).
        for spin in 0..RAINBOW.len() {
            assert_ne!(
                spinner_span(spin).style.fg,
                spinner_span(spin + 1).style.fg,
                "hue did not change between spin={spin} and spin={}",
                spin + 1
            );
        }
    }

    #[test]
    fn loading_status_bar_has_rainbow_bold_spinner_cell() {
        let mut m = loaded_model((80, 24));
        m.pending = Some(Pending {
            target: tgt(9),
            action: HistoryAction::Push,
        });
        m.spin = 3;
        let buffer = render(&m, 80, 24);
        let mut found = false;
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, 23)];
            if cell.modifier.contains(Modifier::BOLD)
                && RAINBOW
                    .iter()
                    .any(|&(r, g, b)| cell.fg == Color::Rgb(r, g, b))
            {
                found = true;
            }
        }
        assert!(
            found,
            "no bold rainbow spinner cell in the loading status bar"
        );
    }

    #[test]
    fn error_state_renders_message() {
        let mut m = loaded_model((80, 24));
        m.status = Some("no path to destination".to_string());
        let buffer = render(&m, 80, 24);
        let status = row_text(&buffer, 23, 80);
        assert!(status.contains("no path"), "error missing: {status:?}");
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
        let mut m = loaded_model((80, 24));
        assert_eq!(m.focus, None);
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, Some(1));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, Some(2));
        // Wrap back to the first link.
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, Some(1));
        // Shift-Tab goes backward, wrapping to the last.
        press(&mut m, KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(m.focus, Some(2));
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
        assert!(
            m.status.as_deref().unwrap_or("").contains("not found"),
            "unknown anchor should note it: {:?}",
            m.status
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
        assert!(
            m.status.as_deref().unwrap_or("").contains("no matches"),
            "a fruitless search should say so: {:?}",
            m.status
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
        assert!(m.status.as_deref().unwrap_or_default().contains("copied"));
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
}
