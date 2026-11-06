//! The window bench (1441, `just window-bench`): the gestures Lew's window
//! stutters at, replayed in real time through iced's user interface as
//! its loop runs on X11 ([`leviculum_ui_iced::headless::drive`]), the
//! frames presented as the software renderer's compositor presents them,
//! damage and all; at three window sizes. One `WINDOW_BENCH` line a size
//! and gesture: frames a second while it runs, a frame's time (mean, 95th
//! percentile, longest, all on the window's thread: update, view and draw,
//! present), the parts' means, the share of the window rasterised, and
//! the views built and messages handled per frame.
//!
//! Run with `LOQUAX_WINDOW_BENCH` naming the file the lines go to:
//! `LOQUAX_WINDOW_BENCH=target/window-bench/run.txt cargo test --release
//! -p loquax --features gui --bins window_bench -- --nocapture`; without
//! it the test returns at once. The map gestures run over the Florence
//! fixture, or over the regions in `LOQUAX_WINDOW_BENCH_MAPS`.

use super::tests::{long_chat, window_over_florence};
use super::*;
use iced::advanced::Layout;
use leviculum_ui_iced::headless::{self, DrawnFrame, Program};
use std::sync::mpsc;

/// The window sizes: the one the goldens use, Lew's (2026-11-06), a
/// full HD screen.
const SIZES: [(u32, u32); 3] = [(900, 560), (1280, 1190), (1920, 1080)];

/// When the gesture begins, after the first frames settled the window.
const SETTLE: Duration = Duration::from_millis(400);

/// A mouse's report interval: 125 a second.
const MOUSE: Duration = Duration::from_millis(8);

/// The window with its worker as iced runs them: the map's frames drawn
/// on a thread, taken in as the loop's messages.
struct Window {
    app: App,
    jobs: mpsc::Sender<Order>,
    done: mpsc::Receiver<Arc<Shown>>,
    /// A frame from the worker came since the last frame presented.
    fresh: std::cell::Cell<bool>,
}

impl Window {
    fn new(app: App) -> Self {
        let (jobs, orders) = mpsc::channel::<Order>();
        let (painted, done) = mpsc::channel();
        let frames = app.frames.clone();
        std::thread::spawn(move || {
            for order in orders {
                if painted.send(draw(&frames, order)).is_err() {
                    break;
                }
            }
        });
        Self {
            app,
            jobs,
            done,
            fresh: std::cell::Cell::new(false),
        }
    }

    /// The worker's next job, as [`App::paint`] hands it to iced's pool;
    /// no prefetch.
    fn dispatch(&mut self) {
        match self.app.next_job() {
            Some(Job::Draw(order)) => {
                let _ = self.jobs.send(*order);
            }
            Some(Job::Prefetch(_)) => self.app.busy = false,
            None => {}
        }
    }
}

impl Program for Window {
    type Message = Message;

    fn view(&self) -> Element<'_, Message> {
        self.app.view()
    }

    fn update(&mut self, message: Message) {
        // The map's messages as the update folds them, the worker's job
        // taken here; the others through the update, whose tasks (a
        // scroll's operation, a timer) the bench does not run.
        let map = matches!(
            message,
            Message::Pointer(_)
                | Message::Painted(_)
                | Message::Frame(_)
                | Message::WheelRest
                | Message::MapSize(_)
                | Message::Prefetched(_)
        );
        if matches!(message, Message::Painted(_)) {
            self.fresh.set(true);
        }
        match map {
            true => {
                if self.app.apply(message) {
                    self.dispatch();
                }
            }
            false => {
                let _ = self.app.update(message);
            }
        }
    }

    fn pump(&self, drawn: bool) -> Vec<Message> {
        let mut out: Vec<Message> = self.done.try_iter().map(Message::Painted).collect();
        if self
            .app
            .wheel
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(WHEEL_REST_MS))
        {
            out.push(Message::WheelRest);
        }
        // `window::frames` while the glide runs or a frame is owed.
        if drawn && self.app.screen == Screen::Map && (self.app.animating() || self.app.owed) {
            out.push(Message::Frame(Instant::now()));
        }
        out
    }

    fn idle(&self) -> bool {
        !self.app.busy && self.app.wheel.is_none() && !self.app.animating() && !self.app.owed
    }

    fn marked(&self) -> bool {
        self.fresh.replace(false)
    }

    fn style(&self) -> (Theme, Color, Color) {
        (
            self.app.theme(),
            chat::colour(self.app.tokens().text),
            chat::colour(self.app.tokens().background),
        )
    }
}

/// The map window `w` by `h` over the Florence fixture (or the regions
/// of `LOQUAX_WINDOW_BENCH_MAPS`), at z15 over Piazza d'Azeglio, and
/// where its map canvas lies.
fn map_window(w: u32, h: u32) -> (App, Rectangle) {
    let mut app = window_over_florence(Vec::new(), 15 * MILLI);
    if let Some(dir) = std::env::var_os("LOQUAX_WINDOW_BENCH_MAPS") {
        let frames = Frames::new(loquax::picture::Maps::new(Some(dir.into())));
        app.seen.maps = frames.painter().epoch();
        app.frames = Arc::new(Mutex::new(frames));
    }
    app.window = Size::new(w as f32, h as f32);
    let node = headless::layout(app.view(), w, h).expect("a renderer");
    // The rail, then the map screen: the column, then the map over its bar.
    let map = Layout::new(&node)
        .children()
        .nth(1)
        .and_then(|screen| screen.children().nth(1))
        .and_then(|column| column.children().next())
        .expect("the map")
        .bounds();
    (app, map)
}

/// A script of events at times since the start; `None` once its last is
/// handed out.
fn script(
    mut timed: Vec<(Duration, iced::Event)>,
) -> impl FnMut(Duration) -> Option<Vec<iced::Event>> {
    timed.sort_by_key(|(t, _)| *t);
    let mut next = 0;
    move |now| {
        if next >= timed.len() {
            return None;
        }
        let from = next;
        while next < timed.len() && timed[next].0 <= now {
            next += 1;
        }
        Some(timed[from..next].iter().map(|(_, e)| e.clone()).collect())
    }
}

fn moved(p: Point) -> iced::Event {
    iced::Event::Mouse(mouse::Event::CursorMoved { position: p })
}

fn button(down: bool) -> iced::Event {
    iced::Event::Mouse(match down {
        true => mouse::Event::ButtonPressed(mouse::Button::Left),
        false => mouse::Event::ButtonReleased(mouse::Button::Left),
    })
}

fn wheel(y: f32) -> iced::Event {
    iced::Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Lines { x: 0.0, y },
    })
}

/// A drag from `from` by `step` a mouse report for `span`: the pointer
/// there first, a settle, the press, the moves, the release.
fn drag(from: Point, step: iced::Vector, span: Duration) -> Vec<(Duration, iced::Event)> {
    let mut out = vec![(Duration::ZERO, moved(from)), (SETTLE, button(true))];
    let n = (span.as_micros() / MOUSE.as_micros()) as u32;
    for i in 1..=n {
        let p = from + step * i as f32;
        out.push((SETTLE + MOUSE * i, moved(p)));
    }
    out.push((SETTLE + MOUSE * (n + 1), button(false)));
    out
}

/// One gesture.
#[derive(Clone, Copy, Debug)]
enum Case {
    MapDrag,
    MapWheel,
    ChatWheel,
    ChatScrollbar,
    ChatDrag,
    ChatTouchpad,
}

impl Case {
    const ALL: [Case; 6] = [
        Case::MapDrag,
        Case::MapWheel,
        Case::ChatWheel,
        Case::ChatScrollbar,
        Case::ChatDrag,
        Case::ChatTouchpad,
    ];

    fn name(self) -> &'static str {
        match self {
            Case::MapDrag => "map-drag",
            Case::MapWheel => "map-wheel",
            Case::ChatWheel => "chat-wheel",
            Case::ChatScrollbar => "chat-scrollbar",
            Case::ChatDrag => "chat-drag",
            Case::ChatTouchpad => "chat-touchpad",
        }
    }

    /// The window and the gesture at `w` by `h`.
    fn setup(self, w: u32, h: u32) -> (App, Vec<(Duration, iced::Event)>) {
        let (wf, hf) = (w as f32, h as f32);
        let two = Duration::from_secs(2);
        match self {
            // Two seconds of a drag to the left, 4 pixels a report (500
            // pixels a second), from the map's right half.
            Case::MapDrag => {
                let (app, map) = map_window(w, h);
                let from = Point::new(map.x + map.width * 0.75, map.y + map.height / 2.0);
                (app, drag(from, iced::Vector::new(-4.0, 0.0), two))
            }
            // Five notches in, one every 120 ms, at the map's middle;
            // the glide and the rest after them.
            Case::MapWheel => {
                let (app, map) = map_window(w, h);
                let at = Point::new(map.x + map.width / 2.0, map.y + map.height / 2.0);
                let mut events = vec![(Duration::ZERO, moved(at))];
                for i in 0..5u32 {
                    events.push((SETTLE + Duration::from_millis(120) * i, wheel(1.0)));
                }
                (app, events)
            }
            // Two seconds of a wheel turned fast, a notch every 50 ms,
            // back into the 3000 messages.
            Case::ChatWheel => {
                let at = Point::new(wf - 200.0, hf / 2.0);
                let mut events = vec![(Duration::ZERO, moved(at))];
                for i in 0..40u32 {
                    events.push((SETTLE + Duration::from_millis(50) * i, wheel(1.0)));
                }
                (long_chat(w, h), events)
            }
            // The scrollbar at the window's right edge taken at the
            // middle and dragged up for two seconds, 2 pixels a report.
            Case::ChatScrollbar => {
                let from = Point::new(wf - 4.0, hf / 2.0);
                (
                    long_chat(w, h),
                    drag(from, iced::Vector::new(0.0, -2.0), two),
                )
            }
            // The messages themselves pressed and dragged down for two
            // seconds, 2 pixels a report, as Lew drags the chat. iced's
            // scrollable scrolls on a finger's drag, not on a mouse's:
            // this scrolls nothing and costs what the widgets do with
            // every move over 3000 bubbles.
            Case::ChatDrag => {
                let from = Point::new(wf - 200.0, hf / 3.0);
                (
                    long_chat(w, h),
                    drag(from, iced::Vector::new(0.0, 2.0), two),
                )
            }
            // The chat dragged on a touchpad, two fingers: two seconds of
            // pixel scrolls, 4 pixels a report, back into the messages.
            Case::ChatTouchpad => {
                let at = Point::new(wf - 200.0, hf / 2.0);
                let mut events = vec![(Duration::ZERO, moved(at))];
                let n = (two.as_micros() / MOUSE.as_micros()) as u32;
                for i in 0..n {
                    events.push((
                        SETTLE + MOUSE * i,
                        iced::Event::Mouse(mouse::Event::WheelScrolled {
                            delta: mouse::ScrollDelta::Pixels { x: 0.0, y: 4.0 },
                        }),
                    ));
                }
                (long_chat(w, h), events)
            }
        }
    }
}

/// What a case measured, over the frames from the gesture's start.
#[derive(Debug)]
pub(super) struct Measured {
    pub size: (u32, u32),
    pub case: &'static str,
    pub fps: f64,
    pub mean: f64,
    pub p95: f64,
    pub max: f64,
    pub parts: (f64, f64, f64),
    pub damage: f64,
    /// The present of a frame showing a map frame new from the worker,
    /// and of one showing the frame on screen moved; zero without.
    pub fresh: f64,
    pub held: f64,
    pub views: f64,
    pub messages: f64,
    pub frames: usize,
    /// The first and the last frame's pixels differ: the gesture moved
    /// something.
    pub moved: bool,
}

impl Measured {
    pub fn line(&self) -> String {
        format!(
            "WINDOW_BENCH size={}x{} case={} fps={:.0} frame_ms_mean={:.2} p95={:.2} max={:.2} update={:.2} draw={:.2} present={:.2} damage={:.2} present_fresh={:.2} present_held={:.2} views_per_frame={:.2} messages_per_frame={:.2} frames={} moved={}",
            self.size.0,
            self.size.1,
            self.case,
            self.fps,
            self.mean,
            self.p95,
            self.max,
            self.parts.0,
            self.parts.1,
            self.parts.2,
            self.damage,
            self.fresh,
            self.held,
            self.views,
            self.messages,
            self.frames,
            self.moved,
        )
    }
}

/// `case` at `w` by `h`, run.
fn measure(case: Case, w: u32, h: u32) -> Measured {
    let (app, events) = case.setup(w, h);
    let mut window = Window::new(app);
    let mut script = script(events);
    let run =
        headless::drive(&mut window, w, h, &mut script, SETTLE).expect("the software renderer");
    let at = SETTLE.as_secs_f64();
    let frames: Vec<&DrawnFrame> = run.frames.iter().filter(|f| f.at >= at).collect();
    let n = frames.len().max(1);
    let mut totals: Vec<f64> = frames.iter().map(|f| f.total_ms()).collect();
    totals.sort_by(f64::total_cmp);
    let mean =
        |f: fn(&DrawnFrame) -> f64| 0.0 + frames.iter().map(|x| f(x)).sum::<f64>() / n as f64;
    let present_of = |marked: bool| {
        let p: Vec<f64> = frames
            .iter()
            .filter(|f| f.marked == marked)
            .map(|f| f.present_ms)
            .collect();
        0.0 + p.iter().sum::<f64>() / p.len().max(1) as f64
    };
    let span = frames.last().map_or(0.0, |f| f.at - at).max(1e-3);
    let pick = |q: usize| {
        totals
            .get((totals.len() * q / 100).min(totals.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0.0)
    };
    Measured {
        size: (w, h),
        case: case.name(),
        fps: frames.len() as f64 / span,
        mean: 0.0 + totals.iter().sum::<f64>() / n as f64,
        p95: pick(95),
        max: totals.last().copied().unwrap_or(0.0),
        parts: (
            mean(|f| f.update_ms),
            mean(|f| f.draw_ms),
            mean(|f| f.present_ms),
        ),
        damage: mean(|f| f64::from(f.damage)),
        fresh: present_of(true),
        held: present_of(false),
        views: frames.iter().filter(|f| f.rebuilt).count() as f64 / n as f64,
        messages: run.messages as f64 / n as f64,
        frames: frames.len(),
        moved: run.marked_rgb.as_ref().is_some_and(|f| f != &run.rgb),
    }
}

/// The bench (`just window-bench`): every gesture at every size, the
/// lines to `LOQUAX_WINDOW_BENCH`'s file and to stdout.
#[test]
fn window_bench() {
    let Some(out) = std::env::var_os("LOQUAX_WINDOW_BENCH") else {
        return;
    };
    let mut lines = Vec::new();
    for (w, h) in SIZES {
        for case in Case::ALL {
            let line = measure(case, w, h).line();
            println!("{line}");
            lines.push(line);
        }
    }
    std::fs::write(&out, lines.join("\n") + "\n").expect("the bench's file");
}

/// The bench's gestures do what they say (1441), at the small size: the
/// map's drag and wheel move the map, the chat's wheel and its scrollbar
/// scroll the messages, each in frames that the window drew; the drag on
/// the messages is measured, whatever it moves. And a wheel's notch on the
/// chat builds no view per frame, where the scrollbar's drag is a message
/// to the update per mouse report or not, as the line says.
#[test]
fn the_benchs_gestures_move_the_window() {
    for case in Case::ALL {
        let m = measure(case, 900, 560);
        println!("{}", m.line());
        assert!(
            m.frames > 3 || matches!(case, Case::ChatDrag),
            "{}",
            m.line()
        );
        if !matches!(case, Case::ChatDrag) {
            assert!(m.moved, "{}", m.line());
        }
    }
}
