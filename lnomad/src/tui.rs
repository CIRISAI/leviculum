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
//! Phase 1 keeps the interaction minimal: quit on `q`/`Ctrl-C`, store the size
//! on resize, and draw the top of the page. Scrolling, mouse hit-testing and
//! link navigation land in later phases; the loop is already structured with a
//! `tokio::select!` so an async page fetch can join it without reshaping.

use std::io::{self, Stdout};

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyModifiers,
    MouseEvent,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as RtLine, Span as RtSpan, Text};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{Frame, Terminal};

use leviculum_micron::MicronDocument;

use crate::render::{layout, RLine, RStyle, RenderedLink};

/// The complete UI state. Pure data: [`update`] is the only thing that mutates
/// it, and it never performs IO.
#[derive(Clone, Debug, Default)]
pub struct Model {
    /// The page laid out into target-agnostic styled lines.
    pub page: Vec<RLine>,
    /// The page's links, with their laid-out positions for hit-testing.
    pub links: Vec<RenderedLink>,
    /// The title shown in the frame's border.
    pub title: String,
    /// The last known terminal size, as `(cols, rows)`.
    pub size: (u16, u16),
    /// Set once the user has asked to quit; the IO loop breaks on it.
    pub quit: bool,
}

impl Model {
    /// Build a model from already laid-out lines and links.
    pub fn new(
        page: Vec<RLine>,
        links: Vec<RenderedLink>,
        title: String,
        size: (u16, u16),
    ) -> Self {
        Self {
            page,
            links,
            title,
            size,
            quit: false,
        }
    }

    /// Lay a parsed document out at `width` and build a model from it.
    pub fn from_document(
        doc: &MicronDocument,
        width: usize,
        title: impl Into<String>,
        size: (u16, u16),
    ) -> Self {
        let (page, links) = layout(doc, width);
        Self::new(page, links, title.into(), size)
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
/// Phase 1 handles only: quit on `q` or `Ctrl-C`, and store the new size on a
/// resize. Everything else is ignored for now.
pub fn update(model: &mut Model, event: AppEvent) {
    match event {
        AppEvent::Quit => model.quit = true,
        AppEvent::Key(key) => {
            let ctrl_c =
                key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
            if key.code == KeyCode::Char('q') || ctrl_c {
                model.quit = true;
            }
        }
        AppEvent::Resize(cols, rows) => model.size = (cols, rows),
        AppEvent::Mouse(_) => {}
    }
}

/// Draw the model into the frame: a single full-screen bordered paragraph
/// showing the page from the top (no scroll yet).
pub fn view(model: &Model, frame: &mut Frame) {
    let text = to_ratatui_text(&model.page);
    let block = Block::bordered().title(model.title.clone());
    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, frame.area());
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
