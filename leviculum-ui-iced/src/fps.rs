//! The window's frame meter (1441): frames per second, the mean and the
//! 95th percentile of a frame's time, and its parts, measured in the
//! window itself while a person uses it.
//!
//! iced times each frame's stages itself (`iced_debug`'s spans); the
//! workspace's vendored `iced_debug` hands the finished spans to
//! [`enable`]'s sink instead of iced's comet tool. A frame is what the
//! window did between two presents:
//!
//! - `update`: the program's update ([`update`], which the program
//!   times) and the widgets' handling of the events (iced's `interact`
//!   span, less the updates it ran inside it);
//! - `draw`: building the view, laying it out and drawing it into the
//!   renderer's layers (iced's `view`, `layout` and `draw` spans);
//! - `present`: the backend's rasterise and present, which for the
//!   software renderer is tiny-skia filling the damaged part of the window
//!   and softbuffer handing it to the display server (iced's `present`).
//!
//! [`overlay`] shows the last second's numbers in a corner, drawn anew
//! each frame, so a frame that builds no view still updates them;
//! the program's log gets them as one `WINDOW_FRAME` line a second while
//! the window draws more than one frame a second.
//!
//! The meter is never the reason for a frame (1455): a redraw the
//! compositor does not present (nothing changed, or only the overlay's
//! numbers, the box the overlay hands the vendored compositor's
//! `set_quiet`) is no frame and is not counted, and the overlay's new
//! numbers wait for the next frame something else draws. While the
//! window draws at most one frame a second the overlay says `idle`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use iced::advanced::text::{self, Renderer as _};
use iced::advanced::widget::Tree;
use iced::advanced::{layout, renderer, Layout, Renderer as _, Widget};
use iced::widget::container;
use iced::{
    alignment, mouse, Border, Color, Element, Font, Length, Pixels, Point, Rectangle, Renderer,
    Size, Theme,
};

/// The window over which the numbers are taken.
const WINDOW: Duration = Duration::from_secs(1);

static ON: AtomicBool = AtomicBool::new(false);
static METER: Mutex<Meter> = Mutex::new(Meter::new());
static LOG: OnceLock<fn(&str)> = OnceLock::new();

/// Measure the window's frames from now on; `log` is handed the
/// `WINDOW_FRAME` line once a second while frames are drawn.
pub fn enable(log: fn(&str)) {
    let _ = LOG.set(log);
    ON.store(true, Ordering::Relaxed);
    iced_debug::set_sink(sink);
}

/// Stop measuring: the overlay and the meter off again. For a bench that
/// measures a window with and without the overlay in one process (1456).
pub fn disable() {
    ON.store(false, Ordering::Relaxed);
    iced_tiny_skia::window::compositor::set_quiet(None);
}

/// A frame an emulated window ([`crate::headless::drive`]) presented at
/// `at`, its parts handed to the meter as iced's spans hand a window's,
/// so the overlay's numbers change per frame as in the window.
pub(crate) fn emulated(at: Instant, parts: Parts) {
    if enabled() {
        let mut meter = meter();
        meter.pending = Parts::default();
        meter.updates.clear();
        let _ = meter.presented(at, parts);
    }
}

/// Whether the meter runs.
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// The program's update began at `start` and took `took`.
pub fn update(start: Instant, took: Duration) {
    if enabled() {
        meter().update(start, took);
    }
}

/// The window's size in screen pixels, for the line.
pub fn set_size(width: u32, height: u32) {
    if enabled() {
        meter().size = (width, height);
    }
}

/// The last second's numbers, once a frame was presented.
pub fn snapshot() -> Option<Snapshot> {
    meter().last.clone()
}

fn meter() -> std::sync::MutexGuard<'static, Meter> {
    METER.lock().unwrap_or_else(PoisonError::into_inner)
}

fn sink(stage: iced_debug::Stage, start: Instant, took: Duration) {
    let line = {
        let mut meter = meter();
        // A redraw the compositor did not present (1455) is no frame.
        match matches!(stage, iced_debug::Stage::Present)
            && !meter.counted(iced_tiny_skia::window::compositor::presents())
        {
            true => meter.unpresented(),
            false => meter.stage(stage, start, took),
        }
    };
    if let (Some(line), Some(log)) = (line, LOG.get()) {
        log(&line);
    }
}

/// A frame's parts, in milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Parts {
    pub update: f64,
    pub draw: f64,
    pub present: f64,
}

impl Parts {
    /// The frame's time: its parts together.
    pub fn total(&self) -> f64 {
        self.update + self.draw + self.present
    }
}

/// The numbers over the last second of frames.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// Frames presented in the last second.
    pub fps: f64,
    /// A frame's time, mean, 95th percentile and longest, ms.
    pub mean_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    /// The parts' means.
    pub parts: Parts,
    /// The window in screen pixels.
    pub size: (u32, u32),
}

impl Snapshot {
    /// The log line.
    pub fn line(&self) -> String {
        format!(
            "WINDOW_FRAME fps={:.0} ms={:.2} p95={:.2} max={:.2} update={:.2} draw={:.2} present={:.2} size={}x{}",
            self.fps,
            self.mean_ms,
            self.p95_ms,
            self.max_ms,
            self.parts.update,
            self.parts.draw,
            self.parts.present,
            self.size.0,
            self.size.1,
        )
    }

    /// The overlay's three lines; `idle` for the rate while the last
    /// second held at most one frame (1455).
    pub fn lines(&self) -> [String; 3] {
        [
            match self.fps > 1.0 {
                true => format!(
                    "{:>3.0} fps  {:5.1} ms  p95 {:5.1}",
                    self.fps, self.mean_ms, self.p95_ms
                ),
                false => format!("   idle   {:5.1} ms", self.mean_ms),
            },
            format!(
                "upd {:4.1}  draw {:4.1}  pres {:4.1}",
                self.parts.update, self.parts.draw, self.parts.present
            ),
            format!("{} x {}", self.size.0, self.size.1),
        ]
    }
}

/// The accounting, apart from iced so a test can feed it.
#[derive(Debug)]
pub struct Meter {
    /// What the frame under way spent so far.
    pending: Parts,
    /// The program's updates in it, which an `interact` span may hold.
    updates: Vec<(Instant, Duration)>,
    /// The frames of the last second: when each was presented, its parts.
    frames: VecDeque<(Instant, Parts)>,
    last: Option<Snapshot>,
    logged: Option<Instant>,
    size: (u32, u32),
    /// The compositor's present count at the last present span.
    presents: u64,
}

impl Default for Meter {
    fn default() -> Self {
        Self::new()
    }
}

impl Meter {
    pub const fn new() -> Self {
        Self {
            pending: Parts {
                update: 0.0,
                draw: 0.0,
                present: 0.0,
            },
            updates: Vec::new(),
            frames: VecDeque::new(),
            last: None,
            logged: None,
            size: (0, 0),
            presents: 0,
        }
    }

    /// Whether the compositor presented since the last present span: its
    /// count `presents` moved.
    pub fn counted(&mut self, presents: u64) -> bool {
        let moved = presents != self.presents;
        self.presents = presents;
        moved
    }

    /// A redraw ended without a present (1455): what it spent is no
    /// frame's, and nothing is logged.
    pub fn unpresented(&mut self) -> Option<String> {
        self.pending = Parts::default();
        self.updates.clear();
        None
    }

    /// The program's update.
    pub fn update(&mut self, start: Instant, took: Duration) {
        self.pending.update += ms(took);
        self.updates.push((start, took));
    }

    /// A span of iced's ended; the line to log when one is due.
    pub fn stage(
        &mut self,
        stage: iced_debug::Stage,
        start: Instant,
        took: Duration,
    ) -> Option<String> {
        use iced_debug::Stage;
        match stage {
            Stage::Interact => {
                // The updates iced ran inside it are counted already.
                let end = start + took;
                let inside: Duration = self
                    .updates
                    .iter()
                    .filter(|(at, d)| *at >= start && *at + *d <= end)
                    .map(|(_, d)| *d)
                    .sum();
                self.pending.update += ms(took.saturating_sub(inside));
                None
            }
            Stage::View | Stage::Layout | Stage::Draw => {
                self.pending.draw += ms(took);
                None
            }
            Stage::Present => {
                let end = start + took;
                let mut parts = std::mem::take(&mut self.pending);
                parts.present = ms(took);
                self.updates.clear();
                self.presented(end, parts)
            }
        }
    }

    /// A frame presented at `at`: the second's numbers anew, and the line
    /// when a second passed since the last and the window kept drawing.
    fn presented(&mut self, at: Instant, parts: Parts) -> Option<String> {
        self.frames.push_back((at, parts));
        while self
            .frames
            .front()
            .is_some_and(|(t, _)| at.saturating_duration_since(*t) >= WINDOW)
        {
            self.frames.pop_front();
        }
        let n = self.frames.len();
        let mut totals: Vec<f64> = self.frames.iter().map(|(_, p)| p.total()).collect();
        totals.sort_by(f64::total_cmp);
        let mean =
            |f: fn(&Parts) -> f64| self.frames.iter().map(|(_, p)| f(p)).sum::<f64>() / n as f64;
        let snapshot = Snapshot {
            fps: n as f64,
            mean_ms: totals.iter().sum::<f64>() / n as f64,
            p95_ms: totals[(n * 95 / 100).min(n - 1)],
            max_ms: totals[n - 1],
            parts: Parts {
                update: mean(|p| p.update),
                draw: mean(|p| p.draw),
                present: mean(|p| p.present),
            },
            size: self.size,
        };
        self.last = Some(snapshot.clone());
        let due = self
            .logged
            .is_none_or(|t| at.saturating_duration_since(t) >= WINDOW);
        (due && n > 1).then(|| {
            self.logged = Some(at);
            snapshot.line()
        })
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// The numbers in the window's top right corner, over whatever is there,
/// taking no pointer; empty until a frame was measured.
pub fn overlay<'a, M: 'a>() -> Element<'a, M> {
    container(Element::new(Overlay))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_right(Length::Fill)
        .padding(6)
        .into()
}

/// The overlay's box.
const BOX: Size = Size::new(250.0, 62.0);

/// The numbers, read anew at every draw, so a frame that builds no view
/// still shows them. Drawn as a widget, not on a canvas (1456): a
/// canvas's text has no bottom and no clip (iced_tiny_skia 0.14.1,
/// `geometry.rs`), so every change of a number damaged a column from the
/// text to the window's foot and redrew all it cut; this text is bounded
/// and clipped to the box, and a change damages the box.
struct Overlay;

impl<M> Widget<M, Theme, Renderer> for Overlay {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(BOX.width), Length::Fixed(BOX.height))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(BOX.width, BOX.height, BOX))
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let Some(snapshot) = snapshot() else {
            return;
        };
        let bounds = layout.bounds();
        // Its numbers alone never present a frame (1455).
        iced_tiny_skia::window::compositor::set_quiet(Some(bounds));
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: Border::default().rounded(6.0),
                ..renderer::Quad::default()
            },
            Color::from_rgba8(0, 0, 0, 0.72),
        );
        for (i, line) in snapshot.lines().into_iter().enumerate() {
            renderer.fill_text(
                text::Text {
                    content: line,
                    bounds: Size::new(bounds.width - 16.0, 18.0),
                    size: Pixels(13.0),
                    line_height: text::LineHeight::default(),
                    font: Font::MONOSPACE,
                    align_x: text::Alignment::Left,
                    align_y: alignment::Vertical::Top,
                    shaping: text::Shaping::Basic,
                    wrapping: text::Wrapping::None,
                },
                Point::new(bounds.x + 8.0, bounds.y + 5.0 + 18.0 * i as f32),
                Color::WHITE,
                bounds,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_debug::Stage;

    /// The overlay's numbers changing damage its box and nothing else
    /// (1456), and so present no frame (1455): drawn over a 400 by 600
    /// window and presented, a second frame with other numbers presents
    /// nothing, the box being the compositor's quiet one. The control,
    /// the quiet box lifted: the third frame with other numbers again
    /// rasterises at most the box's share of the window. A canvas's text,
    /// which has no bottom, damaged a column to the window's foot, more
    /// than an eighth of this window, which the compositor then draws
    /// whole, and which no quiet box holds.
    #[test]
    fn a_change_of_the_numbers_damages_the_box_alone() {
        use crate::headless::{software, Surface};
        use iced::advanced::renderer::Renderer as _;
        let (w, h) = (400u32, 600u32);
        let mut renderer = software(Font::DEFAULT).expect("a renderer");
        let mut surface = Surface::new(w, h).expect("a surface");
        let t0 = Instant::now();
        let mut frame = |at: Duration, present: f64, loud: bool| {
            let _ = meter().presented(
                t0 + at,
                Parts {
                    update: 1.0,
                    draw: 2.0,
                    present,
                },
            );
            let mut view: Element<'_, u8> = overlay();
            let mut tree = Tree::new(&view);
            let size = Size::new(w as f32, h as f32);
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            renderer.reset(Rectangle::with_size(size));
            view.as_widget().draw(
                &tree,
                &mut renderer,
                &Theme::Light,
                &renderer::Style {
                    text_color: Color::BLACK,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &Rectangle::with_size(size),
            );
            if loud {
                iced_tiny_skia::window::compositor::set_quiet(None);
            }
            surface.present(&mut renderer, Color::WHITE)
        };
        assert_eq!(frame(Duration::ZERO, 3.0, false), 1.0);
        let quiet = frame(Duration::from_millis(16), 47.0, false);
        assert_eq!(quiet, 0.0, "the numbers alone presented a frame");
        let changed = frame(Duration::from_millis(32), 9.0, true);
        let most = (BOX.width + 12.0) * (BOX.height + 12.0) / (w * h) as f32;
        assert!(
            changed > 0.0 && changed <= most,
            "{changed} of the window, at most {most}"
        );
    }

    /// A frame is what was spent between two presents: the program's
    /// updates and the widgets' events as `update`, less the updates
    /// iced ran inside its interact span (counted once); view, layout and
    /// draw as `draw`; the present alone as `present`. Sixty such frames
    /// in a second are sixty frames a second, and the line comes once a
    /// second, not per frame.
    #[test]
    fn a_frame_is_its_parts_between_two_presents() {
        let mut m = Meter::new();
        let t0 = Instant::now();
        let d = Duration::from_millis;
        let mut lines = Vec::new();
        for i in 0..120u64 {
            let at = t0 + d(16 * i + 1);
            // The interact span of 3 ms holds an update of 2 ms, which
            // ends first.
            m.update(at + Duration::from_micros(500), d(2));
            m.stage(Stage::Interact, at, d(3));
            m.stage(Stage::View, at + d(3), d(1));
            m.stage(Stage::Layout, at + d(4), d(1));
            m.stage(Stage::Draw, at + d(5), d(2));
            if let Some(line) = m.stage(Stage::Present, at + d(7), d(5)) {
                lines.push(line);
            }
        }
        let s = m.last.clone().expect("a snapshot");
        assert!((s.parts.update - 3.0).abs() < 1e-6, "{:?}", s.parts);
        assert!((s.parts.draw - 4.0).abs() < 1e-6, "{:?}", s.parts);
        assert!((s.parts.present - 5.0).abs() < 1e-6, "{:?}", s.parts);
        assert!((s.mean_ms - 12.0).abs() < 1e-6);
        assert!((62.0..=63.0).contains(&s.fps), "{}", s.fps);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("WINDOW_FRAME fps="), "{}", lines[0]);
        // The control: one frame alone is no animation, no line.
        let mut one = Meter::new();
        assert_eq!(one.stage(Stage::Present, t0, d(5)), None);
        assert!(one.last.is_some());
        assert!(one
            .last
            .as_ref()
            .is_some_and(|s| s.lines()[0].contains("idle")));
        assert!(s.lines()[0].contains("fps"), "{:?}", s.lines());
    }

    /// A redraw the compositor did not present is no frame (1455): its
    /// update and draw are dropped, the frames a second do not move, and
    /// no line is logged; a present after it counts its own parts only.
    #[test]
    fn a_redraw_without_a_present_is_no_frame() {
        let mut m = Meter::new();
        let t0 = Instant::now();
        let d = Duration::from_millis;
        assert!(m.counted(1));
        m.update(t0, d(2));
        let _ = m.stage(Stage::Present, t0 + d(3), d(5));
        // A redraw: an update and a draw, and the count did not move.
        m.update(t0 + d(100), d(7));
        let _ = m.stage(Stage::Draw, t0 + d(107), d(1));
        assert!(!m.counted(1));
        assert_eq!(m.unpresented(), None);
        assert_eq!(m.last.as_ref().map(|s| s.fps), Some(1.0));
        // The next present: its own parts, none of the redraw's.
        assert!(m.counted(2));
        m.update(t0 + d(200), d(1));
        let _ = m.stage(Stage::Present, t0 + d(201), d(4));
        let s = m.last.clone().expect("a snapshot");
        assert_eq!(s.fps, 2.0);
        assert!((s.parts.update - 1.5).abs() < 1e-6, "{:?}", s.parts);
        assert!((s.parts.draw - 0.0).abs() < 1e-6, "{:?}", s.parts);
    }
}
