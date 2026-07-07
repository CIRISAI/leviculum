//! The ratatui/crossterm terminal UI: the testable core and the thin IO shell.
//!
//! The design is TEA (The Elm Architecture) so the interesting logic is pure
//! and driven from headless tests:
//!
//! - [`Model`] is the whole UI state (the laid-out page, its links, the title,
//!   the terminal size, a quit flag).
//! - [`update`] folds an [`AppEvent`] into the model. It does no IO, so a test
//!   feeds it events and inspects the model directly.
//! - [`view`] draws the model into a ratatui [`Frame`]. A test renders it into a
//!   [`ratatui::backend::TestBackend`] and asserts on the resulting buffer, with
//!   no real terminal involved.
//! - [`run_tui`] is the only part that touches a real terminal: it wires
//!   crossterm's [`EventStream`] into the update/view loop, guarded so the
//!   terminal is always restored (RAII [`TerminalGuard`] + a panic hook).
//!
//! Phase 2 adds vertical scrolling. The [`Model`] now owns the parsed
//! [`MicronDocument`] and its current layout width, so a resize *re-wraps* the
//! content to the new width (via [`Model::relayout`]) rather than merely
//! clipping it. Scroll offset is a line index into the laid-out page, moved by
//! a set of [`ScrollCmd`]s bound to both vi and emacs motions (plus the mouse
//! wheel), and always clamped to the page. [`view`] renders only the visible
//! slice and a ratatui `Scrollbar` on the right. Mouse hit-testing and link
//! navigation still land in later phases; the loop keeps its `tokio::select!`
//! so an async page fetch can join it without reshaping.

use std::io::{self, Stdout};

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
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as RtLine, Span as RtSpan, Text};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui::{Frame, Terminal};

use leviculum_micron::MicronDocument;

use crate::render::{layout, RLine, RStyle, RenderedLink};

/// The number of columns reserved on the right for the scrollbar.
const SCROLLBAR_COLS: u16 = 1;
/// How many lines one mouse-wheel notch scrolls.
const WHEEL_STEP: usize = 3;

/// The content layout width for a terminal `cols` wide: full width minus the
/// scrollbar column, never below 1 so wrapping stays well defined.
fn content_width(cols: u16) -> usize {
    (cols.saturating_sub(SCROLLBAR_COLS) as usize).max(1)
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
    /// The title (shown in the top-bar in a later phase; carried for now).
    pub title: String,
    /// The last known terminal size, as `(cols, rows)`.
    pub size: (u16, u16),
    /// Index of the top visible line in `page`. Always clamped to the page.
    pub scroll: usize,
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
            scroll: 0,
            quit: false,
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

    /// The number of page lines visible at once. Phase 2 has no chrome, so this
    /// is the full terminal height.
    pub fn viewport(&self) -> usize {
        self.size.1 as usize
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppEvent {
    /// A key was pressed.
    Key(KeyEvent),
    /// A mouse event occurred (unused in phase 1; carried for later hit-testing).
    Mouse(MouseEvent),
    /// The terminal was resized to `(cols, rows)`.
    Resize(u16, u16),
    /// An explicit request to quit.
    Quit,
}

/// Fold an event into the model. Pure and IO-free.
///
/// Handles quit (`q` / `Ctrl-C`), scroll motions (both vi and emacs idioms plus
/// the mouse wheel), redraw (`Ctrl-L`), and resize (store the size, re-wrap the
/// document to the new width, then re-clamp the scroll offset).
pub fn update(model: &mut Model, event: AppEvent) {
    match event {
        AppEvent::Quit => model.quit = true,
        AppEvent::Key(key) => {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            if key.code == KeyCode::Char('q') || (ctrl && key.code == KeyCode::Char('c')) {
                model.quit = true;
                return;
            }
            // Ctrl-L: recompute the layout and redraw, keeping the offset (just
            // re-clamped, in case the page got shorter).
            if ctrl && key.code == KeyCode::Char('l') {
                let (w, vp) = (model.width, model.viewport());
                model.relayout(w);
                model.clamp_scroll(vp);
                return;
            }
            if let Some(cmd) = key_to_scroll(&key) {
                let vp = model.viewport();
                model.apply_scroll(cmd, vp);
            }
        }
        AppEvent::Resize(cols, rows) => {
            model.size = (cols, rows);
            model.relayout(content_width(cols));
            let vp = model.viewport();
            model.clamp_scroll(vp);
        }
        AppEvent::Mouse(mouse) => {
            let vp = model.viewport();
            match mouse.kind {
                MouseEventKind::ScrollDown => model.scroll_lines_down(WHEEL_STEP, vp),
                MouseEventKind::ScrollUp => model.scroll_lines_up(WHEEL_STEP),
                _ => {}
            }
        }
    }
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

/// Draw the model into the frame: the visible slice of the page in the content
/// area, and a vertical `Scrollbar` in the reserved right-hand column.
///
/// Works at any size: when the page is shorter than the viewport the slice is
/// simply the whole page and the scrollbar shows a full track (no panic).
pub fn view(model: &Model, frame: &mut Frame) {
    let area = frame.area();
    let viewport = area.height as usize;

    // Reserve the rightmost column for the scrollbar; render content to the rest.
    let content = Rect {
        x: area.x,
        y: area.y,
        width: area.width.saturating_sub(SCROLLBAR_COLS),
        height: area.height,
    };

    let end = model.scroll.saturating_add(viewport).min(model.page.len());
    let start = model.scroll.min(end);
    let text = to_ratatui_text(&model.page[start..end]);
    frame.render_widget(Paragraph::new(text), content);

    let mut state = ScrollbarState::new(model.page.len())
        .viewport_content_length(viewport)
        .position(model.scroll);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None);
    frame.render_stateful_widget(scrollbar, area, &mut state);
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

/// Run the interactive TUI over a real terminal until the user quits.
///
/// Enters the UI under a [`TerminalGuard`] (with the panic hook installed),
/// then loops: draw the model, then await the next terminal event and fold it
/// in. The event await sits in a `tokio::select!` so a later phase can join an
/// async page fetch alongside it without restructuring the loop.
pub async fn run_tui(mut model: Model) -> io::Result<()> {
    install_panic_hook();
    let mut guard = TerminalGuard::new(CrosstermTerminal::new())?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut events = EventStream::new();

    // Sync to the real terminal size and re-wrap before the first draw:
    // crossterm does not emit an initial resize event.
    let size = terminal.size()?;
    model.size = (size.width, size.height);
    model.relayout(content_width(size.width));
    let vp = model.viewport();
    model.clamp_scroll(vp);

    loop {
        terminal.draw(|frame| view(&model, frame))?;
        if model.quit {
            break;
        }
        tokio::select! {
            maybe_event = events.next() => match maybe_event {
                Some(Ok(event)) => {
                    if let Some(app) = map_event(event) {
                        update(&mut model, app);
                    }
                }
                Some(Err(err)) => return Err(err),
                None => break,
            },
        }
    }

    guard.restore_now()
}

/// Translate a crossterm event into an [`AppEvent`], dropping the ones phase 1
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

    fn model_from_sample(width: usize, size: (u16, u16)) -> Model {
        Model::from_document(&parse(SAMPLE), width, "Sample", size)
    }

    #[test]
    fn quit_key_sets_quit_flag() {
        let mut model = model_from_sample(80, (80, 24));
        assert!(!model.quit);
        update(
            &mut model,
            AppEvent::Key(key(KeyCode::Char('q'), KeyModifiers::NONE)),
        );
        assert!(model.quit);
    }

    #[test]
    fn ctrl_c_sets_quit_flag() {
        let mut model = model_from_sample(80, (80, 24));
        update(
            &mut model,
            AppEvent::Key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(model.quit);
    }

    #[test]
    fn plain_c_does_not_quit() {
        let mut model = model_from_sample(80, (80, 24));
        update(
            &mut model,
            AppEvent::Key(key(KeyCode::Char('c'), KeyModifiers::NONE)),
        );
        assert!(!model.quit);
    }

    #[test]
    fn resize_updates_size() {
        let mut model = model_from_sample(80, (80, 24));
        update(&mut model, AppEvent::Resize(40, 10));
        assert_eq!(model.size, (40, 10));
    }

    #[test]
    fn view_renders_heading_and_underlined_link_label() {
        // Lay out at the inner width (80 - 2 for the border) so nothing clips.
        let model = model_from_sample(78, (80, 24));
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| view(&model, frame)).expect("draw");
        let buffer = terminal.backend().buffer();

        // Flatten the buffer to a single string to find the heading text.
        let mut flat = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                flat.push_str(buffer[(x, y)].symbol());
            }
            flat.push('\n');
        }
        assert!(flat.contains("Sample Page"), "heading missing:\n{flat}");
        assert!(flat.contains("Alpha"), "link label missing:\n{flat}");

        // The `Alpha` link label must carry LINK_FG + underline in the buffer.
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
            "no underlined LINK_FG 'A' cell found:\n{flat}"
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

    /// The plain text of a laid-out line (its cells' characters).
    fn line_text(line: &RLine) -> String {
        line.cells.iter().map(|c| c.ch).collect()
    }

    /// The text of buffer row `y` across columns `0..width`.
    fn row_text(buffer: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
        let mut s = String::new();
        for x in 0..width {
            s.push_str(buffer[(x, y)].symbol());
        }
        s
    }

    /// A tall single-paragraph document that wraps into many lines.
    fn long_doc() -> leviculum_micron::MicronDocument {
        let words: Vec<String> = (0..300).map(|i| format!("word{i:03}")).collect();
        parse(&words.join(" "))
    }

    /// A model over [`long_doc`], laid out to the content width for `size.0`.
    fn tall_model(size: (u16, u16)) -> Model {
        Model::from_document(&long_doc(), content_width(size.0), "Long", size)
    }

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

        // Clamp at the bottom: neither Line nor Page goes past max.
        m.apply_scroll(ScrollCmd::LineDown, vp);
        assert_eq!(m.scroll, 90, "LineDown clamps at bottom");
        m.apply_scroll(ScrollCmd::PageDown, vp);
        assert_eq!(m.scroll, 90, "PageDown clamps at bottom");

        // Clamp at the top.
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
        // Empty page and zero viewport must not panic or overflow either.
        let mut e = Model::default();
        e.apply_scroll(ScrollCmd::PageDown, 0);
        e.apply_scroll(ScrollCmd::Bottom, 0);
        assert_eq!(e.scroll, 0);
    }

    #[test]
    fn update_scroll_keys() {
        let mut m = tall_model((40, 10)); // viewport 10
        assert_eq!(m.scroll, 0);

        update(
            &mut m,
            AppEvent::Key(key(KeyCode::Char('j'), KeyModifiers::NONE)),
        );
        assert_eq!(m.scroll, 1, "j scrolls down one line");

        update(
            &mut m,
            AppEvent::Key(key(KeyCode::Char('f'), KeyModifiers::CONTROL)),
        );
        assert_eq!(m.scroll, 11, "Ctrl-f pages down by the viewport");

        update(
            &mut m,
            AppEvent::Key(key(KeyCode::Char('G'), KeyModifiers::NONE)),
        );
        let bottom = m.max_scroll(10);
        assert!(bottom > 0);
        assert_eq!(m.scroll, bottom, "G jumps to the bottom");

        update(
            &mut m,
            AppEvent::Key(key(KeyCode::Char('v'), KeyModifiers::ALT)),
        );
        assert_eq!(m.scroll, bottom - 10, "Alt-v pages up by the viewport");

        let before = m.scroll;
        update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollDown)));
        assert_eq!(
            m.scroll,
            (before + WHEEL_STEP).min(bottom),
            "wheel down scrolls by WHEEL_STEP"
        );
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
        let mut m = Model::from_document(&long_doc(), content_width(100), "Long", (100, 10));
        // Jump to the bottom, then resize both ways.
        update(
            &mut m,
            AppEvent::Key(key(KeyCode::Char('G'), KeyModifiers::NONE)),
        );

        update(&mut m, AppEvent::Resize(40, 10));
        let narrow = m.page.len();
        assert!(
            m.scroll <= m.max_scroll(m.viewport()),
            "scroll must stay clamped after shrinking"
        );

        update(&mut m, AppEvent::Resize(100, 10));
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

    #[test]
    fn view_scrolls_slice_and_draws_scrollbar() {
        let w = 40u16;
        let mut m = Model::from_document(&long_doc(), content_width(w), "Long", (w, 10));
        assert!(m.page.len() > 20, "fixture must exceed the viewport");

        let backend = TestBackend::new(w, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");

        // Before scrolling: the top visible row is page line 0.
        terminal.draw(|frame| view(&m, frame)).expect("draw");
        let top0 = row_text(terminal.backend().buffer(), 0, w - SCROLLBAR_COLS);
        assert_eq!(top0.trim_end(), line_text(&m.page[0]).trim_end());

        // After PageDown (viewport 10): the top visible row is page line 10.
        update(
            &mut m,
            AppEvent::Key(key(KeyCode::Char('f'), KeyModifiers::CONTROL)),
        );
        assert_eq!(m.scroll, 10);
        terminal.draw(|frame| view(&m, frame)).expect("draw");
        let buffer = terminal.backend().buffer();
        let top1 = row_text(buffer, 0, w - SCROLLBAR_COLS);
        assert_eq!(top1.trim_end(), line_text(&m.page[10]).trim_end());

        // A scrollbar occupies the reserved right-hand column.
        let mut scrollbar_cell = false;
        for y in 0..10 {
            if buffer[(w - SCROLLBAR_COLS, y)].symbol() != " " {
                scrollbar_cell = true;
            }
        }
        assert!(scrollbar_cell, "scrollbar column should not be empty");
    }

    /// A mock terminal that records whether `restore` ran, via a shared flag.
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
        // Second call is a no-op and drop does not restore again.
        restored.set(false);
        guard.restore_now().expect("restore idempotent");
        assert!(!restored.get(), "restore ran twice");
    }
}
