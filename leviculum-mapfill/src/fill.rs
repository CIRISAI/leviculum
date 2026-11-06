//! Scanline polygon fill with integer edges and per-row buckets.
//!
//! The shape is the classic edge-table filler. Every ring edge is turned
//! into an [`Edge`] once: its first pixel row, its last row, its x where it
//! crosses that first row's centre line, and its x step per row, all in
//! 16.16 fixed point. Edges are bucketed by first row, so a row's work is
//! to merge its bucket into the active list, drop the edges that ended,
//! insertion-sort the list by x (it is nearly sorted from the row above)
//! and emit the spans between crossings under the fill rule. No floats, no
//! allocation: the caller lends the edge storage in a [`Scratch`].
//!
//! Sampling rule: pixel `(x, y)` is inside when its centre `(x + 0.5,
//! y + 0.5)` is, and a centre exactly on a left edge is in, on a right edge
//! out. Two polygons that share an edge therefore neither overlap nor leave
//! a gap between them, which is what removes the tile seams outline drawing
//! showed (`docs/drafts/map-measurement-2026-10.md`).
//!
//! The reference read before writing this is the 112-line `fill.rs` of
//! dfyz/osm-renderer (MIT, `src/draw/fill.rs` at 02bd330a). It walks every
//! edge with Bresenham into a map of rows and fills even-odd between pairs
//! of per-row x extents. This one differs where the board needs it to: it
//! samples pixel centres instead of marking every pixel an edge touches, so
//! fills are exact rather than conservative; it carries the winding
//! direction, so nonzero works as well as even-odd; and it keeps its edges
//! in caller-provided slices rather than a hash map.

use embedded_graphics::prelude::*;

use crate::coverage::Coverage;
use embedded_graphics::primitives::Rectangle;

/// Sub-pixel steps per pixel in the coordinates the filler takes.
///
/// One unit is 1/16 pixel. That is exactly one tile unit when a tile of
/// extent 4096 is drawn at 256 pixels, the Protomaps z15 case.
pub const SUBPIXEL: i32 = 16;
const SUB_SHIFT: u32 = 4;
const HALF_SUB: i32 = SUBPIXEL / 2;
/// Largest coordinate magnitude [`Filler::edge`] takes: 2^22 sub-pixels,
/// 262,144 pixels.
pub const COORD_LIMIT: i32 = 1 << 22;
/// One pixel in 16.16 fixed point.
const ONE: i64 = 1 << 16;
/// End-of-list marker in the bucket and edge links.
const NIL: u16 = u16::MAX;

/// Which points a set of rings encloses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    /// Inside where a ray crosses an odd number of edges.
    EvenOdd,
    /// Inside where the signed crossing count is not zero.
    NonZero,
}

/// One polygon edge, prepared for the row walk.
#[derive(Clone, Copy, Debug, Default)]
pub struct Edge {
    /// x at the current row's centre line, pixels in 16.16.
    x: i32,
    /// x change per row, pixels in 16.16.
    step: i32,
    /// First row past the edge, in clip rows.
    end: u16,
    /// Next edge starting on the same row, or [`NIL`].
    next: u16,
    /// +1 for an edge running down, -1 for up.
    wind: i8,
}

impl Edge {
    /// An unused slot, for statics: `[Edge::EMPTY; N]`. All zero, so a
    /// static array of them lands in `.bss`, not in `.data` and flash.
    pub const EMPTY: Self = Self {
        x: 0,
        step: 0,
        end: 0,
        next: 0,
        wind: 0,
    };
}

/// The storage a [`Filler`] works in: one slot per edge, one bucket head
/// per row of the clip rectangle, one active-list slot per edge.
pub struct Scratch<'a> {
    pub edges: &'a mut [Edge],
    pub heads: &'a mut [u16],
    pub active: &'a mut [u16],
}

/// More edges than the scratch holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overflow;

/// Why a fill did not complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillError<E> {
    /// The rings had more edges than the scratch holds; nothing was drawn.
    Overflow,
    /// The draw target refused a span.
    Target(E),
}

/// What one fill did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FillStats {
    /// Edges kept after the clip, the ones the row walk visited.
    pub edges: u32,
    /// Horizontal spans handed to the target.
    pub spans: u32,
    /// Pixels in those spans.
    pub pixels: u32,
}

impl core::ops::AddAssign for FillStats {
    fn add_assign(&mut self, o: Self) {
        self.edges += o.edges;
        self.spans += o.spans;
        self.pixels += o.pixels;
    }
}

/// Collects ring edges, then fills them into a draw target.
pub struct Filler<'s, 'a> {
    s: &'s mut Scratch<'a>,
    left: i32,
    right: i32,
    top: i32,
    rows: i32,
    n: usize,
    /// Lowest and one past the highest bucket row used, in clip rows.
    lo: i32,
    hi: i32,
}

/// `ceil(a / b)` for `b > 0`.
fn ceil_div(a: i64, b: i64) -> i64 {
    -((-a).div_euclid(b))
}

fn sat(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

impl<'s, 'a> Filler<'s, 'a> {
    /// A filler clipped to `clip`, in pixels. Rows outside it are never
    /// visited and spans are cut at its left and right sides.
    ///
    /// `clip` must be no taller than `scratch.heads` is long, and the
    /// active list must be as long as the edge list.
    pub fn new(scratch: &'s mut Scratch<'a>, clip: Rectangle) -> Self {
        let rows = (clip.size.height as usize).min(scratch.heads.len()) as i32;
        debug_assert!(
            scratch.active.len() >= scratch.edges.len(),
            "active list shorter than edge list"
        );
        scratch.heads[..rows as usize].fill(NIL);
        Self {
            s: scratch,
            left: clip.top_left.x,
            right: clip.top_left.x + clip.size.width as i32,
            top: clip.top_left.y,
            rows,
            n: 0,
            lo: rows,
            hi: 0,
        }
    }

    fn capacity(&self) -> usize {
        self.s
            .edges
            .len()
            .min(self.s.active.len())
            .min(NIL as usize)
    }

    /// Edge slots still free.
    pub fn room(&self) -> usize {
        self.capacity() - self.n
    }

    /// The clip as (left, top, right, bottom) in sub-pixels, right and
    /// bottom exclusive.
    pub fn clip_sub(&self) -> (i32, i32, i32, i32) {
        (
            self.left * SUBPIXEL,
            self.top * SUBPIXEL,
            self.right * SUBPIXEL,
            (self.top + self.rows) * SUBPIXEL,
        )
    }

    /// Forget the edges added since the last fill, for instance after an
    /// [`Overflow`] left a ring half added.
    pub fn clear(&mut self) {
        let (lo, hi) = (self.lo.max(0) as usize, self.hi.max(0) as usize);
        if lo < hi {
            self.s.heads[lo..hi].fill(NIL);
        }
        self.n = 0;
        self.lo = self.rows;
        self.hi = 0;
    }

    /// Whether any edge is waiting to be filled.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Add the edge from `a` to `b`, both in sub-pixels ([`SUBPIXEL`]).
    /// Coordinates must lie within [`COORD_LIMIT`] of the origin, which
    /// keeps the 64-bit setup arithmetic exact.
    ///
    /// Horizontal edges and edges that cross no row centre inside the clip
    /// are dropped, and so are edges wholly right of the clip, since the
    /// winding at any pixel depends only on the edges to its left.
    pub fn edge(&mut self, a: Point, b: Point) -> Result<(), Overflow> {
        debug_assert!(
            [a.x, a.y, b.x, b.y].iter().all(|v| v.abs() <= COORD_LIMIT),
            "edge coordinate beyond COORD_LIMIT"
        );
        if a.y == b.y {
            return Ok(());
        }
        let (t, bot, wind) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
        if t.x.min(bot.x) >= self.right * SUBPIXEL {
            return Ok(());
        }
        // Rows whose centre line, at sub-pixel y = row * 16 + 8, lies in
        // [t.y, bot.y): the half-open interval gives a shared vertex to
        // exactly one of the two edges meeting there.
        let first = ceil_div(i64::from(t.y) - i64::from(HALF_SUB), i64::from(SUBPIXEL));
        let last = ceil_div(i64::from(bot.y) - i64::from(HALF_SUB), i64::from(SUBPIXEL));
        let top = i64::from(self.top);
        let r0 = first.max(top);
        let r1 = last.min(top + i64::from(self.rows));
        if r0 >= r1 {
            return Ok(());
        }
        if self.n >= self.capacity() {
            return Err(Overflow);
        }
        let dy = i64::from(bot.y) - i64::from(t.y);
        let dx = i64::from(bot.x) - i64::from(t.x);
        let sample = r0 * i64::from(SUBPIXEL) + i64::from(HALF_SUB);
        // x at the first sampled centre line, in 16.16 pixels:
        // (t.x + (sample - t.y) * dx / dy) / 16, scaled by 2^16.
        let num = (i64::from(t.x) * dy + (sample - i64::from(t.y)) * dx) << (16 - SUB_SHIFT);
        let x = num.div_euclid(dy);
        let step = (dx << 16).div_euclid(dy);
        let row = (r0 - top) as usize;
        let idx = self.n;
        self.s.edges[idx] = Edge {
            x: sat(x),
            step: sat(step),
            end: (r1 - top) as u16,
            next: self.s.heads[row],
            wind,
        };
        self.s.heads[row] = idx as u16;
        self.n += 1;
        self.lo = self.lo.min(row as i32);
        self.hi = self.hi.max((r1 - top) as i32);
        Ok(())
    }

    /// Add a closed ring. The closing edge from the last point back to the
    /// first is implied; a repeated first point at the end is harmless.
    pub fn ring(&mut self, pts: &[Point]) -> Result<(), Overflow> {
        if pts.len() < 2 {
            return Ok(());
        }
        let mut prev = pts[pts.len() - 1];
        for &p in pts {
            self.edge(prev, p)?;
            prev = p;
        }
        Ok(())
    }

    /// Fill everything added since the last fill, then start empty again.
    pub fn fill<D>(
        &mut self,
        target: &mut D,
        color: D::Color,
        rule: FillRule,
    ) -> Result<FillStats, D::Error>
    where
        D: Coverage,
    {
        let smooth = target.smooth();
        let mut st = FillStats {
            edges: self.n as u32,
            ..FillStats::default()
        };
        let mut na = 0usize;
        for row in self.lo..self.hi {
            let r = row as usize;
            // Merge this row's bucket.
            let mut e = self.s.heads[r];
            while e != NIL {
                self.s.active[na] = e;
                na += 1;
                e = self.s.edges[e as usize].next;
            }
            self.s.heads[r] = NIL;
            // Drop the edges that ended above this row.
            let mut k = 0;
            for i in 0..na {
                let id = self.s.active[i];
                if i32::from(self.s.edges[id as usize].end) > row {
                    self.s.active[k] = id;
                    k += 1;
                }
            }
            na = k;
            // Insertion sort by x: the order changes only where edges cross.
            for i in 1..na {
                let id = self.s.active[i];
                let x = self.s.edges[id as usize].x;
                let mut j = i;
                while j > 0 && self.s.edges[self.s.active[j - 1] as usize].x > x {
                    self.s.active[j] = self.s.active[j - 1];
                    j -= 1;
                }
                self.s.active[j] = id;
            }
            let y = self.top + row;
            let mut wind = 0i32;
            let mut start = 0i32;
            for i in 0..na {
                let ed = self.s.edges[self.s.active[i] as usize];
                let before = wind;
                wind = match rule {
                    FillRule::EvenOdd => wind ^ 1,
                    FillRule::NonZero => wind + i32::from(ed.wind),
                };
                if before == 0 && wind != 0 {
                    start = ed.x;
                } else if before != 0 && wind == 0 {
                    self.span(target, color, y, start, ed.x, smooth, &mut st)?;
                }
            }
            // Still inside after the last edge: the closing edges lay wholly
            // right of the clip and were never stored.
            if wind != 0 {
                self.span(target, color, y, start, i32::MAX, smooth, &mut st)?;
            }
            for i in 0..na {
                let ed = &mut self.s.edges[self.s.active[i] as usize];
                ed.x = ed.x.saturating_add(ed.step);
            }
        }
        self.n = 0;
        self.lo = self.rows;
        self.hi = 0;
        Ok(st)
    }

    #[allow(clippy::too_many_arguments)]
    fn span<D: Coverage>(
        &self,
        target: &mut D,
        color: D::Color,
        y: i32,
        xa: i32,
        xb: i32,
        smooth: bool,
        st: &mut FillStats,
    ) -> Result<(), D::Error> {
        if smooth {
            return self.span_smooth(target, color, y, xa, xb, st);
        }
        // Pixel i is in when its centre i + 0.5 lies in [xa, xb):
        // i from ceil(xa - 0.5) to ceil(xb - 0.5), exclusive.
        let px = |v: i32| ((i64::from(v) - ONE / 2 + ONE - 1) >> 16) as i32;
        let x0 = px(xa).max(self.left);
        let x1 = px(xb).min(self.right);
        if x1 > x0 {
            st.spans += 1;
            st.pixels += (x1 - x0) as u32;
            target.fill_solid(
                &Rectangle::new(Point::new(x0, y), Size::new((x1 - x0) as u32, 1)),
                color,
            )?;
        }
        Ok(())
    }
}

impl Filler<'_, '_> {
    /// The span `[xa, xb)` of row `y`, 16.16 pixels, with its two end
    /// pixels blended by the share of them it covers ([`Coverage`]): pixel
    /// `i` holds `[i, i + 1)`, and the pixels between the ends are set
    /// hard. A span within one pixel blends that pixel by its width.
    fn span_smooth<D: Coverage>(
        &self,
        target: &mut D,
        color: D::Color,
        y: i32,
        xa: i32,
        xb: i32,
        st: &mut FillStats,
    ) -> Result<(), D::Error> {
        let (lo, hi) = (i64::from(self.left) * ONE, i64::from(self.right) * ONE);
        let (a, b) = (i64::from(xa).max(lo), i64::from(xb).min(hi));
        if b <= a {
            return Ok(());
        }
        let (fa, fb) = (a >> 16, b >> 16);
        if fa == fb {
            st.spans += 1;
            return covered(target, color, fa, y, b - a);
        }
        st.spans += 1;
        covered(target, color, fa, y, ((fa + 1) << 16) - a)?;
        if fb > fa + 1 {
            st.pixels += (fb - fa - 1) as u32;
            target.fill_solid(
                &Rectangle::new(
                    Point::new((fa + 1) as i32, y),
                    Size::new((fb - fa - 1) as u32, 1),
                ),
                color,
            )?;
        }
        // The pixel the span ends in, unless it ends on its left edge.
        if b > fb << 16 {
            covered(target, color, fb, y, b - (fb << 16))?;
        }
        Ok(())
    }
}

/// Pixel `(x, y)` covered by `cover` of it, 16.16: blended, set when
/// whole, left when nothing.
fn covered<D: Coverage>(
    target: &mut D,
    color: D::Color,
    x: i64,
    y: i32,
    cover: i64,
) -> Result<(), D::Error> {
    match (cover * 256) >> 16 {
        c if c >= 255 => target.fill_solid(
            &Rectangle::new(Point::new(x as i32, y), Size::new(1, 1)),
            color,
        ),
        c if c >= 1 => target.blend(x as i32, y, color, c as u8),
        _ => Ok(()),
    }
}

/// Fill `rings` in one go: a convenience over [`Filler`] for callers that
/// have every ring at hand. Holes are rings of their own.
pub fn fill_rings<D: Coverage>(
    scratch: &mut Scratch<'_>,
    clip: Rectangle,
    rings: &[&[Point]],
    target: &mut D,
    color: D::Color,
    rule: FillRule,
) -> Result<FillStats, FillError<D::Error>> {
    let mut f = Filler::new(scratch, clip);
    for r in rings {
        f.ring(r).map_err(|_| FillError::Overflow)?;
    }
    f.fill(target, color, rule).map_err(FillError::Target)
}
