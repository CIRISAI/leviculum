//! `loquax tui`: the terminal frontend over the core's screen model
//! ([`loquax::ui`]), slice one (`docs/src/concepts/loquax-ui.md`).
//!
//! The binary's module, not the library's: the engine, the store and the
//! runners it drives are the [`Shell`] both frontends share
//! (`crate::shell`). Three parts here:
//!
//! - [`draw`] turns a [`View`] into a ratatui frame over the shared widgets
//!   (footer, toast, help, the styled-line bridge).
//! - [`run`] is the loop: the real terminal (`run_terminal`), or a key script
//!   that dumps frames to a file (`run_script`), which is how the snapshot
//!   test in `tests/tui_snapshot.rs` drives a session against a daemon.
//! - [`map`] draws the map's picture in the conversation pane's place, by
//!   sixel, kitty, half-blocks or braille.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crossterm::event::{
    Event, EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use futures::StreamExt;
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::{Frame, Terminal};

use leviculum_ui::clipboard::osc52;
use leviculum_ui::color::{resolve_tier_from_env, ColorFlag, Tier};
use leviculum_ui::delivery::DeliveryState;
use leviculum_ui::footer;
use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use leviculum_ui::input::{MouseButton, MouseEvent, MouseEventKind};
use leviculum_ui::strings::Lang;
use leviculum_ui::theme::{resolve_theme, Theme, ThemeFlag};
use leviculum_ui::tokens::Tokens;
use leviculum_ui_ratatui::background::detect_background;
use leviculum_ui_ratatui::event::{key_event, mouse_event, step_event, EventStep};
use leviculum_ui_ratatui::geometry::to_ratatui;
use leviculum_ui_ratatui::images::{self, Between, Picker};
use leviculum_ui_ratatui::style::downgrade_buffer;
use leviculum_ui_ratatui::terminal::{
    draw_changed, install_panic_hook, set_mouse_capture, Held, TerminalOps,
};
use leviculum_ui_ratatui::{help, to_ratatui_text, toast, CrosstermTerminal, TerminalGuard};

use loquax::keys;
use loquax::send::Leg;
use loquax::ui::{self, help_groups, Pane, UiEvent, View};
use loquax::when::WallTime;

pub mod map;

use crate::shell::{
    self, local_now, script_clock, Model, Output, Shell, ShellOptions, PUMP_MS, TICK_MS,
};

/// How long a script's `until` waits by default.
const UNTIL_SECS: u64 = 30;
/// The longest a script's `sleep` may ask for.
const SLEEP_MAX_MS: u64 = 60_000;
/// Everything `loquax tui` was told on the command line.
pub struct TuiOptions {
    /// `--sync-interval`, when given: the schedule's bound in milliseconds,
    /// `None` inside for the timer off.
    pub sync_interval: Option<Option<u64>>,
    pub to: Vec<[u8; 16]>,
    pub pn: Option<String>,
    pub instance: Option<String>,
    pub config: Option<PathBuf>,
    pub timeout: u64,
    pub ascii: bool,
    pub no_color: bool,
    /// `--color`: the ladder's second rung.
    pub color: ColorFlag,
    /// `--theme`, over the config file's `theme` key.
    pub theme: Option<ThemeFlag>,
    /// Start with the mouse not captured, so the terminal's own selection
    /// works; the palette's `mouse` turns capture on.
    pub no_mouse: bool,
    /// `--graphics`: how the map is drawn.
    pub graphics: map::GraphicsFlag,
    pub script: Option<PathBuf>,
    pub frames: Option<PathBuf>,
    pub size: (u16, u16),
    /// `--no-start`: fail when no shared instance answers (#471 step 4).
    pub no_start: bool,
}

/// `loquax tui`.
pub async fn run(options: TuiOptions) -> ExitCode {
    let (mut shell, config) = match shell::start(&ShellOptions {
        sync_interval: options.sync_interval,
        to: options.to.clone(),
        pn: options.pn.clone(),
        instance: options.instance.clone(),
        config: options.config.clone(),
        timeout: options.timeout,
        ascii: options.ascii,
        no_mouse: options.no_mouse,
        scripted: options.script.is_some(),
        log_file: "tui.log",
        no_start: options.no_start,
    })
    .await
    {
        Ok(started) => started,
        Err(code) => return code,
    };
    // A script plays its own terminal: monochrome, dark and English until
    // its `tier`, `theme` and `lang` steps say otherwise, whatever the
    // host's. The real terminal speaks the locale's language, as the window
    // does (1381): `LANG=C` is English.
    let theme_flag = options.theme.or(config.theme).unwrap_or_default();
    if options.script.is_none() {
        shell.model.tier = resolve_tier_from_env(options.no_color, options.color);
        shell.model.lang = loquax::engine::locale();
    }
    // The zoom and the pins in view only under the debug overlay (1447).
    shell.model.debug_overlay = std::env::var_os("LOQUAX_FPS").is_some_and(|v| v == "1");
    let outcome = match &options.script {
        Some(script) => {
            let frames = options
                .frames
                .clone()
                .unwrap_or_else(|| script.with_extension("frames"));
            run_script(&mut shell, script, &frames, &options).await
        }
        None => {
            run_terminal(
                &mut shell,
                theme_flag,
                options.graphics,
                &config.map_drawing,
            )
            .await
        }
    };
    shell::finish(shell, outcome).await
}

/// Said when the terminal view is started with its input or output not a
/// terminal: from a script, a pipe or a service.
pub(crate) const NOT_A_TERMINAL: &str = "the terminal view needs a terminal on its input and \
     output; run it in one, or \"loquax gui\" for the window";

/// The terminal with the keyboard enhancement protocol on top, where the
/// terminal speaks it: `Shift-Enter` and `Ctrl-Enter` then arrive as
/// themselves (loquax-keys-and-layout.md, section 3, "What the terminal can and
/// cannot send"). Pushed after the screen is entered and popped before it is
/// left, so the guard pops it on every way out; the panic hook pops it too.
struct KeysTerminal {
    inner: CrosstermTerminal,
    enhanced: bool,
}

impl TerminalOps for KeysTerminal {
    fn enter(&mut self) -> std::io::Result<()> {
        self.inner.enter()?;
        // The focus reports read receipts wait for (1231); a terminal that
        // sends none is in front once the reader typed in the chat.
        crossterm::execute!(std::io::stdout(), crossterm::event::EnableFocusChange)?;
        if self.enhanced {
            crossterm::execute!(
                std::io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }
        Ok(())
    }

    fn restore(&mut self) -> std::io::Result<()> {
        if self.enhanced {
            let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableFocusChange);
        self.inner.restore()
    }
}

/// The once-at-start `tui.log` line naming the chords live in this terminal.
fn log_keys(model: &Model) {
    let ctx = model.ctx();
    tracing::info!(
        "KEYS enhancement={} newline={} send={}",
        if model.enhanced { "yes" } else { "no" },
        keys::newline_chord(ctx),
        keys::send_chord(ctx)
    );
}

/// The once-at-start `tui.log` line naming how the map is drawn and why,
/// beside the `KEYS` line.
fn log_graphics(painter: &map::MapPainter, chosen: &map::Chosen, facts: &map::Facts) {
    let setup = painter.setup();
    let between = facts.between;
    let names = [
        (between.mosh, "mosh"),
        (between.tmux, "tmux"),
        (between.screen, "screen"),
    ];
    let between: Vec<&str> = names.iter().filter(|(b, _)| *b).map(|(_, n)| *n).collect();
    tracing::info!(
        "GRAPHICS backend={} reason={} detected={} between={} cell={}x{} zooms={}-{} context={}",
        painter.graphics().name(),
        chosen.reason(),
        facts
            .detected
            .map_or("none".to_string(), |p| format!("{p:?}").to_lowercase()),
        match between.is_empty() {
            true => "none".to_string(),
            false => between.join(","),
        },
        setup.cell_px.0,
        setup.cell_px.1,
        setup.zooms.0,
        setup.zooms.1,
        facts.context()
    );
}

/// The real terminal: keys in, frames out, until the reader quits.
async fn run_terminal(
    shell: &mut Shell,
    theme: ThemeFlag,
    graphics: map::GraphicsFlag,
    remembered: &[(String, String)],
) -> Result<(), String> {
    // Without a terminal on both ends there is nothing to draw on and no
    // keys to read: said in a sentence, before raw mode reaches for the
    // controlling terminal, which is ENXIO without one and someone else's
    // screen with one (1570).
    {
        use std::io::IsTerminal;
        if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
            return Err(NOT_A_TERMINAL.to_string());
        }
    }
    install_panic_hook();
    // Before the alternate screen, as lnomad does: the background query reads
    // its answer on stdin, which raw mode would hand to the key handler. The
    // keyboard query likewise. Asked only under `auto`.
    shell.model.theme = resolve_theme(detect_background(theme), theme);
    browse_like_the_terminal(&mut shell.model);
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        // Runs before install_panic_hook's restore, which leaves the screen.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
            previous(info);
        }));
    }
    shell.model.enhanced = enhanced;
    log_keys(&shell.model);
    let mut guard = TerminalGuard::new(KeysTerminal {
        inner: CrosstermTerminal::with_mouse(shell.model.mouse),
        enhanced,
    })
    .map_err(|e| e.to_string())?;
    // A pass of the loop that changes no cell writes nothing (1455): the
    // loop runs every 20 ms for the engine's queue.
    let (writer, mut drawn) = Held::new(std::io::stdout());
    let mut terminal = Terminal::new(CrosstermBackend::new(writer)).map_err(|e| e.to_string())?;
    // The map's backend, asked of the terminal in raw mode, before the
    // event stream reads stdin. Only when a protocol may be taken: braille
    // and half-blocks need no answer, and `--no-color` sends no query.
    let ask = match graphics {
        map::GraphicsFlag::Auto => shell.model.tier != Tier::Mono,
        map::GraphicsFlag::Kitty | map::GraphicsFlag::Sixel => true,
        map::GraphicsFlag::Octant
        | map::GraphicsFlag::Blocks
        | map::GraphicsFlag::Halfblocks
        | map::GraphicsFlag::BrailleThin
        | map::GraphicsFlag::Mosaic
        | map::GraphicsFlag::Braille => false,
    };
    let picker = match ask {
        true => Picker::from_query_stdio().ok(),
        false => None,
    };
    let detected = picker.as_ref().map(Picker::protocol_type);
    let facts = map::Facts::read(detected, shell.model.tier, Between::detect());
    // The way remembered for this terminal wins over the ladder, unless it
    // cannot work here; then the ladder's, said once (1093).
    let context = facts.context();
    let held = remembered
        .iter()
        .rev()
        .find(|(c, _)| *c == context)
        .map(|(_, d)| d.as_str());
    let (chosen, passed_over) = map::choose_start(graphics, held, &facts);
    if let Some(line) = passed_over {
        tracing::info!("GRAPHICS {line}");
        shell.apply(UiEvent::Notice(line));
    }
    let mut memory = map::Memory::new(context, held, chosen.graphics);
    // The map is drawn on its own thread: a key is read while the last
    // one's view is painted and encoded (1075).
    // Read once: the painter is handed it rather than reading it itself.
    let tmux = images::in_tmux();
    let mut painter = map::MapPainter::new(shell.maps.clone(), chosen.graphics, picker.clone())
        .behind_tmux(tmux)
        .threaded();
    log_graphics(&painter, &chosen, &facts);
    shell.apply(UiEvent::MapSetup(map::setup_of(&painter, &facts)));
    let mut map_open = false;
    let log_input = input_log_on(std::env::var_os(INPUT_LOG_VAR).as_deref());
    if log_input {
        tracing::info!("INPUT log=on");
    }
    let events = EventStream::new();
    shell.apply(UiEvent::Clock(local_now()));
    let size = terminal.size().map_err(|e| e.to_string())?;
    shell.apply(UiEvent::Resize {
        width: size.width,
        height: size.height,
    });
    let mut tick = tokio::time::interval(Duration::from_millis(TICK_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pump = tokio::time::interval(Duration::from_millis(PUMP_MS));
    pump.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The wall clock against the monotonic one, read on every tick: the
    // wall running ahead is a suspend the monotonic clock slept through.
    let mut clocks = (local_now().unix_secs, shell.elapsed_ms());
    let mut notices = crate::desktop::Notices::default();
    let mut counts: Vec<([u8; 16], usize)> = Vec::new();
    // What was drawn last (1491): a turn with nothing changed builds no
    // viewport, view or frame; before, every 20 ms pump did.
    let mut presented = ui::Presented::default();
    // A frame that panicked is drawn again on the next input, not on
    // every pump.
    let mut panicked = false;
    // Three parts from here (1492): the input task only reads keys and
    // the mouse into the model's queue, the engine's reader thread only
    // reads the engine, and this loop folds both and draws when the
    // stamp moved. Before, the engine was read on this loop between two
    // reads of the keyboard, so a slow engine step held every key back.
    let (inputs_tx, mut inputs) = tokio::sync::mpsc::unbounded_channel();
    let input = read_input(events, inputs_tx);
    let (reads_tx, reads_rx) = tokio::sync::mpsc::channel(READS_QUEUED);
    let mut engine = None;
    let mut reads = None;
    if let Some(reader) = shell.lend_engine() {
        match EngineThread::spawn(reader, reads_tx) {
            Ok(thread) => {
                engine = Some(thread);
                reads = Some(reads_rx);
            }
            Err(reader) => shell.take_engine_back(reader),
        }
    }

    let result = loop {
        for output in std::mem::take(&mut shell.outputs) {
            if let Output::MapDrawing(n) = output {
                // Another way of drawing the map, at once (1093).
                if let Some(&next) = map::Graphics::ORDER.get(n) {
                    painter = map::MapPainter::new(shell.maps.clone(), next, picker.clone())
                        .behind_tmux(tmux)
                        .threaded();
                    memory.chose(next);
                    tracing::info!("GRAPHICS backend={} reason=chosen", next.name());
                    shell.apply(UiEvent::MapSetup(map::setup_of(&painter, &facts)));
                }
                continue;
            }
            if let Err(error) = run_output(output) {
                shell.apply(UiEvent::Notice(error));
            }
        }
        // Leaving the map writes the way the reader stayed on.
        let open = shell.model.map.is_some();
        if map_open && !open {
            remember(shell, &mut memory);
        }
        map_open = open;
        if shell.redraw && !panicked {
            shell.redraw = false;
            if let Err(error) = terminal.clear() {
                break Err(error.to_string());
            }
            presented.owe();
        }
        // The terminal's size as the frame will have it: a shrink whose
        // resize event has not been read yet (mosh delivers them late) is
        // laid out for now, not for the size before (1087).
        if let Ok(size) = terminal.size() {
            if (size.width, size.height) != shell.model.screen {
                shell.apply(UiEvent::Resize {
                    width: size.width,
                    height: size.height,
                });
            }
        }
        if shell.model.debug_overlay {
            // The frame meter counts frames: it has one every turn.
            presented.owe();
        }
        if !presented.due(frame_stamp(&shell.model)) {
            if shell.model.quit {
                break Ok(());
            }
            if let Some(ended) = wait(
                shell,
                &mut inputs,
                log_input,
                &mut reads,
                painter.ready(),
                &mut pump,
                &mut tick,
                &mut clocks,
                &mut presented,
                &mut panicked,
            )
            .await
            {
                break ended;
            }
            continue;
        }
        say_viewport(shell);
        // The frame shows what the viewport's fold left: the next turn
        // compares with that, not with the turn's start.
        presented.drawn(frame_stamp(&shell.model));
        notify(shell, &mut notices, &mut counts);
        let view = ui::view(&shell.model);
        // A frame that panics is skipped, not the messenger: the panic hook
        // has left the alternate screen by then, so it is entered again and
        // the next frame drawn whole. Each frame is one synchronized update,
        // so a terminal never shows a map picture's erase without the
        // picture (1118).
        match unwound(|| {
            draw_changed(&mut terminal, &mut drawn, |frame| {
                draw_screen(frame, &shell.model, &view, Some(&mut painter))
            })
        }) {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => break Err(error.to_string()),
            Err(panic) => {
                tracing::error!("DRAW frame skipped: {panic}");
                if let Err(error) = guard.enter_again() {
                    break Err(error.to_string());
                }
                if let Err(error) = set_mouse_capture(shell.model.mouse) {
                    break Err(error.to_string());
                }
                // Not drawn again until something happens: a frame that
                // panics every time is one line a key, not a spin.
                shell.redraw = true;
                panicked = true;
            }
        }
        if shell.model.quit {
            break Ok(());
        }
        if let Some(ended) = wait(
            shell,
            &mut inputs,
            log_input,
            &mut reads,
            painter.ready(),
            &mut pump,
            &mut tick,
            &mut clocks,
            &mut presented,
            &mut panicked,
        )
        .await
        {
            break ended;
        }
    };
    input.abort();
    // What the reader read before it stopped is folded, and the outbox is
    // the shell's again for its settle.
    if let Some(thread) = engine {
        if let Some(reader) = thread.stop(shell, reads).await {
            shell.take_engine_back(reader);
        }
    }
    // How many map pictures went up, beside the GRAPHICS line: a held key
    // that drew far fewer than it pressed was coalesced (1075).
    tracing::info!("GRAPHICS paints={}", painter.paints());
    remember(shell, &mut memory);
    guard.restore_now().map_err(|e| e.to_string())?;
    result
}

/// What the terminal's frame is drawn from (1491): the model's revision,
/// the screen's size, and the tick while something on screen moves by it
/// ([`ui::UiModel::animating`]). A turn whose stamp did not move builds
/// nothing.
fn frame_stamp<B>(model: &loquax::ui::UiModel<B>) -> u64 {
    let tick = match model.animating() {
        true => model.tick,
        false => u64::MAX,
    };
    ui::Presented::stamp(
        model.revision,
        &[u64::from(model.screen.0), u64::from(model.screen.1), tick],
    )
}

/// What the terminal's loop turns (1492): the shell, or a test's stand-in
/// whose engine stalls.
trait Turned {
    /// Fold one event of the model's queue.
    fn apply(&mut self, event: UiEvent);
    /// Fold what the engine's reader thread read.
    fn fold_read(&mut self, read: EngineRead);
    /// The shell's own queues: runners, notices, the loader, the clock of
    /// the schedule; the engine too while it is not lent.
    fn poll(&mut self);
    /// The clock's tick.
    fn tick(&mut self, clocks: &mut (i64, u64));
}

impl Turned for Shell {
    fn apply(&mut self, event: UiEvent) {
        Shell::apply(self, event);
    }

    fn fold_read(&mut self, read: EngineRead) {
        Shell::fold_read(self, read);
    }

    fn poll(&mut self) {
        Shell::poll(self);
    }

    fn tick(&mut self, clocks: &mut (i64, u64)) {
        Shell::tick(self, clocks);
    }
}

/// One read of the terminal's input: what crossterm's stream yielded,
/// `None` once it ended.
type InputRead = Option<std::io::Result<Event>>;

/// One read of the engine's reader thread.
type EngineRead = Result<Vec<loquax::outbox::OutboxEvent>, loquax::outbox::OutboxGone>;

/// How many reads the engine's thread may have queued before it waits for
/// the loop to fold them: a first pull's thousands stay in the engine's
/// queue, and a tap holds no event the screen has not folded (1446).
const READS_QUEUED: usize = 64;

/// The input task (1492): reads the terminal's keys, mouse and resizes into
/// the model's queue and does nothing else, so no engine step and no
/// frame stands between a key and its read. Ends with the stream.
fn read_input(
    mut events: EventStream,
    queue: tokio::sync::mpsc::UnboundedSender<InputRead>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let item = events.next().await;
            let last = !matches!(item, Some(Ok(_)));
            if queue.send(item).is_err() || last {
                return;
            }
        }
    })
}

/// The engine's reader on a thread of its own (1492): it hands the taps'
/// commands to the engine and reads the engine's events into the loop's
/// queue, one slice at a time ([`loquax::fanout::SLICE`]), so an engine
/// step that stalls stalls only this thread.
struct EngineThread<O> {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: std::thread::JoinHandle<Option<loquax::fanout::Reader<O>>>,
}

impl<O: loquax::outbox::Outbox + Send + 'static> EngineThread<O> {
    /// The thread started with `reader`, or the reader back when no thread
    /// would start.
    fn spawn(
        reader: loquax::fanout::Reader<O>,
        reads: tokio::sync::mpsc::Sender<EngineRead>,
    ) -> Result<Self, loquax::fanout::Reader<O>> {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = stop.clone();
        // Handed over once the thread runs, so a thread that would not
        // start leaves the reader here.
        let (hand, handed) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("loquax-engine".to_string())
            .spawn(move || {
                let reader = handed.recv().ok()?;
                Some(read_engine(reader, &stopping, &reads))
            });
        match spawned {
            Ok(thread) => match hand.send(reader) {
                Ok(()) => Ok(Self { stop, thread }),
                Err(back) => Err(back.0),
            },
            Err(error) => {
                tracing::warn!("ENGINE reader thread not started, read on the loop: {error}");
                Err(reader)
            }
        }
    }

    /// Stop the thread: every read it queued is folded into `turned`
    /// first, then the reader comes back.
    async fn stop<T: Turned>(
        self,
        turned: &mut T,
        mut reads: Option<tokio::sync::mpsc::Receiver<EngineRead>>,
    ) -> Option<loquax::fanout::Reader<O>> {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(reads) = reads.as_mut() {
            // Ends when the thread returned and dropped its sender.
            while let Some(read) = reads.recv().await {
                turned.fold_read(read);
            }
        }
        match self.thread.join() {
            Ok(reader) => reader,
            Err(_) => {
                tracing::error!("ENGINE reader thread panicked");
                None
            }
        }
    }
}

/// The reader thread's loop: a slice read and queued, or a pump's wait when
/// the engine had nothing, until stopped, the loop gone, or the engine.
fn read_engine<O: loquax::outbox::Outbox>(
    mut reader: loquax::fanout::Reader<O>,
    stop: &std::sync::atomic::AtomicBool,
    reads: &tokio::sync::mpsc::Sender<EngineRead>,
) -> loquax::fanout::Reader<O> {
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        match reader.read_at_most(loquax::fanout::SLICE.chunk) {
            Ok(events) if events.is_empty() => {
                std::thread::sleep(Duration::from_millis(PUMP_MS));
            }
            Ok(events) => {
                if reads.blocking_send(Ok(events)).is_err() {
                    break;
                }
            }
            Err(gone) => {
                let _ = reads.blocking_send(Err(gone));
                break;
            }
        }
    }
    reader
}

/// The next read of the engine's thread; never, when there is none.
async fn next_read(
    reads: &mut Option<tokio::sync::mpsc::Receiver<EngineRead>>,
) -> Option<EngineRead> {
    match reads {
        Some(reads) => reads.recv().await,
        None => std::future::pending().await,
    }
}

/// The loop's wait for what comes next: input, a map picture painted, the
/// engine's reads, the shell's pump or the clock. Input and a picture owe
/// a frame whatever the revision says; the rest fold, and the revision
/// says whether that changed anything shown. None of the branches waits
/// on the engine: its reads come from its own thread, a slice's budget
/// folded at a time and stopped at once when input waits (1464, 1492).
/// Not biased: a drag's stream of motions does not starve the clock.
/// `Some` when the input ended the run.
#[allow(clippy::too_many_arguments)]
async fn wait<T: Turned>(
    shell: &mut T,
    inputs: &mut tokio::sync::mpsc::UnboundedReceiver<InputRead>,
    log_input: bool,
    reads: &mut Option<tokio::sync::mpsc::Receiver<EngineRead>>,
    painted: impl std::future::Future<Output = ()>,
    pump: &mut tokio::time::Interval,
    tick: &mut tokio::time::Interval,
    clocks: &mut (i64, u64),
    presented: &mut ui::Presented,
    panicked: &mut bool,
) -> Option<Result<(), String>> {
    tokio::select! {
        item = inputs.recv() => {
            // A held key's repeats and a drag's motions that queued
            // while the last frame was drawn are one frame at the
            // newest view, not a frame each (1075). A queue whose task
            // is gone is the stream's end.
            let ended = apply_waiting(
                item.flatten(),
                || inputs.try_recv().ok(),
                log_input,
                |event| shell.apply(event),
            );
            *panicked = false;
            presented.owe();
            ended
        }
        _ = painted => {
            presented.owe();
            None
        }
        Some(read) = next_read(reads) => {
            shell.fold_read(read);
            let began = std::time::Instant::now();
            while inputs.is_empty() && began.elapsed() < loquax::fanout::SLICE.budget {
                match reads.as_mut().map(|r| r.try_recv()) {
                    Some(Ok(read)) => shell.fold_read(read),
                    _ => break,
                }
            }
            None
        }
        _ = pump.tick() => {
            shell.poll();
            None
        }
        _ = tick.tick() => {
            shell.tick(clocks);
            None
        }
    }
}

/// New mail in a conversation other than the open one (1341): the
/// terminal's bell, and the desktop's notification where a session bus
/// carries one, by the phone's rule ([`crate::desktop::Notices`]). A
/// terminal does not say whether it is in front, so only the open chat,
/// which the reader sees, stays quiet. The list is read again only when
/// an unread count moved (`counts`).
fn notify(
    shell: &Shell,
    notices: &mut crate::desktop::Notices,
    counts: &mut Vec<([u8; 16], usize)>,
) {
    let now: Vec<_> = (shell.model.conversations.iter())
        .map(|c| (c.peer, c.unread))
        .collect();
    if &now == counts {
        return;
    }
    *counts = now;
    let open = shell
        .model
        .conversations
        .get(shell.model.selected)
        .map(|c| c.peer);
    let rows = crate::desktop::Count::of(&loquax::ui::chat::list_rows(&shell.model));
    let fresh = notices.look(&rows, false, open);
    if fresh.is_empty() {
        return;
    }
    let _ = crossterm::execute!(std::io::stdout(), crossterm::style::Print('\u{7}'));
    for notice in fresh {
        tokio::spawn(async move {
            if let Err(error) = crate::desktop::post(&notice).await {
                tracing::debug!("no notification: {error}");
            }
        });
    }
}

/// Tell the model which bubbles are on screen when that changed (1231).
fn say_viewport(shell: &mut Shell) {
    let Some((peer, keys)) = ui::hit::viewport(&shell.model) else {
        return;
    };
    if shell.model.reading.viewport() != Some(&(peer, keys.clone())) {
        shell.apply(UiEvent::Viewport { peer, keys });
    }
}

/// Write the way of drawing the map the reader chose to the config, when
/// it is not there already; a file that would not take it is said.
fn remember(shell: &mut Shell, memory: &mut map::Memory) {
    let Some((key, value)) = memory.owed() else {
        return;
    };
    match loquax::config::save(shell.home(), &key, value) {
        Ok(()) => {
            memory.written();
            tracing::info!("GRAPHICS remembered={value} context={}", memory.context);
        }
        Err(error) => {
            let line = format!(
                "{}: the map drawing not saved: {error}",
                shell.home().join(loquax::config::CONFIG_FILE).display()
            );
            tracing::warn!("{line}");
            shell.afterwards.push(line);
        }
    }
}

/// `f`'s result, or the message of the panic it raised.
fn unwound<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic without a message".to_string())
    })
}

/// `first` and every event already waiting behind it in the model's queue
/// (`waiting`), applied in order without waiting for more; `Some` when the
/// stream ended. With `log_input` each is an `INPUT` line in `tui.log`
/// first, as it came.
fn apply_waiting(
    first: InputRead,
    mut waiting: impl FnMut() -> Option<InputRead>,
    log_input: bool,
    mut apply: impl FnMut(UiEvent),
) -> Option<Result<(), String>> {
    let mut item = first;
    loop {
        if let (true, Some(Ok(event))) = (log_input, &item) {
            tracing::info!("{}", input_line(event));
        }
        match step_event(item, map_event) {
            EventStep::Apply(event) => apply(event),
            EventStep::Ignore => {}
            EventStep::End(result) => return Some(result.map_err(|e| e.to_string())),
        }
        item = waiting()?;
    }
}

/// The variable that turns the raw input log on: what the terminal sends,
/// before any mapping, so a phone's swipe can be read off `tui.log` (1102).
const INPUT_LOG_VAR: &str = "LOQUAX_INPUT_LOG";

/// Whether the input log's variable turns it on: set, not empty, not `0`.
fn input_log_on(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// One terminal event as an `INPUT` line, every field it carries but a
/// paste's text, of which only the length in bytes is said.
fn input_line(event: &Event) -> String {
    use crossterm::event::{KeyEventKind, MouseButton as Button, MouseEventKind as Kind};
    let button = |b: &Button| match b {
        Button::Left => "left",
        Button::Right => "right",
        Button::Middle => "middle",
        Button::Back => "back",
        Button::Forward => "forward",
    };
    match event {
        Event::Mouse(mouse) => {
            let (kind, pressed) = match &mouse.kind {
                Kind::Down(b) => ("down", Some(button(b))),
                Kind::Up(b) => ("up", Some(button(b))),
                Kind::Drag(b) => ("drag", Some(button(b))),
                Kind::Moved => ("moved", None),
                Kind::ScrollDown => ("scroll_down", None),
                Kind::ScrollUp => ("scroll_up", None),
                Kind::ScrollLeft => ("scroll_left", None),
                Kind::ScrollRight => ("scroll_right", None),
            };
            format!(
                "INPUT kind=mouse event={kind} button={} col={} row={} mods={}",
                pressed.unwrap_or("none"),
                mouse.column,
                mouse.row,
                modifiers(mouse.modifiers),
            )
        }
        Event::Key(key) => {
            let code = match key.code {
                crossterm::event::KeyCode::Char(c) if c.is_ascii_graphic() => format!("char:{c}"),
                crossterm::event::KeyCode::Char(c) => format!("char:U+{:04X}", u32::from(c)),
                crossterm::event::KeyCode::F(n) => format!("f{n}"),
                other => format!("{other:?}").to_lowercase().replace(' ', ""),
            };
            let state = match key.kind {
                KeyEventKind::Press => "press",
                KeyEventKind::Repeat => "repeat",
                KeyEventKind::Release => "release",
            };
            format!(
                "INPUT kind=key code={code} mods={} state={state}",
                modifiers(key.modifiers)
            )
        }
        Event::Resize(width, height) => {
            format!("INPUT kind=resize width={width} height={height}")
        }
        Event::Paste(text) => format!("INPUT kind=paste len={}", text.len()),
        Event::FocusGained => "INPUT kind=focus gained=true".to_string(),
        Event::FocusLost => "INPUT kind=focus gained=false".to_string(),
    }
}

/// `shift+ctrl`, or `none`.
fn modifiers(mods: crossterm::event::KeyModifiers) -> String {
    let names: Vec<String> = mods
        .iter_names()
        .map(|(name, _)| name.to_lowercase())
        .collect();
    match names.is_empty() {
        true => "none".to_string(),
        false => names.join("+"),
    }
}

/// What the model asked of the terminal, done: the copy as OSC 52, the URL
/// to the system's handler as lnomad hands it, the capture switched.
fn run_output(output: Output) -> Result<(), String> {
    match output {
        Output::Copy(text) => {
            crossterm::execute!(std::io::stdout(), crossterm::style::Print(osc52(&text)))
                .map_err(|e| format!("copy: {e}"))
        }
        Output::Open(url) => open::that(&url).map_err(|e| format!("could not open {url}: {e}")),
        Output::Photo(path) | Output::File(path) => {
            open::that(&path).map_err(|e| format!("could not open {}: {e}", path.display()))
        }
        Output::Mouse(on) => set_mouse_capture(on).map_err(|e| format!("mouse: {e}")),
        // The loop switches the painter itself.
        Output::MapDrawing(_) => Ok(()),
    }
}

/// Keys and the mouse become the model's events; a resize tells the model
/// its new pages.
fn map_event(event: Event) -> Option<UiEvent> {
    match event {
        Event::Key(key) => Some(UiEvent::Key(key_event(key))),
        Event::Mouse(mouse) => Some(UiEvent::Mouse(mouse_event(mouse))),
        Event::Resize(width, height) => Some(UiEvent::Resize { width, height }),
        Event::FocusGained => Some(UiEvent::Front(true)),
        Event::FocusLost => Some(UiEvent::Front(false)),
        _ => None,
    }
}

/// A script's `until` condition.
#[derive(Debug)]
enum Until {
    /// The engine said `Ready`.
    Ready,
    /// The newest message we wrote in the selected conversation shows this
    /// state.
    Delivery(DeliveryState),
    /// This many received messages the model holds, across every
    /// conversation. A conversation restored from its kept row and not
    /// opened holds none (46f91e3c, "the boot reads rows not bodies"), so
    /// a restarted TUI counts from 0 whatever its home received before:
    /// a script asks for what arrives while it runs, and periculum's
    /// receipts cells count that way since its 1670 (1559,
    /// `a_restarted_model_counts_only_what_it_holds`).
    /// [`Self::Verified`] counts the same way.
    Received(usize),
    /// This many peers' announces carried a name.
    Named(usize),
    /// This many received messages whose signature verified, across every
    /// conversation.
    Verified(usize),
    /// The send runner reported this leg for the newest message we wrote in
    /// the selected conversation.
    Leg(Leg),
    /// No region load runs or waits (1089).
    MapIdle,
    /// This many fetches (on a client, pulls from the home node) ended.
    Synced(usize),
    /// This many groups the reader is in, joined or its admin (1307).
    Groups(usize),
    /// This many invitations wait for a Join (1307).
    Invited(usize),
    /// This many groups their admin deleted (1307 pass four).
    Ended(usize),
    /// No group is known any more: the admin's own device after a delete.
    NoGroups,
}

impl Until {
    fn holds(&self, model: &Model, legs_seen: &[((usize, usize), Leg)]) -> bool {
        match self {
            Self::Ready => model.address.is_some(),
            Self::Delivery(state) => {
                model
                    .messages_of_selected()
                    .iter()
                    .rev()
                    .find(|m| m.outbound)
                    .and_then(|m| m.delivery)
                    == Some(*state)
            }
            Self::Received(count) => {
                model
                    .conversations
                    .iter()
                    .flat_map(|c| c.messages.iter())
                    .filter(|m| !m.outbound)
                    .count()
                    >= *count
            }
            Self::Named(count) => model.names.heard_count() >= *count,
            Self::MapIdle => model.map_loading.is_none(),
            Self::Synced(count) => model.fetches_done >= *count,
            Self::Groups(count) => {
                model
                    .groups
                    .values()
                    .filter(|g| g.standing == loquax::ui::groups::Standing::Joined)
                    .count()
                    >= *count
            }
            Self::Invited(count) => {
                model
                    .groups
                    .values()
                    .filter(|g| g.standing == loquax::ui::groups::Standing::Invited)
                    .count()
                    >= *count
            }
            Self::Ended(count) => {
                model
                    .groups
                    .values()
                    .filter(|g| g.standing == loquax::ui::groups::Standing::Ended)
                    .count()
                    >= *count
            }
            Self::NoGroups => model.groups.is_empty(),
            Self::Verified(count) => {
                model
                    .conversations
                    .iter()
                    .flat_map(|c| c.messages.iter())
                    .filter(|m| !m.outbound && m.signature == loquax::ui::Signature::Verified)
                    .count()
                    >= *count
            }
            Self::Leg(leg) => {
                let newest = model
                    .messages_of_selected()
                    .iter()
                    .rposition(|m| m.outbound)
                    .map(|m| (model.selected, m));
                newest.is_some_and(|newest| legs_seen.contains(&(newest, *leg)))
            }
        }
    }
}

/// One line of a key script.
#[derive(Debug)]
enum Step {
    Key(KeyEvent),
    /// The terminal world the script plays: enhanced or plain keyboard.
    Keyboard(bool),
    Type(String),
    Tick(u32),
    Frame(String),
    Until(Until, Duration),
    Sleep(u64),
    Clock(WallTime),
    /// Mouse events, at screen cells.
    Mouse(Vec<MouseEvent>),
    /// The colour tier the screen is drawn in from now on.
    Tier(Tier),
    /// Light or dark from now on.
    Theme(Theme),
    /// The language the screen speaks from now on (1381).
    Lang(Lang),
    /// The screen's style map, to the styles file.
    Styles(String),
    /// The terminal's focus came or went.
    Focus(bool),
}

/// The script's key names: `tab`, `backtab`, `enter`, `esc`, `up`, `down`,
/// `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `backspace`,
/// `delete`, `space`, `f1` to `f12`, or one character, each after any of the
/// prefixes `ctrl-`, `alt-` and `shift-` (`shift-enter`, `ctrl-enter`,
/// `alt-<`, `ctrl-j`). An uppercase letter carries its Shift. A chord only an
/// enhanced terminal sends is delivered as such whatever the host terminal
/// is, so a script pins both worlds (`keyboard enhanced` sets which one the
/// screen believes it is in).
fn parse_key(name: &str) -> Result<KeyEvent, String> {
    let mut mods = KeyModifiers::NONE;
    let mut rest = name;
    loop {
        let (prefix, flag) = match rest {
            r if r.starts_with("ctrl-") && r.len() > 5 => ("ctrl-", KeyModifiers::CONTROL),
            r if r.starts_with("alt-") && r.len() > 4 => ("alt-", KeyModifiers::ALT),
            r if r.starts_with("shift-") && r.len() > 6 => ("shift-", KeyModifiers::SHIFT),
            _ => break,
        };
        mods |= flag;
        rest = &rest[prefix.len()..];
    }
    let code = match rest {
        "tab" => KeyCode::Tab,
        "backtab" => {
            mods |= KeyModifiers::SHIFT;
            KeyCode::BackTab
        }
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "space" => KeyCode::Char(' '),
        other => match (single(other), other.strip_prefix('f')) {
            (Some(c), _) => {
                if c.is_uppercase() {
                    mods |= KeyModifiers::SHIFT;
                }
                KeyCode::Char(c)
            }
            (None, Some(n)) => match n.parse::<u8>() {
                Ok(n @ 1..=12) => KeyCode::F(n),
                _ => return Err(format!("unknown key {name:?}")),
            },
            (None, None) => return Err(format!("unknown key {name:?}")),
        },
    };
    Ok(KeyEvent::new(code, mods))
}

fn single(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

/// A mouse event at a script's cell, 1-based as the frames are counted.
fn mouse_at(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col - 1,
        row: row - 1,
        modifiers: KeyModifiers::NONE,
    }
}

/// `click`, `wheel` and `drag`: the events each stands for. Columns and rows
/// are screen cells from 1, the frames' first row and column.
fn parse_mouse(word: &str, rest: &str) -> Result<Vec<MouseEvent>, String> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let cell = |text: &str| -> Result<u16, String> {
        match text.parse::<u16>() {
            Ok(n) if n >= 1 => Ok(n),
            _ => Err(format!("bad cell {text:?}, from 1")),
        }
    };
    let left = MouseButton::Left;
    match (word, words.as_slice()) {
        ("click", [col, row]) => {
            let (col, row) = (cell(col)?, cell(row)?);
            Ok(vec![
                mouse_at(MouseEventKind::Down(left), col, row),
                mouse_at(MouseEventKind::Up(left), col, row),
            ])
        }
        ("wheel", [way, col, row, tail @ ..]) => {
            let kind = match *way {
                "up" => MouseEventKind::ScrollUp,
                "down" => MouseEventKind::ScrollDown,
                other => return Err(format!("unknown wheel {other:?}, up or down")),
            };
            let n = match tail {
                [] => 1,
                [n] => n.parse::<usize>().map_err(|_| format!("bad count {n:?}"))?,
                _ => return Err(format!("wheel takes up|down <col> <row> [n], not {rest:?}")),
            };
            let (col, row) = (cell(col)?, cell(row)?);
            Ok((0..n).map(|_| mouse_at(kind, col, row)).collect())
        }
        ("drag", [c1, r1, c2, r2]) => {
            let (c1, r1, c2, r2) = (cell(c1)?, cell(r1)?, cell(c2)?, cell(r2)?);
            Ok(vec![
                mouse_at(MouseEventKind::Down(left), c1, r1),
                mouse_at(MouseEventKind::Drag(left), c2, r2),
                mouse_at(MouseEventKind::Up(left), c2, r2),
            ])
        }
        ("click", _) => Err(format!("click takes <col> <row>, not {rest:?}")),
        ("wheel", _) => Err(format!("wheel takes up|down <col> <row> [n], not {rest:?}")),
        _ => Err(format!("drag takes <col> <row> <col> <row>, not {rest:?}")),
    }
}

/// `2026-10-24 13:58`, at UTC.
fn parse_clock(text: &str) -> Result<WallTime, String> {
    let bad = || format!("bad clock {text:?}, YYYY-MM-DD HH:MM");
    let (date, time) = text.split_once(' ').ok_or_else(bad)?;
    let mut date = date.split('-').map(str::parse::<u32>);
    let mut time = time.trim().split(':').map(str::parse::<u32>);
    let next = |part: Option<Result<u32, _>>| part.and_then(Result::ok).ok_or_else(bad);
    let (year, month, day) = (next(date.next())?, next(date.next())?, next(date.next())?);
    let (hour, minute) = (next(time.next())?, next(time.next())?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return Err(bad());
    }
    Ok(WallTime::from_civil(
        i64::from(year),
        month,
        day,
        hour,
        minute,
        0,
    ))
}

fn parse_state(name: &str) -> Result<DeliveryState, String> {
    Ok(match name {
        "queued" => DeliveryState::Queued,
        "stamping" => DeliveryState::Stamping,
        "inflight" => DeliveryState::InFlight,
        "sent" => DeliveryState::Sent,
        // `pn` names the state as the screen does since 1153; `mailbox`
        // stays for the scripts written before.
        "pn" | "mailbox" => DeliveryState::InMailbox,
        "arrived" => DeliveryState::Arrived,
        "delivered" => DeliveryState::Delivered,
        "rejected" => DeliveryState::Rejected,
        "failed" => DeliveryState::Failed,
        other => return Err(format!("unknown delivery state {other:?}")),
    })
}

/// Parse a key script. One step a line, `#` starts a comment:
///
/// ```text
/// key <name>                     one key (see parse_key)
/// type <text>                    each character of the rest of the line
/// newline                        Alt-Enter: a line break in compose
/// keyboard <enhanced|plain>      the terminal world the screen is told it
///                                is in (plain until said): its hints offer
///                                Shift-Enter and Ctrl-Enter only when enhanced
/// tick [n]                       n spinner/toast ticks (1)
/// frame [label]                  dump the screen
/// until ready [secs]             the engine is attached
/// until delivery <state> [secs]  the newest sent message shows <state>
/// until received <n> [secs]      n messages have arrived
/// until named <n> [secs]         n peers have announced a name
/// until verified <n> [secs]      n received messages have a verified signature
/// until map [secs]               no map region load runs or waits
/// until synced <n> [secs]        n fetches (pulls on a client) have ended
/// until groups <n> [secs]        the reader is in n groups (1307)
/// until invited <n> [secs]       n invitations wait for a Join (1307)
/// until ended <n> [secs]         n groups their admin deleted (1307 pass four)
/// until nogroups [secs]          the reader knows no group any more
/// until leg <direct|propagated> [secs]
///                                the runner is on that leg for the newest
///                                sent message
/// sleep <ms>                     wait as the idle terminal does, up to 60000:
///                                the engine is read every 20 ms and one tick
///                                applied per 250 ms of the script's clock
/// clock <YYYY-MM-DD> <HH:MM>      the time of day from now on, at UTC (the
///                                script starts at 2026-10-26 12:00); it
///                                stands still until the next clock step
/// click <col> <row>              the left button down and up at a cell
/// wheel <up|down> <col> <row> [n]
///                                n wheel notches at a cell (1)
/// drag <col> <row> <col> <row>   the left button down at the first cell,
///                                moved to the second and up there
/// tier <mono|16|256|truecolor>   the colour tier from now on (mono until
///                                said, whatever the host terminal is)
/// theme <dark|light>             the theme from now on (dark until said)
/// styles <name>                  the screen's style map, one letter a cell
///                                (see style_letter), to the frames file's
///                                path with `.styles` added
/// focus <in|out>                 the terminal's focus report: in front or
///                                behind (a script says none until then, and
///                                is in front once a key or click came)
/// ```
///
/// Cells count from 1 at the top left, as the frames' rows and columns do.
/// What the screen asks of the terminal is written into the frames where it
/// happens: `--- osc52 <sequence> ---` for a copy (the escape character as
/// `\e`), `--- open <url> ---` for a link handed to the system, which a
/// script never launches, and `--- mouse on|off ---`.
fn parse_script(text: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (number, raw) in text.lines().enumerate() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let at = |error: String| format!("line {}: {error}", number + 1);
        let secs = |text: Option<&str>| -> Result<Duration, String> {
            match text {
                None => Ok(Duration::from_secs(UNTIL_SECS)),
                Some(n) => n
                    .parse()
                    .map(Duration::from_secs)
                    .map_err(|_| format!("bad seconds {n:?}")),
            }
        };
        let step = match word {
            "key" => Step::Key(parse_key(rest.trim()).map_err(at)?),
            "type" => Step::Type(rest.to_string()),
            "newline" => Step::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            "keyboard" => match rest.trim() {
                "enhanced" => Step::Keyboard(true),
                "plain" => Step::Keyboard(false),
                other => return Err(at(format!("unknown keyboard {other:?}"))),
            },
            "tick" => Step::Tick(match rest.trim() {
                "" => 1,
                n => n.parse().map_err(|_| at(format!("bad tick count {n:?}")))?,
            }),
            "frame" => Step::Frame(rest.trim().to_string()),
            "clock" => Step::Clock(parse_clock(rest.trim()).map_err(at)?),
            "click" | "wheel" | "drag" => Step::Mouse(parse_mouse(word, rest).map_err(at)?),
            "tier" => match Tier::from_name(rest.trim()) {
                Some(tier) => Step::Tier(tier),
                None => {
                    return Err(at(format!(
                        "unknown tier {:?}, mono, 16, 256 or truecolor",
                        rest.trim()
                    )))
                }
            },
            "theme" => match rest.trim() {
                "dark" => Step::Theme(Theme::Dark),
                "light" => Step::Theme(Theme::Light),
                other => return Err(at(format!("unknown theme {other:?}, dark or light"))),
            },
            "lang" => match rest.trim() {
                "en" => Step::Lang(Lang::En),
                "de" => Step::Lang(Lang::De),
                other => return Err(at(format!("unknown lang {other:?}, en or de"))),
            },
            "styles" => Step::Styles(rest.trim().to_string()),
            "focus" => match rest.trim() {
                "in" => Step::Focus(true),
                "out" => Step::Focus(false),
                other => return Err(at(format!("unknown focus {other:?}, in or out"))),
            },
            "sleep" => {
                let ms = rest.trim();
                match ms.parse::<u64>() {
                    Ok(ms) if ms <= SLEEP_MAX_MS => Step::Sleep(ms),
                    _ => return Err(at(format!("bad sleep {ms:?}, 0 to {SLEEP_MAX_MS} ms"))),
                }
            }
            "until" => {
                let words: Vec<&str> = rest.split_whitespace().collect();
                match words.as_slice() {
                    ["map", tail @ ..] => {
                        Step::Until(Until::MapIdle, secs(tail.first().copied()).map_err(at)?)
                    }
                    ["ready", tail @ ..] => {
                        Step::Until(Until::Ready, secs(tail.first().copied()).map_err(at)?)
                    }
                    ["delivery", state, tail @ ..] => Step::Until(
                        Until::Delivery(parse_state(state).map_err(at)?),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    ["leg", leg, tail @ ..] => Step::Until(
                        Until::Leg(match *leg {
                            "direct" => Leg::Direct,
                            "propagated" => Leg::Propagated,
                            other => return Err(at(format!("unknown leg {other:?}"))),
                        }),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    ["synced", n, tail @ ..] => Step::Until(
                        Until::Synced(
                            n.parse()
                                .map_err(|_| at(format!("bad fetch count {n:?}")))?,
                        ),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    ["received", n, tail @ ..] => Step::Until(
                        Until::Received(
                            n.parse()
                                .map_err(|_| at(format!("bad message count {n:?}")))?,
                        ),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    ["groups", n, tail @ ..] => Step::Until(
                        Until::Groups(
                            n.parse()
                                .map_err(|_| at(format!("bad group count {n:?}")))?,
                        ),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    ["invited", n, tail @ ..] => Step::Until(
                        Until::Invited(
                            n.parse()
                                .map_err(|_| at(format!("bad invitation count {n:?}")))?,
                        ),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    ["ended", n, tail @ ..] => Step::Until(
                        Until::Ended(
                            n.parse()
                                .map_err(|_| at(format!("bad group count {n:?}")))?,
                        ),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    ["nogroups", tail @ ..] => {
                        Step::Until(Until::NoGroups, secs(tail.first().copied()).map_err(at)?)
                    }
                    ["named", n, tail @ ..] => Step::Until(
                        Until::Named(n.parse().map_err(|_| at(format!("bad peer count {n:?}")))?),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    ["verified", n, tail @ ..] => Step::Until(
                        Until::Verified(
                            n.parse()
                                .map_err(|_| at(format!("bad message count {n:?}")))?,
                        ),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    _ => return Err(at(format!("unknown condition {rest:?}"))),
                }
            }
            other => return Err(at(format!("unknown step {other:?}"))),
        };
        steps.push(step);
    }
    Ok(steps)
}

/// A drawn frame as text, one row a line, trailing blanks cut.
///
/// The frame's own buffer, not the test backend's grid: ratatui sends the
/// backend no write for the cell behind a wide character, so the grid
/// keeps whatever stood there before (`日a本` where a line of Latin text
/// was), where a terminal clears the whole glyph. The frame resets that
/// cell to a blank.
fn frame_text(buffer: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut row = String::new();
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

/// A cell's letter in a style map: which token's colours it carries, the
/// first in this order that matches.
///
/// ```text
/// e   the error colour (an error toast's border)
/// t   the toast
/// ^   a selected block's tint, or reverse video (the list's selection, the
///     badge, and the selection and the toast where the tier draws them
///     reversed)
/// .   our own block's tint
/// ,   their block's tint
/// =   the date pill
/// !   the unread rule and its count
///     anything else: the pane
/// ```
///
/// Where two tokens share their colours the earlier letter wins: in 16
/// colours on light the date pill is their block's pair, and draws as `,`.
pub fn style_letter(cell: &ratatui::buffer::Cell, tokens: &Tokens) -> char {
    use leviculum_ui_ratatui::style::ink;
    let fg = |p: leviculum_ui::Paint| p.fg.map(ink);
    let bg = |p: leviculum_ui::Paint| p.bg.map(ink);
    let is = |c: ratatui::style::Color, want: Option<ratatui::style::Color>| want == Some(c);
    if is(cell.fg, fg(tokens.error)) {
        return 'e';
    }
    if is(cell.fg, fg(tokens.toast)) && is(cell.bg, bg(tokens.toast)) {
        return 't';
    }
    if is(cell.bg, bg(tokens.own_selected)) || is(cell.bg, bg(tokens.their_selected)) {
        return '^';
    }
    if is(cell.bg, bg(tokens.own)) {
        return '.';
    }
    if is(cell.bg, bg(tokens.their)) {
        return ',';
    }
    if is(cell.bg, bg(tokens.date_pill)) {
        return '=';
    }
    if is(cell.fg, fg(tokens.unread)) {
        return '!';
    }
    if cell.modifier.contains(Modifier::REVERSED) {
        return '^';
    }
    ' '
}

/// The screen of a test backend as a style map, one row a line, trailing
/// blanks cut.
fn styles_text(backend: &TestBackend, tokens: &Tokens) -> String {
    let buffer = backend.buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let row: String = (0..buffer.area.width)
            .filter_map(|x| buffer.cell((x, y)))
            .map(|cell| style_letter(cell, tokens))
            .collect();
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

/// A key script against the real engine, frames to a file. No tick happens
/// unless the script asks for one (`tick`, or `sleep`'s one per 250 ms of its
/// own length, however long the wall clock took), so a frame is a pure
/// function of the script and the engine's events.
async fn run_script(
    shell: &mut Shell,
    script: &Path,
    frames: &Path,
    options: &TuiOptions,
) -> Result<(), String> {
    let text = std::fs::read_to_string(script).map_err(|e| format!("{}: {e}", script.display()))?;
    let steps = parse_script(&text).map_err(|e| format!("{}: {e}", script.display()))?;
    log_keys(&shell.model);
    // A script asks no terminal and looks for no go-between: `auto` climbs
    // the ladder on its tier alone, so a frame does not depend on where the
    // test runs.
    // Nor does it read a remembered drawing or write one: its home is
    // the test's, and its frames must not depend on a run before.
    let facts = map::Facts::none(shell.model.tier);
    let chosen = map::choose_graphics(options.graphics, &facts);
    let mut painter = map::MapPainter::new(shell.maps.clone(), chosen.graphics, None);
    log_graphics(&painter, &chosen, &facts);
    shell.apply(UiEvent::MapSetup(map::setup_of(&painter, &facts)));
    let mut out =
        std::fs::File::create(frames).map_err(|e| format!("{}: {e}", frames.display()))?;
    let styles_path = {
        let mut path = frames.as_os_str().to_owned();
        path.push(".styles");
        PathBuf::from(path)
    };
    let mut styles: Option<std::fs::File> = None;
    browse_like_the_terminal(&mut shell.model);
    let (width, height) = options.size;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).map_err(|e| e.to_string())?;
    shell.apply(UiEvent::Resize { width, height });
    shell.apply(UiEvent::Clock(script_clock()));
    let mut count = 0usize;
    // A frame of the model, or without one a line of what the screen asked
    // of the terminal.
    let mut dump = |terminal: &mut Terminal<TestBackend>,
                    painter: &mut map::MapPainter,
                    model: Option<&Model>,
                    label: &str| {
        let Some(model) = model else {
            return writeln!(out, "--- {label} ---")
                .and_then(|()| out.flush())
                .map_err(|e| format!("{}: {e}", frames.display()));
        };
        let view = ui::view(model);
        let drawn = terminal
            .draw(|frame| draw_screen(frame, model, &view, Some(painter)))
            .map_err(|e| e.to_string())?;
        count += 1;
        let text = frame_text(drawn.buffer);
        write!(out, "--- frame {count}: {label} ---\n{text}")
            .and_then(|()| out.flush())
            .map_err(|e| format!("{}: {e}", frames.display()))
    };
    for step in steps {
        // What the last step put on screen, before the next acts.
        say_viewport(shell);
        match step {
            Step::Mouse(events) => {
                for event in events {
                    shell.apply(UiEvent::Mouse(event));
                }
            }
            Step::Key(key) => shell.apply(UiEvent::Key(key)),
            Step::Focus(front) => {
                tracing::info!("FOCUS front={front} elapsed_ms={}", shell.elapsed_ms());
                shell.apply(UiEvent::Front(front));
            }
            Step::Keyboard(enhanced) => shell.model.enhanced = enhanced,
            Step::Tier(tier) => shell.model.tier = tier,
            Step::Theme(theme) => shell.model.theme = theme,
            Step::Lang(lang) => shell.model.lang = lang,
            Step::Styles(label) => {
                let view = ui::view(&shell.model);
                terminal
                    .draw(|frame| draw_screen(frame, &shell.model, &view, Some(&mut painter)))
                    .map_err(|e| e.to_string())?;
                let map = styles_text(terminal.backend(), shell.model.tokens());
                let file = match styles.as_mut() {
                    Some(file) => file,
                    None => styles.insert(
                        std::fs::File::create(&styles_path)
                            .map_err(|e| format!("{}: {e}", styles_path.display()))?,
                    ),
                };
                write!(file, "--- styles: {label} ---\n{map}")
                    .and_then(|()| file.flush())
                    .map_err(|e| format!("{}: {e}", styles_path.display()))?;
            }
            Step::Clock(now) => shell.apply(UiEvent::Clock(now)),
            Step::Type(text) => {
                for c in text.chars() {
                    shell.apply(UiEvent::Key(KeyEvent::new(
                        KeyCode::Char(c),
                        KeyModifiers::NONE,
                    )));
                }
            }
            Step::Tick(n) => {
                for _ in 0..n {
                    shell.apply(UiEvent::Tick);
                }
            }
            Step::Frame(label) => {
                // A script's screen is drawn whole every frame anyway.
                shell.redraw = false;
                dump(&mut terminal, &mut painter, Some(&shell.model), &label)?
            }
            Step::Until(until, within) => {
                let deadline = tokio::time::Instant::now() + within;
                loop {
                    shell.poll();
                    say_viewport(shell);
                    if until.holds(&shell.model, &shell.legs_seen) {
                        break;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        dump(
                            &mut terminal,
                            &mut painter,
                            Some(&shell.model),
                            &format!("timeout: {until:?}"),
                        )?;
                        return Err(format!(
                            "script: {until:?} did not hold within {}s",
                            within.as_secs()
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(PUMP_MS)).await;
                }
            }
            Step::Sleep(ms) => {
                // The ticks are counted on the script's clock, not read off
                // the wall: `sleep 2500` is ten ticks on a loaded host too.
                let mut slept = 0;
                let mut ticked = 0;
                while slept < ms {
                    let step = PUMP_MS.min(ms - slept);
                    tokio::time::sleep(Duration::from_millis(step)).await;
                    slept += step;
                    shell.poll();
                    say_viewport(shell);
                    while ticked + TICK_MS <= slept {
                        ticked += TICK_MS;
                        shell.apply(UiEvent::Tick);
                    }
                }
            }
        }
        for output in std::mem::take(&mut shell.outputs) {
            let line = match output {
                Output::MapDrawing(n) => {
                    if let Some(&next) = map::Graphics::ORDER.get(n) {
                        painter = map::MapPainter::new(shell.maps.clone(), next, None);
                        shell.apply(UiEvent::MapSetup(map::setup_of(&painter, &facts)));
                    }
                    continue;
                }
                Output::Copy(text) => format!("osc52 {}", osc52(&text).replace('\x1b', "\\e")),
                Output::Open(url) => format!("open {url}"),
                Output::Photo(path) => format!("photo {}", path.display()),
                Output::File(path) => format!("file {}", path.display()),
                Output::Mouse(on) => format!("mouse {}", if on { "on" } else { "off" }),
            };
            dump(&mut terminal, &mut painter, None, &line)?;
        }
        if shell.model.quit {
            break;
        }
    }
    Ok(())
}

/// One frame of the messenger: the Browse tab in front is lnomad's own
/// screen, drawn by lnomad's terminal view over the whole terminal (1369),
/// with its help, its in-page search and its places list; else Loquax's
/// panes. The tab draws no pictures: the map owns the terminal's picture
/// protocol, and the tab asks for none ([`lnomad::Model::no_images`]).
pub fn draw_screen(
    frame: &mut Frame,
    model: &Model,
    view: &View,
    painter: Option<&mut map::MapPainter>,
) {
    if model.rooms.showing() {
        return draw_rooms(frame, model);
    }
    if loquax::ui::explore::Explore::showing(model) {
        return draw_explore(frame, model);
    }
    match model.browse.showing() {
        true => lnomad::tui::view(
            &model.browse.model,
            &lnomad::tui::ImageStore::default(),
            frame,
        ),
        false => draw(frame, view, painter),
    }
}

/// The Rooms tab (1379, as a chat since 1450) over the whole terminal:
/// the tab's name on top with how the link stands while it is not up,
/// the rooms on the left, the open room on the right as the chat pane
/// draws a conversation (another's bubbles on the left behind their bar,
/// the writer named over a run, ours on the right, the comings and goings
/// as quiet lines in the middle), its people in the pane's title, the
/// compose field with its hint at the bottom and, under it, the keys or
/// the toast.
fn draw_rooms(frame: &mut Frame, model: &Model) {
    use loquax::ui::strings::{StrId, Text as Said};
    let lang = model.lang;
    let node = loquax::ui::rooms::node(model);
    let area = frame.area();
    if area.height < 6 || area.width < 20 {
        return;
    }
    let mut title = Said::key(StrId::TabRooms).resolve(lang);
    if let Some(status) = &node.status {
        title = format!("{title} · {}", status.resolve(lang));
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 5);
    let left_width = (area.width / 4).clamp(12, 24);
    let list = Rect::new(body.x, body.y, left_width, body.height);
    let room = Rect::new(
        body.x + left_width,
        body.y,
        body.width - left_width,
        body.height,
    );
    let rows: Vec<Line> = node
        .rows
        .iter()
        .map(|row| {
            let mark = if row.selected { "▌" } else { " " };
            let unread = match row.unread {
                0 => String::new(),
                n => format!(" ({n})"),
            };
            let style = match row.unread {
                0 => Style::default(),
                _ => Style::default().add_modifier(Modifier::BOLD),
            };
            // A room is public (1577): the globe, where the terminal
            // draws one, and the word beside the name.
            let public = row.public.resolve(lang);
            let public = match model.ascii {
                true => format!(" ({public})"),
                false => format!(" {} {public}", loquax::ui::rooms::PUBLIC_MARK),
            };
            Line::from(vec![
                Span::styled(format!("{mark}{}", row.name), style),
                Span::styled(public, Style::default().add_modifier(Modifier::DIM)),
                Span::styled(unread, style),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(rows).block(pane_block(
            &Said::key(StrId::TabRooms).resolve(lang),
            node.open.is_none(),
            Borders::ALL,
        )),
        list,
    );
    let inner_width = usize::from(room.width.saturating_sub(2));
    let (name, mut lines) = match &node.open {
        Some(open) => {
            let name = format!("{} · {}", open.name, open.subtitle.resolve(lang));
            (name, room_lines(open, inner_width, lang))
        }
        None => {
            let dim = Style::default().add_modifier(Modifier::DIM);
            let mut lines: Vec<Line> = Vec::new();
            // The link's verb (1652): Enter on an empty field takes it.
            if let Some(retry) = &node.retry {
                lines.push(Line::from(Span::styled(
                    format!("Enter: {}", retry.resolve(lang)),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
            }
            // The rooms the hub names (1652): typing one's name joins it.
            if let Some(listed) = &node.listed {
                lines.push(Line::from(Span::styled(
                    listed.title.resolve(lang),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                for room in &listed.rooms {
                    lines.push(Line::from(format!("  {room}")));
                }
                if let Some(note) = &listed.note {
                    lines.push(Line::from(Span::styled(note.resolve(lang), dim)));
                }
                if let Some(other) = &listed.other {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(other.resolve(lang), dim)));
                }
            }
            if let Some(empty) = node.empty.as_ref().map(|e| e.resolve(lang)) {
                lines.push(centred(&empty, inner_width, dim));
            }
            (String::new(), lines)
        }
    };
    // The newest lines that fit, the room's one line kept on top.
    let inner = usize::from(room.height.saturating_sub(2));
    if node.open.is_none() {
        // The hub's rooms read from the top (1652).
        lines.truncate(inner);
    } else if lines.len() > inner && inner > 1 {
        let live = lines.remove(0);
        let keep = lines.split_off(lines.len() - (inner - 1));
        lines = std::iter::once(live).chain(keep).collect();
    }
    frame.render_widget(
        Paragraph::new(lines).block(pane_block(&name, false, Borders::ALL)),
        room,
    );
    let input = Rect::new(area.x, area.y + area.height - 4, area.width, 3);
    let field = match node.input.is_empty() {
        true => Line::from(vec![
            Span::raw("▏"),
            Span::styled(
                node.placeholder.resolve(lang),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ]),
        false => Line::from(format!("{}▏", node.input)),
    };
    frame.render_widget(
        Paragraph::new(field).block(pane_block("", true, Borders::ALL)),
        input,
    );
    let foot = match model.current_toast() {
        Some(toast) => toast.text.clone(),
        None if node.open.is_some() => {
            "Enter send │ Alt-p who is here │ Alt-l leave room │ Esc back".to_string()
        }
        None => "Enter join │ ↑↓ rooms │ Esc chats".to_string(),
    };
    frame.render_widget(
        Paragraph::new(foot),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );
}

/// The Explore tab (1578) over the whole terminal: its name on top, the
/// search field, the pages under their headings (the name, when last
/// seen, how far, `*` for a bookmark), then the public rooms: the rooms
/// the reader is in, the servers and the servers heard on the mesh. A
/// server's sheet (1618) takes the body's place while it is open. The
/// cursor's row is marked `▌`; the window scrolls to keep it in view.
/// Under it the keys or the toast.
fn draw_explore(frame: &mut Frame, model: &Model) {
    use loquax::ui::strings::{Arg as SaidArg, StrId, Text as Said};
    let lang = model.lang;
    let node = loquax::ui::explore::node(model);
    let area = frame.area();
    if area.height < 6 || area.width < 20 {
        return;
    }
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let quiet = Style::default().add_modifier(Modifier::DIM);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            Said::key(StrId::TabExplore).resolve(lang),
            bold,
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let pages = &node.pages;
    let field = match pages.query.is_empty() {
        true => Line::from(vec![
            Span::raw("▏"),
            Span::styled(pages.placeholder.resolve(lang), quiet),
        ]),
        false => Line::from(format!("{}▏", pages.query)),
    };
    frame.render_widget(
        Paragraph::new(field).block(pane_block(&pages.sort.resolve(lang), true, Borders::ALL)),
        Rect::new(area.x, area.y + 1, area.width, 3),
    );
    let mark = |on: bool| if on { "▌" } else { " " };
    let mut lines: Vec<Line> = Vec::new();
    let mut at: Option<usize> = None;
    // How old the mailbox's list is, on top (1644).
    if let Some(as_of) = &pages.as_of {
        lines.push(Line::from(Span::styled(as_of.resolve(lang), quiet)));
    }
    lines.push(Line::from(Span::styled(pages.title.resolve(lang), bold)));
    if let Some(off) = &pages.off {
        lines.push(Line::from(Span::styled(off.resolve(lang), quiet)));
    }
    if let Some((url, cursor)) = &pages.address {
        if *cursor {
            at = Some(lines.len());
        }
        let words = Said::Key(
            StrId::ExploreOpenAddress,
            Vec::from([SaidArg::Str(url.clone())]),
        );
        lines.push(Line::from(format!(
            "{}{}",
            mark(*cursor),
            words.resolve(lang)
        )));
    }
    for group in &pages.groups {
        lines.push(Line::from(Span::styled(
            format!(" {}", group.heading.resolve(lang)),
            quiet,
        )));
        for page in &group.rows {
            if page.cursor {
                at = Some(lines.len());
            }
            let mut said = Vec::new();
            if let Some(seen) = &page.seen {
                said.push(seen.resolve(lang));
            }
            if let Some(hops) = &page.hops {
                said.push(hops.resolve(lang));
            }
            let star = if page.bookmarked { "* " } else { "  " };
            lines.push(Line::from(vec![
                Span::raw(format!(
                    "{}{star}{}",
                    mark(page.cursor),
                    page.label.resolve(lang)
                )),
                Span::styled(format!("  {}", said.join(" · ")), quiet),
            ]));
        }
    }
    if let Some(empty) = &pages.empty {
        lines.push(Line::from(Span::styled(empty.resolve(lang), quiet)));
    }
    let rooms = &node.rooms;
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(rooms.title.resolve(lang), bold)));
    for (i, room) in rooms.joined.iter().enumerate() {
        let cursor = rooms.joined_cursor == Some(i);
        if cursor {
            at = Some(lines.len());
        }
        let unread = match room.unread {
            0 => String::new(),
            n => format!(" ({n})"),
        };
        let public = room.public.resolve(lang);
        let public = match model.ascii {
            true => format!(" ({public})"),
            false => format!(" {} {public}", loquax::ui::rooms::PUBLIC_MARK),
        };
        lines.push(Line::from(vec![
            Span::raw(format!("{}{}", mark(cursor), room.name)),
            Span::styled(public, quiet),
            Span::raw(unread),
        ]));
    }
    let hub = |row: &loquax::ui::explore::HubRow, lines: &mut Vec<Line>, at: &mut Option<usize>| {
        if row.cursor {
            *at = Some(lines.len());
        }
        let chosen = if row.chosen { " ✓" } else { "" };
        let value = row
            .seen
            .iter()
            .chain(row.value.iter())
            .map(|v| v.resolve(lang))
            .collect::<Vec<_>>()
            .join(" · ");
        lines.push(Line::from(vec![
            Span::raw(format!(
                "{}{}{chosen}",
                mark(row.cursor),
                row.name.resolve(lang)
            )),
            Span::styled(format!("  {value}"), quiet),
        ]));
    };
    if let Some(empty) = &rooms.joined_empty {
        lines.push(Line::from(Span::styled(empty.resolve(lang), quiet)));
    }
    // The rooms screen of the server in use (1655).
    if let Some(open) = &rooms.open_rooms {
        if rooms.open_cursor {
            at = Some(lines.len());
        }
        lines.push(Line::from(format!(
            "{}[ {} ]",
            mark(rooms.open_cursor),
            open.resolve(lang)
        )));
    }
    // The servers under their own heading (1621).
    lines.push(Line::from(Span::styled(
        format!(" {}", rooms.servers_title.resolve(lang)),
        quiet,
    )));
    for row in &rooms.hubs {
        hub(row, &mut lines, &mut at);
    }
    if let Some(empty) = &rooms.servers_empty {
        lines.push(Line::from(Span::styled(empty.resolve(lang), quiet)));
    }
    let mut keys = "Enter open │ ↑↓ move │ Alt-b bookmark │ Alt-s order │ Esc chats";
    if let Some(sheet) = &node.sheet {
        // A server's sheet (1618): its name, how far, the button, the
        // rooms it named; nothing is chosen until Enter on the button.
        keys = "Enter use or join │ ↑↓ move │ Esc close";
        lines = Vec::new();
        at = None;
        lines.push(Line::from(Span::styled(sheet.name.resolve(lang), bold)));
        let said: Vec<String> = sheet
            .seen
            .iter()
            .chain(sheet.far.iter())
            .map(|t| t.resolve(lang))
            .collect();
        if !said.is_empty() {
            lines.push(Line::from(Span::styled(said.join(" · "), quiet)));
        }
        lines.push(Line::from(""));
        let mut row = 0;
        if !sheet.chosen {
            if sheet.cursor == row {
                at = Some(lines.len());
            }
            lines.push(Line::from(format!(
                "{}[ {} ]",
                mark(sheet.cursor == row),
                sheet.button.resolve(lang)
            )));
            // Wrapped (1667: the notice that names the profile is longer
            // than a row; the border cut it).
            let cols = usize::from(area.width).saturating_sub(4);
            for said in wrap_cols(&sheet.notice.resolve(lang), cols) {
                lines.push(Line::from(Span::styled(format!("  {said}"), quiet)));
            }
            row += 1;
        } else {
            lines.push(Line::from(format!(" ✓ {}", sheet.button.resolve(lang))));
            // Its rooms screen (1655).
            if let Some(open) = &sheet.open_rooms {
                if sheet.cursor == row {
                    at = Some(lines.len());
                }
                lines.push(Line::from(format!(
                    "{}[ {} ]",
                    mark(sheet.cursor == row),
                    open.resolve(lang)
                )));
            }
            row += 1;
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            sheet.rooms_title.resolve(lang),
            bold,
        )));
        for room in &sheet.rooms {
            if sheet.cursor == row {
                at = Some(lines.len());
            }
            lines.push(Line::from(format!("{}{room}", mark(sheet.cursor == row))));
            row += 1;
        }
        if let Some(none) = &sheet.rooms_note {
            lines.push(Line::from(Span::styled(none.resolve(lang), quiet)));
        }
    }
    let body = Rect::new(area.x, area.y + 4, area.width, area.height - 5);
    let inner = usize::from(body.height.saturating_sub(2)).max(1);
    let skip = at.map_or(0, |at| (at + 1).saturating_sub(inner));
    let lines: Vec<Line> = lines.into_iter().skip(skip).collect();
    frame.render_widget(
        Paragraph::new(lines).block(pane_block("", false, Borders::ALL)),
        body,
    );
    let foot = match model.current_toast() {
        Some(toast) => toast.text.clone(),
        None => keys.to_string(),
    };
    frame.render_widget(
        Paragraph::new(foot),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );
}

/// The open room's lines as the chat pane draws a conversation (1450):
/// the one line on what a room is not in the middle, then each line a
/// bubble or a quiet line; with the header tapped, its people instead.
fn room_lines(open: &loquax::ui::rooms::RoomView, width: usize, lang: Lang) -> Vec<Line<'static>> {
    use loquax::ui::rooms::RoomLineKind;
    let quiet = Style::default().add_modifier(Modifier::DIM);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(people) = &open.people {
        lines.push(Line::from(Span::styled(
            open.people_note.resolve(lang),
            quiet,
        )));
        lines.extend(
            people
                .iter()
                .map(|p| Line::from(format!(" {}", p.resolve(lang)))),
        );
        return lines;
    }
    for live in wrap_cols(&open.live.resolve(lang), width.saturating_sub(4).max(8)) {
        lines.push(centred(&live, width, quiet.add_modifier(Modifier::ITALIC)));
    }
    let bubble = (width * 3 / 4).max(12);
    for line in &open.lines {
        let text = line.text.resolve(lang);
        if !line.bubble {
            let mark = match line.kind {
                RoomLineKind::Error => "! ",
                _ => "",
            };
            lines.push(centred(&format!("── {mark}{text} ──"), width, quiet));
            continue;
        }
        let action = matches!(
            line.kind,
            RoomLineKind::Said(leviculum_rrc::LineKind::Action)
        );
        let said = match action {
            true => format!("{} {text}", line.who.resolve(lang)),
            false => text,
        };
        let style = match action {
            true => Style::default().add_modifier(Modifier::ITALIC),
            false => Style::default(),
        };
        let time = line.time.clone().unwrap_or_default();
        if line.named {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(" {}", line.who.resolve(lang)),
                Style::default().add_modifier(Modifier::BOLD),
            )));
        } else if line.ours {
            lines.push(Line::from(""));
        }
        let mut wrapped = wrap_cols(&said, bubble.saturating_sub(time.len() + 3));
        if let Some(last) = wrapped.last_mut() {
            if !time.is_empty() {
                last.push_str("  ");
                last.push_str(&time);
            }
        }
        for row in wrapped {
            match line.ours {
                true => {
                    let pad = width.saturating_sub(leviculum_ui::text::fit_width(&row) + 1);
                    lines.push(Line::from(vec![
                        Span::raw(" ".repeat(pad)),
                        Span::styled(row, style),
                    ]));
                }
                false => lines.push(Line::from(vec![Span::raw("▌"), Span::styled(row, style)])),
            }
        }
    }
    lines
}

/// `text` in the middle of `width` cells.
fn centred(text: &str, width: usize, style: Style) -> Line<'static> {
    let pad = width.saturating_sub(leviculum_ui::text::fit_width(text)) / 2;
    Line::from(vec![
        Span::raw(" ".repeat(pad)),
        Span::styled(text.to_string(), style),
    ])
}

/// `text` in rows of at most `cols` cells, broken between words, a word
/// longer than a row cut.
fn wrap_cols(text: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in text.split(' ') {
        let mut word = word.to_string();
        loop {
            let used = leviculum_ui::text::fit_width(&row);
            let len = leviculum_ui::text::fit_width(&word);
            let gap = usize::from(!row.is_empty());
            if used + gap + len <= cols {
                if gap == 1 {
                    row.push(' ');
                }
                row.push_str(&word);
                break;
            }
            if !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                continue;
            }
            let head = leviculum_ui::text::truncate_to_cols(&word, cols);
            let rest = word[head.len()..].to_string();
            rows.push(head);
            if rest.is_empty() {
                break;
            }
            word = rest;
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

/// The hosted browser as the terminal draws it: the messenger's theme and
/// colours, no pictures.
pub(crate) fn browse_like_the_terminal(model: &mut Model) {
    let browser = &mut model.browse.model;
    browser.theme = model.theme;
    browser.no_color = model.tier == Tier::Mono;
    browser.depth = match model.tier {
        Tier::Ansi16 => lnomad::ColorDepth::Ansi16,
        Tier::Ansi256 => lnomad::ColorDepth::Ansi256,
        Tier::Mono | Tier::Truecolor => lnomad::ColorDepth::Truecolor,
    };
    browser.no_images = true;
}

/// Bold for the focused pane's border and title.
fn pane_block(title: &str, focused: bool, borders: Borders) -> Block<'static> {
    let (title, style) = match focused {
        true => (
            format!("● {title}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        false => (format!("  {title}"), Style::default()),
    };
    Block::default()
        .borders(borders)
        .border_style(style)
        .title(Span::styled(title, style))
}

/// The compose box's border: its top corners join the conversation pane's
/// sides, so the two read as one column.
const COMPOSE_BORDER: border::Set = border::Set {
    top_left: "├",
    top_right: "┤",
    ..border::PLAIN
};

/// Draw the screen: the list left the full height, the conversation and the
/// compose box under it right, the footer at the bottom; the toast, the
/// palette, the detail view and the help on top.
pub fn draw(frame: &mut Frame, view: &View, painter: Option<&mut map::MapPainter>) {
    let (theme, tier) = (view.theme, view.tier);
    let no_color = tier == Tier::Mono;
    let tokens = leviculum_ui::tokens(tier, theme);
    let compose_lines: Vec<&str> = view.compose.split('\n').collect();
    // The recording bar takes the field's place (1276): its row, and the
    // line under it when it says something.
    let compose_rows = match &view.recording {
        Some(bar) => bar.rows.len(),
        None => compose_lines.len().clamp(1, ui::COMPOSE_MAX_ROWS),
    };
    // The model's rectangles, the ones the mouse hit-tests against.
    let area = frame.area();
    let screen = ui::Screen::of(area.width, area.height, compose_rows);
    let left = to_ratatui(screen.list);
    let messages = to_ratatui(screen.messages);
    let compose = to_ratatui(screen.compose);
    let footer_row = to_ratatui(screen.footer);
    let body = Rect {
        height: footer_row.y,
        ..area
    };

    // The list, or the search in its place: its field on top, `esc closes`
    // on its border; `/ search` on the bottom border while none is open.
    let mut block = pane_block(
        &view.conversations_title,
        matches!(view.focus, Pane::Conversations | Pane::Search),
        Borders::ALL,
    );
    if !view.conversations_title_right.is_empty() {
        block = block
            .title_top(Line::from(format!(" {} ", view.conversations_title_right)).right_aligned());
    }
    if !view.list_hint.is_empty() {
        block = block.title_bottom(Line::from(format!(" {} ", view.list_hint)).right_aligned());
    }
    let inner = block.inner(left);
    frame.render_widget(block, left);
    frame.render_widget(Paragraph::new(to_ratatui_text(&view.conversations)), inner);
    let search_cursor = view
        .search_cursor
        .map(|column| Position::new(inner.x + column as u16, inner.y));

    // The model laid the rows out at this pane's size: drawn as they come.
    let mut block = pane_block(
        &view.messages_title,
        matches!(view.focus, Pane::Messages | Pane::Map),
        Borders::TOP | Borders::LEFT | Borders::RIGHT,
    );
    if !view.messages_title_right.is_empty() {
        block =
            block.title_top(Line::from(format!(" {} ", view.messages_title_right)).right_aligned());
    }
    let inner = block.inner(messages);
    frame.render_widget(block, messages);
    match (&view.map, painter) {
        // The map in the conversation's place: the picture, the pins and
        // the attribution, all inside the pane.
        (Some(shown), Some(painter)) => {
            painter.set_tier(tier);
            painter.set_theme(loquax::picture::map_theme(view.map_theme, Some(view.theme)));
            painter.set_time_zones(view.time_zones);
            painter.render(frame, shown, Style::default().add_modifier(Modifier::DIM));
            if view.map_glyph_test {
                map::draw_glyph_test(frame, to_ratatui(shown.area));
            }
        }
        (Some(shown), None) => {
            // The tracks under the pins (1385), without colour here.
            for t in &shown.tracks {
                if !area.contains(Position::new(t.col, t.row)) {
                    continue;
                }
                let room = usize::from((inner.x + inner.width).saturating_sub(t.col));
                frame
                    .buffer_mut()
                    .set_stringn(t.col, t.row, &t.text, room, Style::default());
            }
            for pin in &shown.pins {
                let apart = pin.apart.iter().map(|a| (a.col, a.row, &a.text));
                for (col, row, text) in std::iter::once((pin.col, pin.row, &pin.text)).chain(apart)
                {
                    // Placed for the size the model last heard of (1087).
                    if !area.contains(Position::new(col, row)) {
                        continue;
                    }
                    // Our own position, the dot in blue (1449 item 2).
                    let style = match pin.own {
                        true => Style::default().fg(Color::Rgb(0x1a, 0x73, 0xe8)),
                        false => Style::default(),
                    };
                    frame
                        .buffer_mut()
                        .set_stringn(col, row, text, usize::from(inner.width), style);
                }
            }
        }
        (None, _) => {
            // Laid out for a taller pane than this frame has (a resize the
            // model has not heard of yet): the bottom rows, where the newest
            // are.
            let excess = view
                .messages
                .len()
                .saturating_sub(usize::from(inner.height));
            frame.render_widget(
                Paragraph::new(to_ratatui_text(&view.messages[excess..])),
                inner,
            );
        }
    }

    // The compose pane: one row a line, up to five, scrolled to the
    // cursor's line and, sideways, to its column.
    let focused = view.focus == Pane::Compose;
    let block = pane_block(&view.compose_title, focused, Borders::ALL)
        .border_set(COMPOSE_BORDER)
        .title_top(Line::from(format!(" {} ", view.compose_hint)).right_aligned());
    let inner = block.inner(compose);
    frame.render_widget(block, compose);
    let (top, scroll, cursor_line, cursor_column) = ui::compose_window(
        &view.compose,
        view.compose_cursor,
        usize::from(inner.width),
        usize::from(inner.height),
    );
    match &view.recording {
        Some(bar) => frame.render_widget(Paragraph::new(to_ratatui_text(&bar.rows)), inner),
        None => {
            let shown: Vec<Line> = compose_lines
                .iter()
                .skip(top)
                .take(usize::from(inner.height.max(1)))
                .map(|line| Line::from(line.chars().skip(scroll).collect::<String>()))
                .collect();
            frame.render_widget(Paragraph::new(Text::from(shown)), inner);
        }
    }
    let overlay = view.palette.is_some() || view.help || view.detail.is_some();
    // No cursor in the recording bar: there is no text to type into.
    if focused && !overlay && view.recording.is_none() {
        frame.set_cursor_position(Position::new(
            inner.x + (cursor_column - scroll) as u16,
            inner.y + (cursor_line - top) as u16,
        ));
    }
    if let (Some(position), false) = (search_cursor, overlay) {
        frame.set_cursor_position(position);
    }

    let slots = footer::layout(view.footer.clone(), screen.footer);
    leviculum_ui_ratatui::footer::render(
        frame,
        footer_row,
        &slots,
        view.footer_hover,
        no_color,
        theme,
    );

    if let Some(note) = &view.toast {
        // Over both columns above the compose box, so it has the screen's
        // width. Wrapped, not cut: a failed auto send's toast names both legs.
        let over = Rect {
            height: messages.height,
            ..body
        };
        let rows = ui::toast_rows(&toast::body(note), toast::max_inner(over));
        toast::render_rows(frame, over, note.kind, &rows, tokens);
    }
    if let Some(detail) = &view.detail {
        draw_detail(frame, detail, view.detail_scroll);
    }
    if let Some(palette) = &view.palette {
        let area = to_ratatui(ui::palette_rect(palette.rows.len(), screen.area()));
        frame.render_widget(Clear, area);
        let block = Block::default().borders(Borders::ALL).title(" commands ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let mut lines = vec![Line::from(format!("> {}", palette.query))];
        lines.extend(to_ratatui_text(&palette.rows).lines);
        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
        frame.set_cursor_position(Position::new(inner.x + 2 + palette.cursor as u16, inner.y));
    }
    if view.help {
        help::render(
            frame,
            frame.area(),
            &help_groups(),
            " Loquax help ",
            view.help_scroll,
            no_color,
            theme,
        );
    }
    // The footer and the help draw lnomad's chrome in RGB; the tokens name
    // their own indexes and pass.
    if let Some(depth) = tier.depth() {
        downgrade_buffer(frame, depth, false);
    }
}

/// A message's detail view, centred over the panes, its rows wrapped, from
/// its `scroll`th row with a scrollbar when the screen is too short for all.
fn draw_detail(frame: &mut Frame, detail: &ui::DetailView, scroll: usize) {
    let screen = from_rect(frame.area());
    let (area, rows) = ui::detail_layout(detail, screen);
    let (viewport, max) = ui::detail_scroll_bounds(detail, screen);
    let scroll = scroll.min(max);
    let area = to_ratatui(area);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", detail.title))
        .title_bottom(Line::from(" Esc closes ").right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines: Vec<Line> = rows
        .into_iter()
        .skip(scroll)
        .take(viewport)
        .map(Line::from)
        .collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
    if max > 0 {
        let mut state = ScrollbarState::new(max).position(scroll);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        frame.render_stateful_widget(scrollbar, area.inner(Margin::new(0, 1)), &mut state);
    }
}

fn from_rect(rect: Rect) -> leviculum_ui::Rect {
    leviculum_ui::Rect::new(rect.x, rect.y, rect.width, rect.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use loquax::home::runner::already_held;
    use loquax::send::SendError;
    use loquax::store::Store;
    use loquax::ui::update;

    /// A terminal golden under `tests/golden`: the frame's text, compared
    /// whole; one that moved is left in the temp dir as
    /// `loquax-golden/<name>` for a look.
    /// `LOQUAX_WRITE_GOLDEN=1` writes the goldens and the frames anew, for
    /// a change that means to move them.
    fn writing_goldens() -> bool {
        std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some()
    }

    /// The German twin's name of a golden or a fixture: `tui-x.txt` is
    /// `tui-x-de.txt`, `tui_x.frames` is `tui_x_de.frames`.
    fn german_name(name: &str) -> String {
        let (stem, ext) = name.rsplit_once('.').expect("a name with its kind");
        let join = if stem.starts_with("tui-") { "-" } else { "_" };
        format!("{stem}{join}de.{ext}")
    }

    /// Whether `screen` holds `words`, said with the screen when not.
    fn says(screen: &str, words: &str) {
        assert!(screen.contains(words), "{words:?} is not said:\n{screen}");
    }

    /// A terminal fixture under `tests/fixtures`: the frames' text,
    /// compared whole; frames that moved are left in the temp dir as
    /// `loquax-golden/<name>`.
    fn same_frames(name: &str, text: &str) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        if writing_goldens() {
            std::fs::write(&path, text).expect("write the fixture");
            return;
        }
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
            let dir = std::env::temp_dir().join("loquax-golden");
            std::fs::create_dir_all(&dir).expect("temp dir");
            std::fs::write(dir.join(name), text).expect("write");
            panic!(
                "{name} moved; this run's is {}\n{text}",
                dir.join(name).display()
            );
        }
    }

    fn same_text(name: &str, text: &str) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(name);
        if writing_goldens() {
            std::fs::write(&path, text).expect("write the golden");
            return;
        }
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
            let dir = std::env::temp_dir().join("loquax-golden");
            std::fs::create_dir_all(&dir).expect("temp dir");
            std::fs::write(dir.join(name), text).expect("write");
            panic!("{name} moved; this run's is {}", dir.join(name).display());
        }
    }

    /// The screen as the terminal's loop draws it, 80 by 24.
    fn whole_screen(model: &Model) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        let view = ui::view(model);
        let drawn = terminal
            .draw(|frame| draw_screen(frame, model, &view, None))
            .expect("draw");
        frame_text(drawn.buffer)
    }

    mod late_message {
        include!("../tests/support/late_message.rs");
    }

    mod mailbox_page {
        include!("../tests/support/mailbox_page.rs");
    }

    /// 1642, the mailbox page names the mailbox: `S` and `2` open it, the
    /// fixture's mailbox on miauhaus reached over the shielded port, both
    /// devices with when each was there (the golden, in both languages);
    /// the unreachable older one says so; `c` opens the profile with the
    /// code. 80 by 40, so the whole page is on the screen.
    #[test]
    fn the_mailbox_page_names_the_mailbox_its_way_and_its_devices() {
        use loquax::ui::settings::Section;
        let key = |c| UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        let screen = |model: &Model| {
            let mut terminal = Terminal::new(TestBackend::new(80, 40)).expect("terminal");
            let view = ui::view(model);
            let drawn = terminal
                .draw(|frame| draw_screen(frame, model, &view, None))
                .expect("draw");
            frame_text(drawn.buffer)
        };
        let open = |lang, reachable| {
            let mut model = Model::new(&[]);
            model.lang = lang;
            mailbox_page::mailbox_told(&mut model, reachable);
            update(&mut model, key('S'));
            update(&mut model, key('2'));
            assert_eq!(model.settings, Some(Some(Section::Mailbox)));
            model
        };
        let german = screen(&open(Lang::De, true));
        for words in [
            "Welches Postfach",
            "Name: Lew Palm",
            "Adresse: 5a3e910c7742d81b6f03a4e258c92d80",
            "Läuft auf: miauhaus",
            "Dieses Gerät verbunden seit 12 Min.",
            "Verbindung zu miauhaus, geschirmt",
            "Fairphone: dieses Gerät",
            "M9: zuletzt da vor 2 Std.",
            "Laptop",
            "c  Meinen Code zeigen",
        ] {
            says(&german, words);
        }
        same_text(&german_name("tui-settings-mailbox.txt"), &german);
        let mut model = open(Lang::En, true);
        let shown = screen(&model);
        says(&shown, "Connection to miauhaus, shielded");
        same_text("tui-settings-mailbox.txt", &shown);
        let away = screen(&open(Lang::En, false));
        says(&away, "This device not connected for 2 h");
        says(&away, "Connection to leviculum.network, not shielded");
        says(&away, "Version: older than this device");
        update(&mut model, key('c'));
        assert!(model.profile_open, "c shows the code");
    }

    /// 1662, a file tells its progress: the terminal draws the bubble of
    /// a 300 KB file mid-transfer, 43 % sent by its link's resource, the
    /// bar and its words under the file's line; the golden is the screen,
    /// 80 by 24, in both languages.
    #[test]
    fn a_file_mid_transfer_is_the_terminals_golden() {
        use loquax::outbox::OutboxEvent;
        let scene = |lang| {
            let peer = [0x41; 16];
            let mut model = Model::new(&[peer]);
            model.lang = lang;
            model.address = Some([0x30; 16]);
            update(&mut model, UiEvent::Clock(shell::script_clock()));
            update(
                &mut model,
                UiEvent::SendFile {
                    peer,
                    name: "Protokoll.pdf".into(),
                    data: vec![7; 300_000],
                },
            );
            update(
                &mut model,
                UiEvent::Engine(OutboxEvent::Queued {
                    message_id: [0x62; 32],
                }),
            );
            update(&mut model, UiEvent::Elapsed(1_000));
            update(
                &mut model,
                UiEvent::Engine(OutboxEvent::Progress {
                    message_id: [0x62; 32],
                    permille: 430,
                }),
            );
            model.focus = loquax::ui::Pane::Messages;
            model.toasts.clear();
            whole_screen(&model)
        };
        let screen = scene(Lang::En);
        says(&screen, "▤ Protokoll.pdf · 300 KB");
        says(&screen, "▰▰▰▰▰▱▱▱▱▱ sending · 43 %");
        same_text("tui-file-progress.txt", &screen);
        let german = scene(Lang::De);
        says(&german, "▰▰▰▰▰▱▱▱▱▱ wird gesendet · 43 %");
        same_text(&german_name("tui-file-progress.txt"), &german);
    }

    /// 1505, a late message sits at its sent time: the terminal draws
    /// Anna's chat of the shared fixture, the message she wrote last night
    /// at 22:10 that came today at 14:05 under last night's day line, the
    /// `new` rule above it, today's two below; the golden is the screen,
    /// 80 by 24.
    #[test]
    fn a_late_message_is_the_terminals_golden() {
        let scene = |lang| {
            let mut model = Model::new(&[]);
            model.lang = lang;
            model.address = Some([0x30; 16]);
            late_message::late_message(&mut model, 1_799_971_200, 3600);
            model.focus = loquax::ui::Pane::Messages;
            model.toasts.clear();
            whole_screen(&model)
        };
        let screen = scene(Lang::En);
        // The chat's rows, right of the list's (which names our last line).
        let lines: Vec<&str> = screen.lines().collect();
        let row = |words: &str| {
            lines
                .iter()
                .rposition(|l| l.contains(words))
                .unwrap_or_else(|| panic!("{words:?} is not on the screen:\n{screen}"))
        };
        assert!(row("Gute Nacht") < row("Kommst du morgen mit?"), "{screen}");
        assert!(
            row("Kommst du morgen mit?") < row("Bis gleich am Bahnhof"),
            "{screen}"
        );
        assert!(
            row("Bis gleich am Bahnhof") < row("Bin unterwegs"),
            "{screen}"
        );
        same_text("tui-late-message.txt", &screen);
        let german = scene(Lang::De);
        says(&german, "Gute Nacht");
        says(&german, "du: Bin unterwegs");
        same_text(&german_name("tui-late-message.txt"), &german);
    }

    mod reply_scene {
        include!("../tests/support/reply_scene.rs");
    }

    /// 1451, the walk's blind spots, a reply and a reaction: the terminal
    /// draws Anna's chat of the shared fixture, our reply to her question,
    /// her thumbs up under it and her photo's line; then a reply to her
    /// last line being written, the draft in the compose box. The terminal
    /// draws no quotes (`a_relayed_group_is_the_terminals_golden`) and no
    /// line over the compose box for a reply being written: the palette's
    /// `reply` says it once in a toast, and the draft keeps its target.
    /// The goldens are the screens, 80 by 24, in English and in German.
    #[test]
    fn a_reply_and_a_reaction_are_the_terminals_golden() {
        let scene = |lang, composing: bool| {
            let mut model = Model::new(&[]);
            model.lang = lang;
            model.address = Some([0x30; 16]);
            reply_scene::reply_scene(&mut model, 1_799_971_200, 3600);
            model.focus = loquax::ui::Pane::Messages;
            if composing {
                reply_scene::reply_composing(&mut model);
                model.focus = loquax::ui::Pane::Compose;
            }
            model.toasts.clear();
            (whole_screen(&model), model)
        };
        let (sent, _) = scene(Lang::En, false);
        for words in [
            "Kommst du morgen mit zum Markt?",
            "Ja, um zehn am Brunnen",
            "\u{1F44D}",
            "photo",
            "Super, bis morgen",
        ] {
            says(&sent, words);
        }
        same_text("tui-reply-sent.txt", &sent);
        let (german, _) = scene(Lang::De, false);
        says(&german, "Ja, um zehn am Brunnen");
        says(&german, "\u{1F44D}");
        same_text(&german_name("tui-reply-sent.txt"), &german);

        let (compose, model) = scene(Lang::En, true);
        says(&compose, reply_scene::REPLY_DRAFT);
        says(&compose, "Super, bis morgen");
        assert_eq!(
            model.conversations[model.selected].draft.reply,
            Some(reply_scene::REPLY_LAST),
            "the draft answers her last line"
        );
        same_text("tui-reply-compose.txt", &compose);
        let (german, _) = scene(Lang::De, true);
        says(&german, reply_scene::REPLY_DRAFT);
        same_text(&german_name("tui-reply-compose.txt"), &german);
    }

    /// The facts the shell gives the profile of a messenger without a
    /// mailbox (`shell::profile_facts` with no client): the name, no
    /// mailbox, this device alone.
    fn no_mailbox_profile(name: &str) -> loquax::ui::profile::ProfileFacts {
        use loquax::ui::profile::{Device, Mailbox, ProfileFacts};
        ProfileFacts {
            name: name.to_string(),
            mailbox: Mailbox::Unknown,
            devices: vec![Device {
                name: String::new(),
                this: true,
            }],
            ..Default::default()
        }
    }

    /// 1451, the very first start, before any mailbox: no conversation,
    /// the empty list names the palette's command; `P` opens the profile,
    /// which says the mailbox is not set up yet. The goldens are the two
    /// screens, 80 by 24, in English and in German.
    #[test]
    fn the_first_start_is_the_terminals_golden() {
        let first = |lang| {
            let mut model = Model::new(&[]);
            model.lang = lang;
            model.address = Some([0x4c; 16]);
            model.profile = no_mailbox_profile("Mia");
            update(
                &mut model,
                UiEvent::Clock(WallTime::from_civil(2026, 11, 10, 9, 30, 3600)),
            );
            model.toasts.clear();
            model
        };
        let mut model = first(Lang::En);
        assert!(model.conversations.is_empty());
        let empty = whole_screen(&model);
        says(&empty, "No conversations yet");
        same_text("tui-first-start.txt", &empty);
        let mut german = first(Lang::De);
        let german_empty = whole_screen(&german);
        says(&german_empty, "Noch keine Nachrichten");
        same_text(&german_name("tui-first-start.txt"), &german_empty);

        update(&mut model, UiEvent::Key(parse_key("P").expect("key")));
        assert!(model.profile_open);
        let profile = whole_screen(&model);
        says(&profile, "Mailbox: not set up yet");
        says(&profile, "Mia");
        same_text("tui-first-start-profile.txt", &profile);
        update(&mut german, UiEvent::Key(parse_key("P").expect("key")));
        let german_profile = whole_screen(&german);
        says(&german_profile, "Postfach: noch nicht eingerichtet");
        same_text(&german_name("tui-first-start-profile.txt"), &german_profile);
    }

    /// 1451, the map with two pins near Florence: Anna's and Ben's
    /// positions as the store hands them back at start, `M` opens the map,
    /// `Tab` selects Anna's pin. The test draws no map picture (no
    /// painter, no region): the pins and their labels on the pane's empty
    /// ground. The goldens are the screen, 80 by 24, in English and in
    /// German.
    #[test]
    fn two_pins_on_the_map_are_the_terminals_golden() {
        use leviculum_lxmf::telemetry::Location;
        use loquax::telemetry::{Fix, Source};
        const ANNA: [u8; 16] = [0xa7; 16];
        const BEN: [u8; 16] = [0xb3; 16];
        let now = 1_800_000_000;
        let fix = |peer, lat: i32, lon: i32, ago: i64| Fix {
            peer,
            via: peer,
            message: None,
            received_at: Some(now - ago),
            location: Location {
                latitude_e6: lat,
                longitude_e6: lon,
                altitude_e2: 5_000,
                speed_e2: 0,
                bearing_e2: 0,
                accuracy_e2: 500,
                last_update: now - ago,
            },
            telemetry: Vec::new(),
            source: Source::Telemetry,
        };
        let map_in = |lang| {
            let mut model = Model::new(&[ANNA, BEN]);
            model.lang = lang;
            model.address = Some([0x30; 16]);
            model.names.remember(ANNA, "Anna");
            model.names.remember(BEN, "Ben");
            update(
                &mut model,
                UiEvent::Clock(WallTime {
                    unix_secs: now,
                    offset_secs: 3600,
                }),
            );
            model.restore_positions(vec![
                fix(ANNA, 43_771_000, 11_254_000, 4 * 60),
                fix(BEN, 43_776_500, 11_247_000, 25 * 60),
            ]);
            update(&mut model, UiEvent::Key(parse_key("M").expect("key")));
            assert!(model.map.is_some(), "M opens the map");
            for _ in 0..4 {
                if model.map.as_ref().and_then(|m| m.selected) == Some(ANNA) {
                    break;
                }
                update(&mut model, UiEvent::Key(parse_key("tab").expect("key")));
            }
            assert_eq!(model.map.as_ref().and_then(|m| m.selected), Some(ANNA));
            model.toasts.clear();
            whole_screen(&model)
        };
        let screen = map_in(Lang::En);
        says(&screen, "Anna");
        says(&screen, "Ben");
        same_text("tui-map-pins.txt", &screen);
        let german = map_in(Lang::De);
        says(&german, "Anna · vor 4 Min.");
        says(&german, "Ben · vor 25 Min.");
        same_text(&german_name("tui-map-pins.txt"), &german);
    }

    /// The screen and under it the cells drawn otherwise than with no
    /// selection (`#`), the copy mode's selection, which reverses them.
    fn screen_with_selection(model: &Model) -> String {
        let draw = |model: &Model| {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
            let view = ui::view(model);
            terminal
                .draw(|frame| draw_screen(frame, model, &view, None))
                .expect("draw")
                .buffer
                .clone()
        };
        let with = draw(model);
        let mut plain = model.clone();
        plain.selection = None;
        let without = draw(&plain);
        let mut out = frame_text(&with);
        out.push_str("--- selected\n");
        for y in 0..with.area.height {
            let row: String = (0..with.area.width)
                .map(|x| match (with.cell((x, y)), without.cell((x, y))) {
                    (Some(a), Some(b)) if a.modifier != b.modifier => '#',
                    _ => ' ',
                })
                .collect();
            out.push_str(row.trim_end());
            out.push('\n');
        }
        out
    }

    /// 1475, the terminal's copy mode on the late message's fixture: `g`
    /// to Anna's first message, `v` puts the cursor on its "G", `l` three
    /// times selects "Gute", drawn reversed (the golden: the screen and the
    /// cells the selection reverses); `y` copies it through OSC 52 and
    /// leaves. `w` goes by words, `j` to the next text row: from "Nacht"
    /// down into "Kommst" is one line a message, writer and time. `Esc`
    /// leaves without a copy, and the Emacs mark with `Ctrl-f` and `Alt-w`
    /// copies as `v` and `y` do; a Shift+arrow enters and moves at once.
    #[test]
    fn the_copy_mode_selects_with_the_keys_and_copies_through_osc52() {
        let press = |model: &mut Model, code: KeyCode, mods: KeyModifiers| {
            update(model, UiEvent::Key(KeyEvent::new(code, mods)))
        };
        let plain = KeyModifiers::NONE;
        let selected = |lang| {
            let mut model = Model::new(&[]);
            model.lang = lang;
            model.address = Some([0x30; 16]);
            late_message::late_message(&mut model, 1_799_971_200, 3600);
            model.focus = loquax::ui::Pane::Messages;
            press(&mut model, KeyCode::Char('g'), plain);
            press(&mut model, KeyCode::Char('v'), plain);
            assert!(model.copy_mode.is_some(), "v is the copy mode");
            assert_eq!(model.detail, None, "and no longer the details");
            for _ in 0..3 {
                press(&mut model, KeyCode::Char('l'), plain);
            }
            model.toasts.clear();
            model
        };
        let mut model = selected(Lang::En);
        same_text("tui-copy-mode.txt", &screen_with_selection(&model));
        let german = screen_with_selection(&selected(Lang::De));
        says(&german, "du: Bin unterwegs");
        same_text(&german_name("tui-copy-mode.txt"), &german);
        let copied = |effects: Vec<ui::UiEffect>| -> Vec<String> {
            effects
                .into_iter()
                .filter_map(|e| match e {
                    ui::UiEffect::Copy(text) => Some(text),
                    _ => None,
                })
                .collect()
        };
        let effects = press(&mut model, KeyCode::Char('y'), plain);
        assert_eq!(copied(effects), vec!["Gute".to_string()]);
        assert!(model.copy_mode.is_none() && model.selection.is_none());

        // By words, then a row down into the next message.
        press(&mut model, KeyCode::Char('v'), plain);
        press(&mut model, KeyCode::Char('w'), plain);
        let from = model.copy_mode.expect("on").cursor;
        // A new anchor where the cursor is: leave and enter again there
        // is vi's way; here the anchor stays at "G" and the selection
        // runs from it.
        assert_eq!(from.1, model.copy_mode.expect("on").anchor.1 + 5);
        press(&mut model, KeyCode::Char('j'), plain);
        press(&mut model, KeyCode::Char('$'), plain);
        let effects = press(&mut model, KeyCode::Char('y'), plain);
        let text = copied(effects).pop().expect("a copy");
        assert!(text.starts_with("Anna, 21:30: Gute Nacht\n"), "{text:?}");
        assert!(text.contains(": Kommst du morgen mit?"), "{text:?}");

        // Esc leaves, nothing copied.
        press(&mut model, KeyCode::Char('v'), plain);
        press(&mut model, KeyCode::Char('l'), plain);
        let effects = press(&mut model, KeyCode::Esc, plain);
        assert!(copied(effects).is_empty());
        assert!(model.copy_mode.is_none() && model.selection.is_none());

        // The Emacs mark.
        press(&mut model, KeyCode::Char(' '), KeyModifiers::CONTROL);
        assert!(model.copy_mode.is_some(), "Ctrl-Space sets the mark");
        for _ in 0..3 {
            press(&mut model, KeyCode::Char('f'), KeyModifiers::CONTROL);
        }
        let effects = press(&mut model, KeyCode::Char('w'), KeyModifiers::ALT);
        assert_eq!(copied(effects), vec!["Gute".to_string()]);

        // Shift+Right enters and moves at once.
        press(&mut model, KeyCode::Right, KeyModifiers::SHIFT);
        let effects = press(&mut model, KeyCode::Char('y'), plain);
        assert_eq!(copied(effects), vec!["Gu".to_string()]);
    }

    /// The copy mode's drawn cells are the characters the core selection
    /// copies (1475): within one message, the drag's text of the drawn
    /// cells is the core's text, for every end the cursor reaches in it.
    #[test]
    fn the_copy_modes_drawn_cells_are_what_it_copies() {
        let mut model = Model::new(&[]);
        model.address = Some([0x30; 16]);
        late_message::late_message(&mut model, 1_799_971_200, 3600);
        model.focus = loquax::ui::Pane::Messages;
        let press = |model: &mut Model, code: KeyCode| {
            update(model, UiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
        };
        press(&mut model, KeyCode::Char('g'));
        press(&mut model, KeyCode::Char('v'));
        for _ in 0..9 {
            press(&mut model, KeyCode::Char('l'));
            let laid = ui::copy_mode::laid(&model);
            let runs = ui::copy_mode::runs(&model, &laid);
            let mode = model.copy_mode.expect("on");
            let core = ui::copy_mode::selection(&runs, &laid, &mode)
                .expect("a selection")
                .text(&runs.runs);
            let drawn = ui::hit::selected_text(&laid, &model.selection.expect("drawn"));
            assert_eq!(core, drawn);
        }
    }

    mod group_ticks {
        include!("../tests/support/group_ticks.rs");
    }

    /// 1466, the group's ticks tell the truth too: the terminal draws the
    /// group's chat of `scene-group-ticks.json`, our line every member's
    /// device has with two ticks and the one Ben's device has not said yet
    /// with one; the golden is the screen, 80 by 24.
    #[test]
    fn a_groups_one_and_two_ticks_are_the_terminals_golden() {
        let scene = |lang| {
            let mut model = Model::new(&[]);
            model.lang = lang;
            model.address = Some([0x30; 16]);
            group_ticks::group_ticks(&mut model, 1_800_000_000, 3600);
            model.focus = loquax::ui::Pane::Messages;
            whole_screen(&model)
        };
        let screen = scene(Lang::En);
        let line = |words: &str| {
            screen
                .lines()
                .rfind(|l| l.contains(words))
                .unwrap_or_else(|| panic!("{words:?} is not on the screen:\n{screen}"))
                .to_string()
        };
        assert!(line("Brot ist da").contains("✓✓"), "{screen}");
        let one = line("Wer kommt mit?");
        assert!(one.contains("✓") && !one.contains("✓✓"), "{screen}");
        same_text("tui-group-ticks.txt", &screen);
        let german = scene(Lang::De);
        says(&german, "du: Wer kommt mit?");
        same_text(&german_name("tui-group-ticks.txt"), &german);
    }

    mod relay_group {
        include!("../tests/support/relay_group.rs");
    }

    /// 1482, a relayed post shows its writer: the terminal draws the relay's
    /// chat of `scene-relay-group.json`, each post under its writer without
    /// the bot's prefix (as the terminal names a group's writer, in the
    /// line), a reaction to a message that is not here in words; the golden
    /// is the screen, 80 by 24. The terminal draws no quotes at all.
    #[test]
    fn a_relayed_group_is_the_terminals_golden() {
        let scene = |lang| {
            let mut model = Model::new(&[]);
            model.lang = lang;
            model.address = Some([0x30; 16]);
            relay_group::relay_group(&mut model, 1_800_000_000, 3600);
            model.focus = loquax::ui::Pane::Messages;
            // The arrivals' toasts would cover our own line.
            model.toasts.clear();
            whole_screen(&model)
        };
        let screen = scene(Lang::En);
        for words in [
            "group via relay",
            "Anna: Hallo zusammen",
            "Ben: Moin Anna",
            "Ben: reacted",
            "to a message that is",
            "Ich bin auch da",
        ] {
            assert!(screen.contains(words), "{words:?}:\n{screen}");
        }
        assert!(
            !screen.contains(&loquax::address::to_hex(&relay_group::RELAY_ANNA)),
            "no hash on screen:\n{screen}"
        );
        same_text("tui-relay-group.txt", &screen);
        let german = scene(Lang::De);
        says(&german, "Gruppe über Relais");
        same_text(&german_name("tui-relay-group.txt"), &german);
    }

    /// 1452: two peers called Lew collide, so both carry their hash tail
    /// and the header of the open one says "Two contacts are called Lew";
    /// Anna, whose name nobody shares, is her name alone. The terminal's
    /// list is too narrow for the sentence, so only the header says it.
    #[test]
    fn a_shared_name_is_the_terminals_golden() {
        let (lew, other, anna) = ([0x1e; 16], [0xe1; 16], [0xa0; 16]);
        let scene = |lang| {
            let mut model = Model::new(&[lew, other, anna]);
            model.lang = lang;
            for (peer, name) in [(lew, "Lew"), (other, "lew "), (anna, "Anna")] {
                update(
                    &mut model,
                    UiEvent::Engine(OutboxEvent::Announced {
                        destination: peer,
                        name: Some(name.to_string()),
                        path_response: false,
                    }),
                );
            }
            model.toasts.clear();
            whole_screen(&model)
        };
        let narrow = scene(Lang::En);
        assert!(
            narrow.contains("Lew ·1e1e  Two contacts are called Lew"),
            "{narrow}"
        );
        assert!(narrow.contains("▌Lew ·1e1e"), "{narrow}");
        assert!(narrow.contains(" lew ·e1e1"), "{narrow}");
        assert!(narrow.contains(" Anna "), "{narrow}");
        assert!(!narrow.contains("Anna ·"), "{narrow}");
        assert!(!narrow.contains('≈'), "{narrow}");
        same_text("tui-same-name.txt", &narrow);
        let german = scene(Lang::De);
        says(&german, "Zwei Kontakte heißen Lew");
        same_text(&german_name("tui-same-name.txt"), &german);
    }

    /// 1450 (walk 1, finding 21): `S` opens the settings, the nine
    /// sections in the phone's order (the golden); a digit opens one, its
    /// rows run by their keys (Privacy's `r` turns read receipts off),
    /// `q` goes back to the list and closes it.
    #[test]
    fn s_opens_the_nine_settings_sections_and_a_key_runs_a_row() {
        use loquax::ui::settings::{Section, SECTIONS};
        let key = |c| UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        let mut model = Model::new(&[]);
        update(&mut model, key('S'));
        assert_eq!(model.settings, Some(None));
        let shown = whole_screen(&model);
        let mut german = Model::new(&[]);
        german.lang = Lang::De;
        update(&mut german, key('S'));
        let german = whole_screen(&german);
        for section in SECTIONS {
            says(&german, section.title().text(Lang::De));
        }
        same_text(&german_name("tui-settings.txt"), &german);
        let mut at = 0;
        for section in SECTIONS {
            let title = section.title().text(model.lang);
            let found = shown[at..]
                .find(title)
                .unwrap_or_else(|| panic!("{title} after the one before\n{shown}"));
            at += found + title.len();
        }
        same_text("tui-settings.txt", &shown);
        update(&mut model, key('6'));
        assert_eq!(model.settings, Some(Some(Section::Privacy)));
        let privacy = whole_screen(&model);
        assert!(privacy.contains("r  Send read receipts: On"), "{privacy}");
        assert!(model.reading.on);
        update(&mut model, key('r'));
        assert!(!model.reading.on, "the row's key runs it");
        assert!(whole_screen(&model).contains("Send read receipts: Off"));
        update(&mut model, key('q'));
        assert_eq!(model.settings, Some(None), "back to the list");
        // The Profile section opens the profile over the list; q closes
        // the profile and leaves the list.
        update(&mut model, key('1'));
        assert!(model.profile_open);
        update(&mut model, key('q'));
        assert_eq!((model.profile_open, model.settings), (false, Some(None)));
        update(&mut model, key('q'));
        assert_eq!(model.settings, None, "closed");
    }

    /// 1577, 1599: Advanced holds the public rooms' hub (the golden): the
    /// closest ten hubs by name and how far (keys `1` to `9` and `0`),
    /// "More…" (`m`) turning to the next ten, `1` chooses the first and the
    /// line says it, `a` asks for another's address in the palette, which
    /// `Enter` takes, and `n` chooses none. No hash on screen.
    #[test]
    fn advanced_chooses_the_public_rooms_hub_by_key_and_by_address() {
        use loquax::ui::settings::Section;
        use loquax::ui::UiEffect;
        let key = |c| UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        let enter = UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let advanced = |lang| {
            let mut model = Model::new(&[]);
            model.lang = lang;
            update(&mut model, key('S'));
            update(&mut model, key('9'));
            assert_eq!(model.settings, Some(Some(Section::Advanced)));
            model
        };
        let mut model = advanced(Lang::En);
        let shown = whole_screen(&model);
        let german = whole_screen(&advanced(Lang::De));
        says(&german, "Server für öffentliche Räume: Keiner");
        same_text(&german_name("tui-settings-advanced.txt"), &german);
        assert!(shown.contains("Server for public rooms: None"), "{shown}");
        same_text("tui-settings-advanced.txt", &shown);
        let shipped = loquax::ui::hubs::shipped();
        // More: the next ten, and back.
        update(&mut model, key('m'));
        let second = whole_screen(&model);
        assert!(
            second.contains(&format!("1  {}", shipped[10].name)),
            "{second}"
        );
        assert!(!second.contains(&shipped[0].name), "{second}");
        for _ in 1..shipped.len().div_ceil(loquax::ui::settings::HUB_PAGE) {
            update(&mut model, key('m'));
        }
        let first = shipped[0].hash;
        // Choosing links at once (1652), the privacy sentence with it.
        assert_eq!(
            update(&mut model, key('1')),
            [
                UiEffect::RememberHash {
                    key: "rooms_hub",
                    hash: Some(first)
                },
                UiEffect::RememberHash {
                    key: "rooms_told",
                    hash: Some(first)
                },
                UiEffect::Rooms(loquax::ui::rooms::RoomAsk::Open { hub: first })
            ]
        );
        assert!(model.rooms.enabled, "the Rooms tab is there");
        let shown = whole_screen(&model);
        assert!(
            shown.contains(&format!("Server for public rooms: {}", shipped[0].name)),
            "{shown}"
        );
        assert!(
            // Never heard here: no distance beside it (1629); the link's
            // state in the rooms screen's words (1655, walk 9, 49).
            shown.contains(&format!("1  {}: Connecting… · \u{2713}", shipped[0].name)),
            "{shown}"
        );
        update(&mut model, key('a'));
        assert!(model.palette.is_some(), "the palette asks");
        let asked = whole_screen(&model);
        assert!(asked.contains("The server's address"), "{asked}");
        for c in "07070707070707070707070707070707".chars() {
            update(&mut model, key(c));
        }
        // The link to the first one closes, the pasted one links.
        assert_eq!(
            update(&mut model, enter),
            [
                UiEffect::Rooms(loquax::ui::rooms::RoomAsk::Close),
                UiEffect::RememberHash {
                    key: "rooms_hub",
                    hash: Some([0x07; 16])
                },
                UiEffect::RememberHash {
                    key: "rooms_told",
                    hash: Some([0x07; 16])
                },
                UiEffect::Rooms(loquax::ui::rooms::RoomAsk::Open { hub: [0x07; 16] })
            ]
        );
        let shown = whole_screen(&model);
        assert!(
            shown.contains("Server for public rooms: Your own server"),
            "{shown}"
        );
        assert!(!shown.contains("0707070707"), "no hash on screen\n{shown}");
        update(&mut model, key('n'));
        assert!(!model.rooms.enabled, "none: no tab");
    }

    /// 1379: the Rooms tab in the terminal: `R` brings it and asks the
    /// shell to link to the hub; the hub's welcome, a join, Bob's line, his
    /// action, the hub's notice and the echo of our own line make the
    /// screen, the golden. A line past the hub's limit stays in the input
    /// with the sentence under it and asks nothing; a terminal whose config
    /// names no hub draws no Rooms for `R` (the control).
    #[test]
    fn the_rooms_tab_draws_the_open_room_under_its_one_line() {
        use leviculum_rrc::LineKind;
        use loquax::ui::rooms::{RoomAsk, RoomsEvent};
        use loquax::ui::UiEffect;
        const HUB: [u8; 16] = [0x07; 16];
        let key = |c| UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        let off = Model::new(&[]);
        let room_in = |lang| {
            let mut model = Model::new(&[]);
            model.lang = lang;
            model.rooms.enabled = true;
            model.rooms.hub = Some(HUB);
            // Told before (1577): no sentence over the golden.
            model.rooms.told = Some(HUB);
            // Opening the rooms links (1652); a join meanwhile waits.
            let effects = update(&mut model, key('R'));
            assert_eq!(effects, vec![UiEffect::Rooms(RoomAsk::Open { hub: HUB })]);
            assert!(model.rooms.showing());
            for c in "#lab".chars() {
                update(&mut model, key(c));
            }
            let effects = update(
                &mut model,
                UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            );
            assert_eq!(effects, vec![], "the join waits for the welcome");
            for event in [
                RoomsEvent::Welcomed {
                    name: Some("periculum".into()),
                    body_limit: 350,
                },
                RoomsEvent::Joined {
                    room: "#lab".into(),
                    nick: None,
                },
                RoomsEvent::Joined {
                    room: "#lab".into(),
                    nick: Some("bob".into()),
                },
                RoomsEvent::Line {
                    room: "#lab".into(),
                    kind: LineKind::Msg,
                    text: "a line, twenty-five bytes".into(),
                    nick: Some("bob".into()),
                    ours: false,
                    ts_ms: 1_793_702_757_876,
                },
                RoomsEvent::Line {
                    room: "#lab".into(),
                    kind: LineKind::Action,
                    text: "waves".into(),
                    nick: Some("bob".into()),
                    ours: false,
                    ts_ms: 1_793_702_758_093,
                },
                RoomsEvent::HubNotice {
                    room: Some("#lab".into()),
                    text: "room #lab: unregistered; mode=(none); topic=(none)".into(),
                },
                RoomsEvent::Line {
                    room: "#lab".into(),
                    kind: LineKind::Msg,
                    text: "hello bob".into(),
                    nick: Some("lew".into()),
                    ours: true,
                    ts_ms: 1_793_702_759_000,
                },
            ] {
                update(&mut model, UiEvent::Rooms(event));
            }
            model
        };
        let mut model = room_in(Lang::En);
        let room = whole_screen(&model);
        assert!(room.contains("A room is live"), "{room}");
        assert!(room.contains("hello bob"), "{room}");
        // 1450 (walk 1, finding 20): a chat, no IRC on the screen, and
        // the field's hint once.
        for irc in ["/me", "/part", "Hub", "rrc", "* bob", "you:"] {
            assert!(!room.contains(irc), "{irc:?} in\n{room}");
        }
        assert!(room.contains("#lab · bob, you"), "the people in the title");
        assert!(room.contains("▏Message"), "the hint in the field");
        assert_eq!(room.matches("Message").count(), 1, "said once\n{room}");
        same_text("tui-rooms-room.txt", &room);
        let german = whole_screen(&room_in(Lang::De));
        says(&german, "hello bob");
        says(&german, "Ein Raum ist live");
        same_text(&german_name("tui-rooms-room.txt"), &german);
        // The header's people, then back.
        let alt = |c| UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT));
        update(&mut model, alt('p'));
        let people = whole_screen(&model);
        assert!(people.contains("Everyone who came in or wrote"), "{people}");
        update(&mut model, alt('p'));

        // Typed and sent: the line goes to the room.
        for c in "hi".chars() {
            update(&mut model, key(c));
        }
        let effects = update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        assert_eq!(
            effects,
            vec![UiEffect::Rooms(RoomAsk::Say {
                room: "#lab".into(),
                kind: LineKind::Msg,
                text: "hi".into(),
            })]
        );
        // Past the limit: kept, said, nothing asked.
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Typed("x".repeat(351))),
        );
        let effects = update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(model.rooms.input.len(), 351);
        let said = whole_screen(&model);
        assert!(said.contains("This line is 351 bytes long"), "{said}");

        // The control: no hub, no Rooms.
        let mut off = off;
        update(&mut off, key('R'));
        assert!(!off.rooms.showing());
        assert!(!whole_screen(&off).contains("A room is live"));
    }

    /// 1644: the line on top of Explore says how old the mailbox's list
    /// is: in German, the link down and the last answer yesterday at
    /// 14:20; the golden. Up and three hours old it says that instead, and
    /// fresh nothing.
    #[test]
    fn explore_says_how_old_the_mailboxs_list_is() {
        use loquax::ui::browse::BrowseEvent;
        use loquax::ui::explore::ExploreEvent;
        let key = |c| UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        const NOW: i64 = 1_800_000_000;
        let answered = NOW - NOW.rem_euclid(86_400) + 14 * 3_600 + 20 * 60;
        let at = |model: &mut Model, unix_secs| {
            model.now = Some(loquax::when::WallTime {
                unix_secs,
                offset_secs: 0,
            });
        };
        let mut model = Model::new(&[]);
        model.lang = Lang::De;
        model.browse.enabled = true;
        browse_like_the_terminal(&mut model);
        at(&mut model, answered);
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Node(lnomad::DiscoveredNode {
                dest_hash: [0x5a; 16],
                name: Some("Marcel's node".to_string()),
                first_seen: (answered - 600) as u64,
                last_seen: (answered - 600) as u64,
                hops: Some(2),
            })),
        );
        update(&mut model, UiEvent::Explore(ExploreEvent::Mailbox(true)));
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Home(loquax::home::pages::Delta {
                stamp: 1,
                more: false,
                rows: Vec::new(),
            })),
        );
        update(&mut model, key('E'));
        assert!(!whole_screen(&model).contains("Stand"), "fresh");
        at(&mut model, answered + 3 * 3_600);
        says(&whole_screen(&model), "Stand: vor 3 Std.");
        update(&mut model, UiEvent::Explore(ExploreEvent::Mailbox(false)));
        at(&mut model, answered + 86_400);
        let screen = whole_screen(&model);
        says(&screen, "Nicht verbunden · Stand von gestern 14:20");
        same_text("tui-explore-as-of-de.txt", &screen);
    }

    /// 1578: `E` brings Explore to the front over the whole terminal: the
    /// field, the pages under their headings (a bookmark on top, then by
    /// last seen), the public rooms with the shipped hubs and the ones
    /// heard; the golden. Enter on a page opens lnomad's screen, its `q`
    /// comes back to the list, Esc leaves for the chats.
    #[test]
    fn explore_is_one_screen_for_pages_and_rooms() {
        use lnomad::DiscoveredNode;
        let key = |c| UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        use loquax::ui::browse::BrowseEvent;
        const NOW: i64 = 1_800_000_000;
        let node = |byte: u8, name: &str, ago: i64, hops| DiscoveredNode {
            dest_hash: [byte; 16],
            name: Some(name.to_string()),
            first_seen: (NOW - ago) as u64,
            last_seen: (NOW - ago) as u64,
            hops: Some(hops),
        };
        let explore_in = |lang| {
            let mut model = Model::new(&[]);
            model.lang = lang;
            model.browse.enabled = true;
            model.now = Some(loquax::when::WallTime {
                unix_secs: NOW,
                offset_secs: 0,
            });
            browse_like_the_terminal(&mut model);
            for found in [
                node(0x5a, "Marcel's node", 600, 2),
                node(0x7c, "Relay Süd", 2 * 86_400, 1),
            ] {
                update(&mut model, UiEvent::Browse(BrowseEvent::Node(found)));
            }
            model.browse.model.bookmarks.add(lnomad::Bookmark {
                url: format!("{}:/page/index.mu", "7c".repeat(16)),
                title: "Relay Süd".to_string(),
            });
            update(&mut model, key('E'));
            assert!(loquax::ui::explore::Explore::showing(&model));
            model
        };
        let mut model = explore_in(Lang::En);
        let list = whole_screen(&model);
        for words in ["Explore", "Pages", "Bookmarks", "Today", "Rooms", "Servers"] {
            assert!(list.contains(words), "{words:?} in\n{list}");
        }
        same_text("tui-explore.txt", &list);
        let german = whole_screen(&explore_in(Lang::De));
        for words in ["Entdecken", "Seiten", "Lesezeichen", "Heute", "Räume"] {
            says(&german, words);
        }
        same_text(&german_name("tui-explore.txt"), &german);
        // Down to Marcel's row (the bookmark is first), Enter opens it.
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        );
        let effects = update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        assert_eq!(effects.len(), 1, "{effects:?}");
        assert!(model.browse.showing());
        update(&mut model, key('q'));
        assert!(
            loquax::ui::explore::Explore::showing(&model),
            "back to the list"
        );
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(!model.explore.open);
        assert!(!whole_screen(&model).contains("Public rooms"));
    }

    /// 1618: a server's row opens its sheet in the terminal and changes
    /// nothing: its name, how far, the button and the rooms it named;
    /// `Esc` closes it back to the list, and `Enter` on the button makes
    /// it the public rooms' server. The golden is the sheet.
    #[test]
    fn a_servers_row_opens_its_sheet_and_changes_nothing() {
        let key = |code| UiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE));
        let shipped = loquax::ui::hubs::shipped();
        let Some(hub) = shipped.first() else {
            return;
        };
        let sheet_in = |lang| {
            let mut model = Model::new(&[]);
            model.lang = lang;
            model.browse.enabled = true;
            browse_like_the_terminal(&mut model);
            update(&mut model, key(KeyCode::Char('E')));
            update(
                &mut model,
                UiEvent::Explore(loquax::ui::explore::ExploreEvent::Hub(hub.hash)),
            );
            model
        };
        let mut model = sheet_in(Lang::En);
        let sheet = whole_screen(&model);
        assert!(sheet.contains(&hub.name), "{sheet}");
        assert!(sheet.contains("Use this server for rooms"), "{sheet}");
        assert!(!sheet.to_lowercase().contains("hops"), "{sheet}");
        same_text("tui-explore-sheet.txt", &sheet);
        let german = whole_screen(&sheet_in(Lang::De));
        says(&german, &hub.name);
        says(&german, "Diesen Server für Räume nutzen");
        same_text(&german_name("tui-explore-sheet.txt"), &german);
        assert_eq!(model.rooms.hub, None, "opening it chose nothing");
        update(&mut model, key(KeyCode::Esc));
        assert!(model.explore.sheet.is_none());
        assert!(loquax::ui::explore::Explore::showing(&model));
        update(
            &mut model,
            UiEvent::Explore(loquax::ui::explore::ExploreEvent::Hub(hub.hash)),
        );
        update(&mut model, key(KeyCode::Enter));
        assert_eq!(model.rooms.hub, Some(hub.hash), "the button chose it");
    }

    /// 1369: the Browse tab in the terminal is lnomad's own screen, drawn
    /// by lnomad's terminal view over the whole terminal: the list it
    /// opens on (two NomadNet nodes the announces named) and a page from
    /// the first, lnomad's sample fixture, each the golden. `q` leaves it
    /// for the messenger's panes; a terminal whose config leaves the tab
    /// off draws those for `B` (the control).
    #[test]
    fn the_browse_tab_is_lnomads_terminal_screen() {
        use lnomad::DiscoveredNode;
        use loquax::ui::browse::BrowseEvent;
        const MARCEL: [u8; 16] = [0x5a; 16];
        let node = |dest_hash, name: &str, hops| DiscoveredNode {
            dest_hash,
            name: Some(name.to_string()),
            first_seen: 1_800_000_000,
            last_seen: 1_800_000_000,
            hops: Some(hops),
        };
        let browse_in = |lang| {
            let mut model = Model::new(&[]);
            model.lang = lang;
            model.browse.enabled = true;
            model.browse.model.clock = Some(1_800_000_060);
            browse_like_the_terminal(&mut model);
            update(
                &mut model,
                UiEvent::Resize {
                    width: 80,
                    height: 24,
                },
            );
            for found in [
                node(MARCEL, "Marcel's node", 2),
                node([0x7c; 16], "Relay Süd", 1),
            ] {
                update(&mut model, UiEvent::Browse(BrowseEvent::Node(found)));
            }
            let off = model.clone();
            update(
                &mut model,
                UiEvent::Key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::NONE)),
            );
            assert!(model.browse.showing());
            (model, off)
        };
        // Enter on Marcel's row, as the reader picks it, and the page.
        let open_page = |model: &mut Model| {
            let row = lnomad::app::places(&model.browse.model)
                .iter()
                .position(
                    |p| matches!(p, lnomad::app::Place::Node { dest_hash, .. } if *dest_hash == MARCEL),
                )
                .expect("Marcel's row");
            let effects = update(
                model,
                UiEvent::Browse(BrowseEvent::Intent(lnomad::Intent::Activate(
                    lnomad::app::scene::NodeId::Place(row),
                ))),
            );
            assert!(matches!(
                &effects[..],
                [loquax::ui::UiEffect::Browse(lnomad::Effect::Navigate(_))]
            ));
            let micron = std::fs::read_to_string(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../lnomad/tests/fixtures/sample.mu"),
            )
            .expect("lnomad's sample page");
            update(
                model,
                UiEvent::Browse(BrowseEvent::Loaded {
                    micron,
                    title: "Marcel's node".to_string(),
                }),
            );
        };
        let (mut model, off) = browse_in(Lang::En);
        let list = whole_screen(&model);
        assert!(list.contains("Marcel's node"), "{list}");
        same_text("tui-browse-list.txt", &list);
        let (mut german, _) = browse_in(Lang::De);
        let german_list = whole_screen(&german);
        says(&german_list, "Marcel's node");
        says(&german_list, "Gehörte Seiten");
        same_text(&german_name("tui-browse-list.txt"), &german_list);

        open_page(&mut model);
        let page = whole_screen(&model);
        same_text("tui-browse-page.txt", &page);
        open_page(&mut german);
        let german_page = whole_screen(&german);
        says(&german_page, "zurück");
        same_text(&german_name("tui-browse-page.txt"), &german_page);

        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        );
        assert!(!model.browse.showing());
        assert_eq!(
            whole_screen(&model),
            whole_screen(&off),
            "the messenger's panes again"
        );

        let mut off = off;
        off.browse.enabled = false;
        update(
            &mut off,
            UiEvent::Key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::NONE)),
        );
        assert!(!off.browse.open);
        assert!(!whole_screen(&off).contains("Marcel's node"), "the control");
    }

    #[test]
    fn a_script_parses_every_step_kind() {
        let steps = parse_script(
            "# a comment\n\nkey tab\nkey ctrl-p\nkey G\ntype hello world\ntick 3\n\
             frame after typing\nuntil ready 5\nuntil delivery delivered\nuntil received 1 9\n",
        )
        .expect("parses");
        assert_eq!(steps.len(), 9);
        assert!(matches!(
            steps[1],
            Step::Key(KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        ));
        assert!(matches!(&steps[3], Step::Type(t) if t == "hello world"));
        assert!(matches!(&steps[5], Step::Frame(l) if l == "after typing"));
        assert!(matches!(
            steps[7],
            Step::Until(Until::Delivery(DeliveryState::Delivered), _)
        ));
        assert!(parse_script("key nope\n").is_err());
        assert!(parse_script("until weather\n").is_err());
    }

    #[test]
    fn sleep_and_until_leg_parse_and_sleep_is_capped() {
        let steps =
            parse_script("sleep 2500\nuntil leg direct 5\nuntil leg propagated\n").expect("parses");
        assert!(matches!(steps[0], Step::Sleep(2500)));
        assert!(matches!(
            steps[1],
            Step::Until(Until::Leg(Leg::Direct), d) if d == Duration::from_secs(5)
        ));
        assert!(matches!(
            steps[2],
            Step::Until(Until::Leg(Leg::Propagated), d) if d == Duration::from_secs(UNTIL_SECS)
        ));
        assert!(parse_script("sleep 60000\n").is_ok());
        assert!(parse_script("sleep 60001\n").is_err());
        assert!(parse_script("sleep\n").is_err());
        assert!(parse_script("until leg sideways\n").is_err());
    }

    #[test]
    fn until_named_parses_a_count_and_refuses_nonsense() {
        let steps = parse_script("until named 2 9\nuntil named 1\n").expect("parses");
        assert!(matches!(
            steps[0],
            Step::Until(Until::Named(2), d) if d == Duration::from_secs(9)
        ));
        assert!(matches!(
            steps[1],
            Step::Until(Until::Named(1), d) if d == Duration::from_secs(UNTIL_SECS)
        ));
        assert!(parse_script("until named\n").is_err());
        assert!(parse_script("until named some\n").is_err());
    }

    /// `clock` names a local date and time at UTC; nonsense is refused.
    #[test]
    fn the_clock_step_parses_a_date_and_a_time() {
        let steps = parse_script("clock 2026-10-24 13:58\n").expect("parses");
        assert!(matches!(
            steps[0],
            Step::Clock(t) if t == WallTime::from_civil(2026, 10, 24, 13, 58, 0)
        ));
        for bad in [
            "clock 2026-10-24\n",
            "clock 2026-13-01 10:00\n",
            "clock 2026-10-24 24:00\n",
            "clock today noon\n",
        ] {
            assert!(parse_script(bad).is_err(), "{bad}");
        }
    }

    /// `until leg` holds for the newest message we wrote, not an older one.
    #[test]
    fn until_leg_reads_the_newest_sent_message() {
        let mut model = Model::new(&[[0xab; 16]]);
        for name in ["tab", "tab", "a", "enter", "b", "enter"] {
            update(&mut model, UiEvent::Key(parse_key(name).expect("key")));
        }
        let until = Until::Leg(Leg::Direct);
        assert!(!until.holds(&model, &[((0, 0), Leg::Direct)]));
        assert!(!until.holds(&model, &[((0, 1), Leg::Propagated)]));
        assert!(until.holds(&model, &[((0, 1), Leg::Direct)]));
    }

    /// 1559: after a restart a conversation known only by its kept row
    /// counts no received message toward `until received`; one that
    /// arrives while the script runs does. Bob's earlier reply sits in a
    /// restored row (unread 1, the store holding 1 message), Dave's
    /// arrives: `received 1` holds, `received 2` does not, as periculum's
    /// `loquax_read_receipt_mixed` expects since its 1670.
    #[test]
    fn a_restarted_model_counts_only_what_it_holds() {
        use loquax::outbox::OutboxEvent;
        use loquax::ui::listing::{Listing, Row};
        const BOB: [u8; 16] = [0xb0; 16];
        const DAVE: [u8; 16] = [0xda; 16];
        let mut model = Model::new(&[]);
        model.restore_rows(
            vec![(
                Row {
                    peer: BOB,
                    unread: 1,
                    activity: 1,
                    unread_ids: vec![[0x0b; 32]],
                    listing: Listing {
                        line: "a reply".to_string(),
                        unread_rows: 1,
                        ..Listing::default()
                    },
                },
                1,
            )],
            Vec::new(),
        );
        assert!(Until::Received(0).holds(&model, &[]));
        assert!(
            !Until::Received(1).holds(&model, &[]),
            "a row is no message"
        );
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 0,
                message_id: [0x0d; 32],
                source: DAVE,
                title: Vec::new(),
                body: b"Dave's reply".to_vec(),
                fields: Vec::new(),
                verified: true,
            }),
        );
        assert!(Until::Received(1).holds(&model, &[]));
        assert!(!Until::Received(2).holds(&model, &[]));
        assert!(Until::Verified(1).holds(&model, &[]));
        assert!(!Until::Verified(2).holds(&model, &[]));
    }

    fn render(model: &Model) -> String {
        let mut model = model.clone();
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 16,
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
        let view = ui::view(&model);
        terminal
            .draw(|frame| draw(frame, &view, None))
            .expect("draw");
        frame_text(terminal.backend().buffer())
    }

    /// The style map of a model drawn 80 x 16 in a tier and theme.
    fn styles(model: &Model, tier: Tier, theme: Theme) -> String {
        let mut model = model.clone();
        model.tier = tier;
        model.theme = theme;
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 16,
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
        let view = ui::view(&model);
        terminal
            .draw(|frame| draw(frame, &view, None))
            .expect("draw");
        styles_text(terminal.backend(), model.tokens())
    }

    /// An error toast's border is the error colour and its inside the toast's;
    /// where the toast is reverse video, all of it is.
    #[test]
    fn an_error_toast_maps_its_border_to_the_error_letter() {
        let mut model = Model::new(&[[0xab; 16]]);
        model.toasts.push(leviculum_ui::toast::Toast {
            kind: leviculum_ui::toast::ToastKind::Error,
            text: "fetch failed".into(),
            shown_at: 0,
        });
        for theme in [Theme::Dark, Theme::Light] {
            for tier in [Tier::Truecolor, Tier::Ansi256] {
                let map = styles(&model, tier, theme);
                assert!(map.contains("eeeeeeeee"), "{tier:?} {theme:?}:\n{map}");
                assert!(map.contains("ettttt"), "{tier:?} {theme:?}:\n{map}");
            }
            let map = styles(&model, Tier::Ansi16, theme);
            assert!(
                !map.contains('e') && map.contains("^^^^^"),
                "16 {theme:?}:\n{map}"
            );
        }
        let map = styles(&model, Tier::Mono, Theme::Dark);
        assert!(map.chars().all(|c| matches!(c, ' ' | '^' | '\n')), "{map}");
    }

    #[test]
    fn a_script_sets_the_tier_and_the_theme_and_asks_for_a_style_map() {
        let steps =
            parse_script("tier 256\ntheme light\nstyles both sides\ntier mono\n").expect("parses");
        assert!(matches!(steps[0], Step::Tier(Tier::Ansi256)));
        assert!(matches!(steps[1], Step::Theme(Theme::Light)));
        assert!(matches!(&steps[2], Step::Styles(l) if l == "both sides"));
        assert!(matches!(steps[3], Step::Tier(Tier::Mono)));
        assert!(parse_script("tier 8\n").is_err());
        assert!(parse_script("theme solarized\n").is_err());
        let steps = parse_script("lang de\nlang en\n").expect("parses");
        assert!(matches!(steps[0], Step::Lang(Lang::De)));
        assert!(matches!(steps[1], Step::Lang(Lang::En)));
        assert!(parse_script("lang fr\n").is_err());
    }

    #[test]
    fn the_three_panes_and_the_footer_are_drawn_with_the_focus_marked() {
        let model = Model::new(&[[0xab; 16]]);
        let screen = render(&model);
        assert!(screen.contains("● Loquax (connecting)"), "{screen}");
        assert!(screen.contains("▌Unknown"), "{screen}");
        assert!(screen.contains("No messages yet"), "{screen}");
        assert!(screen.contains("  to Unknown"), "{screen}");
        assert!(screen.contains("Tab pane"), "{screen}");
    }

    #[test]
    fn ctrl_c_in_compose_keeps_the_line_for_ctrl_y() {
        let mut model = Model::new(&[[0xab; 16]]);
        for name in ["tab", "tab", "h", "i", "ctrl-c"] {
            update(&mut model, UiEvent::Key(parse_key(name).expect("key")));
        }
        assert!(!model.quit);
        assert_eq!(model.compose.value(), "");
        update(&mut model, UiEvent::Key(parse_key("ctrl-y").expect("key")));
        assert_eq!(model.compose.value(), "hi");
    }

    /// The enhanced chords and the modifier prefixes parse, in any order of
    /// prefixes, and the keyboard step names the world.
    #[test]
    fn script_keys_spell_the_enhanced_chords() {
        let parsed = |name: &str| parse_key(name).expect(name);
        assert_eq!(
            parsed("shift-enter"),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
        );
        assert_eq!(
            parsed("ctrl-enter"),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)
        );
        assert_eq!(
            parsed("alt-<"),
            KeyEvent::new(KeyCode::Char('<'), KeyModifiers::ALT)
        );
        assert_eq!(
            parsed("ctrl-shift-T"),
            KeyEvent::new(
                KeyCode::Char('T'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )
        );
        assert_eq!(
            parsed("f5"),
            KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)
        );
        assert_eq!(
            parsed("f"),
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE)
        );
        assert_eq!(
            parsed("-"),
            KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE)
        );
        assert!(parse_key("ctrl-").is_err());
        assert!(parse_key("f13").is_err());
        let steps = parse_script("keyboard enhanced\nkeyboard plain\n").expect("parses");
        assert!(matches!(steps[0], Step::Keyboard(true)));
        assert!(matches!(steps[1], Step::Keyboard(false)));
        assert!(parse_script("keyboard fancy\n").is_err());
    }

    /// In the terminal's field the Emacs line keys are line motions: after a
    /// `Ctrl-j`, `Ctrl-a` goes to the second line's start, `Ctrl-p` up to
    /// the first, `Alt->` to the body's end.
    #[test]
    fn compose_line_keys_move_by_line_in_the_real_field() {
        let mut model = Model::new(&[[0xab; 16]]);
        for name in ["tab", "tab", "a", "b", "ctrl-j", "c", "d", "ctrl-a"] {
            update(&mut model, UiEvent::Key(parse_key(name).expect("key")));
        }
        assert_eq!(model.compose.value(), "ab\ncd");
        assert_eq!(model.compose.cursor(), 3);
        update(&mut model, UiEvent::Key(parse_key("ctrl-p").expect("key")));
        assert_eq!(model.compose.cursor(), 0);
        assert!(model.palette.is_none());
        update(&mut model, UiEvent::Key(parse_key("alt->").expect("key")));
        assert_eq!(model.compose.cursor(), 5);
        update(&mut model, UiEvent::Key(parse_key("ctrl-n").expect("key")));
        assert_eq!(model.compose.cursor(), 5, "no line below the last");
    }

    /// The compose pane's border carries the live chords.
    #[test]
    fn the_compose_border_shows_the_live_hint() {
        let mut model = Model::new(&[[0xab; 16]]);
        assert!(
            render(&model).contains("Enter send · Alt-Enter newline"),
            "{}",
            render(&model)
        );
        model.enhanced = true;
        assert!(render(&model).contains("Enter send · Shift-Enter newline"));
    }

    /// `click`, `wheel` and `drag` become the events a terminal sends, at
    /// cells counted from 1 as the frames are; nonsense is refused.
    #[test]
    fn the_mouse_steps_parse_to_events_at_one_based_cells() {
        let steps = parse_script("click 3 4\nwheel down 10 2 3\nwheel up 1 1\ndrag 2 2 9 5\n")
            .expect("parses");
        let events = |step: &Step| match step {
            Step::Mouse(events) => events.iter().map(|e| (e.kind, e.column, e.row)).collect(),
            _ => Vec::new(),
        };
        let left = MouseButton::Left;
        assert_eq!(
            events(&steps[0]),
            [
                (MouseEventKind::Down(left), 2, 3),
                (MouseEventKind::Up(left), 2, 3)
            ]
        );
        assert_eq!(events(&steps[1]), [(MouseEventKind::ScrollDown, 9, 1); 3]);
        assert_eq!(events(&steps[2]), [(MouseEventKind::ScrollUp, 0, 0)]);
        assert_eq!(
            events(&steps[3]),
            [
                (MouseEventKind::Down(left), 1, 1),
                (MouseEventKind::Drag(left), 8, 4),
                (MouseEventKind::Up(left), 8, 4)
            ]
        );
        for bad in [
            "click 0 1\n",
            "click 1\n",
            "wheel sideways 1 1\n",
            "wheel up 1 1 many\n",
            "drag 1 1 2\n",
        ] {
            assert!(parse_script(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_newline_step_is_alt_enter() {
        let steps = parse_script("newline\n").expect("parses");
        assert!(matches!(
            steps[0],
            Step::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::ALT,
                ..
            })
        ));
    }

    /// The compose pane grows by a row per line, up to five, and a message
    /// longer than its block wraps at a space inside it.
    #[test]
    fn compose_grows_with_its_lines_and_a_long_message_wraps() {
        let mut model = Model::new(&[[0xab; 16]]);
        for name in ["tab", "tab", "a"] {
            update(&mut model, UiEvent::Key(parse_key(name).expect("key")));
        }
        for c in ['b', 'c', 'd', 'e', 'f', 'g'] {
            leviculum_ui::input::InputBuffer::newline(&mut model.compose);
            update(
                &mut model,
                UiEvent::Key(parse_key(&c.to_string()).expect("key")),
            );
        }
        let screen = render(&model);
        let rows: Vec<&str> = screen.lines().collect();
        let top = rows
            .iter()
            .position(|row| row.contains("● to Unknown"))
            .expect("the compose pane");
        // Five rows of text between the borders, the last five lines.
        let inner: Vec<&str> = rows[top + 1..top + 6]
            .iter()
            .map(|row| row.trim_matches(|c: char| c == '│' || c == ' '))
            .collect();
        assert_eq!(inner, ["c", "d", "e", "f", "g"], "{screen}");
        assert!(rows[top + 6].starts_with('└'), "{screen}");

        let mut model = Model::new(&[[0xab; 16]]);
        for name in ["tab", "tab"] {
            update(&mut model, UiEvent::Key(parse_key(name).expect("key")));
        }
        for c in "word ".repeat(20).chars() {
            update(
                &mut model,
                UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
            );
        }
        update(&mut model, UiEvent::Key(parse_key("enter").expect("key")));
        let screen = render(&model);
        // The conversation pane's side of each row, past the list.
        let block: Vec<String> = screen
            .lines()
            .map(|row| row.chars().skip(26).collect::<String>())
            .filter(|row| row.contains("word"))
            .collect();
        assert!(block.len() > 1, "{screen}");
        for row in &block {
            let inside = row.trim_matches(|c: char| c == '│' || c == ' ');
            assert!(inside.starts_with("word"), "{inside:?}");
            assert!(
                inside.ends_with("word") || inside.ends_with('◷'),
                "{inside:?}"
            );
        }
    }

    /// A send that failed both ways: at 80x24 the toast says it in plain
    /// words and where the why is, no leg (1279, walk finding 22); the legs'
    /// reasons are the detail view's. The frame pinned in
    /// `tests/fixtures/tui_toast_two_legs.frame`.
    #[test]
    fn a_two_leg_failure_toasts_plainly() {
        let failed = |lang| {
            let mut model = Model::new(&[[0xab; 16]]);
            model.lang = lang;
            for name in ["tab", "tab", "h", "i", "enter"] {
                update(&mut model, UiEvent::Key(parse_key(name).expect("key")));
            }
            let direct = SendError::Unreachable {
                destination: [0xab; 16],
                waited: Duration::from_secs(30),
            };
            let fallback = SendError::NoPropagationNode(
                "no propagation node: none has been heard".to_string(),
            );
            update(
                &mut model,
                UiEvent::SendDone(Err(format!("direct: {direct}\npropagated: {fallback}"))),
            );
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
            let view = ui::view(&model);
            terminal
                .draw(|frame| draw(frame, &view, None))
                .expect("draw");
            frame_text(terminal.backend().buffer())
        };
        let screen = failed(Lang::En);
        assert!(screen.contains(ui::NOT_SENT), "{screen}");
        assert!(!screen.contains("propagated"), "{screen}");
        // The send needed a relay and there is none: the header says so.
        assert!(screen.contains("no relay"), "{screen}");
        same_frames("tui_toast_two_legs.frame", &screen);
        let german = failed(Lang::De);
        assert!(!german.contains("propagated"), "{german}");
        says(&german, "du: hi");
        same_frames(&german_name("tui_toast_two_legs.frame"), &german);
    }

    /// Our position share's chip row (1273) at 80x24 and at 120x40: under
    /// the mailbox's row on the wide pane, at the top of the narrow one,
    /// the time left and that a click stops it; the frames pinned in
    /// `tests/fixtures/tui_share_chip.frames`. A click on the row stops
    /// the share with its end report, and the row goes.
    #[test]
    fn the_share_chip_row_says_the_time_left_and_a_click_stops_it() {
        use leviculum_lxmf::telemetry::Location;
        use loquax::ui::share::Span;
        let peer = [0xab; 16];
        let t = 1_793_500_000;
        let mut frames_in = Vec::new();
        for lang in Lang::ALL {
            let mut model = Model::new(&[peer]);
            model.lang = lang;
            let clock = |model: &mut Model, at: i64| {
                update(
                    model,
                    UiEvent::Clock(WallTime {
                        unix_secs: at,
                        offset_secs: 3600,
                    }),
                )
            };
            clock(&mut model, t);
            update(
                &mut model,
                UiEvent::OwnFix(Some(Location {
                    latitude_e6: 43_773_100,
                    longitude_e6: 11_256_000,
                    altitude_e2: 0,
                    speed_e2: 0,
                    bearing_e2: 0,
                    accuracy_e2: 500,
                    last_update: t,
                })),
            );
            update(
                &mut model,
                UiEvent::StartShare {
                    peer,
                    span: Span::Hour1,
                },
            );
            clock(&mut model, t + 18 * 60);
            let mut frames = String::new();
            for (w, h) in [(80u16, 24u16), (120, 40)] {
                update(
                    &mut model,
                    UiEvent::Resize {
                        width: w,
                        height: h,
                    },
                );
                let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
                let view = ui::view(&model);
                terminal
                    .draw(|frame| draw(frame, &view, None))
                    .expect("draw");
                let screen = frame_text(terminal.backend().buffer());
                if lang == Lang::En {
                    assert!(
                        screen.contains("⌖ Sharing your position, 42 min left"),
                        "{screen}"
                    );
                    assert_eq!(screen.contains("· click to stop"), w == 120, "{screen}");
                }
                frames.push_str(&format!("=== {w}x{h}\n{screen}"));
            }
            frames_in.push((model, frames));
        }
        let (_, german_frames) = frames_in.pop().expect("German");
        says(&german_frames, "du: Position geteilt");
        let (mut model, frames) = frames_in.pop().expect("English");
        same_frames("tui_share_chip.frames", &frames);
        same_frames(&german_name("tui_share_chip.frames"), &german_frames);
        // The click: on the chip row's first inner cell of the wide pane.
        let screen = ui::Screen::of(120, 40, 1);
        let row = screen.messages.y + 1 + model.chip_row().expect("a chip row") as u16;
        let col = screen.messages.x + 3;
        let mut effects = Vec::new();
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            effects.extend(update(
                &mut model,
                UiEvent::Mouse(MouseEvent {
                    kind,
                    column: col,
                    row,
                    modifiers: KeyModifiers::NONE,
                }),
            ));
        }
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, ui::UiEffect::Send { to, .. } if *to == peer)),
            "the end report: {effects:?}"
        );
        assert!(model.share(&peer).is_none());
        assert_eq!(model.chip_row(), None);
    }

    /// Recording a voice message in the terminal (1276): `Alt-v` in
    /// compose starts it locked and the compose box is the bar, the dot
    /// and the timer, the waveform in block characters, the keys; `p`
    /// pauses, and a microphone that heard nothing says so on the line
    /// under it; the cap stops it into the preview with its line. Each at
    /// 80 x 24 and 120 x 40, pinned in
    /// `tests/fixtures/tui_recording.frames`. Then the keys and the
    /// clicks: `v` sends, `Esc` and a click on `Esc cancel` throw it away,
    /// a click on the silence line asks for the microphone.
    #[test]
    fn the_recording_bar_takes_the_compose_box_and_its_keys() {
        use loquax::ui::recording::{rms, Effect, Input, Phase, WINDOW_MS};
        let peer = [0xab; 16];
        let key = |model: &mut Model, code: KeyCode, mods: KeyModifiers| {
            update(model, UiEvent::Key(KeyEvent::new(code, mods)))
        };
        let fresh = |lang| {
            let mut model = Model::new(&[peer]);
            model.lang = lang;
            model.names.remember(peer, "Anna Schulz");
            update(&mut model, UiEvent::Open { peer });
            update(&mut model, UiEvent::Elapsed(1_000));
            let _ = key(&mut model, KeyCode::Tab, KeyModifiers::NONE);
            let _ = key(&mut model, KeyCode::Tab, KeyModifiers::NONE);
            assert_eq!(model.focus, Pane::Compose);
            model
        };
        // `ms` of sound from 1000 on, a level a window: the speech's or
        // silence.
        let feed = |model: &mut Model, pcm: &[i16]| {
            let window = (loquax::voice::RATE / (1000 / WINDOW_MS)) as usize;
            let mut now = 1_000;
            for chunk in pcm.chunks(window) {
                now += u64::from(WINDOW_MS);
                update(
                    model,
                    UiEvent::Record {
                        peer,
                        input: Input::Level {
                            now,
                            rms: rms(chunk),
                        },
                    },
                );
            }
            now
        };
        let start = |model: &mut Model| {
            let effects = key(model, KeyCode::Char('v'), KeyModifiers::ALT);
            assert!(
                effects.iter().any(|e| matches!(
                    e,
                    ui::UiEffect::Recording {
                        effect: Effect::Start,
                        ..
                    }
                )),
                "{effects:?}"
            );
            assert!(matches!(model.recording.phase, Phase::Locked { .. }));
        };
        let speech = loquax::voice::test_speech(loquax::voice::RATE, 7_400);

        let three = |lang| {
            let mut locked = fresh(lang);
            let typed = key(&mut locked, KeyCode::Char('v'), KeyModifiers::NONE);
            assert!(
                typed.is_empty() && locked.compose.value() == "v",
                "a letter"
            );
            let _ = key(&mut locked, KeyCode::Backspace, KeyModifiers::NONE);
            start(&mut locked);
            feed(&mut locked, &speech);
            // Typing does not reach the hidden field.
            let _ = key(&mut locked, KeyCode::Char('x'), KeyModifiers::NONE);
            assert_eq!(locked.compose.value(), "");

            let mut paused = fresh(lang);
            start(&mut paused);
            feed(&mut paused, &vec![0; 16_000 * 3]);
            let _ = key(&mut paused, KeyCode::Char('p'), KeyModifiers::NONE);
            assert_eq!(paused.recording.phase, Phase::Paused);

            let mut capped = fresh(lang);
            start(&mut capped);
            let now = feed(&mut capped, &speech);
            let since = 1_000;
            update(
                &mut capped,
                UiEvent::Record {
                    peer,
                    input: Input::Tick {
                        now: now.max(since + u64::from(loquax::voice::LONGEST_MS)),
                    },
                },
            );
            assert_eq!(capped.recording.phase, Phase::Preview);

            let mut frames = String::new();
            for (name, model) in [
                ("locked", &mut locked),
                ("paused, nothing heard", &mut paused),
                ("preview at the cap", &mut capped),
            ] {
                for (w, h) in [(80u16, 24u16), (120, 40)] {
                    update(
                        model,
                        UiEvent::Resize {
                            width: w,
                            height: h,
                        },
                    );
                    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
                    let view = ui::view(model);
                    terminal
                        .draw(|frame| draw(frame, &view, None))
                        .expect("draw");
                    let screen = frame_text(terminal.backend().buffer());
                    frames.push_str(&format!("=== {name} {w}x{h}\n{screen}"));
                }
            }
            (locked, paused, frames)
        };
        let (mut locked, mut paused, frames) = three(Lang::En);
        for (want, state) in [
            ("● REC 00:07", "locked"),
            ("p pause · Enter send", "locked"),
            ("p pause · Esc cancel · Enter send", "locked, wide"),
            ("○ REC 00:03", "paused"),
            ("Space play · p resume · Esc cancel · Enter send", "paused"),
            ("Nothing heard. Is the microphone allowed?", "paused"),
            ("○ REC 10:00", "preview"),
            ("That is as long as a voice message can be", "preview"),
        ] {
            assert!(frames.contains(want), "{state}: {want:?}\n{frames}");
        }
        assert!(frames.contains('▇') && frames.contains('▁'), "{frames}");
        same_frames("tui_recording.frames", &frames);
        let (_, _, german) = three(Lang::De);
        says(&german, "Noch keine Nachrichten");
        same_frames(&german_name("tui_recording.frames"), &german);

        // ASCII: the same bar in ASCII alone.
        locked.ascii = true;
        let bar = ui::view(&locked).recording.expect("the bar");
        let row: String = bar.rows[0].cells.iter().map(|c| c.ch).collect();
        assert!(row.starts_with("* REC 00:07  "), "{row}");
        assert!(row.is_ascii(), "{row}");
        locked.ascii = false;

        // `v` sends what is recorded.
        let sent = key(&mut locked, KeyCode::Char('v'), KeyModifiers::NONE);
        assert!(
            sent.iter().any(|e| matches!(
                e,
                ui::UiEffect::Recording {
                    effect: Effect::Finish,
                    ..
                }
            )),
            "{sent:?}"
        );
        assert!(ui::view(&locked).recording.is_none(), "the field is back");
        // The shell hands the coded recording back: its bubble draws the
        // recording's own track as block characters.
        let track = locked.recording.track.clone();
        let data = loquax::voice::encode(&speech, loquax::voice::RATE).expect("coded");
        update(&mut locked, UiEvent::SendVoice { peer, data });
        let bars = track.bars(loquax::ui::voice::BARS, loquax::ui::recording::Fold::Peak);
        let wave = loquax::ui::layout::waveform(&bars, bars.len(), false, false);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
        let view = ui::view(&locked);
        terminal
            .draw(|frame| draw(frame, &view, None))
            .expect("draw");
        let screen = frame_text(terminal.backend().buffer());
        assert!(screen.contains("♪ voice message · 0:08"), "7.4 s: {screen}");
        assert!(screen.contains(&wave), "{wave}\n{screen}");

        // `Esc` throws it away, and stays in compose.
        let mut again = fresh(Lang::En);
        start(&mut again);
        feed(&mut again, &speech[..16_000]);
        let gone = key(&mut again, KeyCode::Esc, KeyModifiers::NONE);
        assert!(gone.iter().any(|e| matches!(
            e,
            ui::UiEffect::Recording {
                effect: Effect::Discard,
                ..
            }
        )));
        assert_eq!(again.focus, Pane::Compose);

        // A click on `Esc cancel` in the bar, and one on the silence line.
        let click = |model: &mut Model, col: u16, row: u16| {
            let mut effects = Vec::new();
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                effects.extend(update(
                    model,
                    UiEvent::Mouse(MouseEvent {
                        kind,
                        column: col,
                        row,
                        modifiers: KeyModifiers::NONE,
                    }),
                ));
            }
            effects
        };
        let screen = ui::Screen::of(120, 40, paused.compose_rows());
        let bar = ui::view(&paused).recording.expect("the bar");
        let place = |verb| {
            bar.buttons
                .iter()
                .find(|b| b.3 == verb)
                .map(|&(r, c, _, _)| {
                    (
                        screen.compose.x + 1 + c as u16,
                        screen.compose.y + 1 + r as u16,
                    )
                })
                .expect("the button")
        };
        use loquax::ui::recording::Verb;
        let (col, row) = place(Verb::AskMicrophone);
        let asked = click(&mut paused, col, row);
        assert!(
            asked.iter().any(|e| matches!(
                e,
                ui::UiEffect::Recording {
                    effect: Effect::AskMicrophone,
                    ..
                }
            )),
            "{asked:?}"
        );
        let (col, row) = place(Verb::Discard);
        let thrown = click(&mut paused, col, row);
        assert!(
            thrown.iter().any(|e| matches!(
                e,
                ui::UiEffect::Recording {
                    effect: Effect::Discard,
                    ..
                }
            )),
            "{thrown:?}"
        );
        assert!(ui::view(&paused).recording.is_none());
    }

    /// The first user in the terminal (1263, finding 6): the empty list
    /// names the palette's command, the palette offers `new conversation`
    /// and `add contact`, and a friend's code typed into either opens the
    /// conversation, `add contact` asking the name next. The three screens
    /// are pinned in `tests/fixtures/tui_palette_new.frames`.
    #[test]
    fn the_palette_starts_a_conversation_and_adds_a_contact() {
        let keys = |model: &mut Model, text: &str| {
            for ch in text.chars() {
                let name = match ch {
                    ' ' => "space".to_string(),
                    _ => ch.to_string(),
                };
                update(model, UiEvent::Key(parse_key(&name).expect("key")));
            }
        };
        let key = |model: &mut Model, name: &str| {
            update(model, UiEvent::Key(parse_key(name).expect("key")));
        };
        let walk = |lang| {
            let en = lang == Lang::En;
            let mut model = Model::new(&[]);
            model.lang = lang;
            update(
                &mut model,
                UiEvent::Resize {
                    width: 80,
                    height: 16,
                },
            );
            let empty = screen(&model, 80, 16);
            assert!(
                empty.contains(if en {
                    ":new conversation"
                } else {
                    "Noch keine"
                }),
                "{empty}"
            );
            assert!(!empty.contains("--to"), "{empty}");

            key(&mut model, ":");
            let listed = screen(&model, 80, 16);
            key(&mut model, "esc");
            key(&mut model, ":");
            keys(&mut model, "new conv");
            key(&mut model, "enter");
            let asking = screen(&model, 80, 16);
            assert!(
                !en || asking.contains("Your contact's code or address"),
                "{asking}"
            );

            // An old code: the address alone, in capitals.
            let bert = loquax::address::to_hex(&[0xbe; 16]).to_ascii_uppercase();
            keys(&mut model, &bert);
            key(&mut model, "enter");
            assert_eq!(
                model.conversations.get(model.selected).map(|c| c.peer),
                Some([0xbe; 16])
            );
            assert_eq!(model.focus, Pane::Compose);

            // Garbage says so in the phone's sentence and opens nothing.
            key(&mut model, "esc");
            key(&mut model, ":");
            keys(&mut model, "new conv");
            key(&mut model, "enter");
            keys(&mut model, "hello");
            key(&mut model, "enter");
            assert_eq!(model.conversations.len(), 1);
            assert!(
                !en || model.toasts.last().map(|t| t.text.as_str())
                    == Some("That is not a Loquax code."),
                "{:?}",
                model.toasts.last()
            );

            // A contact code: the conversation, then its name asked.
            let private: [u8; 64] = core::array::from_fn(|i| i as u8 + 1);
            let key_bytes = leviculum_core::Identity::from_private_key_bytes(&private)
                .expect("a key")
                .public_key_bytes();
            let code = loquax::lxma::encode(&key_bytes).expect("a code");
            let peer = loquax::home::lxmf_address(&key_bytes).expect("an address");
            key(&mut model, ":");
            keys(&mut model, "add contact");
            key(&mut model, "enter");
            keys(&mut model, &code);
            key(&mut model, "enter");
            assert_eq!(
                model.conversations.get(model.selected).map(|c| c.peer),
                Some(peer)
            );
            let naming = screen(&model, 80, 16);
            assert!(!en || naming.contains("your name for"), "{naming}");
            keys(&mut model, "Carol");
            key(&mut model, "enter");
            assert_eq!(model.names.petname(&peer), Some("Carol"));

            format!(
                "--- empty\n{empty}--- palette\n{listed}--- asking\n{asking}--- naming\n{naming}"
            )
        };
        same_frames("tui_palette_new.frames", &walk(Lang::En));
        let german = walk(Lang::De);
        says(&german, "Code oder Adresse deines Kontakts");
        same_frames(&german_name("tui_palette_new.frames"), &german);
    }

    /// 1279, 1443: `P` opens the profile, the scene's, as a tap on the
    /// avatar does on the phone: the name, the address in fours, the
    /// mailbox and how it is reached, the devices, the contact code and
    /// its QR, the modules the window draws (`verbatim_modules`), which a
    /// phone's scanner reads as that code. At 100x60 the whole QR is on
    /// screen; pinned in `tests/fixtures/tui_profile.frames`. Esc closes
    /// it, and a click on the list's title, the reader's own, opens it.
    #[test]
    fn the_profile_shows_the_name_the_address_and_the_code_and_its_qr() {
        use loquax::ui::profile::{Device, Mailbox, ProfileFacts, Rung};
        let private: [u8; 64] = core::array::from_fn(|i| i as u8 + 1);
        let key_bytes = leviculum_core::Identity::from_private_key_bytes(&private)
            .expect("a key")
            .public_key_bytes();
        let code = loquax::lxma::encode(&key_bytes).expect("a code");
        let qr = loquax::home::qr::verbatim_modules(&code).expect("a QR");
        let (width, height) = (100, 60);
        let profile_in = |lang| {
            let mut model = Model::new(&[[0xab; 16]]);
            model.lang = lang;
            model.address = Some([0x4c; 16]);
            model.own_code = Some(ui::OwnCode::of(code.clone(), Some(qr.clone())));
            model.profile = ProfileFacts {
                name: "Mia Sander".to_string(),
                mailbox: Mailbox::Away {
                    name: Some("Mia's computer".to_string()),
                    rung: Some(Rung::Relayed),
                    word: Some(loquax::home::board::Word::Connected),
                },
                devices: vec![
                    Device {
                        name: "Laptop".to_string(),
                        this: true,
                    },
                    Device {
                        name: "Pixel 8".to_string(),
                        this: false,
                    },
                ],
                ..Default::default()
            };
            update(&mut model, UiEvent::Resize { width, height });
            update(&mut model, UiEvent::Key(parse_key("P").expect("key")));
            model
        };
        let mut model = profile_in(Lang::En);
        let shown = screen(&model, width, height);
        assert!(shown.contains(" Your profile "), "{shown}");
        for words in [
            "Mia Sander",
            "4c4c 4c4c 4c4c 4c4c 4c4c 4c4c 4c4c 4c4c",
            "Mailbox: Mia's computer (connected)",
            "Connected over a relay server",
            "Laptop (this device)",
            "Pixel 8",
            "s  Share my address",
        ] {
            assert!(shown.contains(words), "{words}: {shown}");
        }
        // The code wraps at the box's width; its pieces are all there.
        let flat: String = shown
            .lines()
            .map(|row| row.trim_matches(|c| c == '│' || c == ' '))
            .collect();
        assert!(flat.contains(&code), "{shown}");
        let node = loquax::ui::profile::QrNode {
            side: qr.0,
            modules: qr.1.clone(),
        };
        for row in node.half_blocks(4) {
            assert!(shown.contains(row.as_str()), "the QR row {row:?}: {shown}");
        }
        update(&mut model, UiEvent::Key(parse_key("esc").expect("key")));
        assert!(!screen(&model, width, height).contains(" Your profile "));

        same_frames("tui_profile.frames", &shown);
        let german = screen(&profile_in(Lang::De), width, height);
        says(&german, "Mia Sander");
        says(&german, "Dein Profil");
        says(&german, "Postfach: Mia's computer (verbunden)");
        same_frames(&german_name("tui_profile.frames"), &german);

        // The click on the list's title, its third cell: the profile.
        update(
            &mut model,
            UiEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 3,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
        );
        assert!(model.profile_open, "a click on the title opens it");
    }

    #[test]
    fn the_help_and_the_palette_draw_over_the_panes() {
        let mut model = Model::new(&[[0xab; 16]]);
        update(&mut model, UiEvent::Key(parse_key("?").expect("key")));
        assert!(render(&model).contains("Loquax help"));
        update(&mut model, UiEvent::Key(parse_key("esc").expect("key")));
        update(&mut model, UiEvent::Key(parse_key(":").expect("key")));
        let screen = render(&model);
        assert!(screen.contains("commands"), "{screen}");
        assert!(screen.contains("▸ send"), "{screen}");
        assert!(screen.contains("focus compose"), "{screen}");
    }

    /// The first help row on a `width` x `height` screen: the help's drawn
    /// rows ([`leviculum_ui::help::rows`]) matched against the overlay's
    /// inside.
    fn help_shown_from(model: &mut Model, width: u16, height: u16) -> usize {
        update(model, UiEvent::Resize { width, height });
        let frame = screen(model, width, height);
        let rows: Vec<&str> = frame.lines().collect();
        let title = rows
            .iter()
            .position(|row| row.contains("Loquax help"))
            .expect("the help is drawn");
        let inner: Vec<String> = rows[title + 1..]
            .iter()
            .take_while(|row| !row.contains('└'))
            .map(|row| {
                row.trim()
                    .trim_matches(|c| matches!(c, '│' | '█' | '║' | '▐'))
                    .trim()
                    .to_string()
            })
            .collect();
        assert!(!inner.is_empty(), "{frame}");
        let lines = leviculum_ui::help::lines(&help_groups());
        let all: Vec<String> =
            leviculum_ui::help::rows(&help_groups(), leviculum_ui::Rect::new(0, 0, width, height))
                .into_iter()
                .map(|row| {
                    lines[row.line]
                        .chars()
                        .skip(row.chars.start)
                        .take(row.chars.len())
                        .collect::<String>()
                        .trim()
                        .to_string()
                })
                .collect();
        (0..all.len())
            .find(|&at| {
                all.len() - at >= inner.len()
                    && all[at..].iter().zip(&inner).all(|(want, got)| got == want)
            })
            .unwrap_or_else(|| panic!("no help window matches\n{frame}"))
    }

    /// Lew's phone over mosh (1095): a short screen shows part of the help,
    /// and the shared scroll keys and the wheel move through the rest, both
    /// ends clamped, a resize keeping the offset valid.
    #[test]
    fn the_help_scrolls_on_a_short_screen() {
        let mut model = Model::new(&[[0xab; 16]]);
        let (width, height) = (80, 20);
        update(&mut model, UiEvent::Resize { width, height });
        update(&mut model, UiEvent::Key(parse_key("?").expect("key")));
        let (_, max) = leviculum_ui::help::scroll_bounds(
            &help_groups(),
            leviculum_ui::Rect::new(0, 0, width, height),
        );
        assert!(max > 2, "the help overflows 80 x 20");
        let at = |model: &mut Model| help_shown_from(model, width, height);
        let press = |model: &mut Model, k: &str| {
            update(model, UiEvent::Key(parse_key(k).expect("key")));
        };
        let wheel = |model: &mut Model, kind: MouseEventKind| {
            update(
                model,
                UiEvent::Mouse(MouseEvent {
                    kind,
                    column: width / 2,
                    row: height / 2,
                    modifiers: KeyModifiers::NONE,
                }),
            );
        };
        assert_eq!(at(&mut model), 0, "the help opens at the top");
        press(&mut model, "k");
        assert_eq!(at(&mut model), 0, "clamped at the top");
        press(&mut model, "down");
        assert_eq!(at(&mut model), 1);
        press(&mut model, "j");
        assert_eq!(at(&mut model), 2);
        press(&mut model, "up");
        assert_eq!(at(&mut model), 1);
        press(&mut model, "k");
        assert_eq!(at(&mut model), 0);
        wheel(&mut model, MouseEventKind::ScrollDown);
        let wheeled = at(&mut model);
        assert!(wheeled > 0, "the wheel scrolls the help");
        wheel(&mut model, MouseEventKind::ScrollUp);
        assert_eq!(at(&mut model), 0);
        press(&mut model, "pagedown");
        let page = at(&mut model);
        assert!(page > 2, "a page is more than a line");
        press(&mut model, "pageup");
        assert_eq!(at(&mut model), 0);
        press(&mut model, "ctrl-v");
        assert_eq!(at(&mut model), page.min(max));
        press(&mut model, "alt-v");
        assert_eq!(at(&mut model), 0);
        press(&mut model, "end");
        assert_eq!(at(&mut model), max);
        press(&mut model, "j");
        assert_eq!(at(&mut model), max, "clamped at the bottom");
        press(&mut model, "home");
        assert_eq!(at(&mut model), 0);
        press(&mut model, "G");
        assert_eq!(at(&mut model), max);
        assert!(model.help, "no scroll key closes the help");
        // Taller, the bottom comes nearer: the offset follows it, so `k`
        // moves at once instead of working off rows that no longer exist.
        let taller = height + 6;
        update(
            &mut model,
            UiEvent::Resize {
                width,
                height: taller,
            },
        );
        let (_, max_taller) = leviculum_ui::help::scroll_bounds(
            &help_groups(),
            leviculum_ui::Rect::new(0, 0, width, taller),
        );
        assert_eq!(help_shown_from(&mut model, width, taller), max_taller);
        press(&mut model, "k");
        assert_eq!(
            help_shown_from(&mut model, width, taller),
            max_taller.saturating_sub(1)
        );
        press(&mut model, "g");
        assert_eq!(help_shown_from(&mut model, width, taller), 0);
        // Closed and opened again, it starts at the top.
        press(&mut model, "end");
        press(&mut model, "esc");
        press(&mut model, "?");
        assert_eq!(help_shown_from(&mut model, width, taller), 0);
    }

    use leviculum_lxmf::router::MessageState;
    use loquax::outbox::OutboxEvent;

    /// The 80 x 24 mockup's story with Alice and Bob: three days with Alice,
    /// both sides, a mailbox and a sent-unproven mark, Bob opened while Alice
    /// writes twice, Alice opened again at her `2 new`, and a reply on its
    /// way.
    fn mockup(width: u16, height: u16) -> Model {
        mockup_in(Lang::En, width, height)
    }

    /// The mockup's story in `lang`, the language set first as the
    /// terminal sets the locale's.
    fn mockup_in(lang: Lang, width: u16, height: u16) -> Model {
        let alice = [0x4f; 16];
        let bob = [0xb7; 16];
        let mut model = Model::new(&[alice, bob]);
        model.lang = lang;
        update(&mut model, UiEvent::Resize { width, height });
        // Named as the mockup names them: two nameless peers would collide
        // and say so in the header (1452), which is its own golden.
        for (peer, name) in [(alice, "Alice"), (bob, "Bob")] {
            update(
                &mut model,
                UiEvent::Engine(OutboxEvent::Announced {
                    destination: peer,
                    name: Some(name.to_string()),
                    path_response: false,
                }),
            );
        }
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::Ready {
                address: [0xaf; 16],
            }),
        );
        update(
            &mut model,
            UiEvent::MailboxChosen {
                node: [0x9c; 16],
                by_user: false,
            },
        );
        let clock = |m: &mut Model, d, h, mi| {
            update(
                m,
                UiEvent::Clock(WallTime::from_civil(2026, 10, d, h, mi, 3600)),
            );
        };
        let mut n = 0u8;
        let mut recv = |m: &mut Model, from: [u8; 16], body: &str| {
            n += 1;
            update(
                m,
                UiEvent::Engine(OutboxEvent::Received {
                    sent_at: 0,
                    message_id: [n; 32],
                    source: from,
                    title: Vec::new(),
                    body: body.as_bytes().to_vec(),
                    fields: Vec::new(),
                    verified: true,
                }),
            );
        };
        let send = |m: &mut Model, body: &str, id: u8, state: MessageState| {
            m.focus = Pane::Compose;
            for c in body.chars() {
                update(
                    m,
                    UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                );
            }
            update(
                m,
                UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            );
            update(
                m,
                UiEvent::Engine(OutboxEvent::Queued {
                    message_id: [id; 32],
                }),
            );
            update(
                m,
                UiEvent::Engine(OutboxEvent::State {
                    message_id: [id; 32],
                    state,
                }),
            );
            update(m, UiEvent::SendDone(Ok(())));
        };
        clock(&mut model, 23, 19, 40);
        recv(&mut model, alice, "did the board come back from the soak?");
        clock(&mut model, 23, 19, 52);
        send(
            &mut model,
            "yes, 100 % over the night, log is on the share",
            0xa1,
            MessageState::Delivered,
        );
        clock(&mut model, 24, 13, 58);
        recv(&mut model, alice, "are you at the field site tomorrow? Dana says the north gate is locked after six, so the car park by the road it is.");
        clock(&mut model, 24, 14, 2);
        send(&mut model, "yes, from ten. Shall I bring the T114 and the long antenna, or is the stick enough for the hill?", 0xa2, MessageState::Delivered);
        clock(&mut model, 24, 14, 3);
        recv(&mut model, alice, "long one, the hill eats signal");
        clock(&mut model, 26, 9, 12);
        send(
            &mut model,
            "on my way",
            0xa3,
            MessageState::AwaitingCollection,
        );
        clock(&mut model, 26, 9, 31);
        send(
            &mut model,
            "stuck at the level crossing",
            0xa4,
            MessageState::Sent,
        );
        clock(&mut model, 26, 12, 40);
        recv(&mut model, bob, "the antenna is in the van");
        // Bob is opened, Alice writes meanwhile.
        model.focus = Pane::Conversations;
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)),
        );
        clock(&mut model, 26, 14, 4);
        recv(&mut model, alice, "ok, bring the T114.");
        clock(&mut model, 26, 14, 5);
        recv(
            &mut model,
            alice,
            "and the small tripod\nif it fits in the bag",
        );
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)),
        );
        clock(&mut model, 26, 14, 6);
        send(&mut model, "the tripod fits", 0xa5, MessageState::Sending);
        model.toasts.clear();
        model
    }

    fn screen(model: &Model, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let view = ui::view(model);
        terminal
            .draw(|frame| draw(frame, &view, None))
            .expect("draw");
        frame_text(terminal.backend().buffer())
    }

    /// The two mockups of section 5 drawn from a model: the same elements in
    /// the same rows (the text differs), then the cursor on a sent message
    /// in the conversation, its detail view, and the narrow fallback. Pinned
    /// in `tests/fixtures/tui_layout.frames`.
    #[test]
    fn the_layout_draws_the_mockups_structure_at_both_sizes() {
        for lang in Lang::ALL {
            let mut frames = String::new();
            for (width, height) in [(80, 24), (120, 40)] {
                let model = mockup_in(lang, width, height);
                let shot = screen(&model, width, height);
                frames.push_str(&format!("--- {width} x {height} ---\n{shot}"));
                if lang == Lang::De {
                    continue;
                }
                let rows: Vec<&str> = shot.lines().collect();
                // The list: Alice on top with the reply's time and the bar, Bob
                // under her; the header without a trust word for the default
                // state (1279).
                assert!(rows[1].starts_with("│▌Alice "), "{shot}");
                assert!(rows[2].starts_with("│▌you: the tripod fits"), "{shot}");
                assert!(rows[4].starts_with("│ Bob "), "{shot}");
                assert!(rows[0].contains("Alice──"), "{shot}");
                assert!(!rows[0].contains("unknown"), "{shot}");
                // The rule, the date line, ours right, theirs left.
                assert!(shot.contains("── Today ──"), "{shot}");
                assert!(shot.contains(" 2 new "), "{shot}");
                assert!(shot.contains("│ ok, bring the T114.  14:04 "), "{shot}");
                assert!(shot.contains(" the tripod fits  14:06 ⠋  │"), "{shot}");
                assert!(shot.contains("to Alice"), "{shot}");
            }
            let mut model = mockup_in(lang, 80, 24);
            model.focus = Pane::Messages;
            for _ in 0..6 {
                update(&mut model, UiEvent::Key(parse_key("k").expect("key")));
            }
            frames.push_str(&format!("--- cursor ---\n{}", screen(&model, 80, 24)));
            update(&mut model, UiEvent::Key(parse_key("enter").expect("key")));
            let detail = screen(&model, 80, 24);
            assert!(
                lang == Lang::De || detail.contains("arrived: proof received"),
                "{detail}"
            );
            frames.push_str(&format!("--- detail ---\n{detail}"));
            let model = mockup_in(lang, 60, 20);
            frames.push_str(&format!(
                "--- 60 x 20, narrow ---\n{}",
                screen(&model, 60, 20)
            ));
            match lang {
                Lang::En => same_frames("tui_layout.frames", &frames),
                Lang::De => {
                    says(&frames, "ein Relais");
                    same_frames(&german_name("tui_layout.frames"), &frames)
                }
            }
        }
    }

    /// 1529 drawn: a short message, one over three screens long, a short
    /// one, at 80 x 24 and the messages pane focused. At the top the long
    /// one's start; twenty Downs later its middle, the selection's bar on
    /// it; one Right a screen less a line further; the way back up from the
    /// newest in its end. Pinned in `tests/fixtures/tui_long_message.frames`.
    #[test]
    fn a_long_message_is_read_by_the_line() {
        let alice = [0x4f; 16];
        for lang in Lang::ALL {
            let mut model = Model::new(&[alice]);
            model.lang = lang;
            update(
                &mut model,
                UiEvent::Resize {
                    width: 80,
                    height: 24,
                },
            );
            update(
                &mut model,
                UiEvent::Clock(WallTime::from_civil(2026, 10, 26, 9, 0, 3600)),
            );
            let long: Vec<String> = (1..=70)
                .map(|n| format!("line {n} of the long one"))
                .collect();
            for (n, body) in ["short before", &long.join("\n"), "short after"]
                .into_iter()
                .enumerate()
            {
                update(
                    &mut model,
                    UiEvent::Engine(OutboxEvent::Received {
                        sent_at: 0,
                        message_id: [n as u8 + 1; 32],
                        source: alice,
                        title: Vec::new(),
                        body: body.as_bytes().to_vec(),
                        fields: Vec::new(),
                        verified: true,
                    }),
                );
            }
            model.toasts.clear();
            let press = |model: &mut Model, name: &str, times: usize| {
                for _ in 0..times {
                    update(model, UiEvent::Key(parse_key(name).expect("key")));
                }
            };
            press(&mut model, "tab", 1);
            let mut frames = String::new();
            press(&mut model, "home", 1);
            frames.push_str(&format!("--- top ---\n{}", screen(&model, 80, 24)));
            press(&mut model, "down", 20);
            let mid = screen(&model, 80, 24);
            assert!(mid.contains("line 20 of the long one"), "{mid}");
            assert!(!mid.contains("short before"), "{mid}");
            frames.push_str(&format!("--- twenty down ---\n{mid}"));
            press(&mut model, "right", 1);
            frames.push_str(&format!("--- right ---\n{}", screen(&model, 80, 24)));
            press(&mut model, "end", 1);
            press(&mut model, "up", 3);
            let back = screen(&model, 80, 24);
            assert!(back.contains("line 70 of the long one"), "{back}");
            frames.push_str(&format!("--- back up ---\n{back}"));
            match lang {
                Lang::En => same_frames("tui_long_message.frames", &frames),
                Lang::De => {
                    says(&frames, "Unbekannt");
                    same_frames(&german_name("tui_long_message.frames"), &frames)
                }
            }
        }
    }

    /// Reactions (1104), drawn from the model at 80 x 24: a bubble of ours
    /// with two reactions under it, one sent as Columba sends it and one as
    /// Ratspeak 1.0.25 did, its fallback text drawn nowhere; then the
    /// picker over the peer's message, and the reaction it sent marked as
    /// ours. Pinned in `tests/fixtures/tui_reactions.frames`.
    #[test]
    fn reactions_draw_under_the_bubble_and_the_picker() {
        use leviculum_lxmf::constants::FIELD_REACTION;
        use leviculum_lxmf::reaction::Reaction;
        let alice = [0x4f; 16];
        for lang in Lang::ALL {
            let en = lang == Lang::En;
            let mut model = Model::new(&[alice]);
            model.lang = lang;
            update(
                &mut model,
                UiEvent::Resize {
                    width: 80,
                    height: 24,
                },
            );
            update(
                &mut model,
                UiEvent::Clock(WallTime::from_civil(2026, 10, 29, 12, 1, 3600)),
            );
            let recv = |m: &mut Model, id: u8, body: &str, fields: Vec<(i64, Vec<u8>)>| {
                update(
                    m,
                    UiEvent::Engine(OutboxEvent::Received {
                        sent_at: 0,
                        message_id: [id; 32],
                        source: alice,
                        title: Vec::new(),
                        body: body.as_bytes().to_vec(),
                        fields,
                        verified: true,
                    }),
                );
            };
            let reaction = |to: u8, content: &str| {
                let value = Reaction {
                    to: [to; 32],
                    content: content.into(),
                }
                .encode_field_value();
                vec![(FIELD_REACTION, value)]
            };
            recv(&mut model, 1, "lunch at noon?", Vec::new());
            model.focus = Pane::Compose;
            for c in "yes, the usual place".chars() {
                update(
                    &mut model,
                    UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                );
            }
            update(
                &mut model,
                UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            );
            update(
                &mut model,
                UiEvent::Engine(OutboxEvent::Queued {
                    message_id: [0xa1; 32],
                }),
            );
            update(
                &mut model,
                UiEvent::Engine(OutboxEvent::State {
                    message_id: [0xa1; 32],
                    state: MessageState::Delivered,
                }),
            );
            update(&mut model, UiEvent::SendDone(Ok(())));
            recv(&mut model, 2, "", reaction(0xa1, "\u{1F44D}"));
            recv(
                &mut model,
                3,
                "Reacted to your message with \u{2764}\u{FE0F}.",
                reaction(0xa1, "\u{2764}\u{FE0F}"),
            );
            model.toasts.clear();
            let mut frames = String::new();
            let two = screen(&model, 80, 24);
            assert!(!two.contains("Reacted"), "{two}");
            frames.push_str(&format!("--- two reactions ---\n{two}"));
            model.focus = Pane::Messages;
            update(&mut model, UiEvent::Key(parse_key("k").expect("key")));
            update(&mut model, UiEvent::Key(parse_key("e").expect("key")));
            let picker = screen(&model, 80, 24);
            assert!(
                !en || picker.contains("react to: lunch at noon?"),
                "{picker}"
            );
            frames.push_str(&format!("--- picker ---\n{picker}"));
            let effects = update(&mut model, UiEvent::Key(parse_key("1").expect("key")));
            assert!(
                // The reaction and the read-receipt mark (1230).
                matches!(effects.as_slice(), [loquax::ui::UiEffect::Send { fields, .. }]
                if fields.len() == 3 && fields[0].0 == FIELD_REACTION),
                "{effects:?}"
            );
            model.toasts.clear();
            frames.push_str(&format!("--- ours ---\n{}", screen(&model, 80, 24)));
            match lang {
                Lang::En => same_frames("tui_reactions.frames", &frames),
                Lang::De => {
                    says(&frames, "reagiert");
                    same_frames(&german_name("tui_reactions.frames"), &frames)
                }
            }
        }
    }

    /// A photo, a file and a voice message without words (1268), drawn
    /// from the model at 80 x 24: each its media line alone in its block
    /// with its time, no empty row; then one of ours, the photo with its
    /// mark closing the row, and one with words, the media above them.
    /// Pinned in `tests/fixtures/tui_media.frames`.
    #[test]
    fn media_without_words_is_its_line_alone() {
        let alice = [0x4f; 16];
        for lang in Lang::ALL {
            let en = lang == Lang::En;
            let mut model = Model::new(&[alice]);
            model.lang = lang;
            update(
                &mut model,
                UiEvent::Resize {
                    width: 80,
                    height: 24,
                },
            );
            let recv = |m: &mut Model, id: u8, minute: u32, body: &str, fields| {
                update(
                    m,
                    UiEvent::Clock(WallTime::from_civil(2026, 10, 29, 12, minute, 3600)),
                );
                update(
                    m,
                    UiEvent::Engine(OutboxEvent::Received {
                        sent_at: 0,
                        message_id: [id; 32],
                        source: alice,
                        title: Vec::new(),
                        body: body.as_bytes().to_vec(),
                        fields,
                        verified: true,
                    }),
                );
            };
            recv(
                &mut model,
                1,
                1,
                "",
                loquax::ui::photos::photo_fields("jpg", vec![7; 312_400]),
            );
            recv(
                &mut model,
                2,
                11,
                "",
                loquax::ui::files::file_fields("report.pdf", vec![7; 1_400]),
            );
            recv(
                &mut model,
                3,
                21,
                "",
                loquax::ui::voice::voice_fields(loquax::ui::voice::AM_OPUS_OGG, vec![7; 900]),
            );
            model.toasts.clear();
            let mut frames = String::new();
            let alone = screen(&model, 80, 24);
            assert!(!en || alone.contains("▣ photo · 313 KB"), "{alone}");
            assert!(!en || alone.contains("▤ report.pdf · 2 KB"), "{alone}");
            assert!(!en || alone.contains("♪ voice message"), "{alone}");
            frames.push_str(&format!("--- photo, file, voice alone ---\n{alone}"));
            update(
                &mut model,
                UiEvent::Clock(WallTime::from_civil(2026, 10, 29, 12, 31, 3600)),
            );
            update(
                &mut model,
                UiEvent::SendPhoto {
                    peer: alice,
                    format: "jpg".into(),
                    data: vec![0xff, 0xd8, 0xff],
                },
            );
            recv(
                &mut model,
                4,
                41,
                "the view from the hill",
                loquax::ui::photos::photo_fields("jpg", vec![7; 1_000]),
            );
            model.toasts.clear();
            frames.push_str(&format!(
                "--- ours, then words under a photo ---\n{}",
                screen(&model, 80, 24)
            ));
            match lang {
                Lang::En => same_frames("tui_media.frames", &frames),
                Lang::De => {
                    says(&frames, "Sprachnachricht");
                    same_frames(&german_name("tui_media.frames"), &frames)
                }
            }
        }
    }

    /// The search of section 5's 120 x 40 mockup, drawn from the model at
    /// both sizes: the field and the messages group as `tripod` is typed,
    /// the third result opened with its match marked in place, both groups
    /// for a query that is a name and a word of a message, and the list back
    /// after `Esc`. Pinned in `tests/fixtures/tui_search.frames`.
    #[test]
    fn the_search_draws_the_mockups_structure_at_both_sizes() {
        let keys = |model: &mut Model, names: &[&str]| {
            for name in names {
                update(model, UiEvent::Key(parse_key(name).expect("key")));
            }
        };
        let typed = |model: &mut Model, text: &str| {
            for c in text.chars() {
                update(
                    model,
                    UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                );
            }
        };
        for lang in Lang::ALL {
            let en = lang == Lang::En;
            let mut frames = String::new();
            for (width, height) in [(120, 40), (80, 24)] {
                let mut model = mockup_in(lang, width, height);
                update(
                    &mut model,
                    UiEvent::Engine(OutboxEvent::Received {
                        sent_at: 0,
                        message_id: [0x7b; 32],
                        source: [0xb7; 16],
                        title: Vec::new(),
                        body: b"the tripod is with 4f4f4f4f".to_vec(),
                        fields: Vec::new(),
                        verified: true,
                    }),
                );
                model.toasts.clear();
                keys(&mut model, &["esc", "esc", "/"]);
                typed(&mut model, "tripod");
                let shot = screen(&model, width, height);
                assert!(!en || shot.contains("● Search"), "{shot}");
                assert!(!en || shot.contains(" esc closes "), "{shot}");
                assert!(shot.contains("│ / tripod"), "{shot}");
                assert!(!en || shot.contains("│ MESSAGES"), "{shot}");
                assert!(!en || shot.contains("Esc close search"), "{shot}");
                frames.push_str(&format!("--- {width} x {height}: typed ---\n{shot}"));
                keys(&mut model, &["tab", "j", "j", "enter"]);
                let shot = screen(&model, width, height);
                assert!(shot.contains("▌and the small «tripod»"), "{shot}");
                assert!(shot.contains(" the «tripod» fits "), "{shot}");
                frames.push_str(&format!("--- {width} x {height}: opened ---\n{shot}"));
                keys(&mut model, &["/"]);
                for _ in 0.."tripod".len() {
                    keys(&mut model, &["backspace"]);
                }
                typed(&mut model, "4f4f");
                let shot = screen(&model, width, height);
                assert!(!en || shot.contains("│ CONVERSATIONS"), "{shot}");
                assert!(!en || shot.contains("│ MESSAGES"), "{shot}");
                // Alice by her address, which her row no longer shows (1452),
                // and the message that names it.
                assert!(shot.contains("«4f4f»"), "{shot}");
                frames.push_str(&format!("--- {width} x {height}: both groups ---\n{shot}"));
                keys(&mut model, &["esc"]);
                let shot = screen(&model, width, height);
                assert!(!en || shot.contains(" / search "), "{shot}");
                assert!(!shot.contains('«'), "{shot}");
                frames.push_str(&format!("--- {width} x {height}: closed ---\n{shot}"));
            }
            match lang {
                Lang::En => same_frames("tui_search.frames", &frames),
                Lang::De => {
                    says(&frames, "ein Relais");
                    same_frames(&german_name("tui_search.frames"), &frames)
                }
            }
        }
    }

    /// The text a frame shows in `cells`, in order.
    fn under(frame: &str, cells: &[(u16, u16)]) -> String {
        let rows: Vec<Vec<char>> = frame.lines().map(|row| row.chars().collect()).collect();
        cells
            .iter()
            .map(|&(col, row)| {
                rows.get(usize::from(row))
                    .and_then(|r| r.get(usize::from(col)))
                    .copied()
                    .unwrap_or(' ')
            })
            .collect()
    }

    /// The hit boxes as the drawn frame shows them, at both mockup sizes:
    /// the cells the model calls `send` read `send` on the compose box's
    /// border, the mailbox state's read the mailbox words, a badge's its
    /// count and a mark's its glyph.
    #[test]
    fn the_hit_boxes_are_the_cells_of_their_words() {
        use loquax::ui::hit::{target_at, Target};
        for (width, height) in [(80, 24), (120, 40)] {
            let mut model = mockup(width, height);
            // Bob's conversation gets a badge.
            update(
                &mut model,
                UiEvent::Engine(OutboxEvent::Received {
                    sent_at: 0,
                    message_id: [0x77; 32],
                    source: [0xb7; 16],
                    title: Vec::new(),
                    body: b"one for the badge".to_vec(),
                    fields: Vec::new(),
                    verified: true,
                }),
            );
            model.toasts.clear();
            let shot = screen(&model, width, height);
            let mut map: Vec<(Target, Vec<(u16, u16)>)> = Vec::new();
            for row in 0..height {
                for col in 0..width {
                    let target = target_at(&model, col, row);
                    match map.iter_mut().find(|(t, _)| *t == target) {
                        Some((_, cells)) => cells.push((col, row)),
                        None => map.push((target, vec![(col, row)])),
                    }
                }
            }
            let of = |want: &dyn Fn(&Target) -> bool| -> Vec<(u16, u16)> {
                map.iter()
                    .filter(|(t, _)| want(t))
                    .flat_map(|(_, cells)| cells.clone())
                    .collect()
            };
            let send = of(&|t| *t == Target::Send);
            assert_eq!(under(&shot, &send), "send", "{width}: {shot}");
            assert!(send.iter().all(|&(_, row)| row == send[0].1));
            let mailbox = of(&|t| *t == Target::Mailbox);
            // With no trust word for an unnamed peer (1279) the header has
            // room for the node and how at both sizes.
            assert_eq!(under(&shot, &mailbox), "a relay adopted", "{width}: {shot}");
            let badge = of(&|t| matches!(t, Target::Badge(_)));
            assert_eq!(under(&shot, &badge), "[1]", "{width}: {shot}");
            let marks = of(&|t| matches!(t, Target::Block { mark: true, .. }));
            let glyphs = under(&shot, &marks);
            assert!(!glyphs.is_empty(), "{width}: {shot}");
            assert!(
                glyphs.chars().all(|c| "◷✓✔! .*vV⠋⠙⠹⠸⠼⠴".contains(c)),
                "{width}: {glyphs:?}\n{shot}"
            );
        }
    }

    /// A message held from a direct delivery, older than the window the
    /// restart loads, arriving again from the mailbox after the restart
    /// (leviculum 1070, the sender's fallback of 1069): one row in the
    /// store, no second block on the screen. The screen's own check sees
    /// only what is loaded; the store's sees every row.
    #[test]
    fn a_mailbox_copy_of_a_message_below_the_loaded_window_is_not_kept_twice() {
        use loquax::ui::history::LOAD_WINDOW;
        use std::collections::BTreeMap;

        let us = [0x01; 16];
        let alice = [0xa1; 16];
        let direct = [0xd1; 32];
        let from_alice = |id: [u8; 32], body: &str| {
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 0,
                message_id: id,
                source: alice,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields: Vec::new(),
                verified: true,
            })
        };
        let home = tempfile::tempdir().expect("home");

        // The first run: the direct copy, then a window's worth after it.
        let mut store = Store::open_in(home.path(), us).expect("open");
        let mut model = Model::new(&[]);
        update(&mut model, from_alice(direct, "the direct copy"));
        for n in 0..LOAD_WINDOW {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&(n as u64).to_be_bytes());
            update(&mut model, from_alice(id, &format!("later {n}")));
        }
        store.write(&model.take_writes()).expect("write");
        drop(store);

        // The restart, as `run` makes it, and the mailbox copy.
        let mut store = Store::open_in(home.path(), us).expect("reopen");
        let mut model = Model::new(&[]);
        model.restore(store.restore().expect("restore"));
        model.activity = model.activity.max(store.last_key().expect("last key"));
        assert!(
            model.conversations[0]
                .messages
                .iter()
                .all(|m| m.id != Some(direct)),
            "the direct copy lies below the loaded window"
        );
        let again = from_alice(direct, "the direct copy");
        let UiEvent::Engine(event) = &again else {
            unreachable!("an engine event")
        };
        // What `Shell::poll` asks before it folds.
        if !already_held(&store, event) {
            update(&mut model, again);
        }
        store.write(&model.take_writes()).expect("write");

        let everything: BTreeMap<[u8; 16], u64> = [(alice, u64::MAX)].into_iter().collect();
        let rows = store
            .search("the direct copy", &everything)
            .expect("search");
        assert_eq!(rows.len(), 1, "one row: {rows:?}");
        assert_eq!(
            model.conversations[0].messages.len(),
            LOAD_WINDOW,
            "no second block"
        );
    }

    /// A panic in a frame comes back as its message, the messenger goes on
    /// (1087); a frame that draws comes back as it is.
    #[test]
    fn a_panicking_frame_is_its_message() {
        assert_eq!(unwound(|| 7), Ok(7));
        let index = unwound(|| {
            let buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 54, 30));
            buf[(39, 49)].symbol().to_string()
        });
        assert!(
            index.is_err_and(|m| m.contains("index outside of buffer")),
            "the buffer's own message"
        );
        assert_eq!(
            unwound(|| -> u8 { std::panic::panic_any(3u8) }),
            Err("a panic without a message".to_string())
        );
    }

    /// Ten pans queued behind the one that woke the loop, as a held arrow
    /// leaves them while a frame is drawn, are applied in one go: the next
    /// frame is the only one, at the sum of all ten (1075).
    #[test]
    fn ten_queued_pans_are_one_frame_at_their_sum() {
        let mut model = Model::new(&[[0x22; 16]]);
        let mut painter = map::MapPainter::new(
            loquax::picture::Maps::default(),
            map::Graphics::Braille,
            None,
        );
        update(&mut model, UiEvent::MapSetup(painter.setup()));
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 24,
            },
        );
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::NONE)),
        );
        let start = model.map.expect("the map is open");
        let mut want = ui::map::viewport(&start, model.map_area(), &model.map_setup);
        let (w, _) = want.size();
        for _ in 0..10 {
            want.pan((w / 8) as i32, 0);
        }
        let right = || {
            Ok(Event::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Right,
                crossterm::event::KeyModifiers::NONE,
            )))
        };
        // Nine waiting behind the first, then nothing more: the drain must
        // not wait for an eleventh.
        let mut events = (0..9).map(|_| Some(right()));
        let mut applied = 0;
        let ended = apply_waiting(
            Some(right()),
            || events.next(),
            false,
            |event| {
                applied += 1;
                update(&mut model, event);
            },
        );
        assert!(ended.is_none(), "the stream goes on");
        assert_eq!(applied, 10);
        let state = model.map.expect("open");
        assert_eq!(state.centre, want.centre, "ten eighths east");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        let Ok(_) = terminal.draw(|f| draw(f, &ui::view(&model), Some(&mut painter)));
        assert_eq!(painter.paints(), 1, "one frame for the ten");
        assert_eq!(painter.painted_view(), Some(want));
    }

    /// The raw input log: one line an event, every field but a paste's
    /// text, and the variable read as on only when set to something but
    /// `0` (1102).
    #[test]
    fn the_input_log_says_each_event_as_it_came() {
        use crossterm::event::{
            KeyCode as Code, KeyEvent as Key, KeyEventKind, KeyEventState, KeyModifiers as Mods,
            MouseButton as Button, MouseEvent as Mouse, MouseEventKind as Kind,
        };
        let mouse = |kind, modifiers| {
            Event::Mouse(Mouse {
                kind,
                column: 12,
                row: 7,
                modifiers,
            })
        };
        assert_eq!(
            input_line(&mouse(Kind::ScrollLeft, Mods::NONE)),
            "INPUT kind=mouse event=scroll_left button=none col=12 row=7 mods=none"
        );
        assert_eq!(
            input_line(&mouse(
                Kind::Drag(Button::Left),
                Mods::SHIFT | Mods::CONTROL
            )),
            "INPUT kind=mouse event=drag button=left col=12 row=7 mods=shift+control"
        );
        assert_eq!(
            input_line(&Event::Key(Key::new(Code::Char('h'), Mods::ALT))),
            "INPUT kind=key code=char:h mods=alt state=press"
        );
        assert_eq!(
            input_line(&Event::Key(Key {
                code: Code::Char(' '),
                modifiers: Mods::NONE,
                kind: KeyEventKind::Repeat,
                state: KeyEventState::NONE,
            })),
            "INPUT kind=key code=char:U+0020 mods=none state=repeat"
        );
        assert_eq!(
            input_line(&Event::Key(Key::new(Code::F(5), Mods::NONE))),
            "INPUT kind=key code=f5 mods=none state=press"
        );
        assert_eq!(
            input_line(&Event::Key(Key::new(Code::Left, Mods::NONE))),
            "INPUT kind=key code=left mods=none state=press"
        );
        assert_eq!(
            input_line(&Event::Resize(80, 24)),
            "INPUT kind=resize width=80 height=24"
        );
        assert_eq!(
            input_line(&Event::Paste("secret words".to_string())),
            "INPUT kind=paste len=12"
        );
        assert_eq!(
            input_line(&Event::FocusLost),
            "INPUT kind=focus gained=false"
        );
        let on = |v: Option<&str>| input_log_on(v.map(std::ffi::OsStr::new));
        assert!(on(Some("1")));
        assert!(!on(Some("0")));
        assert!(!on(Some("")));
        assert!(!on(None));
    }

    /// An engine whose first read stalls for `stall`, saying on `entered`
    /// when it began: the slow engine step of 1492.
    struct Stalling {
        stall: Duration,
        entered: Option<std::sync::mpsc::Sender<()>>,
    }

    impl loquax::outbox::Outbox for Stalling {
        fn submit(
            &self,
            _command: loquax::outbox::Command,
        ) -> Result<(), loquax::outbox::OutboxGone> {
            Ok(())
        }

        fn try_next_event(
            &mut self,
        ) -> Result<Option<loquax::outbox::OutboxEvent>, loquax::outbox::OutboxGone> {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
                std::thread::sleep(self.stall);
            }
            Ok(None)
        }
    }

    /// The loop's stand-in for the shell: when each event was applied, and
    /// in the old layout the engine read on the loop's own task.
    struct Stand {
        applied: Vec<(std::time::Instant, UiEvent)>,
        inline: Option<(loquax::fanout::Fanout, Stalling)>,
    }

    impl Turned for Stand {
        fn apply(&mut self, event: UiEvent) {
            self.applied.push((std::time::Instant::now(), event));
        }

        fn fold_read(&mut self, _read: EngineRead) {}

        fn poll(&mut self) {
            if let Some((fanout, engine)) = &mut self.inline {
                let _ = fanout.pump(engine);
            }
        }

        fn tick(&mut self, _clocks: &mut (i64, u64)) {}
    }

    /// The stall the key is typed into (1492).
    const STALL: Duration = Duration::from_secs(2);

    /// Turn the loop as the terminal does while a key is typed into the
    /// engine's stall; how long after the key the model had it.
    async fn key_into_a_stall(
        stand: &mut Stand,
        mut reads: Option<tokio::sync::mpsc::Receiver<EngineRead>>,
        entered: std::sync::mpsc::Receiver<()>,
    ) -> (Duration, Option<tokio::sync::mpsc::Receiver<EngineRead>>) {
        let (inputs_tx, mut inputs) = tokio::sync::mpsc::unbounded_channel();
        // The key comes from another thread, as the input task's read
        // does, once the engine is inside its stall.
        let typist = std::thread::spawn(move || {
            entered.recv().expect("the engine stalls");
            std::thread::sleep(Duration::from_millis(100));
            let typed = std::time::Instant::now();
            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('x'),
                crossterm::event::KeyModifiers::NONE,
            );
            inputs_tx.send(Some(Ok(Event::Key(key)))).expect("the loop");
            // Held until the loop is done: a dropped queue is an end.
            (typed, inputs_tx)
        });
        let mut pump = tokio::time::interval(Duration::from_millis(PUMP_MS));
        pump.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tick = tokio::time::interval(Duration::from_millis(TICK_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut clocks = (0, 0);
        let mut presented = ui::Presented::default();
        let mut panicked = false;
        let deadline = std::time::Instant::now() + STALL * 5;
        let applied = loop {
            let key = stand
                .applied
                .iter()
                .find(|(_, e)| matches!(e, UiEvent::Key(k) if k.code == KeyCode::Char('x')));
            if let Some((at, _)) = key {
                break *at;
            }
            assert!(std::time::Instant::now() < deadline, "the key never came");
            let ended = wait(
                stand,
                &mut inputs,
                false,
                &mut reads,
                std::future::pending::<()>(),
                &mut pump,
                &mut tick,
                &mut clocks,
                &mut presented,
                &mut panicked,
            )
            .await;
            assert!(ended.is_none(), "the input goes on");
        };
        let (typed, _inputs_tx) = typist.join().expect("the typist");
        (applied.saturating_duration_since(typed), reads)
    }

    /// 1492: a key typed while the engine's step stalls for 2 s is in the
    /// model within one tick, because the engine is read on its own
    /// thread; the control, the engine read on the loop's task as before
    /// 1492, has it only once the stall is over.
    #[tokio::test]
    async fn a_key_typed_into_a_stalled_engine_step_is_applied_within_one_tick() {
        // The old layout: the pump reads the engine on the loop's task.
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let mut stand = Stand {
            applied: Vec::new(),
            inline: Some((
                loquax::fanout::Fanout::default(),
                Stalling {
                    stall: STALL,
                    entered: Some(entered_tx),
                },
            )),
        };
        let (old, _) = key_into_a_stall(&mut stand, None, entered).await;
        eprintln!("STALL_KEY layout=inline key_ms={}", old.as_millis());
        assert!(
            old >= STALL - Duration::from_millis(400),
            "the control holds the key behind the stall: {old:?}"
        );

        // The terminal's layout: the engine read on its own thread.
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let mut fanout = loquax::fanout::Fanout::default();
        let Ok(reader) = fanout.lend(Stalling {
            stall: STALL,
            entered: Some(entered_tx),
        }) else {
            panic!("lent once")
        };
        let (reads_tx, reads_rx) = tokio::sync::mpsc::channel(READS_QUEUED);
        let Ok(thread) = EngineThread::spawn(reader, reads_tx) else {
            panic!("the reader thread starts")
        };
        let mut stand = Stand {
            applied: Vec::new(),
            inline: None,
        };
        let (new, reads) = key_into_a_stall(&mut stand, Some(reads_rx), entered).await;
        eprintln!("STALL_KEY layout=thread key_ms={}", new.as_millis());
        assert!(
            new < Duration::from_millis(TICK_MS),
            "the key waits for no engine step: {new:?}"
        );
        // Stopping waits out the stall and hands the reader back.
        let reader = thread.stop(&mut stand, reads).await;
        assert!(reader.is_some(), "the reader comes back");
        fanout.take_back(reader.expect("back"));
    }

    /// 1801, the render walk: the terminal's screens (the empty list, a
    /// chat with a late message, the settings) at the cells each of the
    /// walk's frame sizes holds at eight by sixteen logical pixels a cell.
    /// Each frame draws (text on at least a tenth of its rows) and is the
    /// same again. With `LOQUAX_RENDER_WALK=DIR` each frame is written to
    /// `DIR/screens/` as text, its verdicts in `DIR/rows-terminal.tsv`,
    /// for `just render-walk`'s contact sheet.
    #[test]
    fn every_terminal_screen_draws_at_every_walk_size() {
        let out = std::env::var_os("LOQUAX_RENDER_WALK").map(PathBuf::from);
        let sizes = [
            ("1224x2197", 55u16, 49u16),
            ("1080x2400", 51, 57),
            ("1600x2560", 100, 80),
            ("900x560", 112, 35),
            ("2197x1224", 99, 27),
        ];
        /// A screen of the terminal, its model made.
        type Make = fn() -> Model;
        let screens: [(&str, Make); 3] = [
            ("list-empty", || Model::new(&[])),
            ("late-message", || {
                let mut model = Model::new(&[]);
                model.address = Some([0x30; 16]);
                late_message::late_message(&mut model, 1_799_971_200, 3600);
                model.focus = loquax::ui::Pane::Messages;
                model.toasts.clear();
                model
            }),
            ("settings", || {
                let mut model = Model::new(&[]);
                update(
                    &mut model,
                    UiEvent::Key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE)),
                );
                model
            }),
        ];
        let mut rows = String::new();
        let mut failed = Vec::new();
        for (screen, make) in screens {
            for (size, cols, lines) in sizes {
                let shoot = || {
                    let mut model = make();
                    update(
                        &mut model,
                        UiEvent::Resize {
                            width: cols,
                            height: lines,
                        },
                    );
                    let mut terminal =
                        Terminal::new(TestBackend::new(cols, lines)).expect("terminal");
                    let view = ui::view(&model);
                    let drawn = terminal
                        .draw(|frame| draw_screen(frame, &model, &view, None))
                        .expect("draw");
                    frame_text(drawn.buffer)
                };
                let text = shoot();
                let written = text.lines().filter(|l| !l.trim().is_empty()).count();
                let drawn = written * 10 >= usize::from(lines);
                let same = shoot() == text;
                let image = format!("terminal-{screen}-{size}");
                for (rule, pass, note) in [
                    ("screen_drawn", drawn, format!("{written} of {lines} rows")),
                    ("screen_repeats", same, String::new()),
                ] {
                    if !pass {
                        failed.push(format!("{image} {rule} {note}"));
                    }
                    rows.push_str(&format!(
                        "terminal\tscreen\t{screen}\t{size}\t\t\t{image}\t{rule}\t{}\t1.000\t{}\t{note}\n",
                        u8::from(pass),
                        if pass { "pass" } else { "FAIL" }
                    ));
                }
                if let Some(out) = &out {
                    let dir = out.join("screens");
                    std::fs::create_dir_all(&dir).expect("the screens directory");
                    std::fs::write(dir.join(format!("{image}.txt")), &text).expect("the frame");
                }
            }
        }
        if let Some(out) = &out {
            std::fs::write(out.join("rows-terminal.tsv"), &rows).expect("the rows");
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }
}
