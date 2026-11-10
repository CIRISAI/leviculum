//! A window's view without a window: drawn by the software renderer into
//! pixels, and clicked, for the goldens and the tests of the programs'
//! windows (Loquax's since 1145, lnomad's since slice 2 of
//! `docs/src/concepts/lnomad-everywhere.md`).

use iced::advanced::renderer::{self, Headless, Renderer as _};
use iced::advanced::widget::Tree;
use iced::advanced::{clipboard, layout, Layout, Shell};
use iced::{mouse, Color, Element, Event, Point, Rectangle, Renderer, Size, Theme};

/// The software renderer, its default font `font` at 16 pixels; `None`
/// where iced finds no backend to start.
pub(crate) fn software(font: iced::Font) -> Option<Renderer> {
    iced::futures::executor::block_on(<Renderer as Headless>::new(font, iced::Pixels(16.0), None))
}

/// `element` laid out on `w` by `h` and drawn in `theme`, its text in
/// `text` on `background`: the pixels, RGB, row by row; `None` without a
/// renderer.
pub fn render<M>(
    mut element: Element<'_, M>,
    theme: &Theme,
    text: Color,
    background: Color,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    let mut renderer = software(iced::Font::DEFAULT)?;
    let size = Size::new(w as f32, h as f32);
    let mut tree = Tree::new(&element);
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    let viewport = Rectangle::with_size(size);
    renderer.reset(viewport);
    element.as_widget().draw(
        &tree,
        &mut renderer,
        theme,
        &renderer::Style { text_color: text },
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &viewport,
    );
    Some(
        renderer
            .screenshot(iced::Size::new(w, h), 1.0, background)
            .chunks(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
    )
}

/// `element` laid out on `w` by `h`: the tree of its bounds, for a test
/// of where things are; `None` without a renderer.
pub fn layout<M>(mut element: Element<'_, M>, w: u32, h: u32) -> Option<layout::Node> {
    let renderer = software(iced::Font::DEFAULT)?;
    let mut tree = Tree::new(&element);
    Some(element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, Size::new(w as f32, h as f32)),
    ))
}

/// A screen drawn again and again off screen by the software renderer,
/// as a window draws it: one renderer, so its image cache holds a picture
/// shown before, and one widget tree, so a canvas keeps its state between
/// frames. For measuring what a frame costs (1431).
pub struct Offscreen {
    renderer: Renderer,
    tree: Option<Tree>,
    size: Size,
}

impl Offscreen {
    /// `w` by `h` at a density of one; `None` without a renderer.
    pub fn new(w: u32, h: u32) -> Option<Self> {
        Some(Self {
            renderer: software(iced::Font::DEFAULT)?,
            tree: None,
            size: Size::new(w as f32, h as f32),
        })
    }

    /// `element` laid out and rasterised as the window's frame: the time
    /// the layout and the drawing took, and the time the rasterising took
    /// (with the read back of the pixels, which a window does not do).
    pub fn frame<M>(
        &mut self,
        mut element: Element<'_, M>,
        theme: &Theme,
        text: Color,
        background: Color,
    ) -> (std::time::Duration, std::time::Duration) {
        let start = std::time::Instant::now();
        let tree = match &mut self.tree {
            Some(tree) => {
                tree.diff(&element);
                tree
            }
            None => self.tree.insert(Tree::new(&element)),
        };
        let node = element.as_widget_mut().layout(
            tree,
            &self.renderer,
            &layout::Limits::new(Size::ZERO, self.size),
        );
        let viewport = Rectangle::with_size(self.size);
        self.renderer.reset(viewport);
        element.as_widget().draw(
            tree,
            &mut self.renderer,
            theme,
            &renderer::Style { text_color: text },
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &viewport,
        );
        let laid = start.elapsed();
        let size = iced::Size::new(self.size.width as u32, self.size.height as u32);
        let _ = self.renderer.screenshot(size, 1.0, background);
        (laid, start.elapsed() - laid)
    }

    /// `element` laid out once and, for each of `events`, updated with it,
    /// the pointer at `at`, and drawn, as a window redraws a screen whose
    /// view did not change (a scrollable scrolled, an easing on its way):
    /// per event, the time the update and the drawing took and the time
    /// the rasterising took; the pixels of the last frame, RGB; and the
    /// messages the widgets published.
    #[allow(clippy::too_many_arguments)]
    pub fn redraws<M>(
        &mut self,
        mut element: Element<'_, M>,
        events: &[Event],
        at: Point,
        theme: &Theme,
        text: Color,
        background: Color,
    ) -> (
        Vec<(std::time::Duration, std::time::Duration)>,
        Vec<u8>,
        Vec<M>,
    ) {
        let mut tree = Tree::new(&element);
        let limits = layout::Limits::new(Size::ZERO, self.size);
        let mut node = element
            .as_widget_mut()
            .layout(&mut tree, &self.renderer, &limits);
        let viewport = Rectangle::with_size(self.size);
        let size = iced::Size::new(self.size.width as u32, self.size.height as u32);
        let mut times = Vec::new();
        let mut messages = Vec::new();
        let mut last = Vec::new();
        for event in events {
            let start = std::time::Instant::now();
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                event,
                Layout::new(&node),
                mouse::Cursor::Available(at),
                &self.renderer,
                &mut clipboard::Null,
                &mut shell,
                &viewport,
            );
            if shell.is_layout_invalid() {
                node = element
                    .as_widget_mut()
                    .layout(&mut tree, &self.renderer, &limits);
            }
            self.renderer.reset(viewport);
            element.as_widget().draw(
                &tree,
                &mut self.renderer,
                theme,
                &renderer::Style { text_color: text },
                Layout::new(&node),
                mouse::Cursor::Available(at),
                &viewport,
            );
            let drawn = start.elapsed();
            last = self.renderer.screenshot(size, 1.0, background);
            times.push((drawn, start.elapsed() - drawn));
        }
        let rgb = last.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        (times, rgb, messages)
    }
}

/// A window's surface off screen (1441): the software renderer's present
/// as its compositor runs it in a window (iced_tiny_skia 0.14.1,
/// `window/compositor.rs`, `present`), into a buffer of our own that is
/// kept between frames like a window's buffer of age one: the layers
/// compared with the last frame's, only the damaged part filled and
/// rasterised (the whole window once the damage fragments, the vendored
/// compositor's rule since 1456; a scroll as a shift of the frame and the
/// strip it uncovers, its plan since 1468), and nothing read back. What a window
/// adds after it, the
/// display server taking the buffer (softbuffer's present), is not here.
pub struct Surface {
    pixels: Vec<u8>,
    mask: tiny_skia::Mask,
    last: Option<Vec<iced_tiny_skia::Layer>>,
    background: Option<Color>,
    width: u32,
    height: u32,
    /// The last present's damage as the layers' diff gave it, before
    /// grouping and the whole-window rule (1455).
    rects: Vec<Rectangle>,
    /// Whether a scroll is drawn as a shift of the frame on it (1468),
    /// as the window's compositor draws it; off, every frame is drawn
    /// as the damage says.
    shift: bool,
    /// How far the last present moved the frame on it, if it did.
    shifted: Option<i32>,
}

impl Surface {
    /// `w` by `h` at a density of one; `None` for an empty size.
    pub fn new(w: u32, h: u32) -> Option<Self> {
        Some(Self {
            pixels: vec![0; w as usize * h as usize * 4],
            mask: tiny_skia::Mask::new(w, h)?,
            last: None,
            background: None,
            width: w,
            height: h,
            rects: Vec::new(),
            shift: true,
            shifted: None,
        })
    }

    /// Scrolls drawn as a shift of the frame on it, or not (1468).
    pub fn set_shift(&mut self, shift: bool) {
        self.shift = shift;
    }

    /// How far the last present moved the frame on it, physical
    /// pixels, if it did (1468).
    pub fn shifted(&self) -> Option<i32> {
        self.shifted
    }

    /// What `renderer` holds since its last reset, presented on
    /// `background`: the share of the window it rasterised, zero when
    /// nothing changed, which is no present (1455).
    pub fn present(&mut self, renderer: &mut Renderer, background: Color) -> f32 {
        use iced_tiny_skia::graphics::{damage, Viewport};
        let viewport = Viewport::with_physical_size(Size::new(self.width, self.height), 1.0);
        let whole = Rectangle::with_size(viewport.logical_size());
        self.shifted = None;
        let layers = renderer.layers().to_vec();
        let damaged = match (&self.last, self.background == Some(background)) {
            (Some(last), true) => damage::diff(
                last,
                &layers,
                |layer| vec![layer.bounds],
                iced_tiny_skia::Layer::damage,
            ),
            _ => vec![whole],
        };
        // Nothing changed, or only the frame meter's box: no present, and
        // the next frame is compared with what is on it (1455).
        if damaged.is_empty() || iced_tiny_skia::window::compositor::is_quiet(&damaged) {
            return 0.0;
        }
        self.rects = damaged.clone();
        let last = match self.background == Some(background) && self.shift {
            true => self.last.as_deref(),
            false => None,
        };
        let (shift, damaged) =
            iced_tiny_skia::window::compositor::plan(last, &layers, damaged, &viewport);
        let area: f32 = damaged
            .iter()
            .filter_map(|r| r.intersection(&whole))
            .map(|r| r.width * r.height)
            .sum();
        self.shifted = shift.as_ref().map(|s| s.dy);
        if let Some(shift) = &shift {
            iced_tiny_skia::scroll::apply(&mut self.pixels, self.width, shift);
        }
        let Some(mut pixmap) =
            tiny_skia::PixmapMut::from_bytes(&mut self.pixels, self.width, self.height)
        else {
            return 0.0;
        };
        renderer.draw(&mut pixmap, &mut self.mask, &viewport, &damaged, background);
        self.last = Some(layers);
        self.background = Some(background);
        (area / (whole.width * whole.height)).min(1.0)
    }

    /// The last present's damage as the layers' diff gave it, before
    /// grouping and the whole-window rule: what changed.
    pub fn rects(&self) -> &[Rectangle] {
        &self.rects
    }

    /// The pixels on it, RGB, row by row.
    pub fn rgb(&self) -> Vec<u8> {
        // The renderer writes the window's format: blue, green, red, alpha.
        self.pixels
            .chunks(4)
            .flat_map(|p| [p[2], p[1], p[0]])
            .collect()
    }
}

/// A program as an emulated window runs it ([`drive`], 1441).
pub trait Program {
    type Message;
    fn view(&self) -> Element<'_, Self::Message>;
    fn update(&mut self, message: Self::Message);
    /// What else the window's loop hands the program now, besides the
    /// widgets' messages: a worker's results, a subscription's ticks;
    /// `drawn` when a frame was just presented (what iced's
    /// `window::frames` answers).
    fn pump(&self, drawn: bool) -> Vec<Self::Message>;
    /// Whether the program waits on nothing of its own (a worker).
    fn idle(&self) -> bool;
    /// The theme, the text colour and the window's background.
    fn style(&self) -> (Theme, Color, Color);
    /// Whether the frame about to be presented is one the program marks
    /// (a map frame new from its worker, say); asked once a frame.
    fn marked(&self) -> bool {
        false
    }
}

/// One frame of an emulated window: when, and in milliseconds the
/// update (the program's and the widgets'), the view built, laid out and
/// drawn, the present; the share of the window rasterised; whether the view was
/// built anew for it.
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawnFrame {
    /// When it was presented, seconds since the drive began.
    pub at: f64,
    pub update_ms: f64,
    pub draw_ms: f64,
    pub present_ms: f64,
    /// The drive's thread's time on a CPU over the same three parts, in
    /// milliseconds, where [`measure_cpu`] asked for it and the kernel
    /// tells it (1473): what the frame cost, whatever else the host ran.
    pub cpu_ms: Option<f64>,
    pub damage: f32,
    pub rebuilt: bool,
    /// The program marked it ([`Program::marked`]).
    pub marked: bool,
}

impl DrawnFrame {
    pub fn total_ms(&self) -> f64 {
        self.update_ms + self.draw_ms + self.present_ms
    }
}

std::thread_local! {
    static CPU: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether a [`drive`] on this thread measures each frame's CPU time
/// too ([`DrawnFrame::cpu_ms`], 1473); off unless asked, as reading it
/// costs a system call a part.
pub fn measure_cpu(on: bool) {
    CPU.with(|c| c.set(on));
}

/// The calling thread's time on a CPU so far, nanoseconds, by its own
/// clock (`CLOCK_THREAD_CPUTIME_ID`); `None` where there is none. Not
/// `/proc/thread-self/schedstat`: for a running thread it moves only at
/// the scheduler's ticks, 4 ms steps on the host.
#[cfg(unix)]
pub fn thread_cpu_ns() -> Option<u64> {
    use rustix::time::{clock_gettime, ClockId};
    let t = clock_gettime(ClockId::ThreadCPUTime);
    let secs = u64::try_from(t.tv_sec).ok()?;
    let nanos = u64::try_from(t.tv_nsec).ok()?;
    secs.checked_mul(1_000_000_000)?.checked_add(nanos)
}

/// The calling thread's time on a CPU: not told on this target.
#[cfg(not(unix))]
pub fn thread_cpu_ns() -> Option<u64> {
    None
}

/// The CPU time since `from` ([`thread_cpu_ns`]) added to `frame`.
fn add_cpu(frame: &mut DrawnFrame, from: Option<u64>) {
    let Some(from) = from else {
        return;
    };
    if let Some(now) = thread_cpu_ns() {
        let ms = now.saturating_sub(from) as f64 / 1e6;
        frame.cpu_ms = Some(frame.cpu_ms.unwrap_or(0.0) + ms);
    }
}

/// What a [`drive`] did.
#[derive(Debug, Default)]
pub struct Run {
    /// The frames presented.
    pub frames: Vec<DrawnFrame>,
    /// Each presented frame's damage as the layers' diff gave it, before
    /// grouping and the whole-window rule (1455), in `frames`' order.
    pub rects: Vec<Vec<Rectangle>>,
    /// When a redraw changed nothing and so presented nothing (1455),
    /// seconds since the drive began: iced asks a redraw after every
    /// message, and before 1455 each was a present.
    pub unpresented: Vec<f64>,
    /// From the first event to the last frame.
    pub seconds: f64,
    pub events: usize,
    /// Messages the program was handed, and views built.
    pub messages: usize,
    pub views: usize,
    /// The last frame's pixels, RGB, and those on the window when the
    /// drive's mark came.
    pub rgb: Vec<u8>,
    pub marked_rgb: Option<Vec<u8>>,
    /// With a twin ([`drive_twinned`], 1468): the frames drawn as a shift
    /// of the one before, and when a frame had a pixel with a channel more
    /// than one apart from the twin's, which draws every frame without a
    /// shift.
    pub shifted: usize,
    pub differed: Vec<f64>,
    /// With a twin: the largest difference of a channel between the two;
    /// the most pixels of one frame that differed at all, and that
    /// differed by more than one.
    pub worst: u8,
    pub off: usize,
    pub past_one: usize,
    /// With a twin: where the first frame that differed by more than one
    /// differed so, `[x0, y0, x1, y1]` inclusive.
    pub first_difference: Option<[u32; 4]>,
}

/// The pixels of the `w` wide RGBA buffers `a` and `b` with a channel
/// more than one apart, as the box around them, `[x0, y0, x1, y1]`.
fn difference(a: &[u8], b: &[u8], w: u32) -> [u32; 4] {
    let mut out = [u32::MAX, u32::MAX, 0, 0];
    for (i, (p, q)) in a.chunks(4).zip(b.chunks(4)).enumerate() {
        if p.iter().zip(q).any(|(x, y)| x.abs_diff(*y) > 1) {
            let (x, y) = (i as u32 % w, i as u32 / w);
            out = [out[0].min(x), out[1].min(y), out[2].max(x), out[3].max(y)];
        }
    }
    out
}

/// The clock a [`drive`] runs on: what the script is asked with, what the
/// mark is compared with and what a frame's [`DrawnFrame::at`] says
/// (1619). The wall's ([`Wall`]) for a measurement; a test that counts
/// frames owns one that a slow host cannot run out from under it
/// ([`Paced`]), as a frame that takes a second on a loaded host leaves a
/// window of wall seconds without a frame. The program's own timers (a
/// cursor's blink, a redraw asked for a later [`Instant`](std::time::Instant))
/// stay on the wall's: they are the program's, not the script's.
pub trait Clock {
    /// The drive begins: before its first view.
    fn start(&mut self) {}
    /// One turn of the window's loop, before the script is asked;
    /// `redrawn` when the window redrew since the turn before.
    fn turn(&mut self, _redrawn: bool) {}
    /// The time since the drive began.
    fn now(&self) -> std::time::Duration;
    /// A frame was presented at `at` by this clock.
    fn presented(&mut self, _at: std::time::Duration) {}
}

/// The wall's clock: real time since the drive began.
pub struct Wall(std::time::Instant);

impl Wall {
    pub fn new() -> Self {
        Self(std::time::Instant::now())
    }
}

impl Default for Wall {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for Wall {
    fn start(&mut self) {
        self.0 = std::time::Instant::now();
    }

    fn now(&self) -> std::time::Duration {
        self.0.elapsed()
    }
}

/// How many turns of the loop with no redraw count as idle in [`Paced`]:
/// the loop sleeps 200 us a turn when it waits, so at least 100 ms of
/// waiting, and a turn that builds a view is one turn however long the
/// build takes.
pub const IDLE_TURNS: u32 = 500;

/// A clock that the window's redraws move rather than the wall (1472,
/// a clock since 1619): each redraw moves it `step` on, and a stretch
/// with none of the script's events in it is skipped, so a frame sees the
/// script's events of one step whatever the machine's speed. A window
/// that redraws nothing for [`IDLE_TURNS`] turns moves it on too, so an
/// event that changes nothing cannot stall the script.
pub struct Paced {
    /// When the script's events are due, sorted.
    times: Vec<std::time::Duration>,
    step: std::time::Duration,
    clock: std::time::Duration,
    idle: u32,
}

impl Paced {
    pub fn new(mut times: Vec<std::time::Duration>, step: std::time::Duration) -> Self {
        times.sort();
        Self {
            times,
            step,
            clock: std::time::Duration::ZERO,
            idle: 0,
        }
    }
}

impl Clock for Paced {
    fn turn(&mut self, redrawn: bool) {
        self.idle += 1;
        if redrawn || self.idle >= IDLE_TURNS {
            self.idle = 0;
            let stepped = self.clock + self.step;
            // The next event not yet due: the stretch before it is skipped.
            let next = self.times.iter().find(|t| **t > self.clock).copied();
            self.clock = next.map_or(stepped, |next| stepped.max(next));
        }
    }

    fn now(&self) -> std::time::Duration {
        self.clock
    }
}

/// `program` in a window of `w` by `h` as iced 0.14's loop runs it on X11
/// (iced_winit 0.14.1, `lib.rs`), in real time: the events `script` says
/// are due (it is asked with the time since the start, and says `None`
/// once it is over) go to the widgets; their messages, and what the
/// program's [`Program::pump`] hands it, go to its update, after which
/// the view is built anew and the window redrawn; a widget's redraw
/// request (a soft scroll on its way) redraws without a new view. A frame
/// is drawn as soon as one is wanted and the events due are handled,
/// with no frame clock, as winit's redraw on X11 has none. Over once the
/// script is, no redraw is wanted and the program is idle; the pixels on
/// the window at `mark` kept. `None` without a renderer.
pub fn drive<P: Program>(
    program: &mut P,
    w: u32,
    h: u32,
    script: &mut dyn FnMut(std::time::Duration) -> Option<Vec<Event>>,
    mark: std::time::Duration,
) -> Option<Run> {
    drive_with(program, w, h, &mut Wall::new(), script, mark, false)
}

/// [`drive`] on `clock` instead of the wall's (1619): the script, the
/// mark and the frames' times are the clock's.
pub fn drive_on<P: Program>(
    program: &mut P,
    w: u32,
    h: u32,
    clock: &mut dyn Clock,
    script: &mut dyn FnMut(std::time::Duration) -> Option<Vec<Event>>,
    mark: std::time::Duration,
) -> Option<Run> {
    drive_with(program, w, h, clock, script, mark, false)
}

/// [`drive_on`], every frame presented a second time on a twin surface
/// that draws no scroll as a shift (1468), the two compared byte for
/// byte: [`Run::differed`] says when they were not the same.
pub fn drive_twinned<P: Program>(
    program: &mut P,
    w: u32,
    h: u32,
    clock: &mut dyn Clock,
    script: &mut dyn FnMut(std::time::Duration) -> Option<Vec<Event>>,
    mark: std::time::Duration,
) -> Option<Run> {
    drive_with(program, w, h, clock, script, mark, true)
}

fn drive_with<P: Program>(
    program: &mut P,
    w: u32,
    h: u32,
    clock: &mut dyn Clock,
    script: &mut dyn FnMut(std::time::Duration) -> Option<Vec<Event>>,
    mark: std::time::Duration,
    twinned: bool,
) -> Option<Run> {
    use iced::window::RedrawRequest;
    use iced_runtime::user_interface::{Cache, State, UserInterface};
    use std::time::{Duration, Instant};
    let mut renderer = software(iced::Font::DEFAULT)?;
    let mut surface = Surface::new(w, h)?;
    let mut twin = match twinned {
        true => {
            let mut twin = Surface::new(w, h)?;
            twin.set_shift(false);
            Some(twin)
        }
        false => None,
    };
    let size = Size::new(w as f32, h as f32);
    let mut cache = Cache::default();
    let mut cursor = mouse::Cursor::Unavailable;
    let mut run = Run::default();
    let mut pending = DrawnFrame::default();
    let mut want = true;
    let mut over = false;
    let mut drawn = false;
    let mut redrawn = false;
    let mut redraw_at: Option<Instant> = None;
    let mut messages: Vec<P::Message> = Vec::new();
    clock.start();
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let measure = CPU.with(|c| c.get());
    let cpu = || measure.then(thread_cpu_ns).flatten();
    let wants = |state: &State, redraw_at: &mut Option<Instant>| match state {
        State::Outdated => true,
        State::Updated { redraw_request, .. } => match redraw_request {
            RedrawRequest::NextFrame => true,
            RedrawRequest::At(at) => {
                *redraw_at = Some(redraw_at.map_or(*at, |t| t.min(*at)));
                false
            }
            RedrawRequest::Wait => false,
        },
    };
    loop {
        let c = cpu();
        let built = Instant::now();
        let mut ui = UserInterface::build(program.view(), size, cache, &mut renderer);
        pending.draw_ms += ms(built.elapsed());
        add_cpu(&mut pending, c);
        pending.rebuilt = true;
        run.views += 1;
        let mut done = false;
        loop {
            clock.turn(std::mem::take(&mut redrawn));
            let now = clock.now();
            if run.marked_rgb.is_none() && now >= mark {
                run.marked_rgb = Some(surface.rgb());
            }
            if !over {
                match script(now) {
                    Some(events) if !events.is_empty() => {
                        for event in &events {
                            if let Event::Mouse(mouse::Event::CursorMoved { position }) = event {
                                cursor = mouse::Cursor::Available(*position);
                            }
                        }
                        run.events += events.len();
                        let c = cpu();
                        let t = Instant::now();
                        let (state, _) = ui.update(
                            &events,
                            cursor,
                            &mut renderer,
                            &mut clipboard::Null,
                            &mut messages,
                        );
                        pending.update_ms += ms(t.elapsed());
                        add_cpu(&mut pending, c);
                        want |= wants(&state, &mut redraw_at);
                        if matches!(state, State::Outdated) && messages.is_empty() {
                            break;
                        }
                    }
                    Some(_) => {}
                    None => over = true,
                }
            }
            if messages.is_empty() {
                messages = program.pump(std::mem::take(&mut drawn));
            }
            if !messages.is_empty() {
                break;
            }
            if redraw_at.is_some_and(|at| at <= Instant::now()) {
                redraw_at = None;
                want = true;
            }
            if want {
                let c = cpu();
                let t = Instant::now();
                // As iced_winit 0.14.1 (`lib.rs`, `RedrawRequested`): a
                // redraw whose update laid the window out again (a rows
                // widget building the rows scrolled in, 1487) is sent
                // again, three times at most, before the frame is drawn.
                let mut sent = 0;
                let state = loop {
                    let before = messages.len();
                    let (state, _) = ui.update(
                        &[Event::Window(iced::window::Event::RedrawRequested(t))],
                        cursor,
                        &mut renderer,
                        &mut clipboard::Null,
                        &mut messages,
                    );
                    if messages.len() != before || !state.has_layout_changed() || sent >= 2 {
                        break state;
                    }
                    sent += 1;
                };
                pending.update_ms += ms(t.elapsed());
                add_cpu(&mut pending, c);
                if !messages.is_empty() || matches!(state, State::Outdated) {
                    break;
                }
                want = wants(&state, &mut redraw_at);
                let (theme, text, background) = program.style();
                let c = cpu();
                let t = Instant::now();
                ui.draw(
                    &mut renderer,
                    &theme,
                    &renderer::Style { text_color: text },
                    cursor,
                );
                pending.draw_ms += ms(t.elapsed());
                add_cpu(&mut pending, c);
                let c = cpu();
                let t = Instant::now();
                pending.damage = surface.present(&mut renderer, background);
                pending.present_ms = ms(t.elapsed());
                add_cpu(&mut pending, c);
                let at = clock.now();
                pending.at = at.as_secs_f64();
                if let Some(twin) = &mut twin {
                    let _ = twin.present(&mut renderer, background);
                    run.shifted += usize::from(surface.shifted().is_some());
                    let mut past_one = 0;
                    let mut off = 0;
                    for (a, b) in twin.pixels.chunks(4).zip(surface.pixels.chunks(4)) {
                        let d = a
                            .iter()
                            .zip(b)
                            .map(|(x, y)| x.abs_diff(*y))
                            .max()
                            .unwrap_or(0);
                        run.worst = run.worst.max(d);
                        off += usize::from(d > 0);
                        past_one += usize::from(d > 1);
                    }
                    run.off = run.off.max(off);
                    run.past_one = run.past_one.max(past_one);
                    if past_one > 0 {
                        if run.differed.is_empty() {
                            run.first_difference =
                                Some(difference(&twin.pixels, &surface.pixels, w));
                        }
                        run.differed.push(pending.at);
                    }
                }
                drawn = true;
                redrawn = true;
                if pending.damage == 0.0 {
                    // No frame: nothing changed (1455).
                    run.unpresented.push(pending.at);
                    pending = DrawnFrame::default();
                    continue;
                }
                clock.presented(at);
                pending.marked = program.marked();
                crate::fps::emulated(
                    Instant::now(),
                    crate::fps::Parts {
                        update: pending.update_ms,
                        draw: pending.draw_ms,
                        present: pending.present_ms,
                    },
                );
                run.rects.push(surface.rects().to_vec());
                run.frames.push(std::mem::take(&mut pending));
                run.seconds = clock.now().as_secs_f64();
                continue;
            }
            if over && redraw_at.is_none() && program.idle() {
                done = true;
                break;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        cache = ui.into_cache();
        if done {
            break;
        }
        run.messages += messages.len();
        let c = cpu();
        let t = Instant::now();
        for message in messages.drain(..) {
            program.update(message);
        }
        pending.update_ms += ms(t.elapsed());
        add_cpu(&mut pending, c);
        want = true;
    }
    run.rgb = surface.rgb();
    Some(run)
}

/// A left click at `at` on `element` laid out on `w` by `h`: the pointer
/// moves there, the button goes down and comes up. The messages the
/// widgets published, in order; `None` without a renderer.
pub fn click<M>(element: Element<'_, M>, at: Point, w: u32, h: u32) -> Option<Vec<M>> {
    clicks(element, at, 1, w, h)
}

/// `n` left clicks in a row at `at` on `element` laid out on `w` by `h`,
/// as quickly as a double or a triple click (1444): the pointer moves
/// there, the button goes down and comes up `n` times. The messages the
/// widgets published, in order; `None` without a renderer.
pub fn clicks<M>(element: Element<'_, M>, at: Point, n: usize, w: u32, h: u32) -> Option<Vec<M>> {
    let presses = (0..n).flat_map(|_| {
        [
            (mouse::Event::ButtonPressed(mouse::Button::Left), at),
            (mouse::Event::ButtonReleased(mouse::Button::Left), at),
        ]
    });
    let steps: Vec<_> = std::iter::once((mouse::Event::CursorMoved { position: at }, at))
        .chain(presses)
        .collect();
    gesture(element, &steps, w, h)
}

/// The left button pressed at `from`, the pointer moved to `to` in
/// `steps` even steps with it held, and released there (1475): a drag on
/// `element` laid out on `w` by `h`. The messages, in order.
pub fn drag<M>(
    element: Element<'_, M>,
    from: Point,
    to: Point,
    steps: usize,
    w: u32,
    h: u32,
) -> Option<Vec<M>> {
    let mut script = vec![
        (mouse::Event::CursorMoved { position: from }, from),
        (mouse::Event::ButtonPressed(mouse::Button::Left), from),
    ];
    for n in 1..=steps.max(1) {
        let f = n as f32 / steps.max(1) as f32;
        let at = Point::new(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f);
        script.push((mouse::Event::CursorMoved { position: at }, at));
    }
    script.push((mouse::Event::ButtonReleased(mouse::Button::Left), to));
    gesture(element, &script, w, h)
}

/// The mouse events of `script` on `element` laid out once on `w` by `h`,
/// each with the pointer where it says (1475). The messages the widgets
/// published, in order; `None` without a renderer.
pub fn gesture<M>(
    mut element: Element<'_, M>,
    script: &[(mouse::Event, Point)],
    w: u32,
    h: u32,
) -> Option<Vec<M>> {
    let renderer = software(iced::Font::DEFAULT)?;
    let size = Size::new(w as f32, h as f32);
    let mut tree = Tree::new(&element);
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    let viewport = Rectangle::with_size(size);
    let mut messages = Vec::new();
    for (event, at) in script {
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(*event),
            Layout::new(&node),
            mouse::Cursor::Available(*at),
            &renderer,
            &mut clipboard::Null,
            &mut shell,
            &viewport,
        );
    }
    Some(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::flat;
    use iced::widget::{column, text, Space};

    /// A press on a button is its message, a press beside it is none.
    #[test]
    fn a_click_on_a_button_is_its_message() {
        let view = || -> Element<'_, u8> {
            column![
                flat(Space::new().width(100).height(40), 7u8),
                text("under it"),
            ]
            .into()
        };
        assert_eq!(
            click(view(), Point::new(50.0, 20.0), 200, 200),
            Some(vec![7])
        );
        assert_eq!(
            click(view(), Point::new(150.0, 20.0), 200, 200),
            Some(vec![])
        );
    }

    /// The surface rasterises what changed (1441): a first frame is the
    /// whole window, the same frame again nothing, a frame whose one
    /// small square moved only about the square's old and new place; the
    /// pixels are the background and the square where they belong.
    #[test]
    fn a_surface_rasterises_what_changed() {
        use iced::widget::container;
        let ground = Color::from_rgb8(10, 20, 30);
        let ink = Color::from_rgb8(200, 100, 50);
        let mut renderer = software(iced::Font::DEFAULT).expect("a renderer");
        let mut surface = Surface::new(200, 100).expect("a surface");
        let frame = |x: f32, renderer: &mut Renderer, surface: &mut Surface| {
            let square = container(Space::new().width(10).height(10))
                .style(move |_: &Theme| container::Style::default().background(ink));
            let view: Element<'_, u8> = container(square)
                .padding(iced::Padding::ZERO.left(x))
                .into();
            let mut tree = Tree::new(&view);
            let mut view = view;
            let size = Size::new(200.0, 100.0);
            let node = view.as_widget_mut().layout(
                &mut tree,
                renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            renderer.reset(Rectangle::with_size(size));
            view.as_widget().draw(
                &tree,
                renderer,
                &Theme::Light,
                &renderer::Style {
                    text_color: Color::BLACK,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &Rectangle::with_size(size),
            );
            surface.present(renderer, ground)
        };
        assert_eq!(frame(0.0, &mut renderer, &mut surface), 1.0);
        assert_eq!(frame(0.0, &mut renderer, &mut surface), 0.0);
        let moved = frame(50.0, &mut renderer, &mut surface);
        assert!(moved > 0.0 && moved < 0.2, "{moved}");
        let rgb = surface.rgb();
        let px = |x: usize, y: usize| &rgb[(y * 200 + x) * 3..][..3];
        assert_eq!(px(55, 5), &[200, 100, 50]);
        assert_eq!(px(5, 5), &[10, 20, 30], "the old place cleared");
        assert_eq!(px(150, 80), &[10, 20, 30]);
    }

    /// Damage in more pieces than the compositor's rule allows is drawn
    /// as the whole window (1456): six squares on the diagonal, too far
    /// apart for iced to group, each moved, are six small rectangles, a
    /// thousandth of the window together, and the surface rasterises all
    /// of it; the pixels are right. The control: one square moved is
    /// drawn alone.
    #[test]
    fn a_fragmented_damage_is_drawn_whole() {
        use iced::widget::{container, stack};
        const SIDE: u32 = 1200;
        let ground = Color::from_rgb8(10, 20, 30);
        let ink = Color::from_rgb8(200, 100, 50);
        let mut renderer = software(iced::Font::DEFAULT).expect("a renderer");
        let frame = |squares: u16, x: f32, renderer: &mut Renderer, surface: &mut Surface| {
            let square = |i: u16| -> Element<'_, u8> {
                let i = f32::from(i) * 200.0;
                container(
                    container(Space::new().width(10).height(10))
                        .style(move |_: &Theme| container::Style::default().background(ink)),
                )
                .padding(iced::Padding::ZERO.left(i + x).top(i))
                .width(iced::Length::Fill)
                .height(iced::Length::Fill)
                .into()
            };
            let mut view: Element<'_, u8> = stack((0..squares).map(square)).into();
            let mut tree = Tree::new(&view);
            let size = Size::new(SIDE as f32, SIDE as f32);
            let node = view.as_widget_mut().layout(
                &mut tree,
                renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            renderer.reset(Rectangle::with_size(size));
            view.as_widget().draw(
                &tree,
                renderer,
                &Theme::Light,
                &renderer::Style {
                    text_color: Color::BLACK,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &Rectangle::with_size(size),
            );
            surface.present(renderer, ground)
        };
        let mut surface = Surface::new(SIDE, SIDE).expect("a surface");
        assert_eq!(frame(6, 0.0, &mut renderer, &mut surface), 1.0);
        assert_eq!(frame(6, 50.0, &mut renderer, &mut surface), 1.0);
        let rgb = surface.rgb();
        let px = |x: usize, y: usize| &rgb[(y * SIDE as usize + x) * 3..][..3];
        assert_eq!(px(1055, 1005), &[200, 100, 50]);
        assert_eq!(px(1005, 1005), &[10, 20, 30], "the old place cleared");
        let mut one = Surface::new(SIDE, SIDE).expect("a surface");
        assert_eq!(frame(1, 0.0, &mut renderer, &mut one), 1.0);
        let moved = frame(1, 50.0, &mut renderer, &mut one);
        assert!(moved > 0.0 && moved < 0.01, "{moved}");
    }

    /// The pixels are the size asked, the ground where nothing is drawn.
    #[test]
    fn a_render_is_its_size_on_its_ground() {
        let ground = Color::from_rgb8(10, 20, 30);
        let view: Element<'_, u8> = Space::new().into();
        let px = render(view, &Theme::Light, Color::BLACK, ground, 8, 4).expect("a renderer");
        assert_eq!(px.len(), 8 * 4 * 3);
        assert_eq!(&px[..3], &[10, 20, 30]);
    }
}
