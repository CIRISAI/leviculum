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
//! `LOQUAX_IDLE_FPS=1` runs it with the frame meter's overlay on (a
//! switch for the whole process: run the test alone, `--exact`).

use super::bench::map_window;
use super::bench::Window;
use super::tests::chat_model;
use super::*;
use leviculum_ui_iced::headless::{self, Program};
use loquax::ui::update;
use std::cell::RefCell;

/// How long the window is left alone, and how long before that it may
/// settle (its first frames, the map's first picture).
const IDLE: Duration = Duration::from_secs(5);
const SETTLE: Duration = Duration::from_millis(1500);

/// The window size: the goldens', so the debug build stays quick.
const SIZE: (u32, u32) = (900, 560);

/// What the emulated shell sends the window.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Rule {
    /// As `drive` does since 1455 ([`Sent`]).
    Shown,
    /// Before 1455: every state that differs, its clock's second too.
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
    sent: Sent,
    last: Option<Seen>,
    start: Instant,
    ticks: u32,
    /// When the window was handed a state, seconds since the start.
    wakes: Vec<f64>,
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
            let mut seen = Seen {
                pins: self.model.map_pins(),
                tracks: self.model.track_input(),
                track_window: self.model.track_window,
                scene: self.model.scene(LayoutClass::Wide),
                now: self.model.now,
                receipts: self.model.reading.on,
                photo_size: self.model.photo_size,
                ..self.template.clone()
            };
            if self.rule == Rule::Loading {
                seen.loading = Some(format!("Loading Florence, {secs} s"));
            }
            let offered = match self.rule {
                Rule::Shown | Rule::Loading => self.sent.offer(seen),
                Rule::EveryChange => match self.last.as_ref() == Some(&seen) {
                    true => None,
                    false => {
                        self.last = Some(seen.clone());
                        Some(seen)
                    }
                },
            };
            if let Some(seen) = offered {
                self.wakes.push(self.start.elapsed().as_secs_f64());
                out = Some(seen);
            }
        }
        out
    }
}

/// The window with the shell's half beside it.
struct Idle {
    window: Window,
    shell: RefCell<Shell>,
    /// Every message the window was handed: when, seconds since the
    /// start, and its kind.
    handed: Vec<(f64, String)>,
    /// When the drive is let end though the window still waits on its
    /// worker, and whether it had to be.
    deadline: Duration,
    forced: std::cell::Cell<bool>,
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
        let mut out = self.window.pump(drawn);
        if let Some(seen) = self.shell.borrow_mut().pump() {
            out.push(Message::Seen(Box::new(seen)));
        }
        out
    }

    fn idle(&self) -> bool {
        if self.window.idle() {
            return true;
        }
        let late = self.shell.borrow().start.elapsed() >= self.deadline;
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

/// What five idle seconds did, after the settle.
#[derive(Debug)]
struct Quiet {
    /// States the shell handed the window.
    wakes: usize,
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
            "IDLE_WINDOW screen={screen:?} rule={rule:?} seconds={} wakes={} messages={:?} busy={} redraws_unpresented={} presents={} max_damage={:.4} last_rects={}",
            IDLE.as_secs(),
            self.wakes,
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

fn idle(screen: Screen1455, rule: Rule) -> Quiet {
    let (w, h) = SIZE;
    let (app, model) = match screen {
        Screen1455::Chat | Screen1455::Typing => {
            let model = chat_model();
            let (tx, _) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
            app.window = Size::new(w as f32, h as f32);
            app.apply(Message::Seen(Box::new(super::tests::seen_of(&model))));
            (app, model)
        }
        Screen1455::Map => (map_window(w, h).0, chat_model()),
    };
    let template = app.seen.clone();
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
            sent: Sent::default(),
            last: None,
            start: Instant::now(),
            ticks: 0,
            wakes: Vec::new(),
        }),
        handed: Vec::new(),
        deadline: SETTLE + IDLE + Duration::from_secs(2),
        forced: std::cell::Cell::new(false),
    };
    let end = SETTLE + IDLE;
    // Typing: a press into the compose field while the window settles,
    // and one on the list's empty foot after the idle seconds, which
    // takes the focus and ends the blink, as the bench's idle case does.
    let field = iced::Point::new(w as f32 - 250.0, h as f32 - 28.0);
    let list = iced::Point::new(200.0, h as f32 - 40.0);
    let at = |p| iced::Event::Mouse(mouse::Event::CursorMoved { position: p });
    let down = iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
    let up = iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
    let mut steps: Vec<(Duration, iced::Event)> = match screen {
        Screen1455::Typing => vec![
            (SETTLE / 3, at(field)),
            (SETTLE / 3, down.clone()),
            (SETTLE / 3, up.clone()),
            (end, at(list)),
            (end, down),
            (end, up),
        ],
        _ => Vec::new(),
    };
    steps.reverse();
    let mut script = |now: Duration| -> Option<Vec<iced::Event>> {
        let mut events = Vec::new();
        while steps.last().is_some_and(|(t, _)| *t <= now) {
            if let Some((_, event)) = steps.pop() {
                events.push(event);
            }
        }
        match now >= end && steps.is_empty() && events.is_empty() {
            true => None,
            false => Some(events),
        }
    };
    let run = headless::drive(&mut program, w, h, &mut script, SETTLE).expect("a renderer");
    let (from, until) = (SETTLE.as_secs_f64(), end.as_secs_f64());
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
    Quiet {
        wakes: shell.wakes.iter().filter(|&&t| inside(t)).count(),
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
                assert_eq!((quiet.wakes, quiet.messages.len()), (0, 0), "{quiet:?}");
                assert!((1..=12).contains(&quiet.presents), "the blink: {quiet:?}");
                assert!(quiet.damage < 0.1, "the blink drew the window: {quiet:?}");
            }
            (_, Rule::Shown) => assert_eq!(
                (
                    quiet.wakes,
                    quiet.messages.len(),
                    quiet.busy,
                    quiet.unpresented,
                    quiet.presents
                ),
                (0, 0, false, 0, 0),
                "{screen:?} idle: {quiet:?}"
            ),
            (_, Rule::EveryChange) => assert!(
                quiet.wakes >= 4 && quiet.unpresented + quiet.presents >= 4,
                "the control woke nothing: {quiet:?}"
            ),
            (_, Rule::Loading) => {
                assert!(quiet.presents >= 4, "the control drew nothing: {quiet:?}")
            }
        }
    }
}
