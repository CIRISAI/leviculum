//! Wide lines as quads through the filler.
//!
//! Each segment of a polyline becomes a rectangle `width` across. With
//! square caps it is extended by half the width past both ends, which also
//! closes the wedge at a joint up to a right angle. With round joins it is
//! not extended, and a disc of the line's width goes on every vertex where
//! the line turns, and on both ends: a round cap and a round join in one.
//! All quads and discs are built with the same orientation and filled
//! together under [`FillRule::NonZero`], so where they overlap the windings
//! add and the union is filled once. A dashed line is the same walk with
//! only the pieces of the pattern's "on" lengths turned into quads.

use embedded_graphics::prelude::*;

use crate::coverage::Coverage;
use crate::fill::{FillRule, FillStats, Filler};

/// Integer square root, `floor(sqrt(v))`.
pub fn isqrt(v: u64) -> u64 {
    if v < 2 {
        return v;
    }
    // Newton from a power of two at or above the root, below 2^33, so
    // `x + v / x` cannot overflow.
    let mut x = 1u64 << ((64 - v.leading_zeros()) / 2 + 1);
    let mut y = (x + v / x) / 2;
    while y < x {
        x = y;
        y = (x + v / x) / 2;
    }
    x
}

/// How a line is drawn: its width in sub-pixels, its ends and joints, and
/// its dash pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pen {
    pub width: i32,
    /// Round joins and caps rather than square caps.
    pub round: bool,
    /// `(on, off)` in sub-pixels: drawn for `on`, left out for `off`, from
    /// the line's start. Dashes have flat ends.
    pub dash: Option<(i32, i32)>,
}

impl Pen {
    /// A solid line with square caps, `width` sub-pixels wide.
    pub fn square(width: i32) -> Self {
        Self {
            width,
            round: false,
            dash: None,
        }
    }
}

/// `cos` and `sin` of sixteen directions a sixteenth of a turn apart, in
/// units of 2^14: the corners of a round join's disc.
const DISC: [(i32, i32); 16] = [
    (16384, 0),
    (15137, 6270),
    (11585, 11585),
    (6270, 15137),
    (0, 16384),
    (-6270, 15137),
    (-11585, 11585),
    (-15137, 6270),
    (-16384, 0),
    (-15137, -6270),
    (-11585, -11585),
    (-6270, -15137),
    (0, -16384),
    (6270, -15137),
    (11585, -11585),
    (15137, -6270),
];

/// A disc's radius in sub-pixels from which it gets all sixteen corners;
/// a thinner line's disc takes every other one, which is within a
/// sixteenth of a pixel of round at that size.
const FINE_DISC: i64 = 48;

/// A turn whose outer wedge between two flat-ended quads is at most this
/// many sub-pixels deep gets no disc: a quarter of a pixel, below what a
/// pixel centre can tell.
const FLAT_TURN: i64 = 4;

/// Stroke the polyline `pts` (sub-pixels) `width` sub-pixels wide with
/// square caps: [`stroke_pen`] with [`Pen::square`], in a loop of its own.
/// It is every line the M9 draws, and the general walk's bookkeeping for
/// joints and dashes cost it a quarter more instructions (callgrind, 1099).
pub fn stroke<D: Coverage>(
    f: &mut Filler<'_, '_>,
    pts: &[Point],
    width: i32,
    target: &mut D,
    color: D::Color,
) -> Result<FillStats, D::Error> {
    let mut st = FillStats::default();
    let half = i64::from(width.max(1)) / 2;
    let (l, t, r, b) = f.clip_sub();
    let pad = half as i32 + 1;
    for w in pts.windows(2) {
        let (a, z) = (w[0], w[1]);
        if a.x.max(z.x) + pad < l
            || a.x.min(z.x) - pad > r
            || a.y.max(z.y) + pad < t
            || a.y.min(z.y) - pad > b
        {
            continue;
        }
        let dx = i64::from(z.x) - i64::from(a.x);
        let dy = i64::from(z.y) - i64::from(a.y);
        let len = isqrt((dx * dx + dy * dy) as u64) as i64;
        if len == 0 {
            continue;
        }
        if !add(
            f,
            &quad(a, z, dx, dy, len, half, true),
            target,
            color,
            &mut st,
        )? {
            // A scratch smaller than one quad cannot stroke at all: drop
            // the line rather than loop.
            f.clear();
            return Ok(st);
        }
    }
    st += f.fill(target, color, FillRule::NonZero)?;
    Ok(st)
}

/// Stroke the polyline `pts` (sub-pixels) with `pen`.
///
/// Segments whose quad lies wholly outside the filler's clip are skipped.
/// When the scratch runs out of edge slots the shapes so far are filled
/// and the walk goes on: an opaque colour painted twice is the same picture.
pub fn stroke_pen<D: Coverage>(
    f: &mut Filler<'_, '_>,
    pts: &[Point],
    pen: Pen,
    target: &mut D,
    color: D::Color,
) -> Result<FillStats, D::Error> {
    if pen == Pen::square(pen.width) {
        return stroke(f, pts, pen.width, target, color);
    }
    let mut st = FillStats::default();
    let half = i64::from(pen.width.max(1)) / 2;
    let clip = f.clip_sub();
    let pad = half as i32 + 1;
    let outside = |a: Point, z: Point| {
        let (l, t, r, b) = clip;
        a.x.max(z.x) + pad < l
            || a.x.min(z.x) - pad > r
            || a.y.max(z.y) + pad < t
            || a.y.min(z.y) - pad > b
    };
    // Where the dash pattern stands at the current segment's start.
    let mut phase = 0i64;
    // The previous drawn segment's direction and length, for the joint.
    let mut prev: Option<(i64, i64, i64)> = None;
    let round = pen.round && pen.dash.is_none();
    let mut stalled = false;
    for (i, w) in pts.windows(2).enumerate() {
        let (a, z) = (w[0], w[1]);
        let seen = !outside(a, z);
        if !seen && pen.dash.is_none() {
            // Nothing to draw and no pattern to keep: no square root. The
            // next joint, not knowing this direction, gets its disc.
            prev = None;
            continue;
        }
        let dx = i64::from(z.x) - i64::from(a.x);
        let dy = i64::from(z.y) - i64::from(a.y);
        let len = isqrt((dx * dx + dy * dy) as u64) as i64;
        if len == 0 {
            continue;
        }
        if round && seen {
            // The disc at `a`: the start cap, or the joint with the
            // segment before where the line turns.
            let turns = match prev {
                None => true,
                Some((px, py, plen)) => {
                    let cross = (px * dy - py * dx).abs();
                    let dot = px * dx + py * dy;
                    dot <= 0 || cross * half > FLAT_TURN * plen * len
                }
            };
            if turns && !add(f, &disc(a, half), target, color, &mut st)? {
                stalled = true;
                break;
            }
            if i + 2 == pts.len() && !add(f, &disc(z, half), target, color, &mut st)? {
                stalled = true;
                break;
            }
        }
        prev = Some((dx, dy, len));
        let extend = !pen.round && pen.dash.is_none();
        let Some((on, off)) = pen.dash else {
            // Solid, and in view (an unseen solid segment left above): the
            // segment's own ends, no division for them.
            if !add(
                f,
                &quad(a, z, dx, dy, len, half, extend),
                target,
                color,
                &mut st,
            )? {
                stalled = true;
                break;
            }
            continue;
        };
        let (on, off) = (i64::from(on), i64::from(off));
        let pieces = Dashes::new(len, phase, on, off);
        phase = (phase + len) % (on.max(1) + off.max(0));
        if !seen {
            continue;
        }
        let at = |s: i64| Point::new(a.x + (dx * s / len) as i32, a.y + (dy * s / len) as i32);
        for (s0, s1) in pieces {
            if !add(
                f,
                &quad(at(s0), at(s1), dx, dy, len, half, extend),
                target,
                color,
                &mut st,
            )? {
                stalled = true;
                break;
            }
        }
        if stalled {
            break;
        }
    }
    if stalled {
        // A scratch smaller than one shape cannot stroke at all: drop
        // the line rather than loop.
        f.clear();
        return Ok(st);
    }
    st += f.fill(target, color, FillRule::NonZero)?;
    Ok(st)
}

/// Add the ring `r`, filling first when the scratch has no room for it;
/// `false` when it does not fit even into an empty scratch.
#[inline]
fn add<D: Coverage>(
    f: &mut Filler<'_, '_>,
    r: &[Point],
    target: &mut D,
    color: D::Color,
    st: &mut FillStats,
) -> Result<bool, D::Error> {
    if f.room() < r.len() {
        *st += f.fill(target, color, FillRule::NonZero)?;
    }
    Ok(f.ring(r).is_ok())
}

/// The quad from `a` to `z` on a segment of direction `(dx, dy)` and
/// length `len`, `half` to either side, extended by `half` past both ends
/// when `extend`.
#[inline]
fn quad(a: Point, z: Point, dx: i64, dy: i64, len: i64, half: i64, extend: bool) -> [Point; 4] {
    // Half-width along the segment (e) and across it (n).
    let ex = (dx * half / len) as i32;
    let ey = (dy * half / len) as i32;
    let (nx, ny) = (-ey, ex);
    let (ex, ey) = if extend { (ex, ey) } else { (0, 0) };
    [
        Point::new(a.x - ex + nx, a.y - ey + ny),
        Point::new(z.x + ex + nx, z.y + ey + ny),
        Point::new(z.x + ex - nx, z.y + ey - ny),
        Point::new(a.x - ex - nx, a.y - ey - ny),
    ]
}

/// The disc of radius `half` around `c`, its corners turning the way the
/// quads' do, so the two add under the nonzero rule: eight corners, or
/// sixteen from [`FINE_DISC`]. The slice is the ring.
fn disc(c: Point, half: i64) -> DiscRing {
    let step = if half >= FINE_DISC { 1 } else { 2 };
    let mut pts = [Point::zero(); 16];
    let mut n = 0;
    // Clockwise on the screen, as the quads go: y grows downwards.
    for &(cos, sin) in DISC.iter().step_by(step) {
        pts[n] = Point::new(
            c.x + ((i64::from(cos) * half) >> 14) as i32,
            c.y - ((i64::from(sin) * half) >> 14) as i32,
        );
        n += 1;
    }
    DiscRing { pts, n }
}

struct DiscRing {
    pts: [Point; 16],
    n: usize,
}

impl core::ops::Deref for DiscRing {
    type Target = [Point];

    fn deref(&self) -> &[Point] {
        &self.pts[..self.n]
    }
}

/// The "on" pieces of a segment `len` long, as `(from, to)` along it.
struct Dashes {
    len: i64,
    /// Where the pattern stands at the next piece's search start.
    at: i64,
    phase: i64,
    on: i64,
    period: i64,
}

impl Dashes {
    fn new(len: i64, phase: i64, on: i64, off: i64) -> Self {
        let on = on.max(1);
        Self {
            len,
            at: 0,
            phase,
            on,
            period: on + off.max(0),
        }
    }
}

impl Iterator for Dashes {
    type Item = (i64, i64);

    fn next(&mut self) -> Option<(i64, i64)> {
        while self.at < self.len {
            // Position in the pattern at `at`.
            let p = (self.phase + self.at) % self.period;
            if p < self.on {
                let from = self.at;
                let to = (from + self.on - p).min(self.len);
                self.at = to;
                return Some((from, to));
            }
            self.at += self.period - p;
        }
        None
    }
}
