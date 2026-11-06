//! One view from a fixture: project and cull, fill the areas, stroke the
//! lines, each phase timed by a caller-supplied clock.
//!
//! The same function runs in the host harness and on the device, so the
//! two measure the same code. The clock is read three times per feature;
//! on the ESP32-S3 that is the `CCOUNT` register, one instruction.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, PrimitiveStyle};

use crate::coverage::Coverage;
use crate::fill::{FillRule, FillStats, Filler, Scratch, SUBPIXEL};
use crate::fixture::{Feature, Fixture, Kind};
use crate::stroke::{stroke_pen, Pen};
use crate::style::{line_width, Class, LineWidths, MapTheme, Profile, Style, CASING, LINE_WIDTHS};

/// A free-running tick counter; differences are taken wrapping.
pub trait Clock {
    fn now(&mut self) -> u32;
}

/// A clock that never moves, for runs that time only the whole.
pub struct NoClock;

impl Clock for NoClock {
    fn now(&mut self) -> u32 {
        0
    }
}

/// Ticks spent per phase.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Phases {
    /// Clearing the frame to the background.
    pub clear: u64,
    /// Projecting vertices to sub-pixels and culling features outside.
    pub project_clip: u64,
    /// Filling areas.
    pub fill: u64,
    /// Stroking lines.
    pub stroke: u64,
}

impl Phases {
    /// All phases together.
    pub fn total(&self) -> u64 {
        self.clear + self.project_clip + self.fill + self.stroke
    }
}

/// What one view drew.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Features the profile includes.
    pub features: u32,
    /// Of those, wholly outside the view.
    pub culled: u32,
    /// Vertices projected.
    pub vertices: u32,
    /// Area fills.
    pub fill: FillStats,
    /// Line strokes.
    pub stroke: FillStats,
    /// Casings, stroked under the lines ([`Look::casing`]).
    pub casing: FillStats,
    /// Building footprints drawn ([`Pass::building`]), their fills
    /// counted in `fill`.
    pub buildings: u32,
    /// Segments of the buildings' 1 px outlines drawn.
    pub outline: u32,
    /// Tiles whose footprints in view were over the map core's cap and
    /// were drawn as blocks, filled without outlines.
    pub blocks: u32,
    /// Features skipped because the work or edge storage was too small.
    pub overflows: u32,
    /// Most vertices one feature held.
    pub max_feature_vertices: u32,
    pub phases: Phases,
}

/// Per-feature storage for projected vertices: the points, and where each
/// ring ends in them.
pub struct Work<'a> {
    pub points: &'a mut [Point],
    pub ring_ends: &'a mut [u16],
}

fn project(v: i32, shift: i8) -> i32 {
    if shift >= 0 {
        v << shift
    } else {
        v >> -shift
    }
}

/// Sub-pixel bounding box, (left, top, right, bottom), all inclusive.
type Bbox = (i32, i32, i32, i32);

/// Where a feature's tile units land in the target: `(offset + v)` is
/// shifted by `shift` (left when positive), then `origin` sub-pixels are
/// added. A fixture drawn on its own is placed at its own shift and the
/// origin zero; the map core places each tile of a view at its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub shift: i8,
    pub origin: Point,
}

/// Project `f` into `w`: the number of rings, the bounding box and the
/// number of vertices, or `None` when it does not fit.
fn project_feature(
    f: &Feature<'_>,
    at: Placement,
    w: &mut Work<'_>,
) -> Option<(usize, Bbox, usize)> {
    let (mut n, mut nr) = (0usize, 0usize);
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for ring in f.rings() {
        if n + ring.len() > w.points.len() || nr >= w.ring_ends.len() {
            return None;
        }
        for (x, y) in ring.points() {
            let p = Point::new(
                project(f.offset.x + i32::from(x), at.shift) + at.origin.x,
                project(f.offset.y + i32::from(y), at.shift) + at.origin.y,
            );
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
            w.points[n] = p;
            n += 1;
        }
        w.ring_ends[nr] = n as u16;
        nr += 1;
    }
    Some((nr, (x0, y0, x1, y1), n))
}

/// How a pass inks what it draws: the line widths per class, whether
/// areas are filled, the colour table, the casings and the joints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// Each line class's width; 0 leaves the class out.
    pub widths: LineWidths,
    /// `Some`: lines only, all in this colour, and no area filled: the
    /// terminal's braille backend, which has one colour a cell and no fill.
    pub mono: Option<Rgb565>,
    /// The colour table.
    pub theme: MapTheme,
    /// How much wider than its line a casing is, both sides together, in
    /// sub-pixels; 0 draws none.
    pub casing: i32,
    /// Round joins and caps rather than square caps.
    pub round: bool,
    /// Paths dashed. Off on the M9: a dashed path is twice the stroke
    /// edges of a solid one (the Florence fixture's 5,919 became 11,147).
    pub dashed: bool,
}

impl Look {
    /// The light table at the fixtures' widths, square caps, no casings:
    /// the M9's look.
    pub const DEFAULT: Self = Self {
        widths: LINE_WIDTHS,
        mono: None,
        theme: MapTheme::Light,
        casing: 0,
        round: false,
        dashed: false,
    };

    /// The look of `style` at `widths`.
    pub fn of(style: Style, widths: LineWidths) -> Self {
        Self {
            widths,
            mono: None,
            theme: style.theme,
            casing: if style.cased { CASING } else { 0 },
            round: style.cased,
            dashed: style.cased,
        }
    }

    /// The same look with every line `percent` per cent as wide: 200 on a
    /// screen of twice the ordinary density, where a picture drawn at the
    /// screen's own pixels would otherwise show every road half as thick
    /// to the eye. 100 is this look unchanged.
    pub fn scaled(self, percent: u32) -> Self {
        let scale = |w: i32| {
            let wide = i64::from(w) * i64::from(percent) / 100;
            i32::try_from(wide).unwrap_or(i32::MAX)
        };
        Self {
            widths: self.widths.map(scale),
            casing: scale(self.casing),
            ..self
        }
    }

    /// The pen of a line of class `c`, `width` wide: paths dashed three
    /// widths on, two off, in colour where the look dashes; everything
    /// else solid.
    fn pen(&self, c: Class, width: i32) -> Pen {
        let dash = (c == Class::Path && self.dashed && self.mono.is_none())
            .then_some((3 * width, 2 * width));
        Pen {
            width,
            round: self.round,
            dash,
        }
    }
}

impl Default for Look {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// One frame being drawn feature by feature: the filler, the view in
/// sub-pixels, the clock's last reading and the counts so far.
///
/// [`render`] is one pass over one fixture. A caller that draws several
/// fixtures into one frame (the map core, one per tile) opens one pass and
/// hands it the features in the order they are to be painted.
pub struct Pass<'s, 'a> {
    filler: Filler<'s, 'a>,
    view: Bbox,
    prev: u32,
    look: Look,
    /// What the pass drew so far.
    pub stats: RenderStats,
}

impl<'s, 'a> Pass<'s, 'a> {
    /// Clear `target` to `look`'s background and open a pass clipped to
    /// it, inked by `look`.
    ///
    /// `scratch.heads` must hold at least the target's height in rows.
    pub fn begin<D, C>(
        target: &mut D,
        scratch: &'s mut Scratch<'a>,
        look: Look,
        clock: &mut C,
    ) -> Result<Self, D::Error>
    where
        D: Coverage<Color = Rgb565>,
        C: Clock,
    {
        let mut stats = RenderStats::default();
        let clip = target.bounding_box();
        let t = clock.now();
        target.clear(look.theme.palette().background)?;
        let prev = clock.now();
        stats.phases.clear += u64::from(prev.wrapping_sub(t));
        let sub = crate::fill::SUBPIXEL;
        let (vl, vt) = (clip.top_left.x * sub, clip.top_left.y * sub);
        let (vr, vb) = (
            vl + clip.size.width as i32 * sub,
            vt + clip.size.height as i32 * sub,
        );
        Ok(Self {
            filler: Filler::new(scratch, clip),
            view: (vl, vt, vr, vb),
            prev,
            look,
            stats,
        })
    }

    /// The look this pass inks by.
    pub fn look(&self) -> Look {
        self.look
    }

    /// The view in sub-pixels, (left, top, right, bottom), right and
    /// bottom exclusive.
    pub fn view(&self) -> (i32, i32, i32, i32) {
        self.view
    }

    /// Project `f` and test it against the view, counting it when
    /// `count`: the rings, or `None` when it is skipped.
    fn prepare<C: Clock>(
        &mut self,
        f: &Feature<'_>,
        at: Placement,
        pad: i32,
        work: &mut Work<'_>,
        clock: &mut C,
        count: bool,
    ) -> Option<usize> {
        let st = &mut self.stats;
        let (vl, vt, vr, vb) = self.view;
        if count {
            st.features += 1;
        }
        let projected = project_feature(f, at, work);
        let visible = projected.map(|(_, (x0, y0, x1, y1), n)| {
            n > 0 && x1 + pad >= vl && x0 - pad < vr && y1 + pad >= vt && y0 - pad < vb
        });
        let now = clock.now();
        st.phases.project_clip += u64::from(now.wrapping_sub(self.prev));
        self.prev = now;
        let Some((nr, _, n)) = projected else {
            if count {
                st.overflows += 1;
            }
            return None;
        };
        if count {
            st.vertices += n as u32;
            st.max_feature_vertices = st.max_feature_vertices.max(n as u32);
        }
        if visible != Some(true) {
            if count {
                st.culled += 1;
            }
            return None;
        }
        Some(nr)
    }

    /// Draw one feature placed at `at`, if `profile` includes its class
    /// and any of it is in view. A line in a tunnel is drawn halfway to
    /// the background.
    pub fn feature<D, C>(
        &mut self,
        f: &Feature<'_>,
        at: Placement,
        profile: Profile,
        target: &mut D,
        work: &mut Work<'_>,
        clock: &mut C,
    ) -> Result<(), D::Error>
    where
        D: Coverage<Color = Rgb565>,
        C: Clock,
    {
        // A class the look leaves out is skipped as one the profile does:
        // a line class at width 0, every area in the lines-only look.
        let width = match f.kind {
            Kind::Area if self.look.mono.is_some() => return Ok(()),
            Kind::Area => 0,
            Kind::Line => line_width(&self.look.widths, f.class),
        };
        if !profile.includes(f.class) || (f.kind == Kind::Line && width == 0) {
            return Ok(());
        }
        let Some(nr) = self.prepare(f, at, width, work, clock, true) else {
            return Ok(());
        };
        let palette = self.look.theme.palette();
        let st = &mut self.stats;
        let filler = &mut self.filler;
        let mut start = 0usize;
        match f.kind {
            Kind::Area => {
                let mut ok = true;
                for &end in &work.ring_ends[..nr] {
                    let end = usize::from(end);
                    if filler.ring(&work.points[start..end]).is_err() {
                        ok = false;
                        break;
                    }
                    start = end;
                }
                if ok {
                    st.fill += filler.fill(target, palette.area(f.class), FillRule::NonZero)?;
                } else {
                    filler.clear();
                    st.overflows += 1;
                }
                let now = clock.now();
                st.phases.fill += u64::from(now.wrapping_sub(self.prev));
                self.prev = now;
            }
            Kind::Line => {
                let color = match (self.look.mono, f.tunnel()) {
                    (Some(c), _) => c,
                    (None, true) => palette.tunnel(f.class),
                    (None, false) => palette.line(f.class),
                };
                let pen = self.look.pen(f.class, width);
                for &end in &work.ring_ends[..nr] {
                    let end = usize::from(end);
                    st.stroke += stroke_pen(filler, &work.points[start..end], pen, target, color)?;
                    start = end;
                }
                let now = clock.now();
                st.phases.stroke += u64::from(now.wrapping_sub(self.prev));
                self.prev = now;
            }
        }
        Ok(())
    }

    /// Draw one building footprint placed at `at` (1192): `rings` its
    /// rings in tile units, the outer one first, `bbox` the box they lie
    /// in, inclusive (x0, y0, x1, y1). Culled by its box before a vertex is
    /// projected; filled in [`Class::Building`]'s colour, then, with
    /// `outline`, each ring drawn over it as a 1 px aliased line in the
    /// table's [`crate::style::Palette::building_edge`]. The fill samples
    /// pixel centres without antialiasing, so an aliased line matches it,
    /// and costs about half a 1 px stroke through the filler (the study,
    /// section 2). Nothing in the lines-only look.
    #[allow(clippy::too_many_arguments)]
    pub fn building<D, C, R, P>(
        &mut self,
        rings: R,
        bbox: (i32, i32, i32, i32),
        at: Placement,
        outline: bool,
        target: &mut D,
        work: &mut Work<'_>,
        clock: &mut C,
    ) -> Result<(), D::Error>
    where
        D: Coverage<Color = Rgb565>,
        C: Clock,
        R: Iterator<Item = P>,
        P: Iterator<Item = (i16, i16)>,
    {
        if self.look.mono.is_some() {
            return Ok(());
        }
        let st = &mut self.stats;
        st.features += 1;
        let place = |x: i32, y: i32| {
            Point::new(
                project(x, at.shift) + at.origin.x,
                project(y, at.shift) + at.origin.y,
            )
        };
        let (lo, hi) = (place(bbox.0, bbox.1), place(bbox.2, bbox.3));
        let (vl, vt, vr, vb) = self.view;
        // A pixel of room for the outline.
        let pad = SUBPIXEL;
        let visible = hi.x + pad >= vl && lo.x - pad < vr && hi.y + pad >= vt && lo.y - pad < vb;
        if !visible {
            st.culled += 1;
            let now = clock.now();
            st.phases.project_clip += u64::from(now.wrapping_sub(self.prev));
            self.prev = now;
            return Ok(());
        }
        let (mut n, mut nr) = (0usize, 0usize);
        let mut fits = true;
        'rings: for ring in rings {
            if nr >= work.ring_ends.len() {
                fits = false;
                break;
            }
            for (x, y) in ring {
                let Some(slot) = work.points.get_mut(n) else {
                    fits = false;
                    break 'rings;
                };
                *slot = place(i32::from(x), i32::from(y));
                n += 1;
            }
            work.ring_ends[nr] = n as u16;
            nr += 1;
        }
        let now = clock.now();
        st.phases.project_clip += u64::from(now.wrapping_sub(self.prev));
        self.prev = now;
        if !fits {
            st.overflows += 1;
            return Ok(());
        }
        st.vertices += n as u32;
        st.max_feature_vertices = st.max_feature_vertices.max(n as u32);
        let palette = self.look.theme.palette();
        let filler = &mut self.filler;
        let mut start = 0usize;
        let mut ok = true;
        for &end in &work.ring_ends[..nr] {
            let end = usize::from(end);
            if filler.ring(&work.points[start..end]).is_err() {
                ok = false;
                break;
            }
            start = end;
        }
        if !ok {
            filler.clear();
            st.overflows += 1;
            return Ok(());
        }
        st.fill += filler.fill(target, palette.area(Class::Building), FillRule::NonZero)?;
        st.buildings += 1;
        if outline {
            let ink = PrimitiveStyle::with_stroke(palette.building_edge, 1);
            let pixel = |p: Point| Point::new(p.x.div_euclid(SUBPIXEL), p.y.div_euclid(SUBPIXEL));
            let mut start = 0usize;
            for &end in &work.ring_ends[..nr] {
                let end = usize::from(end);
                let ring = &work.points[start..end];
                start = end;
                let Some(&last) = ring.last() else {
                    continue;
                };
                let mut from = pixel(last);
                for &p in ring {
                    let to = pixel(p);
                    if to != from {
                        Line::new(from, to).into_styled(ink).draw(target)?;
                        st.outline += 1;
                        from = to;
                    }
                }
            }
        }
        let now = clock.now();
        st.phases.fill += u64::from(now.wrapping_sub(self.prev));
        self.prev = now;
        Ok(())
    }

    /// Draw the casing of the line `f` placed at `at`, where the look has
    /// casings and the line gets one: a major or secondary road, or any
    /// line on a bridge. A caller strokes every casing of a layer before
    /// the layer's lines, so the casings join under the fills.
    pub fn casing<D, C>(
        &mut self,
        f: &Feature<'_>,
        at: Placement,
        profile: Profile,
        target: &mut D,
        work: &mut Work<'_>,
        clock: &mut C,
    ) -> Result<(), D::Error>
    where
        D: Coverage<Color = Rgb565>,
        C: Clock,
    {
        let line = line_width(&self.look.widths, f.class);
        if f.kind != Kind::Line
            || self.look.mono.is_some()
            || self.look.casing == 0
            || line == 0
            || !(f.class.is_cased() || f.bridge())
            || !profile.includes(f.class)
        {
            return Ok(());
        }
        let width = line + self.look.casing;
        let Some(nr) = self.prepare(f, at, width, work, clock, false) else {
            return Ok(());
        };
        let color = self.look.theme.palette().casing_of(f.class);
        let pen = Pen {
            width,
            round: self.look.round,
            dash: None,
        };
        let mut start = 0usize;
        for &end in &work.ring_ends[..nr] {
            let end = usize::from(end);
            self.stats.casing += stroke_pen(
                &mut self.filler,
                &work.points[start..end],
                pen,
                target,
                color,
            )?;
            start = end;
        }
        let now = clock.now();
        self.stats.phases.stroke += u64::from(now.wrapping_sub(self.prev));
        self.prev = now;
        Ok(())
    }
}

/// Draw `fx` under `profile` into `target`, which is cleared first.
///
/// `scratch.heads` must hold at least the view's height in rows. The
/// clip is the target's bounding box.
pub fn render<D, C>(
    fx: &Fixture<'_>,
    profile: Profile,
    target: &mut D,
    scratch: &mut Scratch<'_>,
    work: &mut Work<'_>,
    clock: &mut C,
) -> Result<RenderStats, D::Error>
where
    D: Coverage<Color = Rgb565>,
    C: Clock,
{
    let mut pass = Pass::begin(target, scratch, Look::DEFAULT, clock)?;
    let at = Placement {
        shift: fx.shift(),
        origin: Point::zero(),
    };
    for f in fx.features() {
        pass.feature(&f, at, profile, target, work, clock)?;
    }
    Ok(pass.stats)
}
