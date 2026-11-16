//! When nothing changes, nothing is drawn (1455): the window opened on a
//! chat and on the map, run as iced's loop runs it on X11
//! ([`leviculum_ui_iced::headless::drive`]) with the shell's half
//! emulated as `drive` in `gui.rs` runs it (the model's clock and tick
//! every [`TICK_MS`], what the window shows offered to the window after
//! each), then left alone for five seconds: no message wakes the
//! window, no redraw is asked, no frame is presented.
//!
//! Before 1455 the shell sent the window a new state on every second of
//! the model's clock, iced redraws after every message, and the
//! compositor presented every redraw, an unchanged one too: softbuffer
//! hands the whole window to the X server on each present. The controls
//! run that rule again ([`Rule::EveryChange`]) and a shell that changes
//! a word on the map every second ([`Rule::Loading`]).
//!
//! The five seconds start when the window has settled, by its events and
//! not by the clock (1473): its first frames drawn, the map's worker done
//! painting (the window idle, as `drive` asks it), a frame drawn after
//! the last message of the window's own and a whole tick without a
//! frame since, at the earliest after [`SETTLE`] and at the latest after
//! [`SETTLE_MAX`]. Before 1473 it settled for a fixed
//! 1.5 s, and a debug-build worker still painting under a loaded host
//! put its `Painted` into the five seconds (red once in 1468's run:
//! `messages: [("Painted", 5)], presents: 4`).
//!
//! A control's five seconds are held open until it has seen its count
//! (1623): on a loaded host an unoptimised frame takes long enough that
//! five wall seconds held three redraws, red where the rule had held.
//!
//! `LOQUAX_IDLE_FPS=1` runs it with the frame meter's overlay on (a
//! switch for the whole process: run the test alone, `--exact`).

use super::bench::map_window;
use super::bench::Window;
use super::tests::chat_model;
use super::*;
use leviculum_ui_iced::headless::{self, Program};
use loquax::ui::update;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// How long the window is left alone, and how long before that it may
/// settle at the earliest (its first frames, the map's first picture).
const IDLE: Duration = Duration::from_secs(5);
const SETTLE: Duration = Duration::from_millis(1500);
/// How long it may take to settle at the most: a debug build's map
/// worker on a loaded host paints its first picture in a few seconds.
const SETTLE_MAX: Duration = Duration::from_secs(20);
/// How long the window's own messages must have been quiet to settle.
const QUIET: Duration = Duration::from_millis(TICK_MS);

/// What a control has to see in its window (1623): this many wakes and
/// redraws for [`Rule::EveryChange`], this many presents for
/// [`Rule::Loading`]. The blink with the compose field pressed into has
/// to show one frame.
const CONTROL: usize = 4;

/// How long past [`IDLE`] a control's window is held open at most while
/// it has not seen [`CONTROL`] (1623). The shell's tick runs on the wall,
/// one a second; an unoptimised frame on a host loaded by a parallel run
/// took long enough that five wall seconds held three redraws
/// (`redrawn=3`, `presents: 3`), red where the rule had held all the
/// while. The window now stays open until the control has seen its
/// count, so a slow host stretches the window instead of emptying it; a
/// rule that wakes nothing still reads red after this guard.
const HOLD_AT_MOST: Duration = Duration::from_secs(60);

/// When the shell woke the window and when the window redrew and
/// presented, from the shell's start: what a control's window waits on
/// (1623), shared by the shell, the clock and the script.
#[derive(Default)]
struct Tally {
    wakes: Vec<Duration>,
    redraws: Vec<Duration>,
    presents: Vec<Duration>,
}

impl Tally {
    /// Whether the window from `from` to `until` has seen what the
    /// case's control asks; the idle cases ask nothing.
    fn seen(&self, screen: Screen1455, rule: Rule, from: Duration, until: Duration) -> bool {
        let since =
            |times: &[Duration]| times.iter().filter(|t| **t >= from && **t < until).count();
        match (screen, rule) {
            (Screen1455::Typing, _) => since(&self.presents) >= 1,
            (_, Rule::Shown) => true,
            (_, Rule::EveryChange) => {
                since(&self.wakes) >= CONTROL && since(&self.redraws) >= CONTROL
            }
            (_, Rule::Loading) => since(&self.presents) >= CONTROL,
        }
    }
}

/// The clock the drive runs on: the shell's, so the frames, the wakes
/// and the messages are all timed from one start; it tallies the redraws
/// and the presents (1623).
struct Since(Instant, Rc<RefCell<Tally>>);

impl headless::Clock for Since {
    fn turn(&mut self, redrawn: bool) {
        if redrawn {
            let now = self.now();
            self.1.borrow_mut().redraws.push(now);
        }
    }

    fn now(&self) -> Duration {
        self.0.elapsed()
    }

    fn presented(&mut self, at: Duration) {
        self.1.borrow_mut().presents.push(at);
    }
}

/// The window size: the goldens', so the debug build stays quick.
const SIZE: (u32, u32) = (900, 560);

/// What the emulated shell sends the window.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Rule {
    /// As `drive` does since 1491 ([`Driving`]): a state built only when
    /// its stamp moved, sent only when the window shows it differently
    /// ([`Sent`], 1455).
    Shown,
    /// Before 1455 and 1491: a state built on every pass, sent whenever
    /// it differs, its clock's second too.
    EveryChange,
    /// As [`Rule::Shown`], and the map bar's loading word changes every
    /// second, as it does while a region arrives.
    Loading,
}

/// The shell's half: the model, its clock, what was sent.
struct Shell {
    model: crate::shell::Model,
    /// The model's clock when the run starts: on a whole minute, so the
    /// five seconds cross none.
    clock: WallTime,
    template: Seen,
    rule: Rule,
    driving: Driving,
    browsing: crate::browsing::Browsing,
    last: Option<Seen>,
    start: Instant,
    ticks: u32,
    /// When the window was handed a state, seconds since the start.
    wakes: Vec<f64>,
    /// When the shell built a state for the window, seconds since the
    /// start (1491).
    builds: Vec<f64>,
    /// The wakes again, for the script that holds a control open (1623).
    tally: Rc<RefCell<Tally>>,
}

impl Shell {
    /// The ticks due by now, each the model's clock and tick, then what
    /// the window shows offered to it.
    fn pump(&mut self) -> Option<Seen> {
        let mut out = None;
        while Duration::from_millis(TICK_MS) * self.ticks <= self.start.elapsed() {
            self.ticks += 1;
            let secs = i64::from(self.ticks) * TICK_MS as i64 / 1000;
            let clock = WallTime {
                unix_secs: self.clock.unix_secs + secs,
                ..self.clock
            };
            update(&mut self.model, UiEvent::Clock(clock));
            update(&mut self.model, UiEvent::Tick);
            // The drive's own builder (`drive` in `gui.rs`), the maps
            // directory and the map's finish the window was made with.
            let (model, browsing, template) = (&self.model, &self.browsing, &self.template);
            let (start, builds, rule) = (self.start, &mut self.builds, self.rule);
            let mut build = || {
                builds.push(start.elapsed().as_secs_f64());
                let mut seen = Seen {
                    maps: template.maps,
                    style: template.style,
                    ..Seen::of_model(model, template.maps, browsing)
                };
                if rule == Rule::Loading {
                    seen.loading = Some(format!("Loading Florence, {secs} s"));
                }
                seen
            };
            let offered = match self.rule {
                Rule::Shown => self
                    .driving
                    .pass(Seen::stamp(model, template.maps, browsing, None), build),
                // The region's word is the shell's, and moves the stamp
                // as `map_loading` moves the revision.
                Rule::Loading => self.driving.pass(
                    Seen::stamp(model, template.maps, browsing, None) ^ secs as u64,
                    build,
                ),
                Rule::EveryChange => {
                    let seen = build();
                    match self.last.as_ref() == Some(&seen) {
                        true => None,
                        false => {
                            self.last = Some(seen.clone());
                            Some(seen)
                        }
                    }
                }
            };
            if let Some(seen) = offered {
                let now = self.start.elapsed();
                self.wakes.push(now.as_secs_f64());
                self.tally.borrow_mut().wakes.push(now);
                out = Some(seen);
            }
        }
        out
    }
}

/// How the window settles before its idle seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Settle {
    /// By its events (1473), the rule.
    Events,
    /// For [`SETTLE`] by the clock, as before 1473: the control a late
    /// worker makes red.
    Clock,
}

/// The window with the shell's half beside it.
struct Idle {
    window: Window,
    shell: RefCell<Shell>,
    /// Every message the window was handed: when, seconds since the
    /// start, and its kind.
    handed: Vec<(f64, String)>,
    /// When the window last was handed a message of its own (not the
    /// shell's state), and when it last drew, since the start.
    own: Cell<Duration>,
    drawn: Cell<Option<Duration>>,
    /// When the window settled, since the start, shared with the script
    /// that ends the run [`IDLE`] after it; and whether it had to be
    /// settled at [`SETTLE_MAX`].
    settled: Rc<Cell<Option<Duration>>>,
    timed_out: Cell<bool>,
    settle: Settle,
    /// A worker that paints late (the positive control): its pictures
    /// are handed to the window only from this long after the start.
    late: Option<Duration>,
    held: RefCell<Vec<Message>>,
    /// Whether the drive was let end though the window still waited on
    /// its worker, two seconds after the idle ones ([`Idle::ended`]).
    forced: Cell<bool>,
    /// When the idle seconds, a control's held open, ended (1623).
    ended: Rc<Cell<Option<Duration>>>,
}

impl Idle {
    fn elapsed(&self) -> Duration {
        self.shell.borrow().start.elapsed()
    }

    /// Settled once, when `out`, the window's own messages of this pump,
    /// are none.
    fn settle(&self, out: &[Message]) {
        if self.settled.get().is_some() {
            return;
        }
        let now = self.elapsed();
        if now < SETTLE {
            return;
        }
        // Drawn since its last message and quiet a tick since: a message
        // is followed by a redraw, and on a loaded host the view it
        // builds first can take longer than a tick.
        let quiet = out.is_empty()
            && self.held.borrow().is_empty()
            && self
                .drawn
                .get()
                .is_some_and(|at| at >= self.own.get() && now.saturating_sub(at) >= QUIET)
            && self.window.idle();
        if self.settle == Settle::Clock || quiet {
            self.settled.set(Some(now));
        } else if now >= SETTLE_MAX {
            self.timed_out.set(true);
            self.settled.set(Some(now));
        }
    }
}

impl Program for Idle {
    type Message = Message;

    fn view(&self) -> Element<'_, Message> {
        self.window.view()
    }

    fn update(&mut self, message: Message) {
        let at = self.shell.borrow().start.elapsed().as_secs_f64();
        let kind = format!("{message:?}");
        let kind = kind.split(['(', ' ', '{']).next().unwrap_or("").to_string();
        self.handed.push((at, kind));
        if !matches!(message, Message::Seen(_)) {
            self.own.set(self.elapsed());
        }
        // The shell's state as the window's update folds it, the map's
        // worker handed what it asks for then: the bench hands it only
        // the map's own messages, and would leave it waiting (1455).
        match matches!(message, Message::Seen(_)) {
            true => {
                if self.window.app.apply(message) {
                    self.window.dispatch();
                }
            }
            false => self.window.update(message),
        }
    }

    fn pump(&self, drawn: bool) -> Vec<Message> {
        if drawn {
            self.drawn.set(Some(self.elapsed()));
        }
        let mut out = self.window.pump(drawn);
        if let Some(late) = self.late {
            let mut held = self.held.borrow_mut();
            match self.elapsed() >= late {
                true => out.append(&mut held),
                false => {
                    let (painted, rest) = out
                        .into_iter()
                        .partition(|m| matches!(m, Message::Painted(_)));
                    held.extend::<Vec<_>>(painted);
                    out = rest;
                }
            }
        }
        self.settle(&out);
        if let Some(seen) = self.shell.borrow_mut().pump() {
            out.push(Message::Seen(Box::new(seen)));
        }
        out
    }

    fn idle(&self) -> bool {
        if self.window.idle() && self.held.borrow().is_empty() {
            return true;
        }
        let late = self
            .ended
            .get()
            .is_some_and(|at| self.elapsed() >= at + Duration::from_secs(2));
        if late && !self.forced.get() {
            let app = &self.window.app;
            println!(
                "IDLE_BUSY busy={} wheel={:?} animating={} owed={}",
                app.busy,
                app.wheel.is_some(),
                app.animating(),
                app.owed
            );
        }
        self.forced.set(late);
        late
    }

    fn style(&self) -> (Theme, Color, Color) {
        self.window.style()
    }
}

/// The screen the window is left on.
#[derive(Clone, Copy, Debug)]
enum Screen1455 {
    Chat,
    Map,
    /// The chat with the compose field pressed into: the cursor blinks.
    Typing,
}

/// What five idle seconds did, after the settle; a control's held open
/// until it saw its count.
#[derive(Debug)]
struct Quiet {
    /// How long the window was: [`IDLE`], or longer for a control held
    /// open ([`HOLD_AT_MOST`]).
    seconds: f64,
    /// When the window settled, seconds since the start, and whether
    /// only [`SETTLE_MAX`] settled it.
    settled: f64,
    timed_out: bool,
    /// States the shell built, and handed the window (1491).
    builds: usize,
    wakes: usize,
    /// The wakes a redraw followed before the next wake, wherever it
    /// landed: a wake late in the five seconds is redrawn after them
    /// (1486's control under load: 4 wakes, 3 redraws inside them).
    redrawn: usize,
    /// Every message the window was handed, by kind.
    messages: Vec<(String, usize)>,
    /// The window still waited on its worker at the end.
    busy: bool,
    /// Redraws that presented nothing.
    unpresented: usize,
    /// Frames presented, and the largest share of the window one
    /// rasterised.
    presents: usize,
    damage: f32,
    /// The damage of the last frame presented, as the layers' diff
    /// gave it.
    rects: Vec<iced::Rectangle>,
}

impl Quiet {
    fn line(&self, screen: Screen1455, rule: Rule) -> String {
        format!(
            "IDLE_WINDOW screen={screen:?} rule={rule:?} seconds={:.1} settled={:.2} timed_out={} builds={} wakes={} redrawn={} messages={:?} busy={} redraws_unpresented={} presents={} max_damage={:.4} last_rects={}",
            self.seconds,
            self.settled,
            self.timed_out,
            self.builds,
            self.wakes,
            self.redrawn,
            self.messages,
            self.busy,
            self.unpresented,
            self.presents,
            self.damage,
            self.rects
                .iter()
                .map(|r| format!("{:.0}x{:.0}@{:.0},{:.0}", r.width, r.height, r.x, r.y))
                .collect::<Vec<_>>()
                .join("+")
        )
    }
}

/// The chat's model with nothing left to happen by the clock: the
/// toasts its sends said have aged out ([`loquax::ui::TOAST_TICKS`]), as
/// they have on a screen left alone. A toast that ages out inside the
/// five seconds is a change the window shows (1491: the drive builds
/// what the model shows, the notice among it), not an idle wake.
fn settled_model() -> crate::shell::Model {
    let mut model = chat_model();
    for _ in 0..loquax::ui::TOAST_TICKS {
        update(&mut model, UiEvent::Tick);
    }
    assert!(model.toasts.is_empty(), "a toast outlived its ticks");
    model
}

fn idle(screen: Screen1455, rule: Rule) -> Quiet {
    idle_with(screen, rule, Settle::Events, None)
}

/// [`idle`], settled as `settle` says, the map's worker handing its
/// pictures over no earlier than `late` after the start.
fn idle_with(screen: Screen1455, rule: Rule, settle: Settle, late: Option<Duration>) -> Quiet {
    let (w, h) = SIZE;
    let (app, model) = match screen {
        Screen1455::Chat | Screen1455::Typing => {
            let model = settled_model();
            let (tx, _) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
            app.window = Size::new(w as f32, h as f32);
            app.apply(Message::Seen(Box::new(super::tests::seen_of(&model))));
            (app, model)
        }
        Screen1455::Map => (map_window(w, h).0, settled_model()),
    };
    let template = app.seen.clone();
    let tally: Rc<RefCell<Tally>> = Rc::default();
    let mut program = Idle {
        window: Window::new(app),
        shell: RefCell::new(Shell {
            clock: model.now.unwrap_or(WallTime {
                unix_secs: 1_793_000_000 - 1_793_000_000 % 60,
                offset_secs: 0,
            }),
            model,
            template,
            rule,
            driving: Driving::default(),
            browsing: crate::browsing::Browsing::off(),
            last: None,
            start: Instant::now(),
            ticks: 0,
            wakes: Vec::new(),
            builds: Vec::new(),
            tally: tally.clone(),
        }),
        handed: Vec::new(),
        own: Cell::new(Duration::ZERO),
        drawn: Cell::new(None),
        settled: Rc::new(Cell::new(None)),
        timed_out: Cell::new(false),
        settle,
        late,
        held: RefCell::new(Vec::new()),
        forced: Cell::new(false),
        ended: Rc::default(),
    };
    let start = program.shell.borrow().start;
    let settled = program.settled.clone();
    let (script_tally, script_ended) = (tally.clone(), program.ended.clone());
    // Typing: a press into the compose field while the window settles,
    // and one on the list's empty foot after the idle seconds, which
    // takes the focus and ends the blink, as the bench's idle case does.
    let field = iced::Point::new(w as f32 - 250.0, h as f32 - 28.0);
    let list = iced::Point::new(200.0, h as f32 - 40.0);
    let at = |p| iced::Event::Mouse(mouse::Event::CursorMoved { position: p });
    let down = iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
    let up = iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
    // The press into the field at a third of the settle; the one on
    // the list when the idle seconds end, whenever that is.
    let (mut early, mut after): (Vec<iced::Event>, Vec<iced::Event>) = match screen {
        Screen1455::Typing => (
            vec![at(field), down.clone(), up.clone()],
            vec![at(list), down, up],
        ),
        _ => (Vec::new(), Vec::new()),
    };
    let mut script = |now: Duration| -> Option<Vec<iced::Event>> {
        let mut events = Vec::new();
        if now >= SETTLE / 3 {
            events.append(&mut early);
        }
        // The idle seconds end IDLE after the settle, held open while a
        // control has not seen its count (1623): a case whose count the
        // IDLE seconds held keeps exactly those, though the script may be
        // asked a slow frame after them.
        if let (None, Some(at)) = (script_ended.get(), settled.get()) {
            let tally = script_tally.borrow();
            let end = at + IDLE;
            if now >= end && tally.seen(screen, rule, at, end) {
                script_ended.set(Some(end));
            } else if now >= end && (tally.seen(screen, rule, at, now) || now >= end + HOLD_AT_MOST)
            {
                script_ended.set(Some(now));
            }
        }
        let over = script_ended.get().is_some();
        if over {
            events.append(&mut after);
        }
        match over && events.is_empty() {
            true => None,
            false => Some(events),
        }
    };
    let mut clock = Since(start, tally.clone());
    let run = headless::drive_on(&mut program, w, h, &mut clock, &mut script, SETTLE)
        .expect("a renderer");
    let settled = program.settled.get().unwrap_or(SETTLE_MAX);
    let until = program
        .ended
        .get()
        .unwrap_or(settled + IDLE)
        .max(settled + IDLE);
    let (from, until) = (settled.as_secs_f64(), until.as_secs_f64());
    let shell = program.shell.borrow();
    let inside = |t: f64| t >= from && t < until;
    let presented: Vec<_> = run.frames.iter().filter(|f| inside(f.at)).collect();
    let rects = run
        .frames
        .iter()
        .zip(&run.rects)
        .rev()
        .find(|(f, _)| inside(f.at))
        .map(|(_, r)| r.clone())
        .unwrap_or_default();
    let mut messages: Vec<(String, usize)> = Vec::new();
    for (_, kind) in program.handed.iter().filter(|(t, _)| inside(*t)) {
        match messages.iter_mut().find(|(k, _)| k == kind) {
            Some((_, n)) => *n += 1,
            None => messages.push((kind.clone(), 1)),
        }
    }
    // Each wake with its redraw: the first redraw at or after it, before
    // the next wake, wherever the five seconds end.
    let mut redraws: Vec<f64> = run.frames.iter().map(|f| f.at).collect();
    redraws.extend(run.unpresented.iter().copied());
    let redrawn = (shell.wakes.iter().enumerate())
        .filter(|(_, &t)| inside(t))
        .filter(|&(i, &t)| {
            let next = shell.wakes.get(i + 1).copied().unwrap_or(f64::INFINITY);
            redraws.iter().any(|&r| r >= t && r < next)
        })
        .count();
    Quiet {
        seconds: until - from,
        settled: from,
        timed_out: program.timed_out.get(),
        builds: shell.builds.iter().filter(|&&t| inside(t)).count(),
        wakes: shell.wakes.iter().filter(|&&t| inside(t)).count(),
        redrawn,
        messages,
        busy: program.forced.get(),
        unpresented: run.unpresented.iter().filter(|&&t| inside(t)).count(),
        presents: presented.len(),
        damage: presented.iter().map(|f| f.damage).fold(0.0, f32::max),
        rects,
    }
}

/// The guard (1455): a chat and the map left alone for five seconds
/// with the shell's clock running wake the window not once and present
/// nothing. The controls make it red: the rule before 1455, a state per
/// second of the clock, wakes the window every second (the redraws now
/// present nothing, as nothing changed); a word on the map that changes
/// every second presents a frame every second, its word's rectangle.
/// With the compose field pressed into, the cursor's blink is the one
/// redraw left, two a second, and it rasterises a small part of the
/// window: the cursor and the send button after it, whose quad iced
/// compares by its place in the layer's list, which the cursor's quad
/// coming and going moves; grouped, one strip of 0.04 of the window.
/// Before 1455's patch to the vendored iced_tiny_skia every canvas shape
/// on screen counted as changed, and each blink rasterised the whole
/// window.
#[test]
fn an_idle_window_draws_nothing() {
    if std::env::var_os("LOQUAX_IDLE_FPS").is_some() {
        leviculum_ui_iced::fps::enable(|_| {});
    }
    // Every case measured first, then held, so a red run says them all.
    let cases = [
        (Screen1455::Chat, Rule::Shown),
        (Screen1455::Chat, Rule::EveryChange),
        (Screen1455::Map, Rule::Shown),
        (Screen1455::Map, Rule::EveryChange),
        (Screen1455::Map, Rule::Loading),
        (Screen1455::Typing, Rule::Shown),
    ];
    let measured: Vec<Quiet> = cases
        .iter()
        .map(|&(screen, rule)| {
            let quiet = idle(screen, rule);
            println!("{}", quiet.line(screen, rule));
            quiet
        })
        .collect();
    if let Ok(file) = std::env::var("LOQUAX_IDLE_LINES") {
        let lines: Vec<String> = cases
            .iter()
            .zip(&measured)
            .map(|(&(screen, rule), quiet)| quiet.line(screen, rule))
            .collect();
        let _ = std::fs::write(file, lines.join("\n") + "\n");
    }
    for (&(screen, rule), quiet) in cases.iter().zip(&measured) {
        match (screen, rule) {
            (Screen1455::Typing, _) => {
                assert_eq!(
                    (quiet.builds, quiet.wakes, quiet.messages.len()),
                    (0, 0, 0),
                    "{quiet:?}"
                );
                assert!((1..=12).contains(&quiet.presents), "the blink: {quiet:?}");
                assert!(quiet.damage < 0.1, "the blink drew the window: {quiet:?}");
            }
            (_, Rule::Shown) => assert_eq!(
                (
                    quiet.timed_out,
                    quiet.builds,
                    quiet.wakes,
                    quiet.messages.len(),
                    quiet.busy,
                    quiet.unpresented,
                    quiet.presents
                ),
                (false, 0, 0, 0, false, 0, 0),
                "{screen:?} idle: {quiet:?}"
            ),
            // The rule before 1491 builds on every tick, twenty in the
            // five seconds, and wakes the window on every second of the
            // clock; each wake is redrawn, counted with its wake.
            (_, Rule::EveryChange) => assert!(
                quiet.builds >= 16 && quiet.wakes >= CONTROL && quiet.redrawn >= CONTROL,
                "the control woke nothing: {quiet:?}"
            ),
            (_, Rule::Loading) => {
                assert!(
                    quiet.presents >= CONTROL,
                    "the control drew nothing: {quiet:?}"
                )
            }
        }
    }
}

/// The settle waits for a worker that paints late (1473): the map's
/// pictures handed over four seconds after the start, long past
/// [`SETTLE`], as a debug build's worker on a loaded host hands them.
/// Settled by its events the window starts its idle seconds after the
/// last picture and they stay quiet; settled by the clock, as before
/// 1473, the late pictures land in them (the control).
#[test]
fn a_worker_that_paints_late_is_waited_for() {
    let late = Duration::from_secs(4);
    let waited = idle_with(Screen1455::Map, Rule::Shown, Settle::Events, Some(late));
    println!("{}", waited.line(Screen1455::Map, Rule::Shown));
    let clocked = idle_with(Screen1455::Map, Rule::Shown, Settle::Clock, Some(late));
    println!("{}", clocked.line(Screen1455::Map, Rule::Shown));
    assert!(
        waited.settled >= late.as_secs_f64() && !waited.timed_out,
        "settled before the late worker: {waited:?}"
    );
    assert_eq!(
        (
            waited.wakes,
            waited.messages.len(),
            waited.busy,
            waited.presents
        ),
        (0, 0, false, 0),
        "the late worker was counted: {waited:?}"
    );
    assert!(
        clocked.messages.iter().any(|(kind, _)| kind == "Painted") && clocked.presents > 0,
        "the clock's settle did not count the late worker: {clocked:?}"
    );
}
