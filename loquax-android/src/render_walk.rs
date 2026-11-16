//! The render walk's rules (1801): what `just render-walk` checks in every
//! map frame it draws through the facade, and in every gesture it plays.
//!
//! The walk (`examples/render_walk.rs`, driven by `scripts/render-walk.py`)
//! draws the phone's map exactly as the app does: through
//! [`crate::map::render_into`], into a Bitmap sized the way `MapView.java`
//! sizes it ([`JavaBitmap`], the margins of `MapFrame.margin`), so a frame
//! the library draws at another size fails here as it fails on the phone.
//! What the screen then shows is kept as Java keeps it ([`Screen`]): the
//! frame's view when one lands, and when one fails the last good one
//! scaled to the zoom the fingers asked for, which is how a failed globe
//! frame became Lew's "small rectangle" on 2026-11-13.
//!
//! Each rule is named, has one documented threshold, and says pass or fail
//! with the value it measured ([`Verdict`]). The frame rules:
//!
//! - [`BITMAP_IS_FRAME`]: the frame fits the Bitmap Java made (0 misfits).
//! - [`VIEW_COVERED`]: no more than [`BLANK_MAX_PCT`] per cent of the
//!   screen is left undrawn.
//! - [`WORLD_SPANS_VIEW`]: on a turning globe, no more than
//!   [`SPACE_MAX_PCT`] per cent of the middle line across the shorter side
//!   is space: the world at its smallest fills the view (1796).
//! - [`NO_ERROR_TEXT`]: the frame says no error and no notice.
//! - [`NO_MAP_CHIP_OVER_DATA`]: no "no map here" chip where the world's
//!   outline is drawn (zoom at most [`leviculum_map::outline::MAX_ZOOM`]).
//! - [`LABEL_INSIDE_VIEW`]: no shown name straddles the screen's edge.
//! - [`LABEL_CLEAR_OF_CONTROLS`]: no shown name lies under the map's two
//!   round buttons (`activity_conversations.xml`: 56 dp, 16 dp from the
//!   end, 16 dp and 84 dp from the bottom).
//! - [`LABEL_OVERLAP`]: no two shown names overlap.
//! - [`LABEL_CURRENT_ZOOM`]: once a frame at rest has settled, no name is
//!   from a deeper zoom than the frame's, and none is still fading out.
//! - [`LABEL_ON_WORLD`]: on a turning globe, moving or not, no name floats
//!   in space: at most two of its box's corners and middle are space.
//!
//! The four label rules before the last judge a frame at rest only: while
//! a finger moves, the names of the last rest frame ride along by design.
//!
//! The gesture rules ([`sequence_rules`]):
//!
//! - [`FRAME_P95`]: the 95th percentile of the gesture's frame times is at
//!   most half a frame of the display (the GPU rule: 8.3 ms at 60 Hz).
//! - [`HEAP_RETURNS`]: the live heap at the gesture's end is at most
//!   [`HEAP_KEPT_MAX_MB`] above its start.
//! - [`HEAP_PEAK`]: the live heap never grows more than [`HEAP_PEAK_MAX_MB`]
//!   above the gesture's start.
//! - [`VIEW_KEPT`]: a device rotation and back leaves the centre and zoom
//!   where they were.
//!
//! Host only: nothing of this is in the app.

use leviculum_map::labels::Rect;
use leviculum_map::outline::MAX_ZOOM;
use leviculum_map::viewport::MILLI;
use loquax::mapframe::DrawnName;

use crate::map::Canvas;
use crate::schema::MapShown;

/// `MapFrame.OVERSCAN`: the margin past each edge Java asks, as a share
/// of the screen side. A test reads the Java source so the two agree.
pub const OVERSCAN: f32 = 0.25;

/// `MapFrame.margin`: no margin while the last frame turned the globe,
/// a quarter of the side otherwise.
pub fn java_margin(side: u32, turns: bool) -> u32 {
    match turns {
        true => 0,
        false => (side as f32 * OVERSCAN).round() as u32,
    }
}

/// A pixel no frame draws: frames are opaque, this is transparent
/// magenta. The Bitmap is filled with it before each frame, and a screen
/// pixel Java has nothing for is it too.
pub const SENTINEL: [u8; 4] = [255, 0, 255, 0];

/// The Bitmap `MapView` makes before it asks for a frame: of the size Java
/// decided, cleared to [`SENTINEL`] before each frame. A frame of another
/// size fails with the words `bitmap.rs` says on the phone.
pub struct JavaBitmap {
    pub size: (u32, u32),
    pub rgba: Vec<u8>,
}

impl JavaBitmap {
    /// A Bitmap of `w` x `h`, all [`SENTINEL`].
    pub fn new(w: u32, h: u32) -> Self {
        let mut bitmap = Self {
            size: (w, h),
            rgba: Vec::new(),
        };
        bitmap.clear();
        bitmap
    }

    /// Every pixel [`SENTINEL`] again, before the next frame: outside the
    /// frame's time, as Java's spare Bitmap holds an old frame and costs
    /// the library nothing.
    pub fn clear(&mut self) {
        let len = self.size.0 as usize * self.size.1 as usize * 4;
        self.rgba.resize(len, 0);
        for px in self.rgba.chunks_exact_mut(4) {
            px.copy_from_slice(&SENTINEL);
        }
    }
}

impl Canvas for JavaBitmap {
    fn pixels(&mut self, w: u32, h: u32) -> Result<&mut [u8], String> {
        if self.size != (w, h) {
            return Err(format!(
                "map: the bitmap is {}x{}, the frame {w}x{h}",
                self.size.0, self.size.1
            ));
        }
        let len = w as usize * h as usize * 4;
        self.rgba.resize(len, 0);
        Ok(&mut self.rgba)
    }
}

/// What the screen shows, as `MapView` shows it: the view's part of the
/// last good frame, and while frames fail that frame scaled about the
/// screen's middle to the zoom asked since.
#[derive(Clone, Debug, Default)]
pub struct Screen {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    /// The zoom, in thousandths, the picture was drawn at.
    pub zoom_milli: u32,
}

impl Screen {
    /// The view of a frame `fw` x `fh` that landed, its margins `mx`, `my`.
    pub fn landed(
        frame: &[u8],
        fw: u32,
        (mx, my): (u32, u32),
        (w, h): (u32, u32),
        zoom_milli: u32,
    ) -> Self {
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..h {
            let row = (y + my) as usize * fw as usize * 4;
            let from = row + mx as usize * 4;
            let to = from + w as usize * 4;
            match frame.get(from..to) {
                Some(px) => rgba.extend_from_slice(px),
                None => (0..w).for_each(|_| rgba.extend_from_slice(&SENTINEL)),
            }
        }
        Self {
            w,
            h,
            rgba,
            zoom_milli,
        }
    }

    /// What a failed frame leaves on a `w` x `h` screen asked at
    /// `zoom_milli`: this picture scaled about the middle by the zoom
    /// between, the rest [`SENTINEL`]. Nothing at all before a first frame.
    pub fn failed(&self, (w, h): (u32, u32), zoom_milli: u32) -> Self {
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        let by = (f64::from(zoom_milli) - f64::from(self.zoom_milli)) / f64::from(MILLI);
        let scale = by.exp2();
        let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
        let (ox, oy) = (f64::from(self.w) / 2.0, f64::from(self.h) / 2.0);
        for y in 0..h {
            for x in 0..w {
                let sx = (f64::from(x) + 0.5 - cx) / scale + ox;
                let sy = (f64::from(y) + 0.5 - cy) / scale + oy;
                let inside =
                    sx >= 0.0 && sy >= 0.0 && sx < f64::from(self.w) && sy < f64::from(self.h);
                let px = match inside {
                    true => {
                        let i = (sy as usize * self.w as usize + sx as usize) * 4;
                        self.rgba.get(i..i + 4).unwrap_or(&SENTINEL)
                    }
                    false => &SENTINEL,
                };
                rgba.extend_from_slice(px);
            }
        }
        Self {
            w,
            h,
            rgba,
            zoom_milli: self.zoom_milli,
        }
    }
}

/// One rule's answer for one frame or gesture.
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub rule: &'static str,
    pub value: f64,
    pub limit: f64,
    pub pass: bool,
    /// What failed, in words: the error, the names.
    pub note: String,
}

impl Verdict {
    fn at_most(rule: &'static str, value: f64, limit: f64, note: String) -> Self {
        Self {
            rule,
            value,
            limit,
            pass: value <= limit,
            note,
        }
    }
}

pub const BITMAP_IS_FRAME: &str = "bitmap_is_frame";
pub const VIEW_COVERED: &str = "view_covered";
pub const WORLD_SPANS_VIEW: &str = "world_spans_view";
pub const NO_ERROR_TEXT: &str = "no_error_text";
pub const NO_MAP_CHIP_OVER_DATA: &str = "no_map_chip_over_data";
pub const LABEL_INSIDE_VIEW: &str = "label_inside_view";
pub const LABEL_CLEAR_OF_CONTROLS: &str = "label_clear_of_controls";
pub const LABEL_OVERLAP: &str = "label_overlap";
pub const LABEL_CURRENT_ZOOM: &str = "label_current_zoom";
pub const LABEL_ON_WORLD: &str = "label_on_world";
pub const FRAME_P95: &str = "frame_p95";
pub const HEAP_RETURNS: &str = "heap_returns";
pub const HEAP_PEAK: &str = "heap_peak";
pub const VIEW_KEPT: &str = "view_kept";

/// Per cent of the screen that may be undrawn: a pixel row's worth of
/// rounding at the edges, nothing a person sees.
pub const BLANK_MAX_PCT: f64 = 0.5;
/// Per cent of the globe's middle line that may be space: its rim's
/// smoothing.
pub const SPACE_MAX_PCT: f64 = 2.0;
/// Megabytes the live heap may keep after a gesture: the tile cache's
/// budget ([`loquax::tiles::BUDGET_MB`]) and the names' sprites with it.
pub const HEAP_KEPT_MAX_MB: f64 = 160.0;
/// Megabytes the live heap may grow during a gesture: the kept budget
/// and a few frames of the largest size in flight.
pub const HEAP_PEAK_MAX_MB: f64 = 320.0;

/// Everything a frame rule reads about one frame.
pub struct FrameFacts<'a> {
    pub shown: &'a MapShown,
    /// What the screen shows after it ([`Screen`]).
    pub screen: &'a Screen,
    /// The screen's size in pixels and its density.
    pub view: (u32, u32),
    pub density: f32,
    /// The names the frame drew, in its pixels ([`crate::map::drawn_names`]).
    pub names: &'a [DrawnName],
    /// The frame is at rest and its fades are done: the label rules apply.
    pub settled: bool,
    /// The space colour of the globe in the frame's table.
    pub space: [u8; 3],
}

/// The map's round buttons on a `w` x `h` screen at `density`, in screen
/// pixels: friends at the bottom end, locate above it.
pub fn controls((w, h): (u32, u32), density: f32) -> [Rect; 2] {
    let dp = |v: f32| (v * density).round() as i32;
    let (w, h) = (w as i32, h as i32);
    let button = |bottom: f32| Rect {
        x0: w - dp(16.0 + 56.0),
        y0: h - dp(bottom + 56.0),
        x1: w - dp(16.0),
        y1: h - dp(bottom),
    };
    [button(16.0), button(84.0)]
}

/// The union of a name's boxes, moved from frame pixels to screen pixels.
fn name_box(name: &DrawnName, (mx, my): (u32, u32)) -> Option<Rect> {
    let (mx, my) = (mx as i32, my as i32);
    let first = name.boxes.first()?;
    let mut r = Rect {
        x0: first.x0,
        y0: first.y0,
        x1: first.x1,
        y1: first.y1,
    };
    for b in &name.boxes {
        r.x0 = r.x0.min(b.x0);
        r.y0 = r.y0.min(b.y0);
        r.x1 = r.x1.max(b.x1);
        r.y1 = r.y1.max(b.y1);
    }
    Some(Rect {
        x0: r.x0 - mx,
        y0: r.y0 - my,
        x1: r.x1 - mx,
        y1: r.y1 - my,
    })
}

fn intersects(a: &Rect, b: &Rect) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

/// The frame rules of the module's list for one frame.
pub fn frame_rules(f: &FrameFacts<'_>) -> Vec<Verdict> {
    let mut out = Vec::new();
    let error = f.shown.error.clone().unwrap_or_default();
    let misfit = !f.shown.ok && error.contains("the bitmap is");
    out.push(Verdict::at_most(
        BITMAP_IS_FRAME,
        f64::from(u8::from(misfit)),
        0.0,
        if misfit { error.clone() } else { String::new() },
    ));
    let pixels = (f.screen.rgba.len() / 4).max(1);
    let blank = f
        .screen
        .rgba
        .chunks_exact(4)
        .filter(|px| *px == SENTINEL)
        .count()
        + (f.view.0 as usize * f.view.1 as usize).saturating_sub(f.screen.rgba.len() / 4);
    out.push(Verdict::at_most(
        VIEW_COVERED,
        blank as f64 * 100.0 / pixels as f64,
        BLANK_MAX_PCT,
        String::new(),
    ));
    let space = match f.shown.turns && f.shown.ok {
        true => space_on_middle_line(f.screen, f.space),
        false => 0.0,
    };
    out.push(Verdict::at_most(
        WORLD_SPANS_VIEW,
        space,
        SPACE_MAX_PCT,
        String::new(),
    ));
    let words: Vec<String> = [
        (!f.shown.ok && !misfit).then(|| error.clone()),
        f.shown.notice.clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    out.push(Verdict::at_most(
        NO_ERROR_TEXT,
        words.len() as f64,
        0.0,
        words.join(" | "),
    ));
    let chip = f.shown.ok && f.shown.no_map && f.shown.zoom_milli <= u32::from(MAX_ZOOM) * MILLI;
    out.push(Verdict::at_most(
        NO_MAP_CHIP_OVER_DATA,
        f64::from(u8::from(chip)),
        0.0,
        String::new(),
    ));
    if f.settled && f.shown.ok {
        out.extend(label_rules(f));
    }
    if f.shown.turns && f.shown.ok {
        let adrift = names_in_space(f);
        out.push(Verdict::at_most(
            LABEL_ON_WORLD,
            adrift.len() as f64,
            0.0,
            adrift.join(", "),
        ));
    }
    out
}

/// The shown names on screen whose box has three or more of its four
/// corners and its middle on the globe's space colour.
fn names_in_space(f: &FrameFacts<'_>) -> Vec<String> {
    let (w, h) = (f.screen.w as i32, f.screen.h as i32);
    let space = |x: i32, y: i32| {
        if x < 0 || y < 0 || x >= w || y >= h {
            return false;
        }
        let i = (y as usize * w as usize + x as usize) * 4;
        f.screen.rgba.get(i..i + 3) == Some(&f.space[..])
    };
    f.names
        .iter()
        .filter(|n| n.alpha > 0)
        .filter_map(|n| name_box(n, (f.shown.margin_x, f.shown.margin_y)).map(|b| (n, b)))
        .filter(|(_, b)| b.x1 > 0 && b.y1 > 0 && b.x0 < w && b.y0 < h)
        .filter(|(_, b)| {
            let (x1, y1) = (b.x1 - 1, b.y1 - 1);
            let points = [
                (b.x0, b.y0),
                (x1, b.y0),
                (b.x0, y1),
                (x1, y1),
                ((b.x0 + x1) / 2, (b.y0 + y1) / 2),
            ];
            points.iter().filter(|&&(x, y)| space(x, y)).count() >= 3
        })
        .map(|(n, _)| n.text.clone())
        .collect()
}

/// Per cent of the screen's middle line across its shorter side that is
/// the globe's space colour.
fn space_on_middle_line(screen: &Screen, space: [u8; 3]) -> f64 {
    let (w, h) = (screen.w as usize, screen.h as usize);
    let at = |x: usize, y: usize| {
        let i = (y * w + x) * 4;
        screen.rgba.get(i..i + 3)
    };
    let line: Vec<Option<&[u8]>> = match w <= h {
        true => (0..w).map(|x| at(x, h / 2)).collect(),
        false => (0..h).map(|y| at(w / 2, y)).collect(),
    };
    let n = line.len().max(1);
    let spaced = line.iter().filter(|px| **px == Some(&space[..])).count();
    spaced as f64 * 100.0 / n as f64
}

/// The label rules of a settled frame.
fn label_rules(f: &FrameFacts<'_>) -> Vec<Verdict> {
    let margin = (f.shown.margin_x, f.shown.margin_y);
    let screen = Rect {
        x0: 0,
        y0: 0,
        x1: f.view.0 as i32,
        y1: f.view.1 as i32,
    };
    let shown: Vec<(&DrawnName, Rect)> = f
        .names
        .iter()
        .filter(|n| n.alpha > 0)
        .filter_map(|n| name_box(n, margin).map(|b| (n, b)))
        .filter(|(_, b)| intersects(b, &screen))
        .collect();
    let straddle: Vec<&str> = shown
        .iter()
        .filter(|(_, b)| b.x0 < 0 || b.y0 < 0 || b.x1 > screen.x1 || b.y1 > screen.y1)
        .map(|(n, _)| n.text.as_str())
        .collect();
    let buttons = controls(f.view, f.density);
    let under: Vec<&str> = shown
        .iter()
        .filter(|(_, b)| buttons.iter().any(|c| intersects(b, c)))
        .map(|(n, _)| n.text.as_str())
        .collect();
    let mut pairs = Vec::new();
    for (i, (a, _)) in shown.iter().enumerate() {
        for (b, _) in shown.iter().skip(i + 1) {
            let hit = a
                .boxes
                .iter()
                .any(|p| b.boxes.iter().any(|q| intersects(p, q)));
            if hit {
                pairs.push(format!("{}/{}", a.text, b.text));
            }
        }
    }
    let stale: Vec<String> = shown
        .iter()
        .filter(|(n, _)| !n.shown || n.min_zoom > f.shown.zoom)
        .map(|(n, _)| format!("{} (z{})", n.text, n.min_zoom))
        .collect();
    let list = |v: &[&str]| v.join(", ");
    vec![
        Verdict::at_most(
            LABEL_INSIDE_VIEW,
            straddle.len() as f64,
            0.0,
            list(&straddle),
        ),
        Verdict::at_most(
            LABEL_CLEAR_OF_CONTROLS,
            under.len() as f64,
            0.0,
            list(&under),
        ),
        Verdict::at_most(LABEL_OVERLAP, pairs.len() as f64, 0.0, pairs.join(", ")),
        Verdict::at_most(
            LABEL_CURRENT_ZOOM,
            stale.len() as f64,
            0.0,
            stale.join(", "),
        ),
    ]
}

/// What a gesture measured: its frame times and the live heap.
#[derive(Clone, Debug, Default)]
pub struct Played {
    pub frame_ms: Vec<f64>,
    /// The display's rate the frames are held to.
    pub hz: f64,
    pub heap_start: usize,
    pub heap_peak: usize,
    pub heap_end: usize,
    /// For a rotation: how far the view moved, in pixels of the screen,
    /// and its zoom in thousandths, from before to after.
    pub moved_px: Option<f64>,
    pub zoom_moved_milli: Option<u32>,
}

/// The `q` quantile of `ms` (nearest rank), 0 for none.
pub fn quantile(ms: &[f64], q: f64) -> f64 {
    let mut v = ms.to_vec();
    v.sort_by(f64::total_cmp);
    match v.len() {
        0 => 0.0,
        n => v[((n - 1) as f64 * q).round() as usize],
    }
}

/// The gesture rules of the module's list.
pub fn sequence_rules(p: &Played) -> Vec<Verdict> {
    let mb = |b: usize| b as f64 / (1u64 << 20) as f64;
    let half = 1000.0 / p.hz.max(1.0) / 2.0;
    let p95 = quantile(&p.frame_ms, 0.95);
    let mut out = vec![
        Verdict::at_most(
            FRAME_P95,
            p95,
            half,
            format!(
                "p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms, {} frames, target {:.0} Hz",
                quantile(&p.frame_ms, 0.5),
                p95,
                quantile(&p.frame_ms, 1.0),
                p.frame_ms.len(),
                p.hz
            ),
        ),
        Verdict::at_most(
            HEAP_RETURNS,
            mb(p.heap_end.saturating_sub(p.heap_start)),
            HEAP_KEPT_MAX_MB,
            format!(
                "start {:.0} MB, peak {:.0} MB, end {:.0} MB",
                mb(p.heap_start),
                mb(p.heap_peak),
                mb(p.heap_end)
            ),
        ),
        Verdict::at_most(
            HEAP_PEAK,
            mb(p.heap_peak.saturating_sub(p.heap_start)),
            HEAP_PEAK_MAX_MB,
            String::new(),
        ),
    ];
    if let (Some(px), Some(z)) = (p.moved_px, p.zoom_moved_milli) {
        out.push(Verdict::at_most(
            VIEW_KEPT,
            px + f64::from(z),
            1.0,
            format!("moved {px:.1} px, zoom by {z} milli"),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::labels::LabelKind;

    fn shown(zoom: u8) -> MapShown {
        MapShown {
            ok: true,
            w: 200,
            h: 300,
            margin_x: 50,
            margin_y: 75,
            zoom,
            zoom_milli: u32::from(zoom) * MILLI,
            ..MapShown::default()
        }
    }

    fn name(text: &str, min_zoom: u8, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> DrawnName {
        DrawnName {
            text: text.to_string(),
            kind: LabelKind::City,
            min_zoom,
            at: (0.0, 0.0),
            angle: 0.0,
            boxes: vec![Rect { x0, y0, x1, y1 }],
            alpha: 255,
            shown: true,
        }
    }

    fn filled(w: u32, h: u32, px: [u8; 4]) -> Screen {
        Screen {
            w,
            h,
            rgba: px.repeat(w as usize * h as usize),
            zoom_milli: 5000,
        }
    }

    fn verdict(v: &[Verdict], rule: &str) -> Verdict {
        v.iter().find(|v| v.rule == rule).cloned().expect(rule)
    }

    /// A frame with nothing wrong: every rule green.
    fn clean() -> (MapShown, Screen, Vec<DrawnName>) {
        // Frame pixels: the screen starts at (50, 75).
        let names = vec![
            name("A", 5, (60, 85, 100, 95)),
            name("B", 3, (60, 120, 100, 130)),
        ];
        (shown(5), filled(300, 400, [9, 9, 9, 255]), names)
    }

    fn rules(s: &MapShown, screen: &Screen, names: &[DrawnName]) -> Vec<Verdict> {
        frame_rules(&FrameFacts {
            shown: s,
            screen,
            view: (300, 400),
            density: 1.0,
            names,
            settled: true,
            space: [1, 2, 3],
        })
    }

    #[test]
    fn a_clean_frame_passes_every_rule() {
        let (s, screen, names) = clean();
        let v = rules(&s, &screen, &names);
        assert_eq!(v.len(), 9);
        assert!(v.iter().all(|v| v.pass), "{v:?}");
    }

    #[test]
    fn a_bitmap_of_another_size_fails_and_says_so() {
        let mut bitmap = JavaBitmap::new(300, 450);
        let error = bitmap.pixels(200, 300).err().unwrap_or_default();
        assert_eq!(error, "map: the bitmap is 300x450, the frame 200x300");
        assert!(bitmap.pixels(300, 450).is_ok());
        let (_, screen, names) = clean();
        let failed = MapShown {
            ok: false,
            error: Some(error),
            ..shown(5)
        };
        let v = rules(&failed, &screen, &names);
        assert!(!verdict(&v, BITMAP_IS_FRAME).pass);
        assert!(
            verdict(&v, NO_ERROR_TEXT).pass,
            "the misfit is its own rule"
        );
    }

    #[test]
    fn a_blank_half_fails_the_cover() {
        let (s, mut screen, names) = clean();
        let half = screen.rgba.len() / 2;
        for px in screen.rgba[half..].chunks_exact_mut(4) {
            px.copy_from_slice(&SENTINEL);
        }
        let v = verdict(&rules(&s, &screen, &names), VIEW_COVERED);
        assert!(!v.pass && (v.value - 50.0).abs() < 1.0, "{v:?}");
    }

    #[test]
    fn a_failed_frame_shrinks_the_last_picture_to_a_rectangle() {
        let good = filled(300, 400, [9, 9, 9, 255]);
        // Two steps out: a quarter of each side is left, the rest blank.
        let shrunk = good.failed((300, 400), 3000);
        let blank = shrunk
            .rgba
            .chunks_exact(4)
            .filter(|p| *p == SENTINEL)
            .count();
        let share = blank as f64 / (300.0 * 400.0);
        assert!((share - 15.0 / 16.0).abs() < 0.02, "{share}");
        let (s, _, names) = clean();
        assert!(!verdict(&rules(&s, &shrunk, &names), VIEW_COVERED).pass);
        let same = good.failed((300, 400), 5000);
        assert_eq!(same.rgba, good.rgba);
    }

    #[test]
    fn space_across_a_turning_globe_fails_the_span() {
        let (mut s, mut screen, names) = clean();
        s.turns = true;
        assert!(verdict(&rules(&s, &screen, &names), WORLD_SPANS_VIEW).pass);
        // The middle row's left fifth is space: a disk narrower than the view.
        let row = 200 * 300 * 4;
        for px in screen.rgba[row..row + 60 * 4].chunks_exact_mut(4) {
            px.copy_from_slice(&[1, 2, 3, 255]);
        }
        let v = verdict(&rules(&s, &screen, &names), WORLD_SPANS_VIEW);
        assert!(!v.pass && (v.value - 20.0).abs() < 0.1, "{v:?}");
        s.turns = false;
        assert!(verdict(&rules(&s, &screen, &names), WORLD_SPANS_VIEW).pass);
    }

    #[test]
    fn a_name_in_space_beside_a_turning_globe_fails_on_world() {
        let (mut s, mut screen, mut names) = clean();
        // The screen's top band (rows 0..40) is space; A sits in it.
        for px in screen.rgba[..300 * 40 * 4].chunks_exact_mut(4) {
            px.copy_from_slice(&[1, 2, 3, 255]);
        }
        names.truncate(1);
        assert!(
            rules(&s, &screen, &names)
                .iter()
                .all(|v| v.rule != LABEL_ON_WORLD),
            "a flat map has no space to float in"
        );
        s.turns = true;
        let v = verdict(&rules(&s, &screen, &names), LABEL_ON_WORLD);
        assert!(!v.pass && v.note == "A", "{v:?}");
        // B is on the world.
        let (s, _, names) = clean();
        let s = MapShown { turns: true, ..s };
        let mut screen = filled(300, 400, [9, 9, 9, 255]);
        for px in screen.rgba[..300 * 40 * 4].chunks_exact_mut(4) {
            px.copy_from_slice(&[1, 2, 3, 255]);
        }
        let v = verdict(&rules(&s, &screen, &names[1..]), LABEL_ON_WORLD);
        assert!(v.pass, "{v:?}");
    }

    #[test]
    fn a_notice_or_an_error_fails_the_text_rule() {
        let (mut s, screen, names) = clean();
        s.notice = Some("map: this area is loaded already".to_string());
        assert!(!verdict(&rules(&s, &screen, &names), NO_ERROR_TEXT).pass);
        let (_, screen, names) = clean();
        let s = MapShown {
            ok: false,
            error: Some("map: no bitmap to draw into".to_string()),
            ..shown(5)
        };
        assert!(!verdict(&rules(&s, &screen, &names), NO_ERROR_TEXT).pass);
    }

    #[test]
    fn the_no_map_chip_fails_only_where_the_outline_draws() {
        let (mut s, screen, names) = clean();
        s.no_map = true;
        assert!(!verdict(&rules(&s, &screen, &names), NO_MAP_CHIP_OVER_DATA).pass);
        s.zoom_milli = (u32::from(MAX_ZOOM) + 1) * MILLI;
        s.zoom = MAX_ZOOM + 1;
        assert!(verdict(&rules(&s, &screen, &names), NO_MAP_CHIP_OVER_DATA).pass);
    }

    #[test]
    fn a_name_across_the_edge_fails_inside_view() {
        let (s, screen, mut names) = clean();
        names.push(name("Murmansk", 3, (330, 100, 370, 110)));
        let v = verdict(&rules(&s, &screen, &names), LABEL_INSIDE_VIEW);
        assert!(!v.pass && v.note == "Murmansk", "{v:?}");
        // Wholly in the margin: not on screen, not counted.
        let (s, screen, mut names) = clean();
        names.push(name("Far", 3, (0, 0, 40, 10)));
        assert!(verdict(&rules(&s, &screen, &names), LABEL_INSIDE_VIEW).pass);
    }

    #[test]
    fn a_name_under_a_button_fails_the_controls() {
        let (s, screen, mut names) = clean();
        // The friends button of a 300 x 400 screen at density 1 is
        // x 228..284, y 328..384; in frame pixels plus (50, 75).
        names.push(name("Kosovo", 3, (290, 410, 320, 420)));
        let v = verdict(&rules(&s, &screen, &names), LABEL_CLEAR_OF_CONTROLS);
        assert!(!v.pass && v.note == "Kosovo", "{v:?}");
        assert_eq!(
            controls((300, 400), 1.0),
            [
                Rect {
                    x0: 228,
                    y0: 328,
                    x1: 284,
                    y1: 384
                },
                Rect {
                    x0: 228,
                    y0: 260,
                    x1: 284,
                    y1: 316
                }
            ]
        );
    }

    #[test]
    fn two_names_on_one_spot_fail_the_overlap() {
        let (s, screen, mut names) = clean();
        names.push(name("Wümme", 5, (70, 88, 110, 98)));
        let v = verdict(&rules(&s, &screen, &names), LABEL_OVERLAP);
        assert!(!v.pass && v.note == "A/Wümme", "{v:?}");
    }

    #[test]
    fn a_name_of_a_deeper_zoom_or_still_fading_fails_after_the_settle() {
        let (s, screen, mut names) = clean();
        names.push(name("Eckstever Straße", 16, (60, 140, 100, 150)));
        let v = verdict(&rules(&s, &screen, &names), LABEL_CURRENT_ZOOM);
        assert!(!v.pass && v.note == "Eckstever Straße (z16)", "{v:?}");
        let (s, screen, mut names) = clean();
        names[1].shown = false;
        assert!(!verdict(&rules(&s, &screen, &names), LABEL_CURRENT_ZOOM).pass);
        // A moving frame is not judged by its names.
        let (s, screen, mut names) = clean();
        names.push(name("Eckstever Straße", 16, (60, 140, 100, 150)));
        let v = frame_rules(&FrameFacts {
            shown: &s,
            screen: &screen,
            view: (300, 400),
            density: 1.0,
            names: &names,
            settled: false,
            space: [1, 2, 3],
        });
        assert!(v.iter().all(|v| v.rule != LABEL_CURRENT_ZOOM));
    }

    #[test]
    fn the_gesture_rules_hold_time_heap_and_view() {
        let mb = 1usize << 20;
        let good = Played {
            frame_ms: vec![4.0; 40],
            hz: 60.0,
            heap_start: 100 * mb,
            heap_peak: 200 * mb,
            heap_end: 120 * mb,
            moved_px: Some(0.0),
            zoom_moved_milli: Some(0),
        };
        let v = sequence_rules(&good);
        assert_eq!(v.len(), 4);
        assert!(v.iter().all(|v| v.pass), "{v:?}");
        let mut slow = good.clone();
        slow.frame_ms = vec![9.0; 40];
        assert!(!verdict(&sequence_rules(&slow), FRAME_P95).pass);
        let mut kept = good.clone();
        kept.heap_end = 300 * mb;
        assert!(!verdict(&sequence_rules(&kept), HEAP_RETURNS).pass);
        let mut grown = good.clone();
        grown.heap_peak = 500 * mb;
        assert!(!verdict(&sequence_rules(&grown), HEAP_PEAK).pass);
        let mut lost = good;
        lost.moved_px = Some(12.0);
        assert!(!verdict(&sequence_rules(&lost), VIEW_KEPT).pass);
    }

    #[test]
    fn the_quantiles_are_the_nearest_rank() {
        let ms: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(quantile(&ms, 0.5), 51.0);
        assert_eq!(quantile(&ms, 0.95), 95.0);
        assert_eq!(quantile(&ms, 1.0), 100.0);
        assert_eq!(quantile(&[], 0.5), 0.0);
    }

    /// The margins mirror `MapFrame.java`'s: the same share, none while
    /// the globe turns.
    #[test]
    fn the_margins_are_the_java_ones() {
        let java = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/android/src/network/leviculum/loquax/MapFrame.java"
        ))
        .expect("MapFrame.java");
        assert!(
            java.contains(&format!("static final float OVERSCAN = {OVERSCAN}f;")),
            "MapFrame.OVERSCAN moved"
        );
        assert!(java.contains("return turns ? 0 : Math.round(side * OVERSCAN);"));
        assert_eq!(java_margin(1224, false), 306);
        assert_eq!(java_margin(2197, false), 549);
        assert_eq!(java_margin(2197, true), 0);
    }
}
