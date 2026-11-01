//! A photo whole, zoomed (1269; Lew 2026-11-01): the viewer's state, the
//! same on the phone and in the window. The platform reports gestures in
//! screen pixels (a pinch's factor about its centre, a pan, a double tap,
//! the phone's wheel is the window's), this folds them; what is drawn is
//! [`Zoom::rect`], where the picture lands on the screen.
//!
//! At 1x the picture fits the screen whole, centred. It zooms up to
//! [`Zoom::max_scale`]: 4x, or further when the picture has more pixels,
//! so its native size can be reached one picture pixel to one screen
//! pixel. A pinch keeps the picture point under its centre where it is; a
//! pan moves the picture; a double tap toggles 1x and 2x about the tap.
//! The picture never leaves the screen: on an axis where it is narrower
//! than the screen it stays centred, where it is wider its edges never
//! come inside the screen's. A coast after a fling is the platform's
//! physics, bounded by [`Zoom::room`]; at 1x a swipe down closes
//! ([`Zoom::swipe_closes`]).

/// The double tap's zoom.
pub const DOUBLE_TAP: f64 = 2.0;
/// The least of the most a picture zooms.
pub const MAX_AT_LEAST: f64 = 4.0;
/// A scale this near 1 is 1: the viewer is at rest.
const EPSILON: f64 = 1e-6;

/// Where the picture lands on the screen, in screen pixels: its top left
/// corner and its size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// How far the picture may still move on screen before an edge stops it,
/// in screen pixels: `left` the most it moves left (as a negative `dx`),
/// `right` the most it moves right, `up` and `down` alike. Every one is 0
/// or more.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Room {
    pub left: f64,
    pub right: f64,
    pub up: f64,
    pub down: f64,
}

/// The viewer's state: a screen, a picture, the zoom and the pan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zoom {
    view: (f64, f64),
    picture: (f64, f64),
    /// The zoom, a multiple of the fit: 1 to [`Zoom::max_scale`].
    scale: f64,
    /// The picture's centre from the screen's centre, screen pixels.
    pan: (f64, f64),
}

fn abs(x: f64) -> f64 {
    if x < 0.0 {
        -x
    } else {
        x
    }
}

/// A size that is not a size is one pixel: nothing divides by zero.
fn positive(x: f64) -> f64 {
    if x.is_finite() && x >= 1.0 {
        x
    } else {
        1.0
    }
}

impl Zoom {
    /// A `view_w` by `view_h` screen showing a `picture_w` by
    /// `picture_h` picture (its native pixels) whole: at 1x.
    pub fn new(view_w: f64, view_h: f64, picture_w: f64, picture_h: f64) -> Self {
        Self {
            view: (positive(view_w), positive(view_h)),
            picture: (positive(picture_w), positive(picture_h)),
            scale: 1.0,
            pan: (0.0, 0.0),
        }
        .held()
    }

    /// The state as it was handed on, held to its bounds: the phone keeps
    /// it between gestures.
    pub fn from_parts(view: (f64, f64), picture: (f64, f64), scale: f64, pan: (f64, f64)) -> Self {
        let finite = |x: f64, or: f64| if x.is_finite() { x } else { or };
        Self {
            view: (positive(view.0), positive(view.1)),
            picture: (positive(picture.0), positive(picture.1)),
            scale: finite(scale, 1.0),
            pan: (finite(pan.0, 0.0), finite(pan.1, 0.0)),
        }
        .held()
    }

    /// The screen.
    pub fn view(&self) -> (f64, f64) {
        self.view
    }

    /// The picture's native size.
    pub fn picture(&self) -> (f64, f64) {
        self.picture
    }

    /// The zoom, a multiple of the fit.
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// The picture's centre from the screen's centre.
    pub fn pan(&self) -> (f64, f64) {
        self.pan
    }

    /// Screen pixels per picture pixel at 1x: the picture whole on the
    /// screen, up or down.
    pub fn fit(&self) -> f64 {
        let (vw, vh) = self.view;
        let (pw, ph) = self.picture;
        (vw / pw).min(vh / ph)
    }

    /// The most it zooms: 4x, or the picture's native size when that is
    /// larger on screen.
    pub fn max_scale(&self) -> f64 {
        (1.0 / self.fit()).max(MAX_AT_LEAST)
    }

    /// At 1x: the picture whole, where a swipe down or Back closes.
    pub fn at_rest(&self) -> bool {
        self.scale <= 1.0 + EPSILON
    }

    /// The picture's size on screen.
    fn size(&self) -> (f64, f64) {
        let k = self.fit() * self.scale;
        (self.picture.0 * k, self.picture.1 * k)
    }

    /// The most the centre moves off the screen's centre on each axis.
    fn slack(&self) -> (f64, f64) {
        let (w, h) = self.size();
        (
            ((w - self.view.0) / 2.0).max(0.0),
            ((h - self.view.1) / 2.0).max(0.0),
        )
    }

    /// The scale within its bounds, the pan within the slack.
    fn held(mut self) -> Self {
        self.scale = self.scale.clamp(1.0, self.max_scale());
        if abs(self.scale - 1.0) <= EPSILON {
            self.scale = 1.0;
        }
        let (sx, sy) = self.slack();
        self.pan = (self.pan.0.clamp(-sx, sx), self.pan.1.clamp(-sy, sy));
        self
    }

    /// Where the picture lands on the screen.
    pub fn rect(&self) -> Placed {
        let (w, h) = self.size();
        Placed {
            left: self.view.0 / 2.0 + self.pan.0 - w / 2.0,
            top: self.view.1 / 2.0 + self.pan.1 - h / 2.0,
            width: w,
            height: h,
        }
    }

    /// How far the picture may still move before an edge stops it.
    pub fn room(&self) -> Room {
        let (sx, sy) = self.slack();
        Room {
            left: (self.pan.0 + sx).max(0.0),
            right: (sx - self.pan.0).max(0.0),
            up: (self.pan.1 + sy).max(0.0),
            down: (sy - self.pan.1).max(0.0),
        }
    }

    /// The zoom to `scale` (held to its bounds), the picture point under
    /// the screen point `(x, y)` kept there as far as the edges allow.
    pub fn zoom_to(&mut self, scale: f64, x: f64, y: f64) {
        if !scale.is_finite() || !x.is_finite() || !y.is_finite() {
            return;
        }
        let to = scale.clamp(1.0, self.max_scale());
        let k = to / self.scale;
        // The anchor from the screen's centre; the picture's centre moves
        // away from it by the zoom's ratio.
        let (ax, ay) = (x - self.view.0 / 2.0, y - self.view.1 / 2.0);
        self.pan = (ax + (self.pan.0 - ax) * k, ay + (self.pan.1 - ay) * k);
        self.scale = to;
        *self = self.held();
    }

    /// A pinch moved: the zoom by `factor` about its centre `(x, y)`.
    pub fn pinch(&mut self, factor: f64, x: f64, y: f64) {
        if factor.is_finite() && factor > 0.0 {
            self.zoom_to(self.scale * factor, x, y);
        }
    }

    /// A finger (or the mouse) moved the picture by `(dx, dy)`; how far it
    /// moved, less where an edge stopped it. At 1x it does not move.
    pub fn pan_by(&mut self, dx: f64, dy: f64) -> (f64, f64) {
        if !dx.is_finite() || !dy.is_finite() {
            return (0.0, 0.0);
        }
        let before = self.pan;
        self.pan = (before.0 + dx, before.1 + dy);
        *self = self.held();
        (self.pan.0 - before.0, self.pan.1 - before.1)
    }

    /// A double tap at `(x, y)`: zoomed, back to 1x; at 1x, 2x about the
    /// tap.
    pub fn double_tap(&mut self, x: f64, y: f64) {
        match self.at_rest() {
            true => self.zoom_to(DOUBLE_TAP, x, y),
            false => {
                self.scale = 1.0;
                self.pan = (0.0, 0.0);
            }
        }
    }

    /// Back while zoomed: 1x again; whether it was at 1x already, so
    /// Back closes the viewer.
    pub fn back(&mut self) -> bool {
        if self.at_rest() {
            return true;
        }
        self.scale = 1.0;
        self.pan = (0.0, 0.0);
        false
    }

    /// The screen turned or the window was resized: the same zoom on the
    /// new screen, held to it.
    pub fn resize(&mut self, view_w: f64, view_h: f64) {
        self.view = (positive(view_w), positive(view_h));
        *self = self.held();
    }

    /// Whether a one-finger drag of `(dx, dy)` from where it went down
    /// closes the viewer: at 1x, down by an eighth of the screen, more
    /// down than sideways.
    pub fn swipe_closes(&self, dx: f64, dy: f64) -> bool {
        self.at_rest() && dy >= self.view.1 / 8.0 && dy > 2.0 * abs(dx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64) -> bool {
        abs(a - b) < 1e-6
    }

    /// A 1000 by 2000 phone screen showing a 2000 by 1000 landscape
    /// picture: it fits at 0.5, 1000 by 500, centred.
    fn landscape() -> Zoom {
        Zoom::new(1000.0, 2000.0, 2000.0, 1000.0)
    }

    #[test]
    fn at_one_x_the_picture_fits_whole_and_centred() {
        let zoom = landscape();
        assert!(zoom.at_rest());
        assert!(near(zoom.fit(), 0.5));
        let r = zoom.rect();
        assert_eq!(
            r,
            Placed {
                left: 0.0,
                top: 750.0,
                width: 1000.0,
                height: 500.0
            }
        );
        // Nothing to pan at 1x.
        assert_eq!(
            zoom.room(),
            Room {
                left: 0.0,
                right: 0.0,
                up: 0.0,
                down: 0.0
            }
        );
    }

    #[test]
    fn the_most_is_four_x_or_the_native_size_when_larger() {
        // Fit 0.5: native size is 2x on screen, so 4x it is.
        assert!(near(landscape().max_scale(), 4.0));
        // A 6000 px wide picture on a 1000 px screen: native is 6x.
        let big = Zoom::new(1000.0, 2000.0, 6000.0, 3000.0);
        assert!(near(big.max_scale(), 6.0));
        let mut zoom = big;
        zoom.pinch(100.0, 500.0, 1000.0);
        assert!(near(zoom.scale(), 6.0));
        // One picture pixel to one screen pixel.
        assert!(near(zoom.rect().width, 6000.0));
    }

    #[test]
    fn a_pinch_clamps_to_one_and_the_most() {
        let mut zoom = landscape();
        zoom.pinch(0.5, 500.0, 1000.0);
        assert!(near(zoom.scale(), 1.0));
        assert!(zoom.at_rest());
        zoom.pinch(10.0, 500.0, 1000.0);
        assert!(near(zoom.scale(), 4.0));
        zoom.pinch(1.5, 500.0, 1000.0);
        assert!(near(zoom.scale(), 4.0));
        // Zooming out from the most to below 1 lands on 1x, centred.
        zoom.pinch(0.01, 100.0, 900.0);
        assert!(zoom.at_rest());
        assert_eq!(zoom.pan(), (0.0, 0.0));
        // A factor that is no factor changes nothing.
        let before = zoom;
        zoom.pinch(f64::NAN, 1.0, 1.0);
        zoom.pinch(0.0, 1.0, 1.0);
        zoom.pinch(-2.0, 1.0, 1.0);
        assert_eq!(zoom, before);
    }

    /// The picture point under `(x, y)`, in its own pixels.
    fn under(zoom: &Zoom, x: f64, y: f64) -> (f64, f64) {
        let r = zoom.rect();
        let k = r.width / zoom.picture().0;
        ((x - r.left) / k, (y - r.top) / k)
    }

    #[test]
    fn a_pinch_keeps_the_point_under_its_centre() {
        // A picture of the screen's shape fills it, so both axes zoom
        // past the screen's edges and the anchor holds on both.
        let mut zoom = Zoom::new(1000.0, 2000.0, 500.0, 1000.0);
        // Inside the picture, off centre.
        let (x, y) = (700.0, 1100.0);
        let before = under(&zoom, x, y);
        zoom.pinch(2.0, x, y);
        assert!(near(zoom.scale(), 2.0));
        let after = under(&zoom, x, y);
        assert!(
            near(before.0, after.0) && near(before.1, after.1),
            "{before:?} {after:?}"
        );
        // Again, from zoomed, at another point: still anchored.
        let (x, y) = (300.0, 900.0);
        let before = under(&zoom, x, y);
        zoom.pinch(1.5, x, y);
        let after = under(&zoom, x, y);
        assert!(
            near(before.0, after.0) && near(before.1, after.1),
            "{before:?} {after:?}"
        );
    }

    #[test]
    fn an_anchor_near_an_edge_is_held_so_the_picture_stays_on_screen() {
        let mut zoom = landscape();
        // At the screen's far left: the anchor would pull the picture's
        // left edge inside the screen; the edge holds instead.
        zoom.pinch(2.0, 0.0, 1000.0);
        let r = zoom.rect();
        assert!(near(r.left, 0.0), "{r:?}");
        assert!(near(r.width, 2000.0));
        // Taller than the screen? 1000 high on 2000: stays centred.
        assert!(near(r.top, 500.0), "{r:?}");
    }

    #[test]
    fn a_pan_moves_within_the_edges_and_reports_what_it_moved() {
        let mut zoom = landscape();
        assert_eq!(zoom.pan_by(100.0, 100.0), (0.0, 0.0));
        zoom.pinch(2.0, 500.0, 1000.0);
        // 2000 wide on 1000: 500 each way; 1000 high on 2000: none.
        let room = zoom.room();
        assert!(
            near(room.left, 500.0) && near(room.right, 500.0),
            "{room:?}"
        );
        assert!(near(room.up, 0.0) && near(room.down, 0.0), "{room:?}");
        let moved = zoom.pan_by(300.0, 50.0);
        assert!(near(moved.0, 300.0) && near(moved.1, 0.0), "{moved:?}");
        let moved = zoom.pan_by(300.0, 0.0);
        assert!(near(moved.0, 200.0), "{moved:?}");
        assert!(near(zoom.rect().left, 0.0));
        assert!(near(zoom.room().right, 0.0) && near(zoom.room().left, 1000.0));
    }

    #[test]
    fn a_double_tap_toggles_one_and_two_x_at_the_tap() {
        let mut zoom = landscape();
        let (x, y) = (800.0, 1000.0);
        let before = under(&zoom, x, y);
        zoom.double_tap(x, y);
        assert!(near(zoom.scale(), 2.0));
        let after = under(&zoom, x, y);
        assert!(
            near(before.0, after.0) && near(before.1, after.1),
            "{before:?} {after:?}"
        );
        // From any zoom, back to the whole picture.
        zoom.pinch(1.7, 200.0, 900.0);
        zoom.double_tap(10.0, 10.0);
        assert!(zoom.at_rest());
        assert_eq!(zoom.rect(), landscape().rect());
    }

    #[test]
    fn back_unzooms_first_and_closes_at_one_x() {
        let mut zoom = landscape();
        zoom.pinch(3.0, 500.0, 1000.0);
        assert!(!zoom.back());
        assert!(zoom.at_rest());
        assert!(zoom.back());
    }

    #[test]
    fn a_swipe_down_closes_at_one_x_only() {
        let mut zoom = landscape();
        assert!(zoom.swipe_closes(10.0, 300.0));
        assert!(!zoom.swipe_closes(10.0, 200.0), "less than an eighth");
        assert!(!zoom.swipe_closes(200.0, 300.0), "more sideways");
        assert!(!zoom.swipe_closes(0.0, -400.0), "up");
        zoom.pinch(2.0, 500.0, 1000.0);
        assert!(!zoom.swipe_closes(0.0, 600.0));
    }

    #[test]
    fn a_resize_keeps_the_zoom_and_holds_the_pan() {
        let mut zoom = landscape();
        zoom.pinch(2.0, 500.0, 1000.0);
        zoom.pan_by(500.0, 0.0);
        // The phone turns: 2000 by 1000, the picture fits at 1.0 now.
        zoom.resize(2000.0, 1000.0);
        assert!(near(zoom.scale(), 2.0));
        let r = zoom.rect();
        assert!(r.left <= 0.0 && r.left + r.width >= 2000.0, "{r:?}");
        assert!(r.top <= 0.0 && r.top + r.height >= 1000.0, "{r:?}");
    }

    #[test]
    fn handed_on_parts_are_held() {
        let zoom = Zoom::from_parts((1000.0, 2000.0), (2000.0, 1000.0), 9.0, (1e9, -1e9));
        assert!(near(zoom.scale(), 4.0));
        let r = zoom.rect();
        assert!(near(r.left, 0.0), "{r:?}");
        let zoom = Zoom::from_parts((0.0, f64::NAN), (0.0, 0.0), f64::NAN, (f64::NAN, 0.0));
        assert!(zoom.at_rest());
        assert_eq!(zoom.pan(), (0.0, 0.0));
    }

    #[test]
    fn the_picture_never_leaves_the_screen_under_any_gesture() {
        // A held walk of gestures: after each, the picture covers the
        // screen on every axis it is wider on, and is centred on the
        // others.
        let mut zoom = Zoom::new(1080.0, 2340.0, 1600.0, 1200.0);
        let mut seed: u64 = 1269;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as f64) / f64::from(1u32 << 31)
        };
        for _ in 0..2000 {
            let (x, y) = (next() * 1080.0, next() * 2340.0);
            match (next() * 4.0) as u32 {
                0 => zoom.pinch(0.5 + next() * 1.5, x, y),
                1 => {
                    zoom.pan_by((next() - 0.5) * 2000.0, (next() - 0.5) * 2000.0);
                }
                2 => zoom.double_tap(x, y),
                _ => zoom.resize(800.0 + next() * 1600.0, 800.0 + next() * 1600.0),
            }
            let (vw, vh) = zoom.view();
            let r = zoom.rect();
            for (start, len, screen) in [(r.left, r.width, vw), (r.top, r.height, vh)] {
                if len >= screen {
                    assert!(
                        start <= 1e-6 && start + len >= screen - 1e-6,
                        "{r:?} {zoom:?}"
                    );
                } else {
                    assert!(near(start, (screen - len) / 2.0), "{r:?} {zoom:?}");
                }
            }
            assert!(zoom.scale() >= 1.0 && zoom.scale() <= zoom.max_scale());
        }
    }
}
