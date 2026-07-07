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

use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyModifiers,
    MouseEvent, MouseEventKind,
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
use throbber_widgets_tui::{Throbber, ThrobberState};
use tokio::sync::{mpsc, Mutex};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;
use unicode_width::UnicodeWidthStr;

use leviculum_micron::MicronDocument;

use crate::browser::{self, BrowserOptions};
use crate::fetch::Session;
use crate::render::{layout, RLine, RStyle, RenderedLink};
use crate::url::{parse_url, Target};

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

/// Browse-mode status hints.
const BROWSE_HINTS: &str = "j/k scroll  : address  R reload  M-←/→ back/fwd  ? help  q quit";
/// Address-mode status hints.
const ADDRESS_HINTS: &str = "Enter: go   Esc: cancel";

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
    /// The current interaction mode (browse or address entry).
    pub mode: Mode,
    /// The address-bar editor.
    pub input: Input,
    /// A transient status/error message shown in the status bar, or `None` for
    /// the default hints.
    pub status: Option<String>,
    /// The loading spinner's animation state.
    pub spinner: ThrobberState,
    /// Whether the keybinding help overlay is shown.
    pub show_help: bool,
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
        let (page, links) = layout(doc, width);
        Self {
            doc: doc.clone(),
            width,
            page,
            links,
            title: title.into(),
            size,
            ..Self::default()
        }
    }

    /// Re-wrap the stored document to `width`, replacing `page`/`links`. The
    /// caller is responsible for re-clamping `scroll` afterwards.
    pub fn relayout(&mut self, width: usize) {
        self.width = width;
        let (page, links) = layout(&self.doc, width);
        self.page = page;
        self.links = links;
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
}

/// A UI input event, decoupled from crossterm so [`update`] is trivially
/// testable and the event source can be swapped later.
#[derive(Clone, Debug)]
pub enum AppEvent {
    /// A key was pressed.
    Key(KeyEvent),
    /// A mouse event occurred (wheel scrolling; clicks land in a later phase).
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
            model.spinner.calc_next();
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
                MouseEventKind::ScrollDown => model.scroll_lines_down(WHEEL_STEP, vp),
                MouseEventKind::ScrollUp => model.scroll_lines_up(WHEEL_STEP),
                _ => {}
            }
            Vec::new()
        }
        AppEvent::Key(key) => update_key(model, key),
    }
}

/// Fold a completed page load: replace the document, relayout, reset scroll, set
/// the title, and record the navigation in history per its pending action.
fn apply_loaded(model: &mut Model, doc: MicronDocument, title: String) {
    let pending = model.pending.take();
    model.doc = doc;
    model.relayout(content_width(model.size.0));
    model.scroll = 0;
    model.title = title;
    model.status = None;
    if let Some(pending) = pending {
        model.current_dest = Some(pending.target.dest_hash);
        match pending.action {
            HistoryAction::Push => model.history.visit(pending.target),
            HistoryAction::Goto(idx) => model.history.goto(idx),
        }
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

    match model.mode {
        Mode::Address => update_address_key(model, key),
        Mode::Browse => update_browse_key(model, key, ctrl),
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
        model.mode = Mode::Address;
        model.input.reset();
        model.status = None;
        return Vec::new();
    }

    // Toggle the help overlay.
    if key.code == KeyCode::Char('?') {
        model.show_help = true;
        return Vec::new();
    }

    // Reload the current page (no history change).
    if key.code == KeyCode::Char('R') {
        if let Some(target) = model.history.current().cloned() {
            let idx = model.history.idx;
            model.pending = Some(Pending {
                target: target.clone(),
                action: HistoryAction::Goto(idx),
            });
            model.status = None;
            return vec![Effect::Navigate(target)];
        }
        return Vec::new();
    }

    // Back / forward (Alt-Left / Alt-Right): peek the target and re-fetch it,
    // moving the cursor only once it loads.
    if alt && key.code == KeyCode::Left && model.history.can_back() {
        let idx = model.history.idx - 1;
        let target = model.history.stack[idx].clone();
        model.pending = Some(Pending {
            target: target.clone(),
            action: HistoryAction::Goto(idx),
        });
        model.status = None;
        return vec![Effect::Navigate(target)];
    }
    if alt && key.code == KeyCode::Right && model.history.can_forward() {
        let idx = model.history.idx + 1;
        let target = model.history.stack[idx].clone();
        model.pending = Some(Pending {
            target: target.clone(),
            action: HistoryAction::Goto(idx),
        });
        model.status = None;
        return vec![Effect::Navigate(target)];
    }

    if let Some(cmd) = key_to_scroll(&key) {
        let vp = model.viewport();
        model.apply_scroll(cmd, vp);
    }
    Vec::new()
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
    if model.show_help {
        render_help(frame, frame.area());
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

    let mut state = ScrollbarState::new(model.page.len())
        .viewport_content_length(viewport)
        .position(model.scroll);
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
    let title_row = Rect { height: 1, ..area };
    let title = RtLine::from(RtSpan::styled(
        model.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(title), title_row);

    if area.height < TOPBAR_ROWS {
        return;
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    let normal = Style::default();
    for cr in top_bar_controls(area) {
        match cr.control {
            Control::Back => {
                let style = if model.history.can_back() {
                    normal
                } else {
                    dim
                };
                frame.render_widget(Paragraph::new(RtSpan::styled(BACK_LABEL, style)), cr.rect);
            }
            Control::Forward => {
                let style = if model.history.can_forward() {
                    normal
                } else {
                    dim
                };
                frame.render_widget(
                    Paragraph::new(RtSpan::styled(FORWARD_LABEL, style)),
                    cr.rect,
                );
            }
            Control::Reload => {
                let style = if model.history.current().is_some() {
                    normal
                } else {
                    dim
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

/// Draw the status bar: the loading spinner while a fetch is in flight, else a
/// status/error message, else the context key-hints.
fn render_status(model: &Model, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    if let Some(pending) = &model.pending {
        let label = format!(" loading {}", pending.target.path);
        let throbber = Throbber::default().label(label).style(dim);
        frame.render_widget(Paragraph::new(throbber.to_line(&model.spinner)), area);
    } else if let Some(msg) = &model.status {
        let err = Style::default().fg(Color::Rgb(255, 95, 95));
        frame.render_widget(Paragraph::new(RtSpan::styled(msg.clone(), err)), area);
    } else {
        let hints = match model.mode {
            Mode::Address => ADDRESS_HINTS,
            Mode::Browse => BROWSE_HINTS,
        };
        frame.render_widget(Paragraph::new(RtSpan::styled(hints, dim)), area);
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
    "  :                   enter an address",
    "  R                   reload the page",
    "  M-← / M-→           back / forward",
    "  Esc / Ctrl-g        cancel a load",
    "  ?                   toggle this help",
    "  q / Ctrl-c          quit",
];

/// Draw the centered help overlay listing the keybindings.
fn render_help(frame: &mut Frame, area: Rect) {
    let width = 44u16.min(area.width);
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
            Effect::Quit => model.quit = true,
        }
    }
}

/// Run the interactive TUI, owning the [`Session`] and driving navigation.
///
/// Does the initial fetch of `initial` and every subsequent navigation as a
/// background task; the task's result rejoins the `tokio::select!` loop as a
/// [`AppEvent::PageLoaded`] / [`AppEvent::LoadFailed`], so the UI stays live
/// (spinner animates, keys and the address editor work) while a page loads. A
/// cancel drops the in-flight task; a slow or failed fetch never blocks the UI.
pub async fn run_tui(session: Session, initial: Target, opts: BrowserOptions) -> io::Result<()> {
    install_panic_hook();
    let mut guard = TerminalGuard::new(CrosstermTerminal::new())?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut events = EventStream::new();

    let mut model = Model::default();
    let size = terminal.size()?;
    model.size = (size.width, size.height);
    model.relayout(content_width(size.width));

    let session = Arc::new(Mutex::new(session));
    let (tx, mut rx) = mpsc::unbounded_channel::<FetchOutcome>();
    let mut inflight: Option<tokio::task::JoinHandle<()>> = None;
    let mut generation: u64 = 0;

    // Kick off the initial navigation.
    model.pending = Some(Pending {
        target: initial.clone(),
        action: HistoryAction::Push,
    });
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
            maybe_event = events.next() => match maybe_event {
                Some(Ok(event)) => {
                    if let Some(app) = map_event(event) {
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
                }
                Some(Err(err)) => break Err(err),
                None => break Ok(()),
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
    fn address_mode_renders_editor() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Char(':'), KeyModifiers::NONE);
        type_str(&mut m, "abc");
        let buffer = render(&m, 80, 24);
        let text = flat(&buffer);
        assert!(text.contains("abc"), "editor content missing:\n{text}");
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
}
