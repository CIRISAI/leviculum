//! The window bench (1441, `just window-bench`): the gestures Lew's window
//! stutters at, replayed in real time through iced's user interface as
//! its loop runs on X11 ([`leviculum_ui_iced::headless::drive`]), the
//! frames presented as the software renderer's compositor presents them,
//! damage and all; at four window sizes. The chat's gestures run over two
//! conversations, 1441's 3000 one-line bubbles (`chat-*`) and one shaped
//! like Lew's real chats (`real-*`, 1456), and the idle case counts the
//! compose field's blink frames; at Lew's tall size every gesture runs a
//! second time with the frame meter's overlay on (`+fps`). The overlay is
//! a switch for the whole process: only `window_bench` turns it on, and
//! it runs alone (`--exact`). One `WINDOW_BENCH` line a size
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

use super::tests::{
    long_chat, long_chat_of, real_chat, real_chat_grown, seen_of, window_over_florence, ANNA,
};
use super::*;
use iced::advanced::Layout;
use leviculum_ui_iced::headless::{self, DrawnFrame, Program};
use loquax::host_load::HostLoad;
use std::sync::mpsc;

/// The window sizes: the one the goldens use, Lew's three (2026-11-06,
/// the second his window on the tall screen where it ran at 6 to 7
/// frames a second, 1456, the third the same screen's window a little
/// wider, where the CPU track's bar is set, 1442), a full HD screen.
const SIZES: [(u32, u32); 5] = [
    (900, 560),
    (1280, 1190),
    (1276, 2099),
    (1527, 2099),
    (1920, 1080),
];

/// The sizes the gestures run at a second time with the frame meter's
/// overlay on (`--fps`, 1456): Lew's tall windows, where he read it.
const OVERLAY_SIZES: [(u32, u32); 2] = [(1276, 2099), (1527, 2099)];

/// When the gesture begins, after the first frames settled the window.
const SETTLE: Duration = Duration::from_millis(400);

/// A mouse's report interval: 125 a second.
const MOUSE: Duration = Duration::from_millis(8);

/// The window with its worker as iced runs them: the map's frames drawn
/// on a thread, taken in as the loop's messages.
pub(super) struct Window {
    pub(super) app: App,
    jobs: mpsc::Sender<Order>,
    done: mpsc::Receiver<Arc<Shown>>,
    /// A frame from the worker came since the last frame presented.
    fresh: std::cell::Cell<bool>,
}

impl Window {
    pub(super) fn new(app: App) -> Self {
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
    pub(super) fn dispatch(&mut self) {
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
pub(super) fn map_window(w: u32, h: u32) -> (App, Rectangle) {
    let mut app = window_over_florence(Vec::new(), 15 * MILLI);
    if let Some(dir) = std::env::var_os("LOQUAX_WINDOW_BENCH_MAPS") {
        let frames = Frames::new(loquax::picture::Maps::new(Some(dir.into())));
        app.seen.maps = frames.painter().epoch();
        app.frames = Arc::new(Mutex::new(frames));
    }
    app.window = Size::new(w as f32, h as f32);
    // The window without the frame meter's overlay, which stacks over it
    // and would move the path below (1456).
    let node = headless::layout(app.window_view(), w, h).expect("a renderer");
    // The rail, then the map screen: the map over its bar (1430).
    let map = Layout::new(&node)
        .children()
        .nth(1)
        .and_then(|screen| screen.children().next())
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

/// The step a [`headless::Paced`] clock moves by a redraw in the tests
/// that count frames ([`a_scroll_drawn_as_a_shift_is_the_scroll_drawn_whole`],
/// [`the_benchs_gestures_move_the_window`]): the chat wheel's notch
/// interval, so a frame sees one notch.
const PACE: Duration = Duration::from_millis(50);

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

/// The conversation a chat gesture runs over.
#[derive(Clone, Copy, Debug)]
enum Chat {
    /// 3000 one-line bubbles ([`long_chat`]), 1441's.
    Long,
    /// A few hundred long, wrapping messages with quotes and wide bubbles
    /// of ours, shaped like Lew's ([`real_chat`], 1456).
    Real,
}

impl Chat {
    fn open(self, w: u32, h: u32) -> App {
        match self {
            Chat::Long => long_chat(w, h),
            Chat::Real => real_chat(w, h),
        }
    }
}

/// One gesture.
#[derive(Clone, Copy, Debug)]
enum Case {
    MapDrag,
    MapWheel,
    ChatWheel(Chat),
    ChatScrollbar(Chat),
    ChatDrag(Chat),
    ChatTouchpad(Chat),
    /// The compose field focused and nothing else: the cursor's blink, two
    /// frames a second (1456).
    ChatIdle(Chat),
}

/// From the idle case's settle, when the blink frames are counted, and
/// when the focus leaves the field again.
const IDLE_FROM: Duration = Duration::from_millis(600);
const IDLE_UNTIL: Duration = Duration::from_millis(3000);

/// The blink frames the idle case holds the focus for in the test
/// ([`Held`]): one more than [`the_benchs_gestures_move_the_window`]'s
/// three.
const IDLE_FRAMES: usize = 4;

/// How long past [`IDLE_UNTIL`] a [`Held`] clock holds at most: a guard
/// against a window that never blinks, which would hold it for ever; the
/// test's count then says the frames that came.
const HOLD_AT_MOST: Duration = Duration::from_secs(60);

/// The wall's clock, but held just short of `until` until `least` frames
/// were presented since `from` (1619): the idle case's frames counted by
/// the blinks the window drew, not by a window of wall seconds, which a
/// frame of a second on a loaded host (`chat-drag frame_ms_mean=1200`
/// beside 1100 tests) can leave empty. Unloaded the blink draws its
/// frames inside the wall's window and the hold never begins.
struct Held {
    start: Instant,
    from: Duration,
    until: Duration,
    least: usize,
    seen: usize,
}

impl Held {
    fn new(from: Duration, until: Duration, least: usize) -> Self {
        Self {
            start: Instant::now(),
            from,
            until,
            least,
            seen: 0,
        }
    }
}

impl headless::Clock for Held {
    fn start(&mut self) {
        self.start = Instant::now();
    }

    fn now(&self) -> Duration {
        let wall = self.start.elapsed();
        let hold = self.seen < self.least && wall < self.until + HOLD_AT_MOST;
        match hold && wall >= self.until {
            true => self.until - Duration::from_micros(1),
            false => wall,
        }
    }

    fn presented(&mut self, at: Duration) {
        if at >= self.from && at < self.until {
            self.seen += 1;
        }
    }
}

/// The clock a case is measured on.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pace {
    /// The wall's: the bench's measurements (frames a second are a wall's).
    Wall,
    /// The test's: a gesture paced by the window's redraws
    /// ([`headless::Paced`], [`PACE`] a redraw), the idle case's blink
    /// held for [`IDLE_FRAMES`] frames ([`Held`]).
    Test,
}

impl Case {
    const ALL: [Case; 12] = [
        Case::MapDrag,
        Case::MapWheel,
        Case::ChatWheel(Chat::Long),
        Case::ChatScrollbar(Chat::Long),
        Case::ChatDrag(Chat::Long),
        Case::ChatTouchpad(Chat::Long),
        Case::ChatIdle(Chat::Long),
        Case::ChatWheel(Chat::Real),
        Case::ChatScrollbar(Chat::Real),
        Case::ChatDrag(Chat::Real),
        Case::ChatTouchpad(Chat::Real),
        Case::ChatIdle(Chat::Real),
    ];

    fn name(self) -> &'static str {
        match self {
            Case::MapDrag => "map-drag",
            Case::MapWheel => "map-wheel",
            Case::ChatWheel(Chat::Long) => "chat-wheel",
            Case::ChatScrollbar(Chat::Long) => "chat-scrollbar",
            Case::ChatDrag(Chat::Long) => "chat-drag",
            Case::ChatTouchpad(Chat::Long) => "chat-touchpad",
            Case::ChatIdle(Chat::Long) => "chat-idle",
            Case::ChatWheel(Chat::Real) => "real-wheel",
            Case::ChatScrollbar(Chat::Real) => "real-scrollbar",
            Case::ChatDrag(Chat::Real) => "real-drag",
            Case::ChatTouchpad(Chat::Real) => "real-touchpad",
            Case::ChatIdle(Chat::Real) => "real-idle",
        }
    }

    /// The frames measured: from when, until when (the end without).
    fn span(self) -> (Duration, Option<Duration>) {
        match self {
            Case::ChatIdle(_) => (SETTLE + IDLE_FROM, Some(SETTLE + IDLE_UNTIL)),
            _ => (SETTLE, None),
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
            Case::ChatWheel(chat) => {
                let at = Point::new(wf - 200.0, hf / 2.0);
                let mut events = vec![(Duration::ZERO, moved(at))];
                for i in 0..40u32 {
                    events.push((SETTLE + Duration::from_millis(50) * i, wheel(1.0)));
                }
                (chat.open(w, h), events)
            }
            // The scrollbar at the window's right edge taken at the
            // middle and dragged up for two seconds, 2 pixels a report.
            Case::ChatScrollbar(chat) => {
                let from = Point::new(wf - 4.0, hf / 2.0);
                (
                    chat.open(w, h),
                    drag(from, iced::Vector::new(0.0, -2.0), two),
                )
            }
            // The messages themselves pressed and dragged down for two
            // seconds, 2 pixels a report, as Lew drags the chat. iced's
            // scrollable scrolls on a finger's drag, not on a mouse's:
            // this scrolls nothing and costs what the widgets do with
            // every move over 3000 bubbles.
            Case::ChatDrag(chat) => {
                let from = Point::new(wf - 200.0, hf / 3.0);
                (
                    chat.open(w, h),
                    drag(from, iced::Vector::new(0.0, 2.0), two),
                )
            }
            // The chat dragged on a touchpad, two fingers: two seconds of
            // pixel scrolls, 4 pixels a report, back into the messages.
            Case::ChatTouchpad(chat) => {
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
                (chat.open(w, h), events)
            }
            // A press into the compose field at the window's foot, then
            // nothing for longer than the frames counted (the cursor
            // blinks), then a press on the chat list's empty foot, which
            // takes the focus and ends the blinking.
            Case::ChatIdle(chat) => {
                let field = Point::new(wf - 250.0, hf - 28.0);
                let list = Point::new(200.0, hf - 40.0);
                let events = vec![
                    (Duration::ZERO, moved(field)),
                    (SETTLE, button(true)),
                    (SETTLE + MOUSE, button(false)),
                    (SETTLE + IDLE_UNTIL, moved(list)),
                    (SETTLE + IDLE_UNTIL + MOUSE, button(true)),
                    (SETTLE + IDLE_UNTIL + MOUSE * 2, button(false)),
                ];
                (chat.open(w, h), events)
            }
        }
    }
}

/// What a case measured, over the frames from the gesture's start.
#[derive(Debug)]
pub(super) struct Measured {
    pub size: (u32, u32),
    /// The gesture's name, `+fps` with the frame meter's overlay on.
    pub case: String,
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

/// `case` at `w` by `h`, run; with the frame meter's overlay over the
/// window when `overlay`, as `loquax --fps` draws it.
fn measure(case: Case, w: u32, h: u32, overlay: bool) -> Measured {
    measure_slowed(case, w, h, overlay, 1)
}

/// [`measure`] with the gesture's clock `slow` times slower: its events
/// that much further apart, for a profiler that runs the program that
/// much slower (1468).
fn measure_slowed(case: Case, w: u32, h: u32, overlay: bool, slow: u32) -> Measured {
    measure_on(case, w, h, overlay, slow, Pace::Wall)
}

/// [`measure_slowed`] on the clock `pace` says: the wall's for the bench,
/// the test's own for [`the_benchs_gestures_move_the_window`] (1619).
fn measure_on(case: Case, w: u32, h: u32, overlay: bool, slow: u32, pace: Pace) -> Measured {
    let (app, events) = case.setup(w, h);
    let events: Vec<_> = events.into_iter().map(|(t, e)| (t * slow, e)).collect();
    let (from, until) = case.span();
    let (from, until) = (from * slow, until.map(|u| u * slow));
    let mut clock: Box<dyn headless::Clock> = match (pace, until) {
        (Pace::Wall, _) => Box::new(headless::Wall::new()),
        (Pace::Test, Some(until)) => Box::new(Held::new(from, until, IDLE_FRAMES)),
        (Pace::Test, None) => Box::new(headless::Paced::new(
            events.iter().map(|(t, _)| *t).collect(),
            PACE * slow,
        )),
    };
    let mut window = Window::new(app);
    let mut script = script(events);
    if overlay {
        leviculum_ui_iced::fps::enable(|_| {});
    }
    let run = headless::drive_on(
        &mut window,
        w,
        h,
        clock.as_mut(),
        &mut script,
        SETTLE * slow,
    );
    if overlay {
        leviculum_ui_iced::fps::disable();
    }
    let run = run.expect("the software renderer");
    let at = from.as_secs_f64();
    let until = until.map_or(f64::INFINITY, |u| u.as_secs_f64());
    let frames: Vec<&DrawnFrame> = run
        .frames
        .iter()
        .filter(|f| f.at >= at && f.at < until)
        .collect();
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
    let span = match case.span().1 {
        Some(_) => until - at,
        None => frames.last().map_or(0.0, |f| f.at - at),
    }
    .max(1e-3);
    let pick = |q: usize| {
        totals
            .get((totals.len() * q / 100).min(totals.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0.0)
    };
    Measured {
        size: (w, h),
        case: format!("{}{}", case.name(), if overlay { "+fps" } else { "" }),
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
    let runs = SIZES
        .iter()
        .flat_map(|&size| Case::ALL.map(|case| (size, case, false)))
        .chain(
            OVERLAY_SIZES
                .iter()
                .flat_map(|&size| Case::ALL.map(|case| (size, case, true))),
        );
    for ((w, h), case, overlay) in runs {
        let line = measure(case, w, h, overlay).line();
        println!("{line}");
        lines.push(line);
    }
    std::fs::write(&out, lines.join("\n") + "\n").expect("the bench's file");
}

/// One gesture of the bench alone, for a profiler (1468): the case
/// `LOQUAX_PROFILE_CASE` names (`chat-touchpad`, `+fps` for the overlay)
/// at `LOQUAX_PROFILE_SIZE` (`1527x2099` without), its clock slowed
/// `LOQUAX_PROFILE_SLOW` times (a profiler's own slowness), its line
/// printed.
/// Under callgrind, `--dump-after='*headless*Surface*present*'` writes a
/// profile per frame. Without the variable the test returns at once.
#[test]
fn one_gesture_for_a_profiler() {
    let Some(name) = std::env::var("LOQUAX_PROFILE_CASE").ok() else {
        return;
    };
    let (w, h) = std::env::var("LOQUAX_PROFILE_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1527, 2099));
    let overlay = name.ends_with("+fps");
    let bare = name.trim_end_matches("+fps");
    let case = Case::ALL
        .into_iter()
        .find(|c| c.name() == bare)
        .expect("a case of the bench");
    let slow = std::env::var("LOQUAX_PROFILE_SLOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    println!("{}", measure_slowed(case, w, h, overlay, slow).line());
}

/// The bench's gestures do what they say (1441), at the small size: the
/// map's drag and wheel move the map, the chat's wheel and its scrollbar
/// scroll the messages, each in frames that the window drew; the drag on
/// the messages is measured, whatever it moves. And a wheel's notch on the
/// chat builds no view per frame, where the scrollbar's drag is a message
/// to the update per mouse report or not, as the line says. The idle
/// case draws the cursor's blink frames. The real-shaped chat's gestures
/// run only in an optimised build: unoptimised, its first view (300 long
/// paragraphs shaped) takes longer than the gesture lasts, and the
/// release bench runs them (`the_real_chat_is_shaped_like_lews` holds the
/// fixture in every build). Every gesture counted draws more than three
/// frames, in either build: the gestures run on a clock the window's
/// redraws move ([`headless::Paced`]) and the idle case holds the focus
/// until the blink drew [`IDLE_FRAMES`] frames ([`Held`]), so a host
/// slowed by a parallel run stretches the test, not its count (1619:
/// `chat-idle frames=0` beside 1100 tests, where a wall window of 2.4 s
/// held no frame; before it an unoptimised gesture had to draw one frame
/// only, 1442). The drag on the messages runs on the wall's clock: it
/// asserts nothing, and paced it would build a view of 3000 bubbles for
/// each of its forty steps.
#[test]
fn the_benchs_gestures_move_the_window() {
    let optimised = !cfg!(debug_assertions);
    for case in Case::ALL {
        if !optimised
            && matches!(
                case,
                Case::ChatWheel(Chat::Real)
                    | Case::ChatScrollbar(Chat::Real)
                    | Case::ChatDrag(Chat::Real)
                    | Case::ChatTouchpad(Chat::Real)
                    | Case::ChatIdle(Chat::Real)
            )
        {
            continue;
        }
        let pace = match case {
            Case::ChatDrag(_) => Pace::Wall,
            _ => Pace::Test,
        };
        let m = measure_on(case, 900, 560, false, 1, pace);
        println!("{}", m.line());
        assert!(
            m.frames > 3 || matches!(case, Case::ChatDrag(_)),
            "{}",
            m.line()
        );
        if !matches!(case, Case::ChatDrag(_) | Case::ChatIdle(_)) {
            assert!(m.moved, "{}", m.line());
        }
    }
}

/// How far a channel of an anti-aliased edge's pixel may lie from the
/// twin's: a quarter of full coverage, one of the four rows tiny-skia
/// samples a pixel's row in.
const EDGE: u8 = 64;

/// How many pixels of a frame may lie more than one step from the twin's.
const EDGE_PIXELS: usize = 4;

/// A program driven with the host's load sampled at every pump.
pub(super) struct Watched<'a, P> {
    pub(super) inner: &'a mut P,
    pub(super) host: &'a HostLoad,
}

impl<P: Program> Program for Watched<'_, P> {
    type Message = P::Message;

    fn view(&self) -> Element<'_, P::Message> {
        self.inner.view()
    }

    fn update(&mut self, message: P::Message) {
        self.inner.update(message);
    }

    fn pump(&self, drawn: bool) -> Vec<P::Message> {
        self.host.sample();
        self.inner.pump(drawn)
    }

    fn idle(&self) -> bool {
        self.inner.idle()
    }

    fn style(&self) -> (Theme, Color, Color) {
        self.inner.style()
    }

    fn marked(&self) -> bool {
        self.inner.marked()
    }
}

/// A scroll drawn as a shift of the frame on the window (1468) is the
/// scroll drawn as the damage says, frame for frame, up to the partial
/// pixels of anti-aliased edges: the chat's wheel, touchpad and scrollbar
/// gestures run with a twin surface that never shifts, and in no frame
/// does a channel differ by more than [`EDGE`], nor more than
/// [`EDGE_PIXELS`] pixels by more than one step. tiny-skia does not draw
/// an edge's partial pixel to the same byte when a clip cuts the shape
/// (its mask rounds the coverage a second time) as when it is drawn
/// whole, nor a curve or a stroke at two heights (the rounding of its
/// path's points); a strip redrawn after a shift cuts the bubbles it
/// crosses, and the moved rows keep the edges where the last frame drew
/// them. Measured over three runs of every case: all but one pixel a
/// frame within one step, that one within 10 (the real-shaped chat's
/// scrollbar at 1527x2099). A shift by a wrong row, the control, moves
/// thousands of pixels by up to 228. And frames were shifted, else the
/// comparison held nothing. At the small size in every build; the
/// real-shaped chat and Lew's tall window where the build is optimised
/// (its first view is slow unoptimised, as above).
///
/// The gestures run on [`paced`]'s clock, not the wall's (1472): a frame
/// can only be a shift when the chat moved less than its pane since the
/// last one, and on the wall's clock an unoptimised build under load
/// drew the wheel's 40 notches in 2 frames, 20 notches past a 560-pixel
/// pane, so nothing shifted and the control failed on the machine's
/// speed, not on the renderer.
#[test]
fn a_scroll_drawn_as_a_shift_is_the_scroll_drawn_whole() {
    let optimised = !cfg!(debug_assertions);
    let mut runs = vec![(900, 560, Chat::Long)];
    if optimised {
        runs.extend([
            (900, 560, Chat::Real),
            (1527, 2099, Chat::Long),
            (1527, 2099, Chat::Real),
        ]);
    }
    for (w, h, chat) in runs {
        for case in [
            Case::ChatWheel(chat),
            Case::ChatTouchpad(chat),
            Case::ChatScrollbar(chat),
        ] {
            let (app, events) = case.setup(w, h);
            let mut window = Window::new(app);
            let mut clock = headless::Paced::new(events.iter().map(|(t, _)| *t).collect(), PACE);
            let mut script = script(events);
            let run = headless::drive_twinned(&mut window, w, h, &mut clock, &mut script, SETTLE)
                .expect("the software renderer");
            let what = format!(
                "{} at {w}x{h}: {} frames, {} shifted, at most {} pixels a frame off, {} by more than one step, by at most {}; more at {:?}, first within {:?}",
                case.name(),
                run.frames.len(),
                run.shifted,
                run.off,
                run.past_one,
                run.worst,
                run.differed,
                run.first_difference
            );
            println!("{what}");
            assert!(run.worst <= EDGE, "{what}");
            assert!(run.past_one <= EDGE_PIXELS, "{what}");
            // A scrollbar's drag over 3000 messages moves them further
            // than the pane is high with every report: nothing to keep.
            // The gestures are paced by the window's redraws (1472, 1619),
            // so how many frames they draw does not hang on the host's load.
            if !matches!(case, Case::ChatScrollbar(_)) {
                assert!(run.shifted > 0, "{what}");
            }
        }
    }
}

/// How many messages a first pull brings the window per state in
/// [`a_first_pull_never_costs_a_frame_over_the_budget`]: the shell folds
/// [`loquax::fanout::SLICE`] at a time and sends the window what changed
/// after each poll; on the host a 50 ms slice folded 16 to 35 events.
const PULL_SLICE: usize = 10;

/// A frame's budget while a first pull arrives (1464): one refresh of a
/// 60 Hz screen.
const REFRESH_BUDGET_MS: f64 = 1000.0 / 60.0;

/// The window as a first pull reaches it: after each frame drawn, the
/// next state the shell sent.
struct Pulling {
    window: Window,
    states: std::cell::RefCell<std::collections::VecDeque<Seen>>,
}

impl Program for Pulling {
    type Message = Message;

    fn view(&self) -> Element<'_, Message> {
        self.window.view()
    }

    fn update(&mut self, message: Message) {
        self.window.update(message);
    }

    fn pump(&self, drawn: bool) -> Vec<Message> {
        let mut out = self.window.pump(drawn);
        if drawn {
            if let Some(next) = self.states.borrow_mut().pop_front() {
                out.push(Message::Seen(Box::new(next)));
            }
        }
        out
    }

    fn idle(&self) -> bool {
        self.states.borrow().is_empty() && self.window.idle()
    }

    fn marked(&self) -> bool {
        self.window.marked()
    }

    fn style(&self) -> (Theme, Color, Color) {
        self.window.style()
    }
}

/// The first pull of the real-shaped chat never costs the window a frame
/// over the refresh budget (1464). Before 1464 the shell folded a whole
/// pull in one poll and the window saw nothing until all of it was in;
/// now it folds a slice per poll and sends the window each state: the
/// chat open, it grows [`PULL_SLICE`] messages a state, each drawn as it
/// comes. Every state arrives and the last frame shows the whole chat;
/// in an optimised build no frame of the pull takes longer than
/// [`REFRESH_BUDGET_MS`] (6.5 to 10 ms each at 900 by 560 on the host).
/// Counted in the drive's thread's CPU time (1473), where the wall clock
/// counted the host's other work too (590 ms frames at load 18, 1469),
/// and only on a quiet host ([`HostLoad`]): two tests beside it doubled
/// even the CPU time (9.8 to 18.2 ms, cores and caches shared), so under
/// load it prints that it did not measure. Where the thread's clock is
/// not told, the wall clock on the quiet host.
/// Unoptimised, a view of these 300 long paragraphs shaped takes longer
/// than a frame (as [`the_benchs_gestures_move_the_window`] says), so
/// the frame times are only printed there.
#[test]
fn a_first_pull_never_costs_a_frame_over_the_budget() {
    let (w, h) = (900, 560);
    let mut states = std::collections::VecDeque::new();
    let mut opened = false;
    let mut count = 0;
    let model = real_chat_grown(|model| {
        if !opened {
            loquax::ui::update(model, UiEvent::Open { peer: ANNA });
            opened = true;
        }
        count += 1;
        if count % PULL_SLICE == 0 {
            states.push_back(seen_of(model));
        }
    });
    states.push_back(seen_of(&model));
    let slices = states.len();
    let first = states.pop_front().expect("a state");
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
    app.window = Size::new(w as f32, h as f32);
    app.apply(Message::Seen(Box::new(first)));
    let mut pulling = Pulling {
        window: Window::new(app),
        states: std::cell::RefCell::new(states),
    };
    let mut script = |now: Duration| (now < SETTLE).then(Vec::new);
    // A frame's cost is the drive's thread's time on a CPU (1473): the
    // wall clock counted the host too, 590 ms frames at load 18 (1469).
    headless::measure_cpu(true);
    let host = HostLoad::start();
    let mut watched = Watched {
        inner: &mut pulling,
        host: &host,
    };
    let run =
        headless::drive(&mut watched, w, h, &mut script, SETTLE).expect("the software renderer");
    headless::measure_cpu(false);
    // The pull's frames: the first frame is the window opening on the
    // chat before the pull's first state, its present a fresh surface's
    // (14.6 ms of its 20.7 in the release run that set the budget).
    let longest = run
        .frames
        .iter()
        .skip(1)
        .map(DrawnFrame::total_ms)
        .fold(0.0, f64::max);
    // The CPU time where the kernel tells it, else the wall clock.
    let cpu: Option<f64> = run
        .frames
        .iter()
        .skip(1)
        .map(|f| f.cpu_ms)
        .try_fold(0.0, |most: f64, ms| ms.map(|ms| most.max(ms)));
    let views = run.frames.iter().filter(|f| f.rebuilt).count();
    println!(
        "FIRST_PULL states={slices} frames={} views={views} longest_ms={longest:.2} longest_cpu_ms={cpu:?}",
        run.frames.len()
    );
    assert!(
        pulling.states.borrow().is_empty(),
        "every state reached the window"
    );
    let chat = pulling
        .window
        .app
        .scene
        .chat
        .as_ref()
        .expect("the chat open");
    let bubbles: usize = chat.groups.iter().map(|g| g.bubbles.len()).sum();
    assert!(
        bubbles >= 300,
        "the whole chat at the end: {bubbles} bubbles"
    );
    assert!(
        views >= slices - 1,
        "a view per state: {views} for {slices}"
    );
    if !cfg!(debug_assertions) && host.measured("the first pull's frames") {
        let (ms, clock) = match cpu {
            Some(cpu) => (cpu, "of CPU"),
            None => (longest, "by the wall clock"),
        };
        assert!(
            ms < REFRESH_BUDGET_MS,
            "a frame of the first pull took {ms:.2} ms {clock} (budget {REFRESH_BUDGET_MS:.1} ms)"
        );
    }
}

/// The window with what its rows widgets did counted at every frame
/// drawn (1487): per frame, what [`leviculum_ui_iced::rows::counted`] says.
struct Counted {
    window: Window,
    frames: std::cell::RefCell<Vec<leviculum_ui_iced::rows::Counts>>,
}

impl Program for Counted {
    type Message = Message;

    fn view(&self) -> Element<'_, Message> {
        self.window.view()
    }

    fn update(&mut self, message: Message) {
        self.window.update(message);
    }

    fn pump(&self, drawn: bool) -> Vec<Message> {
        if drawn {
            self.frames
                .borrow_mut()
                .push(leviculum_ui_iced::rows::counted());
        }
        self.window.pump(drawn)
    }

    fn idle(&self) -> bool {
        self.window.idle()
    }

    fn marked(&self) -> bool {
        self.window.marked()
    }

    fn style(&self) -> (Theme, Color, Color) {
        self.window.style()
    }
}

/// How much more a frame of the wheel may cost over 30,000 bubbles than
/// over 300, by the frame's update: the noise of two runs of the same
/// (the bench's A/A moved rows by up to a quarter, 1470).
const SAME_FRAME: f64 = 1.3;

/// The update a frame of the chat's wheel may take (1487's bar), in
/// milliseconds: the conversation's length no longer counts.
const UPDATE_BAR_MS: f64 = 0.2;

/// A frame of the chat's wheel costs the same over 30,000 bubbles as over
/// 300 (1487): the chat's rows widget hands a frame's event to the rows
/// in view and their margin, and builds and lays out only the rows that
/// scroll in. Counted (every build), after the first frame that measured
/// every row once: the most rows one event of the gesture was handed to,
/// and the most rows one layout built, within a few rows of each other
/// for the two lengths and far under the 300 (per event and per layout,
/// not per frame: a loaded host bunches several events into a frame).
/// Timed (an optimised build on a quiet host,
/// [`HostLoad`]): the frames' update, mean, within [`SAME_FRAME`] of each
/// other, and under [`UPDATE_BAR_MS`]. Before 1487 the update grew with
/// the conversation: 0.73 to 0.85 ms a frame over 3000 bubbles, 0.12 over
/// the 300 of the real-shaped chat (`just window-bench`). Unoptimised,
/// 3000 stand in for 30,000, to keep the debug suite short.
#[test]
fn thirty_thousand_bubbles_cost_the_frame_of_three_hundred() {
    let optimised = !cfg!(debug_assertions);
    let (w, h) = (900, 560);
    let long = if optimised { 30_000 } else { 3000 };
    let host = HostLoad::start();
    let mut seen = Vec::new();
    for n in [300, long] {
        let (_, events) = Case::ChatWheel(Chat::Long).setup(w, h);
        let mut clock = headless::Paced::new(events.iter().map(|(t, _)| *t).collect(), PACE);
        let mut script = script(events);
        let mut counted = Counted {
            window: Window::new(long_chat_of(n, w, h)),
            frames: std::cell::RefCell::new(Vec::new()),
        };
        leviculum_ui_iced::rows::count(true);
        let mut watched = Watched {
            inner: &mut counted,
            host: &host,
        };
        let run = headless::drive_on(&mut watched, w, h, &mut clock, &mut script, SETTLE)
            .expect("the software renderer");
        leviculum_ui_iced::rows::count(false);
        let at = SETTLE.as_secs_f64();
        let frames: Vec<&DrawnFrame> = run.frames.iter().filter(|f| f.at >= at).collect();
        let update = frames.iter().map(|f| f.update_ms).sum::<f64>() / frames.len().max(1) as f64;
        // The first frame measured every row: the frames after it. Per
        // event and per layout, so that frames a loaded host bunches
        // (several events in one) count the same.
        let counts = counted.frames.borrow();
        let handed = counts
            .iter()
            .skip(2)
            .map(|c| c.handed / c.events.max(1))
            .max()
            .unwrap_or(0);
        let built = counts
            .iter()
            .skip(2)
            .map(|c| c.built / c.layouts.max(1))
            .max()
            .unwrap_or(0);
        let first = counts.first().map_or(0, |c| c.built);
        println!(
            "LONG_CHAT bubbles={n} frames={} update_ms_mean={update:.3} most_handed_per_event={handed} most_built_per_layout={built} first_built={first}",
            frames.len()
        );
        assert!(frames.len() > 3, "{n} bubbles: {} frames", frames.len());
        assert!(
            first > n as usize,
            "{n} bubbles: the first frame measured {first} rows"
        );
        seen.push((handed, built, update));
    }
    let ((h0, b0, u0), (h1, b1, u1)) = (seen[0], seen[1]);
    assert!(h0 < 300 && h1 < 300, "rows handed an event: {h0} and {h1}");
    assert!(
        h1 <= h0 + 8 && h0 <= h1 + 8,
        "rows handed an event: {h0} for 300, {h1} for {long}"
    );
    assert!(
        b1 <= b0 + 8 && b0 <= b1 + 8,
        "rows built in a layout: {b0} for 300, {b1} for {long}"
    );
    if optimised && host.measured("the long chat's frames") {
        assert!(
            u1 <= u0 * SAME_FRAME + 0.02,
            "a frame's update: {u0:.3} ms for 300, {u1:.3} for {long}"
        );
        assert!(
            u1 < UPDATE_BAR_MS,
            "a frame's update over {long} bubbles: {u1:.3} ms"
        );
    }
}

/// How many frames of the wheel's notches a long frame spans in
/// [`a_wheel_notch_during_a_long_frame_is_not_lost`]: each frame lasts
/// eight notches of the gesture.
const LONG_FRAME: u32 = 8;

/// A notch of the wheel that comes while a frame is drawn is not lost
/// (1487). iced 0.14 runs the window on one thread (iced_winit 0.14.1,
/// `lib.rs`): the window's input is queued as it comes (`events.push`)
/// and handed to the widgets in order, all of it, before the next frame
/// (`AboutToWait`), and a frame is the update with `RedrawRequested`, the
/// draw and the present; while a frame is drawn the input waits in the
/// display server's socket and comes with the next turn. A notch adds to
/// what the soft scroll has left to scroll ([`leviculum_ui_iced::smooth`]),
/// so notches that come together scroll as far as notches a frame apart.
/// The proof: the chat's wheel gesture (40 notches, 50 ms apart) on a
/// clock that moves [`LONG_FRAME`] notches a frame (every frame "takes"
/// 400 ms, so its notches come during it, together), against the same
/// gesture a notch a frame: the window ends on the same place, up to the
/// edge pixels of a scroll drawn as a shift ([`EDGE`], [`EDGE_PIXELS`]).
/// The control: the same long frames with one notch left out end
/// elsewhere.
#[test]
fn a_wheel_notch_during_a_long_frame_is_not_lost() {
    let (w, h) = (900, 560);
    let run = |step: Duration, drop_one: bool| {
        let (app, mut events) = Case::ChatWheel(Chat::Long).setup(w, h);
        if drop_one {
            let last = events.len() - 1;
            events.remove(last);
        }
        let notches = events.len() - 1;
        let mut window = Window::new(app);
        let mut clock = headless::Paced::new(events.iter().map(|(t, _)| *t).collect(), step);
        let mut script = script(events);
        let run = headless::drive_on(&mut window, w, h, &mut clock, &mut script, SETTLE)
            .expect("the software renderer");
        (run, notches)
    };
    let (each, n_each) = run(PACE, false);
    let (long, n_long) = run(PACE * LONG_FRAME, false);
    let (lost, n_lost) = run(PACE * LONG_FRAME, true);
    // The frames drawn while the notches came (the easing after them
    // aside): a notch each, or eight.
    let during = |run: &headless::Run| {
        let (from, until) = (SETTLE.as_secs_f64(), (SETTLE + PACE * 40).as_secs_f64());
        run.frames
            .iter()
            .filter(|f| f.at >= from && f.at < until)
            .count()
    };
    let compare = |a: &[u8], b: &[u8]| {
        let mut worst = 0u8;
        let mut past_one = 0;
        for (x, y) in a.iter().zip(b) {
            let d = x.abs_diff(*y);
            worst = worst.max(d);
            past_one += usize::from(d > 1);
        }
        (worst, past_one)
    };
    let (worst, past_one) = compare(&each.rgb, &long.rgb);
    let (lost_worst, lost_past) = compare(&each.rgb, &lost.rgb);
    println!(
        "a notch a frame: {n_each} notches in {} frames ({} during them); {LONG_FRAME} notches a frame: {n_long} in {} ({}), \
         at the end {past_one} channels more than one step off, by at most {worst}; one notch left out: \
         {n_lost} notches, {lost_past} channels off, by at most {lost_worst}",
        each.frames.len(),
        during(&each),
        long.frames.len(),
        during(&long)
    );
    assert!(
        during(&long) * 4 <= during(&each),
        "the long frames took the notches together: {} frames during the notches against {}",
        during(&long),
        during(&each)
    );
    assert!(
        worst <= EDGE && past_one <= 3 * EDGE_PIXELS,
        "a notch was lost: {past_one} channels off, by {worst}"
    );
    assert!(
        lost_past > 1000 && lost_worst > EDGE,
        "the control: a notch left out moved nothing"
    );
}
