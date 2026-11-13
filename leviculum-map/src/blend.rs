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
use alloc::vec::Vec;

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

/// Newton stops within this of the asked pixel.
pub const INVERSE_EPS_PX: f64 = 0.03125;
/// The limb is drawn as a polygon of this many corners all round.
const LIMB_STEPS: u32 = 360;

/// Mercator's latitude limit, radians: 85.0511°.
const MERCATOR_LAT: f64 = 1.484_422_229_745_332_4;

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
        // The globe's size at the world is the zoom's alone, the same
        // wherever it is turned to (1762): a drag towards a pole turns it
        // and does not swell it by Mercator's 1/cos φ. Both terms are
        // drawn that much smaller, so they still agree at the centre and
        // morph no more than before; the factor slides to one with `t`,
        // where the view is Mercator exactly.
        let globe = match shape {
            Shape::Globe => libm::pow(cos0.max(1e-9), 1.0 - t.clamp(0.0, 1.0)),
            Shape::EqualEarth => 1.0,
        };
        let unit = libm::exp2(24.0 - zoom) / globe;
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
            Shape::Globe => (0.0, 0.0, 1.0, m * globe, m * globe),
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
        (self.lam_rel(w.x), gudermann(f64::from(w.y)))
    }

    /// The longitude of a world x relative to the central meridian,
    /// wrapped to (-π, π]: the cut of the world projection is the far
    /// side of the view. A ring that crosses it is unwrapped by the
    /// drawer and placed through
    /// [`to_screen_angles`](Self::to_screen_angles).
    pub fn lam_rel(&self, x: u32) -> f64 {
        let lon = f64::from(x) / WORLD_F * TAU - PI;
        let mut lam = lon - self.lon0;
        if lam <= -PI {
            lam += TAU;
        } else if lam > PI {
            lam -= TAU;
        }
        lam
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

    /// Whether this is the orthographic globe, whose far half is hidden
    /// behind the horizon and cut there (1762).
    pub fn is_globe(&self) -> bool {
        self.shape == Shape::Globe
    }

    /// The unit vector of `(λ, φ)` in the view's frame: x east and y north
    /// at the centre, z towards the eye. `z` is the cosine of the angular
    /// distance to the view centre; negative is behind the horizon.
    fn frame(&self, lam: f64, phi: f64) -> [f64; 3] {
        let (sp, cp) = (libm::sin(phi), libm::cos(phi));
        let (sl, cl) = (libm::sin(lam), libm::cos(lam));
        [
            cp * sl,
            self.cos0 * sp - self.sin0 * cp * cl,
            self.sin0 * sp + self.cos0 * cp * cl,
        ]
    }

    /// `(λ, φ)` of a view-frame vector, λ relative to the central meridian
    /// in (-π, π].
    fn angles_of(&self, v: [f64; 3]) -> (f64, f64) {
        let [x, y, z] = v;
        let sp = (y * self.cos0 + z * self.sin0).clamp(-1.0, 1.0);
        let lam = libm::atan2(x, z * self.cos0 - y * self.sin0);
        (lam, libm::asin(sp))
    }

    /// [`to_screen`](Self::to_screen) of a point given by its relative
    /// longitude, which may run past ±π along a ring, and its latitude,
    /// radians. The Mercator term flattens the overshoot onto the edge
    /// meridian and holds the latitude to Mercator's ±85.05°; the world
    /// term is the shape's own (Equal Earth's clamped to its edge
    /// meridian, the globe periodic).
    pub fn to_screen_angles(&self, lam: f64, phi: f64) -> (f64, f64) {
        let (cx, cy) = (f64::from(self.width) / 2.0, f64::from(self.height) / 2.0);
        let lm = lam.clamp(-PI, PI);
        let pm = phi.clamp(-MERCATOR_LAT, MERCATOR_LAT);
        let wy = WORLD_F / 2.0 * (1.0 - libm::asinh(libm::tan(pm)) / PI);
        let mx = cx + lm * (WORLD_F / TAU) / self.unit;
        let my = cy + (wy - f64::from(self.centre.y)) / self.unit;
        if self.t >= 1.0 {
            return (mx, my);
        }
        let shaped = match self.shape {
            Shape::EqualEarth => lm,
            Shape::Globe => lam,
        };
        let (ex, ey) = self.shape_forward(shaped, phi);
        let bx = cx + self.sx * (ex - self.ex0);
        let by = cy - self.sy * (ey - self.ey0);
        let t = self.t;
        ((1.0 - t) * bx + t * mx, (1.0 - t) * by + t * my)
    }

    /// The horizon point at screen angle `alpha` (radians, measured as on
    /// the screen: x right, y down), on the screen in sub-pixels. At
    /// `t = 0` the limb circle; in the handover its image under the
    /// blend. Its longitude is the plain relative one in (-π, π]: along
    /// the horizon that jumps only at the seam ([`seam`](Self::seam)),
    /// where an outline goes over the pole.
    fn limb_at(&self, alpha: f64) -> (i64, i64) {
        let (lam, phi) = self.angles_of([libm::cos(alpha), -libm::sin(alpha), 0.0]);
        self.sub(lam, phi)
    }

    /// The seam of the handover on the horizon: the screen angle where the
    /// horizon crosses the far meridian, on the side of the visible pole,
    /// and that pole's latitude. The Mercator term jumps a world's width
    /// there, so an outline passing it goes over the pole instead. `None`
    /// at `t = 0`, where nothing jumps, and for a centre on the equator,
    /// whose horizon meets the far meridian only at the poles.
    fn seam(&self) -> Option<(f64, f64)> {
        if self.t <= 0.0 {
            return None;
        }
        match self.sin0 {
            s if s > 1e-12 => Some((-PI / 2.0, PI / 2.0)),
            s if s < -1e-12 => Some((PI / 2.0, -PI / 2.0)),
            _ => None,
        }
    }

    /// The detour over the pole at the seam `seam`, coming from the side
    /// `sense` leaves: the pole on the far meridian's one side, then on
    /// its other.
    fn over_pole(&self, seam: f64, pole: f64, sense: f64, out: &mut Vec<(i64, i64)>) {
        let side = |a: f64| self.angles_of([libm::cos(a), -libm::sin(a), 0.0]).0;
        out.push(self.sub(side(seam - sense * 1e-6), pole));
        out.push(self.sub(side(seam + sense * 1e-6), pole));
    }

    /// The globe's outline, the horizon all round, in sub-pixels: what
    /// the sea is filled in and the ground is left around (1762). Empty
    /// for Equal Earth, whose world has no horizon. The first `n` points
    /// (the count returned) are the horizon itself; in the handover a
    /// view off the equator closes over its visible pole after them
    /// ([`seam`](Self::seam)). At `t = 0` there is no detour.
    pub fn limb_sub(&self, out: &mut Vec<(i64, i64)>) -> usize {
        out.clear();
        if !self.is_globe() {
            return 0;
        }
        let seam = self.seam();
        let start = seam.map_or(0.0, |(a, _)| a);
        // Start just past the seam and end just before it: the horizon's
        // longitudes run from one side of the far meridian to the other
        // without a jump.
        let eps = 1e-6;
        for k in 0..=LIMB_STEPS {
            let f = f64::from(k) / f64::from(LIMB_STEPS);
            out.push(self.limb_at(start + eps + f * (TAU - 2.0 * eps)));
        }
        let n = out.len();
        if let Some((a, pole)) = seam {
            self.over_pole(a, pole, 1.0, out);
        }
        n
    }

    /// Whether everything between the relative longitudes `lam0` and
    /// `lam0 + width` and the latitudes `phi0..=phi1` (radians) lies
    /// behind the globe's horizon: a bound, never wrong the other way, so
    /// a ring culled by its box needs no projecting at all (1762). Never
    /// under Equal Earth.
    pub fn behind(&self, lam0: f64, width: f64, phi0: f64, phi1: f64) -> bool {
        if !self.is_globe() {
            return false;
        }
        // The largest cos λ over the span.
        let next = libm::ceil(lam0 / TAU) * TAU;
        let cmax = match next <= lam0 + width {
            true => 1.0,
            false => libm::cos(lam0).max(libm::cos(lam0 + width)),
        };
        // cos c ≤ sin φ₀ sin φ + cos φ₀ cos φ cmax, as cos φ₀ cos φ ≥ 0;
        // its largest value over the latitudes, ends and stationary point.
        let (a, b) = (self.sin0, self.cos0 * cmax);
        let f = |phi: f64| a * libm::sin(phi) + b * libm::cos(phi);
        let mut best = f(phi0).max(f(phi1));
        let star = libm::atan2(a, b);
        if (phi0..=phi1).contains(&star) {
            best = best.max(libm::hypot(a, b));
        }
        best < 0.0
    }

    /// One ring of the globe, its points `(λ, φ)` (relative longitude,
    /// latitude; radians), cut at the horizon (1762): the part behind it
    /// is dropped, and where the ring leaves and comes back the limb
    /// closes it, the arc going the ring's own way round. Rings for an
    /// even-odd fill come out, in sub-pixels: the ring itself, or nothing
    /// when it is wholly behind, or the near part and, where the arcs
    /// alone close the outside instead of the inside, the whole limb as a
    /// second ring. In the handover the ring goes over the visible pole
    /// where it crosses the far meridian ([`seam`](Self::seam)), as the
    /// limb does, so land and sea agree there.
    pub fn globe_ring(&self, ring: &[(f64, f64)], out: &mut Vec<Vec<(i64, i64)>>) {
        out.clear();
        let n = ring.len();
        if n < 3 {
            return;
        }
        let v: Vec<[f64; 3]> = ring.iter().map(|&(l, p)| self.frame(l, p)).collect();
        let mut path: Vec<(i64, i64)> = Vec::new();
        let mut last: Option<([f64; 3], f64)> = None;
        let Some(start) = v.iter().position(|p| p[2] < 0.0) else {
            // Wholly in front; the closing step, too, may cross the seam.
            for (i, &(l, p)) in ring.iter().enumerate() {
                self.put(&mut path, &mut last, v[i], wrap(l), p);
            }
            let (l0, p0) = ring[0];
            self.put(&mut path, &mut last, v[0], wrap(l0), p0);
            path.pop();
            out.push(path);
            return;
        };
        if v.iter().all(|p| p[2] < 0.0) {
            return;
        }
        // The ring's own sense on the screen (x right, y down, as east and
        // south): every projection here keeps it. The longitudes are
        // unwrapped along the ring for the area.
        let mut area = 0.0;
        let mut prev = ring[0].0;
        for i in 0..n {
            let (la, pa) = (prev, ring[i].1);
            let (lb, pb) = (unwrap_near(ring[(i + 1) % n].0, prev), ring[(i + 1) % n].1);
            area += la * -pb - lb * -pa;
            prev = lb;
        }
        let sense = if area < 0.0 { -1.0 } else { 1.0 };
        let mut arcs: Vec<(f64, f64)> = Vec::new();
        let mut first_entry: Option<f64> = None;
        let mut open: Option<f64> = None;
        for k in 0..n {
            let i = (start + k) % n;
            let j = (i + 1) % n;
            let (za, zb) = (v[i][2], v[j][2]);
            if (za < 0.0) != (zb < 0.0) {
                let f = za / (za - zb);
                let x = v[i][0] + (v[j][0] - v[i][0]) * f;
                let y = v[i][1] + (v[j][1] - v[i][1]) * f;
                let r = libm::hypot(x, y).max(1e-12);
                let c = [x / r, y / r, 0.0];
                let alpha = libm::atan2(-c[1], c[0]);
                let (lam, phi) = self.angles_of(c);
                if za < 0.0 {
                    // Coming back: the arc from where it left, which
                    // passes the seam itself where it does.
                    match open.take() {
                        Some(from) => {
                            self.arc(from, alpha, sense, &mut path);
                            arcs.push((from, alpha));
                        }
                        None => first_entry = Some(alpha),
                    }
                    last = None;
                }
                self.put(&mut path, &mut last, c, lam, phi);
                if za >= 0.0 {
                    open = Some(alpha);
                }
            }
            if zb >= 0.0 {
                self.put(&mut path, &mut last, v[j], wrap(ring[j].0), ring[j].1);
            }
        }
        // The last exit closes onto the first entry.
        if let (Some(from), Some(to)) = (open, first_entry) {
            self.arc(from, to, sense, &mut path);
            arcs.push((from, to));
        }
        if path.len() >= 3 {
            out.push(path);
        }
        // Just past an exit, the ring's own way round, the limb is inside
        // the ring. When an even number of arcs covers that point the
        // arcs closed the outside: the whole limb flips it back.
        if let Some(&(exit, _)) = arcs.first() {
            let probe = exit + sense * 1e-7;
            let covered = arcs
                .iter()
                .filter(|&&(a, b)| {
                    let at = match sense > 0.0 {
                        true => turn(probe - a),
                        false => turn(a - probe),
                    };
                    at < arc_span(a, b, sense)
                })
                .count();
            if covered % 2 == 0 {
                let mut limb = Vec::new();
                self.limb_sub(&mut limb);
                out.push(limb);
            }
        }
    }

    /// Push the near point `v` (`(λ, φ)`, λ in (-π, π]) onto `path`; in
    /// the handover, when the step from the point before crosses the far
    /// meridian, over the visible pole first.
    fn put(
        &self,
        path: &mut Vec<(i64, i64)>,
        last: &mut Option<([f64; 3], f64)>,
        v: [f64; 3],
        lam: f64,
        phi: f64,
    ) {
        if let (Some((_, pole)), Some((u, l))) = (self.seam(), *last) {
            if libm::fabs(lam - l) > PI {
                // Where the step meets the far meridian: x = 0 in the
                // frame.
                let f = u[0] / (u[0] - v[0]);
                let c = [
                    u[0] + (v[0] - u[0]) * f,
                    u[1] + (v[1] - u[1]) * f,
                    u[2] + (v[2] - u[2]) * f,
                ];
                let r = libm::sqrt(c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).max(1e-12);
                let (_, phc) = self.angles_of([c[0] / r, c[1] / r, c[2] / r]);
                let side = if l > 0.0 { PI } else { -PI };
                path.push(self.sub(side, phc));
                path.push(self.sub(side, pole));
                path.push(self.sub(-side, pole));
                path.push(self.sub(-side, phc));
            }
        }
        path.push(self.sub(lam, phi));
        *last = Some((v, lam));
    }

    /// The limb from screen angle `from` to `to`, going `sense` round
    /// (positive: increasing angle), its inner points pushed in
    /// sub-pixels, over the pole where it passes the seam.
    fn arc(&self, from: f64, to: f64, sense: f64, out: &mut Vec<(i64, i64)>) {
        let span = arc_span(from, to, sense);
        let steps = libm::ceil(span / (TAU / f64::from(LIMB_STEPS))) as u32;
        let seam = self.seam();
        // How far along the arc the seam lies, if it does.
        let at_seam = seam
            .map(|(a, _)| arc_span(from, a, sense))
            .filter(|&d| d < span);
        let mut passed = false;
        for k in 1..steps {
            let d = span * f64::from(k) / f64::from(steps);
            if let (Some(ds), Some((a, pole)), false) = (at_seam, seam, passed) {
                if d >= ds {
                    self.over_pole(a, pole, sense, out);
                    passed = true;
                }
            }
            out.push(self.limb_at(from + sense * d));
        }
        if let (Some(_), Some((a, pole)), false) = (at_seam, seam, passed) {
            self.over_pole(a, pole, sense, out);
        }
    }

    /// [`to_screen_angles`](Self::to_screen_angles) in sub-pixels.
    fn sub(&self, lam: f64, phi: f64) -> (i64, i64) {
        let (x, y) = self.to_screen_angles(lam, phi);
        (sub_px(x), sub_px(y))
    }

    /// Where the segment from `a` to `b` crosses the globe's horizon, in
    /// sub-pixels, when one end is in front of it and the other behind: a
    /// stroked line is cut there, not at its last near point (1762).
    pub fn horizon_sub(&self, a: World, b: World) -> Option<(i64, i64)> {
        if !self.is_globe() {
            return None;
        }
        let (la, pa) = self.angles(a);
        let (lb, pb) = self.angles(b);
        let (va, vb) = (self.frame(la, pa), self.frame(lb, pb));
        if (va[2] < 0.0) == (vb[2] < 0.0) {
            return None;
        }
        let f = va[2] / (va[2] - vb[2]);
        let c = [va[0] + (vb[0] - va[0]) * f, va[1] + (vb[1] - va[1]) * f];
        Some(self.limb_at(libm::atan2(-c[1], c[0])))
    }

    /// [`to_screen`](Self::to_screen) in sub-pixels (sixteenths), the
    /// drawers' unit, clamped to a range every later cast holds.
    pub fn to_screen_sub(&self, w: World) -> (i64, i64) {
        let (x, y) = self.to_screen(w);
        (sub_px(x), sub_px(y))
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

/// `lam` brought into (-π, π].
fn wrap(lam: f64) -> f64 {
    let w = lam - libm::round(lam / TAU) * TAU;
    if w <= -PI {
        w + TAU
    } else {
        w
    }
}

/// The longitude `lam` a whole number of turns over, within half a turn
/// of `near`: continuous along a path.
fn unwrap_near(lam: f64, near: f64) -> f64 {
    lam - libm::round((lam - near) / TAU) * TAU
}

/// `v` brought into [0, 2π).
fn turn(v: f64) -> f64 {
    let r = v - libm::floor(v / TAU) * TAU;
    if r >= TAU {
        0.0
    } else {
        r
    }
}

/// How far round from `from` to `to`, going `sense` round, in [0, 2π).
fn arc_span(from: f64, to: f64, sense: f64) -> f64 {
    match sense > 0.0 {
        true => turn(to - from),
        false => turn(from - to),
    }
}

/// Pixels to sub-pixels (sixteenths), clamped to a range every later
/// cast holds.
fn sub_px(v: f64) -> i64 {
    libm::round((v * 16.0).clamp(-1e9, 1e9)) as i64
}

/// The latitude of a ring point at world y, radians, for the globe
/// (1762): Mercator's edge rows are the poles. A ring that closes along
/// the bottom of the world (Antarctica) closes through the pole on the
/// sphere, so the cap south of 85° is land and not a hole.
pub fn ring_latitude(wy: u32) -> f64 {
    let phi = gudermann(f64::from(wy));
    match phi {
        p if p > EDGE_LAT => PI / 2.0,
        p if p < -EDGE_LAT => -PI / 2.0,
        p => p,
    }
}

/// A latitude past this is on Mercator's edge row: 85.04°.
const EDGE_LAT: f64 = 1.484_229_6;

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
