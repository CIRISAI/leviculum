//! The world view's projection: Equal Earth (or the orthographic globe)
//! at world zooms, flowing into Mercator as the view zooms in (1710).
//!
//! One mapping for the view, a blend of two screen positions:
//!
//! ```text
//! P(point) = (1 - t) * W_c(point) + t * M_c(point)
//! ```
//!
//! `M_c` is the view's Mercator as every deeper zoom draws it, the centre
//! at the middle of the screen. `W_c` is the world projection — [`Shape`]:
//! Equal Earth (Šavrič, Patterson, Jenny 2018, the polynomial below) or
//! the orthographic globe — with its central meridian at the view centre's
//! longitude, so north is up at the centre in both and nothing tilts while
//! blending, and scaled and shifted so that the view centre maps to the
//! same pixel and the local scale at the centre agrees with Mercator's.
//!
//! Equal Earth is not conformal: at the centre its east–west and
//! north–south scales differ ([`Blend::anisotropy`]), their geometric mean
//! exactly one (it is equal-area). The match takes that mean, so at
//! `t = 0` the picture is true Equal Earth and areas keep their ratios;
//! as `t` grows each axis' scale slides towards Mercator's
//! (`e^(±t·ln k)`), so near the handover the world term agrees with
//! Mercator locally and the middle of the screen morphs as little as the
//! shapes themselves allow. The globe's centre scale is one in every
//! direction; nothing slides.
//!
//! `t` is a smooth step of the view zoom ([`t_of_zoom`]): 0 up to
//! [`WORLD_ZOOM`], 1 from [`MERCATOR_ZOOM`] on, `6u⁵-15u⁴+10u³` between —
//! C² at both ends, no kink. From `t = 1` the view is Mercator exactly
//! and every tile, golden and drawer below stays as it is; a blended view
//! never draws a region's tiles, so no raster or tile is ever reprojected
//! (`crate::choice::drawn` keeps regions on the linear path).
//!
//! Because both mappings put the centre on the same pixel at the same
//! local scale, what is in the middle of the screen does not move during
//! a zoom beyond what the zoom itself explains; only the far parts of a
//! world view bend. Panning turns the central meridian with the view, so
//! the world wraps and nothing jumps at the antimeridian.
//!
//! The screen-to-world direction has no closed form under the blend:
//! [`Blend::to_world`] runs Newton on the forward mapping, started at the
//! Mercator inverse, and answers only when it lands within a thirty-second
//! of a pixel ([`INVERSE_EPS_PX`]); a point off the world (outside Equal
//! Earth's boundary, off the globe's limb) gets `None`.

use crate::mercator::World;
use crate::viewport::Viewport;

/// Equal Earth's polynomial coefficients (Šavrič, Patterson, Jenny 2018,
/// "The Equal Earth map projection", IJGIS 33(3), equations 2 and 3).
const A1: f64 = 1.340264;
const A2: f64 = -0.081106;
const A3: f64 = 0.000893;
const A4: f64 = 0.003796;

/// `sin θ = M sin φ`: the square root of three over two.
const M: f64 = 0.866_025_403_784_438_6;

const PI: f64 = core::f64::consts::PI;
const TAU: f64 = core::f64::consts::TAU;
/// The world's width in world units.
const WORLD_F: f64 = 4_294_967_296.0;

/// Up to this view zoom the world projection stands alone (`t = 0`).
pub const WORLD_ZOOM: f64 = 2.0;
/// From this view zoom on the view is Mercator exactly (`t = 1`): below
/// the shallowest zoom regions are cut at, so their tiles never see the
/// blend.
pub const MERCATOR_ZOOM: f64 = 5.0;

/// Newton stops when the screen error is below this, in pixels.
const INVERSE_EPS_PX: f64 = 0.03125;
/// Newton gives up after this many steps.
const INVERSE_STEPS: u32 = 24;

/// The projection a world view blends out of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Shape {
    /// Equal Earth: areas true to size, the whole world on screen.
    #[default]
    EqualEarth,
    /// The orthographic globe: the near hemisphere, the limb a circle.
    Globe,
}

/// Equal Earth's projection of `(λ, φ)` (longitude relative to the
/// central meridian, latitude; radians): x east and y north, in units
/// whose local scale has geometric mean one everywhere (equal-area).
fn equal_earth(lam: f64, phi: f64) -> (f64, f64) {
    let theta = libm::asin((M * libm::sin(phi)).clamp(-1.0, 1.0));
    let t2 = theta * theta;
    let t6 = t2 * t2 * t2;
    let x = lam * libm::cos(theta) / (M * (A1 + 3.0 * A2 * t2 + t6 * (7.0 * A3 + 9.0 * A4 * t2)));
    let y = theta * (A1 + A2 * t2 + t6 * (A3 + A4 * t2));
    (x, y)
}

/// The blend's smooth step: 0 at `lo` and below, 1 at `hi` and above,
/// `6u⁵-15u⁴+10u³` between — its first and second derivatives vanish at
/// both ends, so a zoom ramp has no kink entering or leaving the blend.
pub fn t_of_zoom(zoom: f64, lo: f64, hi: f64) -> f64 {
    if hi <= lo {
        return if zoom >= hi { 1.0 } else { 0.0 };
    }
    let u = ((zoom - lo) / (hi - lo)).clamp(0.0, 1.0);
    u * u * u * (u * (u * 6.0 - 15.0) + 10.0)
}

/// A world view's blended mapping: Mercator world coordinates to screen
/// pixels and back, at one centre, zoom and size.
#[derive(Clone, Copy, Debug)]
pub struct Blend {
    shape: Shape,
    centre: World,
    width: u32,
    height: u32,
    zoom: f64,
    t: f64,
    /// The centre's longitude and latitude, radians: the central meridian
    /// and the scale anchor.
    lon0: f64,
    phi0: f64,
    sin0: f64,
    cos0: f64,
    /// Mercator world units a pixel spans.
    unit: f64,
    /// The world term's axis scales, pixels per projection unit.
    sx: f64,
    sy: f64,
    /// The world projection of the centre, subtracted so it lands on the
    /// middle pixel.
    ex0: f64,
    ey0: f64,
    /// East–west over north–south local scale at the centre, 1 for the
    /// globe.
    anisotropy: f64,
}

impl Blend {
    /// The blend for a view: `t` from the zoom by [`t_of_zoom`], between
    /// [`WORLD_ZOOM`] — or the width's [`Viewport::min_zoom`] where a wide
    /// screen's world floor is deeper — and [`MERCATOR_ZOOM`], kept at
    /// least one zoom above the lower bound.
    pub fn new(shape: Shape, centre: World, zoom: f64, width: u32, height: u32) -> Self {
        let lo = WORLD_ZOOM.max(f64::from(Viewport::min_zoom(width)));
        let hi = MERCATOR_ZOOM.max(lo + 1.0);
        let t = t_of_zoom(zoom, lo, hi);
        Self::with_t(shape, centre, zoom, width, height, t)
    }

    /// The blend with `t` given: a supersampled draw of the same view at
    /// another zoom keeps its `t`, so both pictures are the same mapping.
    pub fn with_t(shape: Shape, centre: World, zoom: f64, width: u32, height: u32, t: f64) -> Self {
        let lon0 = f64::from(centre.x) / WORLD_F * TAU - PI;
        let phi0 = gudermann(f64::from(centre.y));
        let (sin0, cos0) = (libm::sin(phi0), libm::cos(phi0));
        let unit = libm::exp2(24.0 - zoom);
        // Pixels per radian of arc at the centre: 256·2^zoom pixels around
        // the world, over 2π, over the parallel's cos φ.
        let m = 256.0 * libm::exp2(zoom) / (TAU * cos0.max(1e-9));
        let (ex0, ey0, anisotropy, sx, sy) = match shape {
            Shape::EqualEarth => {
                let theta = libm::asin((M * sin0).clamp(-1.0, 1.0));
                let t2 = theta * theta;
                let t6 = t2 * t2 * t2;
                let d = A1 + 3.0 * A2 * t2 + t6 * (7.0 * A3 + 9.0 * A4 * t2);
                let ey0 = theta * (A1 + A2 * t2 + t6 * (A3 + A4 * t2));
                // East–west scale per unit of arc; north–south is its
                // reciprocal (equal-area), so the geometric mean is one
                // and `m` alone matches Mercator's scale at the centre.
                let k = libm::cos(theta) / (M * d * cos0.max(1e-9));
                // Each axis slides from the true shape at t = 0 to
                // Mercator's local scale at t = 1.
                let slide = libm::pow(k, t);
                (0.0, ey0, k * k, m / slide, m * slide)
            }
            Shape::Globe => (0.0, 0.0, 1.0, m, m),
        };
        Self {
            shape,
            centre,
            width: width.max(1),
            height: height.max(1),
            zoom,
            t: t.clamp(0.0, 1.0),
            lon0,
            phi0,
            sin0,
            cos0,
            unit,
            sx,
            sy,
            ex0,
            ey0,
            anisotropy,
        }
    }

    /// How far the blend is towards Mercator, 0 to 1.
    pub fn t(&self) -> f64 {
        self.t
    }

    /// The view zoom the blend was built for.
    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    /// The view's centre.
    pub fn centre(&self) -> World {
        self.centre
    }

    /// Which world projection blends here.
    pub fn shape(&self) -> Shape {
        self.shape
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The central meridian and the centre's latitude, radians: what the
    /// anisotropy is reported against.
    pub fn centre_angles(&self) -> (f64, f64) {
        (self.lon0, self.phi0)
    }

    /// East–west over north–south local scale at the centre before the
    /// axes slide: Equal Earth's k² (0.74 at the equator, above 2 past
    /// 65°), exactly 1 for the globe.
    pub fn anisotropy(&self) -> f64 {
        self.anisotropy
    }

    /// The same mapping at another zoom and size, `t` and centre kept:
    /// what a supersampling drawer (the terminal's source one zoom
    /// deeper at twice the grain) uses so its picture is this view's,
    /// only finer.
    pub fn resized(&self, zoom: f64, width: u32, height: u32) -> Self {
        Self::with_t(self.shape, self.centre, zoom, width, height, self.t)
    }

    /// `(λ, φ)` of a world point, λ relative to the central meridian in
    /// (-π, π].
    fn angles(&self, w: World) -> (f64, f64) {
        let lon = f64::from(w.x) / WORLD_F * TAU - PI;
        let mut lam = lon - self.lon0;
        if lam <= -PI {
            lam += TAU;
        } else if lam > PI {
            lam -= TAU;
        }
        (lam, gudermann(f64::from(w.y)))
    }

    /// The world projection of `(λ, φ)` with the centre's frame applied:
    /// x east, y north, local geometric-mean scale one at the centre.
    fn shape_forward(&self, lam: f64, phi: f64) -> (f64, f64) {
        match self.shape {
            Shape::EqualEarth => equal_earth(lam, phi),
            Shape::Globe => {
                let (sp, cp) = (libm::sin(phi), libm::cos(phi));
                let (sl, cl) = (libm::sin(lam), libm::cos(lam));
                let x = cp * sl;
                let y = self.cos0 * sp - self.sin0 * cp * cl;
                let cosc = self.sin0 * sp + self.cos0 * cp * cl;
                if cosc >= 0.0 {
                    (x, y)
                } else {
                    // The far half, pushed out to the limb so a ring that
                    // crosses it stays closed; culled where it matters by
                    // the caller through `facing`.
                    let r = libm::hypot(x, y);
                    if r < 1e-12 {
                        (0.0, 1.0)
                    } else {
                        (x / r, y / r)
                    }
                }
            }
        }
    }

    /// Whether `w` is on the near side of the globe; always under Equal
    /// Earth. A name or a pin on the far side is not shown at world zoom.
    pub fn facing(&self, w: World) -> bool {
        match self.shape {
            Shape::EqualEarth => true,
            Shape::Globe => {
                let (lam, phi) = self.angles(w);
                self.sin0 * libm::sin(phi) + self.cos0 * libm::cos(phi) * libm::cos(lam) >= 0.0
            }
        }
    }

    /// Where `w` lands in pixels from the view's top-left corner: the
    /// blend of the world projection's place and Mercator's, both with
    /// the centre on the middle pixel.
    pub fn to_screen(&self, w: World) -> (f64, f64) {
        let (cx, cy) = (f64::from(self.width) / 2.0, f64::from(self.height) / 2.0);
        // Mercator as `ExactView::to_screen`: x the copy nearest the
        // centre, by the wrapping difference.
        let dx = f64::from(w.x.wrapping_sub(self.centre.x) as i32);
        let dy = f64::from(w.y) - f64::from(self.centre.y);
        let (mx, my) = (cx + dx / self.unit, cy + dy / self.unit);
        if self.t >= 1.0 {
            return (mx, my);
        }
        let (lam, phi) = self.angles(w);
        let (ex, ey) = self.shape_forward(lam, phi);
        let bx = cx + self.sx * (ex - self.ex0);
        let by = cy - self.sy * (ey - self.ey0);
        let t = self.t;
        ((1.0 - t) * bx + t * mx, (1.0 - t) * by + t * my)
    }

    /// [`to_screen`](Self::to_screen) in sub-pixels (sixteenths), the
    /// drawers' unit, clamped to a range every later cast holds.
    pub fn to_screen_sub(&self, w: World) -> (i64, i64) {
        let (x, y) = self.to_screen(w);
        let sub = |v: f64| libm::round((v * 16.0).clamp(-1e9, 1e9)) as i64;
        (sub(x), sub(y))
    }

    /// The world point that lands on `(x, y)` pixels: Newton on the
    /// forward mapping from the Mercator inverse, `None` where no point
    /// does (off Equal Earth's boundary, off the globe) or Newton does
    /// not close within [`INVERSE_EPS_PX`].
    pub fn to_world(&self, x: f64, y: f64) -> Option<World> {
        let (cx, cy) = (f64::from(self.width) / 2.0, f64::from(self.height) / 2.0);
        // Continuous world coordinates; ux wraps, uy is clamped.
        let mut ux = f64::from(self.centre.x) + (x - cx) * self.unit;
        let mut uy = (f64::from(self.centre.y) + (y - cy) * self.unit).clamp(0.0, WORLD_F - 1.0);
        let at = |ux: f64, uy: f64| -> (f64, f64) {
            self.to_screen(World {
                x: wrap_world(ux),
                y: uy.clamp(0.0, WORLD_F - 1.0) as u32,
            })
        };
        let h = self.unit;
        for _ in 0..INVERSE_STEPS {
            let (px, py) = at(ux, uy);
            let (fx, fy) = (px - x, py - y);
            if libm::fabs(fx) < INVERSE_EPS_PX && libm::fabs(fy) < INVERSE_EPS_PX {
                return Some(World {
                    x: wrap_world(ux),
                    y: uy.clamp(0.0, WORLD_F - 1.0) as u32,
                });
            }
            let (qx, qy) = at(ux + h, uy);
            let (rx, ry) = at(ux, uy + h);
            let (j11, j21) = ((qx - px) / h, (qy - py) / h);
            let (j12, j22) = ((rx - px) / h, (ry - py) / h);
            let det = j11 * j22 - j12 * j21;
            if libm::fabs(det) < 1e-18 {
                return None;
            }
            let dx = (fx * j22 - fy * j12) / det;
            let dy = (fy * j11 - fx * j21) / det;
            // A step never crosses more than a quarter of the world: far
            // outside the picture Newton would otherwise overshoot.
            let cap = WORLD_F / 4.0;
            ux -= dx.clamp(-cap, cap);
            uy = (uy - dy.clamp(-cap, cap)).clamp(0.0, WORLD_F - 1.0);
        }
        None
    }
}

/// The latitude of a continuous Mercator world y, radians.
fn gudermann(y: f64) -> f64 {
    let n = PI * (1.0 - 2.0 * y / WORLD_F);
    libm::atan(libm::sinh(n))
}

/// A continuous world x back onto the wrapped `u32` axis.
fn wrap_world(x: f64) -> u32 {
    let v = x - libm::floor(x / WORLD_F) * WORLD_F;
    if v >= WORLD_F {
        0
    } else {
        v as u32
    }
}
