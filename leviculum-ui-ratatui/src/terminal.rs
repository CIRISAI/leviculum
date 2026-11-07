//! The terminal lifecycle: raw mode, the alternate screen and mouse capture
//! on the way in, all of it reversed on the way out, on every path out
//! (normal return, `?`, panic unwind, and a panic's own message).

use std::io::{self, Stdout, Write};
use std::sync::{Arc, Mutex, PoisonError};

use crossterm::cursor::MoveTo;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, Clear, ClearType,
    EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::QueueableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::{Frame, Terminal};

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
    mouse: bool,
}

impl CrosstermTerminal {
    /// A terminal driver writing to stdout, capturing the mouse.
    pub fn new() -> Self {
        Self::with_mouse(true)
    }

    /// The same, capturing the mouse only when `mouse` is set: without
    /// capture the terminal's own selection and copy work as in any other
    /// program (loquax's `--no-mouse`).
    pub fn with_mouse(mouse: bool) -> Self {
        Self {
            out: io::stdout(),
            mouse,
        }
    }
}

/// Turn mouse capture on or off while the UI is up: a program's runtime
/// toggle. Leaving the UI turns it off either way.
pub fn set_mouse_capture(on: bool) -> io::Result<()> {
    let mut out = io::stdout();
    match on {
        true => execute!(out, EnableMouseCapture),
        false => execute!(out, DisableMouseCapture),
    }
}

impl Default for CrosstermTerminal {
    fn default() -> Self {
        Self::new()
    }
}

/// The bytes that enter the UI: the alternate screen, mouse capture when
/// `mouse` is set, then a cleared screen with the cursor home. ratatui's first
/// draw diffs against a blank buffer and never writes a blank cell, so the
/// clear is what blanks them. Not every terminal gives a fresh alternate
/// screen: mosh 1.4.0 ignores `?1049h` (its `get_DEC_mode` has no case for
/// it), and the shell output stayed visible behind the first frame.
fn write_enter<W: Write>(out: &mut W, mouse: bool) -> io::Result<()> {
    execute!(out, EnterAlternateScreen)?;
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }
    execute!(out, Clear(ClearType::All), MoveTo(0, 0))
}

impl TerminalOps for CrosstermTerminal {
    fn enter(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        write_enter(&mut self.out, self.mouse)
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

    /// Enter the UI again after something left it while the guard held it:
    /// the panic hook does, and a caller that caught the panic carries on.
    pub fn enter_again(&mut self) -> io::Result<()> {
        self.ops.enter()?;
        self.restored = false;
        Ok(())
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
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        let _ = execute!(
            out,
            EndSynchronizedUpdate,
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let _ = disable_raw_mode();
        previous(info);
    }));
}

/// Draw one frame between a begin and an end of synchronized update (mode
/// 2026): the terminal shows the screen as it was until the frame is whole,
/// never a state inside it. A picture is written as an erase of its cells
/// and then the sixel; foot fed frames back to back showed the erased pane
/// in 128 of 206 grabs of a map drag with names, and none with the frame
/// bracketed (1118). A terminal without the mode ignores both. The end is
/// written when `render` panics as well, so a frame that panics never holds
/// the screen.
pub fn draw_synced<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    render: impl FnOnce(&mut Frame),
) -> io::Result<()> {
    terminal.backend_mut().queue(BeginSynchronizedUpdate)?;
    let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        terminal.draw(render).map(|_| ())
    }));
    let ended = execute!(terminal.backend_mut(), EndSynchronizedUpdate);
    match drawn {
        Ok(drawn) => drawn.and(ended),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// A terminal's writer that can hold what a frame writes (1455), so a
/// frame that changes nothing on the screen writes nothing at all:
/// ratatui writes its attribute reset and the cursor's state on every
/// draw, and the synchronized update's begin and end around it, a few
/// dozen bytes the terminal (and an ssh or mosh link) is woken for on
/// every pass of the program's loop. Passes everything through while it
/// holds nothing; [`draw_changed`] holds a frame through its [`Drawn`].
pub struct Held<W: Write> {
    inner: W,
    held: Arc<Mutex<Option<Vec<u8>>>>,
}

impl<W: Write> Held<W> {
    /// `inner` held by the [`Drawn`] given with it.
    pub fn new(inner: W) -> (Self, Drawn) {
        let held = Arc::new(Mutex::new(None));
        let drawn = Drawn {
            held: Arc::clone(&held),
            buffer: None,
            bytes: Vec::new(),
        };
        (Self { inner, held }, drawn)
    }

    fn holding(&self) -> std::sync::MutexGuard<'_, Option<Vec<u8>>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<W: Write> Write for Held<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(held) = self.holding().as_mut() {
            held.extend_from_slice(buf);
            return Ok(buf.len());
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.holding().is_some() {
            return Ok(());
        }
        self.inner.flush()
    }
}

/// The last frame [`draw_changed`] wrote, its cells and its bytes, and
/// the hold on its terminal's [`Held`] writer.
pub struct Drawn {
    held: Arc<Mutex<Option<Vec<u8>>>>,
    buffer: Option<ratatui::buffer::Buffer>,
    bytes: Vec<u8>,
}

impl Drawn {
    fn hold(&self, on: bool) -> Option<Vec<u8>> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        match on {
            true => held.replace(Vec::new()),
            false => held.take(),
        }
    }
}

/// [`draw_synced`], written only when it changes the screen (1455): the
/// frame is drawn with its bytes held, and they are thrown away when its
/// cells are those of the last frame written and its bytes are too (no
/// cell, the same cursor). ratatui keeps its own copy of the last frame
/// either way, which is these cells, so the next frame's diff is right.
/// Whether it was written. `last` is the one [`Held::new`] gave with the
/// terminal's writer.
pub fn draw_changed<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<Held<W>>>,
    last: &mut Drawn,
    render: impl FnOnce(&mut Frame),
) -> io::Result<bool> {
    let _ = last.hold(true);
    let begun = terminal
        .backend_mut()
        .queue(BeginSynchronizedUpdate)
        .map(|_| ());
    let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        terminal.draw(render).map(|frame| frame.buffer.clone())
    }));
    let ended = terminal
        .backend_mut()
        .queue(EndSynchronizedUpdate)
        .map(|_| ());
    let bytes = last.hold(false).unwrap_or_default();
    // A frame that panics wrote nothing yet: the begin is held with it.
    let buffer = match drawn {
        Ok(drawn) => drawn?,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    begun.and(ended)?;
    if last.buffer.as_ref() == Some(&buffer) && last.bytes == bytes {
        return Ok(false);
    }
    let out = terminal.backend_mut();
    out.write_all(&bytes)?;
    out.flush()?;
    last.buffer = Some(buffer);
    last.bytes = bytes;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

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

    /// Entered again after a caught panic left the UI: the drop restores
    /// once more.
    #[test]
    fn guard_entered_again_restores_again() {
        let entered = Rc::new(Cell::new(false));
        let restored = Rc::new(Cell::new(false));
        let mut guard = TerminalGuard::new(MockOps {
            entered: entered.clone(),
            restored: restored.clone(),
        })
        .expect("enter");
        guard.restore_now().expect("restore");
        entered.set(false);
        restored.set(false);
        guard.enter_again().expect("enter again");
        assert!(entered.get());
        drop(guard);
        assert!(
            restored.get(),
            "restore must run on drop after entering again"
        );
    }

    /// The screen is cleared and the cursor homed after the alternate screen
    /// switch, with and without mouse capture.
    #[test]
    fn enter_clears_after_the_alternate_screen() {
        let clear = "\x1b[2J\x1b[1;1H";
        for mouse in [false, true] {
            let mut out = Vec::new();
            write_enter(&mut out, mouse).expect("write");
            let text = String::from_utf8(out).expect("utf8");
            let alt = text.find("\x1b[?1049h").expect("alternate screen");
            assert!(
                text.ends_with(clear),
                "mouse={mouse}: no clear at the end: {text:?}"
            );
            assert!(alt < text.len() - clear.len());
        }
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

    /// A writer whose bytes the test keeps after the terminal has them.
    #[derive(Clone, Default)]
    struct Sink(Rc<std::cell::RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn sink_terminal(sink: &Sink) -> Terminal<CrosstermBackend<Sink>> {
        let area = ratatui::layout::Rect::new(0, 0, 10, 2);
        let options = ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(area),
        };
        Terminal::with_options(CrosstermBackend::new(sink.clone()), options).expect("terminal")
    }

    /// The frame's bytes stand between the begin and the end of a
    /// synchronized update, the begin first and the end last (1118).
    #[test]
    fn a_synced_frame_is_bracketed_by_mode_2026() {
        let sink = Sink::default();
        let mut terminal = sink_terminal(&sink);
        draw_synced(&mut terminal, |f| {
            f.render_widget(ratatui::widgets::Paragraph::new("map"), f.area())
        })
        .expect("draw");
        let text = String::from_utf8(sink.0.borrow().clone()).expect("utf8");
        let (begin, end) = ("\x1b[?2026h", "\x1b[?2026l");
        assert!(text.starts_with(begin), "{text:?}");
        assert!(text.ends_with(end), "{text:?}");
        let map = text.find("map").expect("the frame");
        assert!(begin.len() <= map && map < text.len() - end.len());
    }

    /// A frame that panics still ends the update, so the terminal does not
    /// keep the screen held; the panic goes on to the caller.
    #[test]
    fn a_frame_that_panics_still_ends_the_update() {
        let sink = Sink::default();
        let mut terminal = sink_terminal(&sink);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            draw_synced(&mut terminal, |_| panic!("a frame that panics"))
        }));
        assert!(panicked.is_err(), "the panic reaches the caller");
        let text = String::from_utf8(sink.0.borrow().clone()).expect("utf8");
        assert!(text.starts_with("\x1b[?2026h"), "{text:?}");
        assert!(text.ends_with("\x1b[?2026l"), "{text:?}");
    }

    /// A terminal left alone writes nothing (1455): five seconds of the
    /// loop's 20 ms passes drawing the same screen, the cursor in the
    /// same place, write the first frame and then at most its cursor's
    /// bytes once (the first pass after a change has no cell to write but
    /// its own cursor bytes are new); nothing after. The controls: a
    /// clock that moves a cell every pass writes every pass; a cursor
    /// that moves writes; [`draw_synced`], which the loop drew with
    /// before, writes every pass of the unchanged screen.
    #[test]
    fn an_unchanged_screen_writes_nothing() {
        use ratatui::layout::{Position, Rect};
        use ratatui::widgets::Paragraph;
        use ratatui::{TerminalOptions, Viewport};
        type Screen = Terminal<CrosstermBackend<Held<Sink>>>;
        let terminal = || -> (Screen, Drawn, Sink) {
            let sink = Sink::default();
            let (writer, drawn) = Held::new(sink.clone());
            let terminal = Terminal::with_options(
                CrosstermBackend::new(writer),
                TerminalOptions {
                    viewport: Viewport::Fixed(Rect::new(0, 0, 30, 4)),
                },
            )
            .expect("a terminal");
            (terminal, drawn, sink)
        };
        const PASSES: u32 = 250;
        // Idle: the same words and cursor every pass.
        let (mut idle, mut last, sink) = terminal();
        let written = || sink.0.borrow().len();
        let mut writes = Vec::new();
        for _ in 0..PASSES {
            let before = written();
            draw_changed(&mut idle, &mut last, |f| {
                f.render_widget(Paragraph::new("Anna: see you at ten"), f.area());
                f.set_cursor_position(Position::new(3, 2));
            })
            .expect("drawn");
            writes.push(written() - before);
        }
        let passes_written = writes.iter().filter(|&&n| n > 0).count();
        assert!(writes[0] > 20, "the first frame: {writes:?}");
        assert!(
            passes_written <= 2,
            "{passes_written} passes wrote: {:?}",
            &writes[..8]
        );
        assert!(writes[2..].iter().all(|&n| n == 0), "{:?}", &writes[..8]);
        // The control: a cell that changes every pass writes every pass.
        let (mut ticking, mut ticked, ticks) = terminal();
        let mut wrote = 0;
        for i in 0..PASSES {
            let before = ticks.0.borrow().len();
            draw_changed(&mut ticking, &mut ticked, |f| {
                f.render_widget(Paragraph::new(format!("{:03}", i % 1000)), f.area());
            })
            .expect("drawn");
            wrote += usize::from(ticks.0.borrow().len() > before);
        }
        assert_eq!(wrote, PASSES as usize);
        // The control: the cursor moved, nothing else.
        let before = written();
        let moved = draw_changed(&mut idle, &mut last, |f| {
            f.render_widget(Paragraph::new("Anna: see you at ten"), f.area());
            f.set_cursor_position(Position::new(4, 2));
        })
        .expect("drawn");
        assert!(moved && written() > before);
        // The control: before 1455 every pass wrote, the screen unchanged.
        let plain = Sink::default();
        let mut before_1455 = sink_terminal(&plain);
        let mut wrote = 0;
        for _ in 0..PASSES {
            let before = plain.0.borrow().len();
            draw_synced(&mut before_1455, |f| {
                f.render_widget(Paragraph::new("Anna"), f.area());
                f.set_cursor_position(Position::new(3, 1));
            })
            .expect("drawn");
            wrote += usize::from(plain.0.borrow().len() > before);
        }
        assert_eq!(wrote, PASSES as usize);
    }
}
