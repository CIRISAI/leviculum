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
//! [`overlay`] shows the last second's numbers in a corner, drawn on a
//! canvas each frame, so a frame that builds no view still updates them;
//! the program's log gets them as one `WINDOW_FRAME` line a second while
//! the window draws more than one frame a second.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Text};
use iced::widget::container;
use iced::{mouse, Color, Element, Font, Length, Point, Rectangle, Renderer, Theme};

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
    let line = meter().stage(stage, start, took);
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

    /// The overlay's three lines.
    pub fn lines(&self) -> [String; 3] {
        [
            format!(
                "{:>3.0} fps  {:5.1} ms  p95 {:5.1}",
                self.fps, self.mean_ms, self.p95_ms
            ),
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
        }
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
    container(Canvas::new(Overlay).width(250).height(62))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_right(Length::Fill)
        .padding(6)
        .into()
}

struct Overlay;

impl<M> canvas::Program<M> for Overlay {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let Some(snapshot) = snapshot() else {
            return Vec::new();
        };
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill(
            &Path::rounded_rectangle(Point::ORIGIN, bounds.size(), 6.0.into()),
            Color::from_rgba8(0, 0, 0, 0.72),
        );
        for (i, line) in snapshot.lines().into_iter().enumerate() {
            frame.fill_text(Text {
                content: line,
                position: Point::new(8.0, 5.0 + 18.0 * i as f32),
                color: Color::WHITE,
                size: 13.0.into(),
                font: Font::MONOSPACE,
                ..Text::default()
            });
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_debug::Stage;

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
    }
}
