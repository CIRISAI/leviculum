//! The world outline: what a view shows where no region covers it
//! (`docs/src/concepts/loquax-map.md`, section 7, "No default region").
//!
//! Natural Earth's 1:50m land polygons, version 5.1.2, public domain,
//! baked by `scripts/bake-world-outline.py` into a table of 159,357 bytes
//! (1,422 rings, 59,239 points) that is part of the binary. The format is
//! the script's: `LWO2`, the bits a coordinate has, a ring count, then per
//! ring its point count with a hole flag, the length of its deltas, its
//! first point, its box as distances from that point, and the deltas to
//! each further point, packed at a width the ring chose with an escape for
//! a long step. Counts and distances are LEB128 varints. A point is the
//! world coordinate shifted right by `32 - bits`; at 18 bits a step is one
//! pixel at [`MAX_ZOOM`], so a coast there is never more than half a pixel
//! off.
//!
//! The country borders are a second table in the same format, `LWL2`:
//! Natural Earth's 1:50m land boundary lines of the same version, 393 lines
//! and 19,760 points in 49,512 bytes ([`BORDERS_50M`]). A line is a ring
//! that is not closed and has no holes. Maritime boundaries are left out:
//! over water the coasts already part the countries.
//!
//! [`Ink::Colour`] fills the land in the earth colour over water;
//! [`Ink::Lines`] draws the coasts alone, one pixel wide. A ring is clipped
//! to a box a few pixels outside the view before it reaches the filler, so
//! every coordinate stays inside the filler's limit at any zoom, and the
//! edges the clip adds lie outside the view, where a stroked coast does not
//! show them. The borders go over either, one pixel wide, in the table's
//! border colour under [`Ink::Colour`] and in the ink under [`Ink::Lines`], clipped
//! segment by segment; only down to [`MAX_ZOOM`], below which 1:50m lines
//! would be off by more than a pixel beside a region's own data.
//!
//! Around the land and the borders the world view draws the rest of the
//! far picture (1683, `expedition-map-concept-2026-11.md` section 2), the
//! same version and source, each a table of the same two formats:
//! [`LAKES_50M`] (44,502 bytes) fills water over the land and strokes the
//! shore under [`Ink::Lines`], [`RIVERS_50M`] (36,236) draws the rivers
//! and lake centre lines whose `scalerank` is 4 or less, [`ICE_50M`]
//! (16,422) the glaciated areas of `scalerank` 1 or less and
//! [`SHELVES_50M`] (14,344) Antarctica's ice shelves, both in
//! [`ice`]'s colour, and [`DISPUTED_50M`] (1,828) the disputed boundary
//! lines, stroked dashed. [`draw_world`] draws them all in that order;
//! [`draw`] is the land and the borders alone.
//!
//! The names the world view sets come from [`NAMES_50M`] (59,859 bytes,
//! `LWN1`): populated places, country names, the seas, the mountain
//! ranges, deserts and peaks, each a point with the kind, rank and
//! shallowest zoom [`crate::labels::LabelKind`] gives it, and the German
//! name where it differs (the locale the table is baked for, as the
//! region fixtures are cut for German). [`names`] reads the ones a view
//! shows; the frontends place them by the core's one rule. The M9 carries
//! [`NAMES_M9`] (12,146 bytes), the same names down to its z0 to z3
//! overview without the places it draws from that overview.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use leviculum_mapfill::coverage::Coverage;
use leviculum_mapfill::fill::{Edge, FillRule, FillStats, Filler, Scratch};
use leviculum_mapfill::stroke::stroke;
use leviculum_mapfill::style::{Class, MapTheme};
use leviculum_mapfill::SUBPIXEL;

use crate::draw::Ink;
use crate::labels::{LabelKind, ViewLabel};
use crate::viewport::Viewport;

/// The table, Natural Earth 1:50m land.
pub const WORLD_50M: &[u8] = include_bytes!("../data/ne_50m_land.lwo");

/// The borders, Natural Earth 1:50m land boundary lines.
pub const BORDERS_50M: &[u8] = include_bytes!("../data/ne_50m_admin_0_boundary_lines_land.lwl");

/// The lakes, Natural Earth 1:50m.
pub const LAKES_50M: &[u8] = include_bytes!("../data/ne_50m_lakes.lwo");

/// The rivers and lake centre lines, `scalerank` 4 or less.
pub const RIVERS_50M: &[u8] = include_bytes!("../data/ne_50m_rivers_lake_centerlines.lwl");

/// The glaciated areas, `scalerank` 1 or less.
pub const ICE_50M: &[u8] = include_bytes!("../data/ne_50m_glaciated_areas.lwo");

/// Antarctica's ice shelves.
pub const SHELVES_50M: &[u8] = include_bytes!("../data/ne_50m_antarctic_ice_shelves_polys.lwo");

/// The disputed boundary lines, drawn dashed.
pub const DISPUTED_50M: &[u8] =
    include_bytes!("../data/ne_50m_admin_0_boundary_lines_disputed_areas.lwl");

/// The names of the world view.
pub const NAMES_50M: &[u8] = include_bytes!("../data/ne_50m_names.lwn");

/// The names the M9 carries over its world overview.
pub const NAMES_M9: &[u8] = include_bytes!("../data/ne_50m_names_m9.lwn");

/// The attribution line a view of the outline carries.
pub const ATTRIBUTION: &str = "Natural Earth";

/// The deepest zoom the outline is worth drawing at without a region: at
/// z10 a step of the table is one pixel, and 1:50m data holds no detail
/// finer than that.
pub const MAX_ZOOM: u8 = 10;

/// How far outside the view, in sub-pixels, a ring is clipped: more than
/// the coast's width, so the clip's own edges never show.
const GUARD: i64 = 4 * SUBPIXEL as i64;

/// One ring of a ring table, or one line of a line table.
#[derive(Clone, Copy, Debug)]
pub struct Ring<'a> {
    /// A hole in the ring before it.
    pub hole: bool,
    /// West, north, east, south of its points.
    pub bbox: [u32; 4],
    len: usize,
    first: (u32, u32),
    /// Bits a coordinate has, and bits a packed delta has.
    bits: u32,
    width: u32,
    deltas: &'a [u8],
}

/// Reads bits least significant first; past the end it reads zeros.
struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn take(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..n {
            let byte = self.bytes.get(self.at / 8).copied().unwrap_or(0);
            v |= u32::from((byte >> (self.at % 8)) & 1) << i;
            self.at += 1;
        }
        v
    }

    /// One delta: `width` bits, or the escape and `bits + 1` more.
    fn delta(&mut self, width: u32, bits: u32) -> i64 {
        let escape = (1u32 << width) - 1;
        let z = match self.take(width) {
            v if v == escape => self.take(bits + 1),
            v => v,
        };
        i64::from(z >> 1) ^ -i64::from(z & 1)
    }
}

impl Ring<'_> {
    /// Its points, as table units.
    pub fn points(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let mut bits = Bits {
            bytes: self.deltas,
            at: 0,
        };
        let (mut x, mut y) = (i64::from(self.first.0), i64::from(self.first.1));
        let (width, coord) = (self.width, self.bits);
        (0..self.len).map(move |i| {
            if i > 0 {
                x += bits.delta(width, coord);
                y += bits.delta(width, coord);
            }
            (x.max(0) as u32, y.max(0) as u32)
        })
    }

    /// How many points it has.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// An unsigned LEB128 varint at `at`, which moves past it; `None` past the
/// end or past 32 bits.
fn varint(table: &[u8], at: &mut usize) -> Option<u32> {
    let mut v = 0u32;
    for shift in (0..35).step_by(7) {
        let byte = *table.get(*at)?;
        *at += 1;
        v |= u32::from(byte & 0x7F).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// The magic of a ring table and of a line table.
const RINGS: &[u8; 4] = b"LWO2";
const LINES: &[u8; 4] = b"LWL2";

/// The bits a coordinate of `table` has, `None` for no table of either
/// kind.
pub fn bits(table: &[u8]) -> Option<u32> {
    match table.get(..5) {
        Some(head)
            if (head[..4] == *RINGS || head[..4] == *LINES) && (8..=24).contains(&head[4]) =>
        {
            Some(u32::from(head[4]))
        }
        _ => None,
    }
}

/// The rings of a ring table, none of a line table; a table cut short ends
/// early, it never panics.
pub fn rings(table: &[u8]) -> impl Iterator<Item = Ring<'_>> {
    entries(table, RINGS)
}

/// The lines of a line table, none of a ring table, as [`Ring`]s that are
/// not closed; a table cut short ends early, it never panics.
pub fn lines(table: &[u8]) -> impl Iterator<Item = Ring<'_>> {
    entries(table, LINES)
}

fn entries<'a>(table: &'a [u8], magic: &[u8; 4]) -> impl Iterator<Item = Ring<'a>> {
    let bits = bits(table).unwrap_or(16);
    let mut at = 5usize;
    let count = match self::bits(table) {
        Some(_) if table[..4] == *magic => varint(table, &mut at).unwrap_or(0),
        _ => 0,
    };
    (0..count).map_while(move |_| {
        let mut v = || varint(table, &mut at);
        let (head, size, x, y) = (v()?, v()?, v()?, v()?);
        let (west, north, east, south) = (v()?, v()?, v()?, v()?);
        let width = u32::from(*table.get(at)?);
        if !(1..=bits + 1).contains(&width) {
            return None;
        }
        let deltas = table.get(at + 1..at + 1 + size as usize)?;
        at += 1 + size as usize;
        Some(Ring {
            hole: head & 1 != 0,
            bbox: [
                x.checked_sub(west)?,
                y.checked_sub(north)?,
                x.checked_add(east)?,
                y.checked_add(south)?,
            ],
            len: (head >> 1) as usize,
            first: (x, y),
            bits,
            width,
            deltas,
        })
    })
}

/// The outline's water colour in `theme`, the region's water.
pub fn water(theme: MapTheme) -> Rgb565 {
    theme.palette().area(Class::Water)
}

/// The ice colour in `theme`: the snow and ice no region renderer draws,
/// so the palette has none; a pale blue-white on the light table, a step
/// paler than the land on the dark one.
pub fn ice(theme: MapTheme) -> Rgb565 {
    match theme {
        MapTheme::Light => Rgb565::new(29, 60, 30),
        MapTheme::Dark => Rgb565::new(12, 26, 16),
    }
}

/// What one layer of the world added to a view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayerStats {
    /// Ring copies (or lines) at least partly in view.
    pub rings: u32,
    /// Ring copies wholly outside it.
    pub culled: u32,
    /// Points handed to the filler after the clip.
    pub vertices: u32,
    /// Areas filled ([`Ink::Colour`]).
    pub fill: FillStats,
    /// Lines stroked ([`Ink::Lines`], and rivers under both inks).
    pub stroke: FillStats,
    /// Rings dropped because the edge storage was too small.
    pub overflows: u32,
}

/// What one outline view drew.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutlineStats {
    /// The land: ring copies at least partly in view.
    pub rings: u32,
    /// Ring copies wholly outside it.
    pub culled: u32,
    /// Points handed to the filler after the clip.
    pub vertices: u32,
    /// Land filled ([`Ink::Colour`]).
    pub fill: FillStats,
    /// Coasts stroked ([`Ink::Lines`]).
    pub stroke: FillStats,
    /// Border line copies at least partly in view.
    pub borders: u32,
    /// Borders stroked.
    pub border: FillStats,
    /// Rings dropped because the edge storage was too small.
    pub overflows: u32,
    /// The lakes: ring copies in view, and what they filled or stroked.
    pub lakes: u32,
    pub lake: FillStats,
    /// The rivers: lines in view, and their strokes.
    pub rivers: u32,
    pub river: FillStats,
    /// The glaciated areas and the ice shelves: ring copies in view, and
    /// what they filled or stroked.
    pub ice: u32,
    pub snowy: FillStats,
    /// The disputed boundary lines in view, and their dashed strokes.
    pub disputed: u32,
    pub dash: FillStats,
}

/// Clip `ring` to `[x0, x1] x [y0, y1]`, Sutherland-Hodgman, edge by edge.
fn clip(ring: &[(i64, i64)], x0: i64, y0: i64, x1: i64, y1: i64, out: &mut Vec<(i64, i64)>) {
    let mut cur: Vec<(i64, i64)> = ring.to_vec();
    // Each side: inside test, and the crossing of a-b with the side.
    type Side = (fn(i64, i64, i64) -> bool, bool, i64);
    let sides: [Side; 4] = [
        (|v, _, lim| v >= lim, true, x0),
        (|v, _, lim| v <= lim, true, x1),
        (|_, v, lim| v >= lim, false, y0),
        (|_, v, lim| v <= lim, false, y1),
    ];
    for (inside, vertical, lim) in sides {
        let mut next = Vec::with_capacity(cur.len() + 4);
        let Some(&last) = cur.last() else {
            break;
        };
        let mut prev = last;
        for &p in &cur {
            let (pin, qin) = (inside(p.0, p.1, lim), inside(prev.0, prev.1, lim));
            if pin != qin {
                // The crossing, rounded toward the inside does not matter:
                // the clip box is outside the view.
                let cross = match vertical {
                    true => {
                        let t_num = lim - prev.0;
                        let den = p.0 - prev.0;
                        (lim, prev.1 + (p.1 - prev.1) * t_num / den)
                    }
                    false => {
                        let t_num = lim - prev.1;
                        let den = p.1 - prev.1;
                        (prev.0 + (p.0 - prev.0) * t_num / den, lim)
                    }
                };
                next.push(cross);
            }
            if pin {
                next.push(p);
            }
            prev = p;
        }
        cur = next;
    }
    out.clear();
    out.extend(cur);
}

/// Clip the segment `a`-`b` to `[x0, x1] x [y0, y1]`, side by side; `None`
/// when it lies wholly outside.
fn clip_segment(
    mut a: (i64, i64),
    mut b: (i64, i64),
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
) -> Option<((i64, i64), (i64, i64))> {
    // Each side: the coordinate it bounds, its limit, and whether inside
    // is above the limit.
    for (vertical, lim, above) in [
        (true, x0, true),
        (true, x1, false),
        (false, y0, true),
        (false, y1, false),
    ] {
        let inside = |p: (i64, i64)| {
            let v = if vertical { p.0 } else { p.1 };
            if above {
                v >= lim
            } else {
                v <= lim
            }
        };
        let (ain, bin) = (inside(a), inside(b));
        if !ain && !bin {
            return None;
        }
        if ain != bin {
            let cross = match vertical {
                true => (lim, a.1 + (b.1 - a.1) * (lim - a.0) / (b.0 - a.0)),
                false => (a.0 + (b.0 - a.0) * (lim - a.1) / (b.1 - a.1), lim),
            };
            match ain {
                true => b = cross,
                false => a = cross,
            }
        }
    }
    Some((a, b))
}

/// Every table the world view draws, empty for one it does not (1683).
#[derive(Clone, Copy, Debug)]
pub struct World<'a> {
    /// The land, [`WORLD_50M`].
    pub land: &'a [u8],
    /// The lakes, [`LAKES_50M`].
    pub lakes: &'a [u8],
    /// The rivers and lake centre lines, [`RIVERS_50M`].
    pub rivers: &'a [u8],
    /// The glaciated areas, [`ICE_50M`].
    pub ice: &'a [u8],
    /// The Antarctic ice shelves, [`SHELVES_50M`].
    pub shelves: &'a [u8],
    /// The country borders, [`BORDERS_50M`].
    pub borders: &'a [u8],
    /// The disputed boundary lines, [`DISPUTED_50M`].
    pub disputed: &'a [u8],
}

/// The whole world view, the tables of the crate.
pub const WORLD: World<'static> = World {
    land: WORLD_50M,
    lakes: LAKES_50M,
    rivers: RIVERS_50M,
    ice: ICE_50M,
    shelves: SHELVES_50M,
    borders: BORDERS_50M,
    disputed: DISPUTED_50M,
};

/// The boundary lines alone, for a picture drawn from another source
/// (1683): the M9's overview region carries no boundary class, so its
/// borders and disputed lines come from this.
pub const BOUNDARIES: World<'static> = World {
    land: &[],
    lakes: &[],
    rivers: &[],
    ice: &[],
    shelves: &[],
    borders: BORDERS_50M,
    disputed: DISPUTED_50M,
};

/// Draw the outline of `table` (normally [`WORLD_50M`]) and the borders of
/// `borders` (normally [`BORDERS_50M`], or empty for none) for `view` into
/// `target` in `theme`'s colours, which is cleared first: to water under
/// [`Ink::Colour`], to the background under [`Ink::Lines`].
pub fn draw<D>(
    table: &[u8],
    borders: &[u8],
    view: &Viewport,
    ink: Ink,
    theme: MapTheme,
    target: &mut D,
) -> Result<OutlineStats, D::Error>
where
    D: Coverage<Color = Rgb565>,
{
    let world = World {
        land: table,
        lakes: &[],
        rivers: &[],
        ice: &[],
        shelves: &[],
        borders,
        disputed: &[],
    };
    draw_world(&world, view, ink, theme, target)
}

/// Draw the world view of `world` for `view` into `target` in `theme`'s
/// colours, which is cleared first: to water under [`Ink::Colour`], to the
/// background under [`Ink::Lines`]. The layers go in the order the map
/// reads them: the land, the lakes over it, the rivers, the ice and the
/// shelves, then the borders and the disputed lines dashed over all.
/// Nothing is drawn past [`MAX_ZOOM`] but the land, which the drawer itself
/// does not limit.
pub fn draw_world<D>(
    world: &World,
    view: &Viewport,
    ink: Ink,
    theme: MapTheme,
    target: &mut D,
) -> Result<OutlineStats, D::Error>
where
    D: Coverage<Color = Rgb565>,
{
    let mut st = OutlineStats::default();
    let palette = theme.palette();
    let land = palette.background;
    target.clear(match ink {
        Ink::Colour => water(theme),
        Ink::Lines(_) => land,
    })?;
    // The colour a layer fills in, or the ink it is stroked in.
    let shade = |fill: Rgb565| match ink {
        Ink::Colour => fill,
        Ink::Lines(colour) => colour,
    };
    // Room for the largest polygon of every ring table, its holes in,
    // twice over for the corners the clip may add.
    let mut room = 16usize;
    for table in [world.land, world.lakes, world.ice, world.shelves] {
        let mut polygon = 0usize;
        for r in rings(table) {
            polygon = match r.hole {
                true => polygon + r.len(),
                false => r.len(),
            };
            room = room.max(2 * polygon + 16);
        }
    }
    let room = room.min(usize::from(u16::MAX));
    let (_, h) = view.size();
    let mut edges = vec![Edge::EMPTY; room];
    let mut active = vec![0u16; room];
    let mut heads = vec![0u16; h as usize];
    let mut scratch = Scratch {
        edges: &mut edges,
        heads: &mut heads,
        active: &mut active,
    };
    let mut filler = Filler::new(&mut scratch, target.bounding_box());
    let here = view.zoom() <= MAX_ZOOM;
    let landst = rings_into(world.land, view, ink, shade(land), &mut filler, target)?;
    st.rings = landst.rings;
    st.culled = landst.culled;
    st.vertices = landst.vertices;
    st.fill = landst.fill;
    st.stroke = landst.stroke;
    st.overflows = landst.overflows;
    if here {
        let water = water(theme);
        let s = rings_into(world.lakes, view, ink, shade(water), &mut filler, target)?;
        st.lakes = s.rings;
        st.lake += match ink {
            Ink::Colour => s.fill,
            Ink::Lines(_) => s.stroke,
        };
        st.overflows += s.overflows;
        let rivers = lines_into(
            world.rivers,
            view,
            Point::zero(),
            shade(water),
            None,
            &mut filler,
            target,
        )?;
        st.rivers = rivers.rings;
        st.river += rivers.stroke;
        let ice = ice(theme);
        for table in [world.ice, world.shelves] {
            let s = rings_into(table, view, ink, shade(ice), &mut filler, target)?;
            st.ice += s.rings;
            st.snowy += match ink {
                Ink::Colour => s.fill,
                Ink::Lines(_) => s.stroke,
            };
            st.overflows += s.overflows;
        }
    }
    if !here || self::bits(world.borders) != self::bits(world.land) {
        return Ok(st);
    }
    let colour = match ink {
        Ink::Colour => palette.border,
        Ink::Lines(colour) => colour,
    };
    let b = lines_into(
        world.borders,
        view,
        Point::zero(),
        colour,
        None,
        &mut filler,
        target,
    )?;
    st.borders = b.rings;
    st.border += b.stroke;
    let d = lines_into(
        world.disputed,
        view,
        Point::zero(),
        colour,
        Some(DASH),
        &mut filler,
        target,
    )?;
    st.disputed = d.rings;
    st.dash += d.stroke;
    Ok(st)
}

/// Stroke the borders of `world` and its disputed lines, dashed, over a
/// picture already drawn for `view` (1683): the M9's overview, whose
/// region carries no boundary class. Nothing is filled or cleared, and
/// `filler` is the caller's, so a firmware with fixed buffers sizes it
/// itself. `at` is where the view's top-left lies on `target`, which is
/// how a frontend draws the picture below its bars. Past [`MAX_ZOOM`], or
/// with no border table, nothing is drawn.
pub fn bounds<D>(
    world: &World,
    view: &Viewport,
    at: Point,
    colour: Rgb565,
    filler: &mut Filler<'_, '_>,
    target: &mut D,
) -> Result<OutlineStats, D::Error>
where
    D: Coverage<Color = Rgb565>,
{
    let mut st = OutlineStats::default();
    if view.zoom() > MAX_ZOOM {
        return Ok(st);
    }
    let (Some(bits), disputed) = (self::bits(world.borders), self::bits(world.disputed)) else {
        return Ok(st);
    };
    let b = lines_into(world.borders, view, at, colour, None, filler, target)?;
    st.borders = b.rings;
    st.border += b.stroke;
    if disputed == Some(bits) {
        let d = lines_into(world.disputed, view, at, colour, Some(DASH), filler, target)?;
        st.disputed = d.rings;
        st.dash += d.stroke;
    }
    Ok(st)
}

/// A dashed line's lengths in sub-pixels: [`DASH_ON`] drawn, [`DASH_OFF`]
/// left.
const DASH: (i64, i64) = (DASH_ON * SUBPIXEL as i64, DASH_OFF * SUBPIXEL as i64);
/// A dash's length in pixels, and the gap after it (1683): the disputed
/// boundary lines' pattern, short enough to read as dashed at z0.
pub const DASH_ON: i64 = 4;
/// The gap after a dash, in pixels.
pub const DASH_OFF: i64 = 3;

/// Fill every ring of `table` in `colour` under [`Ink::Colour`], or stroke
/// them closed in it under [`Ink::Lines`], for `view`, through `filler`:
/// what the layer added.
fn rings_into<D>(
    table: &[u8],
    view: &Viewport,
    ink: Ink,
    colour: Rgb565,
    filler: &mut Filler<'_, '_>,
    target: &mut D,
) -> Result<LayerStats, D::Error>
where
    D: Coverage<Color = Rgb565>,
{
    let mut st = LayerStats::default();
    let (w, h) = view.size();
    let (left, top) = view.top_left_sub();
    let sub = i64::from(SUBPIXEL);
    let (vw, vh) = (i64::from(w) * sub, i64::from(h) * sub);
    // Table units to view sub-pixels: a unit is 2^(32 - bits) world
    // units, a sub-pixel 2^(20 - zoom).
    let Some(bits) = bits(table) else {
        return Ok(st);
    };
    let up = i32::from(view.zoom()) + 12 - bits as i32;
    let scale = |v: u32| -> i64 {
        match up >= 0 {
            true => i64::from(v) << up,
            false => i64::from(v) >> -up,
        }
    };
    let world = 1i64 << (12 + u32::from(view.zoom()));
    let mut ring = Vec::new();
    let mut clipped = Vec::new();
    let mut points: Vec<Point> = Vec::new();
    for k in left.div_euclid(world)..=(left + vw - 1).div_euclid(world) {
        let dx = k * world - left;
        for r in rings(table) {
            // A new polygon: fill the one before, whose holes are in.
            if !r.hole && ink == Ink::Colour && !filler.is_empty() {
                st.fill += filler.fill(target, colour, FillRule::EvenOdd)?;
            }
            let [x0, y0, x1, y1] = r.bbox;
            if scale(x1) + dx < -GUARD
                || scale(x0) + dx > vw + GUARD
                || scale(y1) - top < -GUARD
                || scale(y0) - top > vh + GUARD
            {
                st.culled += 1;
                continue;
            }
            st.rings += 1;
            ring.clear();
            ring.extend(r.points().map(|(x, y)| (scale(x) + dx, scale(y) - top)));
            clip(&ring, -GUARD, -GUARD, vw + GUARD, vh + GUARD, &mut clipped);
            if clipped.len() < 3 {
                continue;
            }
            st.vertices += clipped.len() as u32;
            points.clear();
            points.extend(clipped.iter().map(|&(x, y)| Point::new(x as i32, y as i32)));
            match ink {
                Ink::Colour => {
                    if filler.ring(&points).is_err() {
                        filler.clear();
                        st.overflows += 1;
                    }
                }
                Ink::Lines(_) => {
                    points.push(points[0]);
                    st.stroke += stroke(filler, &points, SUBPIXEL, target, colour)?;
                }
            }
        }
        if ink == Ink::Colour && !filler.is_empty() {
            st.fill += filler.fill(target, colour, FillRule::EvenOdd)?;
        }
    }
    Ok(st)
}

/// Stroke every line of `table` in `colour`, dashed when `dash` gives the
/// lengths of a dash and its gap, for `view`, through `filler`: what the
/// layer added. A segment is clipped to the box around the view first; runs
/// the clip keeps joined are stroked as one line.
fn lines_into<D>(
    table: &[u8],
    view: &Viewport,
    at: Point,
    colour: Rgb565,
    dash: Option<(i64, i64)>,
    filler: &mut Filler<'_, '_>,
    target: &mut D,
) -> Result<LayerStats, D::Error>
where
    D: Coverage<Color = Rgb565>,
{
    let mut st = LayerStats::default();
    let (w, h) = view.size();
    let (left, top) = view.top_left_sub();
    let sub = i64::from(SUBPIXEL);
    let (vw, vh) = (i64::from(w) * sub, i64::from(h) * sub);
    let Some(bits) = bits(table) else {
        return Ok(st);
    };
    let up = i32::from(view.zoom()) + 12 - bits as i32;
    let scale = |v: u32| -> i64 {
        match up >= 0 {
            true => i64::from(v) << up,
            false => i64::from(v) >> -up,
        }
    };
    let world = 1i64 << (12 + u32::from(view.zoom()));
    let mut points: Vec<Point> = Vec::new();
    for k in left.div_euclid(world)..=(left + vw - 1).div_euclid(world) {
        let dx = k * world - left;
        for l in lines(table) {
            let [x0, y0, x1, y1] = l.bbox;
            if scale(x1) + dx < -GUARD
                || scale(x0) + dx > vw + GUARD
                || scale(y1) - top < -GUARD
                || scale(y0) - top > vh + GUARD
            {
                st.culled += 1;
                continue;
            }
            st.rings += 1;
            // Runs of segments the clip keeps joined, stroked one by one.
            points.clear();
            let mut prev: Option<(i64, i64)> = None;
            for (x, y) in l.points() {
                let p = (scale(x) + dx, scale(y) - top);
                let Some(a) = prev.replace(p) else {
                    continue;
                };
                let Some((a, b)) = clip_segment(a, p, -GUARD, -GUARD, vw + GUARD, vh + GUARD)
                else {
                    continue;
                };
                let (a, b) = (
                    Point::new(a.0 as i32 + at.x, a.1 as i32 + at.y),
                    Point::new(b.0 as i32 + at.x, b.1 as i32 + at.y),
                );
                if points.last() != Some(&a) {
                    st.stroke += flush(points.as_slice(), dash, filler, target, colour)?;
                    points.clear();
                    points.push(a);
                }
                points.push(b);
            }
            st.stroke += flush(points.as_slice(), dash, filler, target, colour)?;
        }
    }
    Ok(st)
}

/// Stroke `points`, wholly or in its dashes.
fn flush<D>(
    points: &[Point],
    dash: Option<(i64, i64)>,
    filler: &mut Filler<'_, '_>,
    target: &mut D,
    colour: Rgb565,
) -> Result<FillStats, D::Error>
where
    D: Coverage<Color = Rgb565>,
{
    if points.len() < 2 {
        return Ok(FillStats::default());
    }
    let Some((on, off)) = dash else {
        return stroke(filler, points, SUBPIXEL, target, colour);
    };
    let mut st = FillStats::default();
    for run in dashes(points, on, off) {
        if run.len() > 1 {
            st += stroke(filler, &run, SUBPIXEL, target, colour)?;
        }
    }
    Ok(st)
}

/// The square root of `v`, rounded down: integer, so a dash falls on the
/// same sub-pixel on every target.
fn isqrt(v: u64) -> u64 {
    if v < 2 {
        return v;
    }
    let (mut x, mut y) = (v, v.div_ceil(2));
    while y < x {
        x = y;
        y = (x + v / x) / 2;
    }
    x
}

/// The pieces of `line` a dash pattern of `on` drawn and `off` left paints,
/// in sub-pixels: every dash a line of its own, the pattern carrying on
/// across the line's segments.
fn dashes(line: &[Point], on: i64, off: i64) -> Vec<Vec<Point>> {
    let mut out: Vec<Vec<Point>> = Vec::new();
    let period = on + off;
    if period <= 0 {
        return vec![line.to_vec()];
    }
    let mut current: Option<Vec<Point>> = None;
    // How far into the pattern the last segment ended.
    let mut phase = 0i64;
    for w in line.windows(2) {
        let ((x0, y0), (x1, y1)) = ((w[0].x, w[0].y), (w[1].x, w[1].y));
        let (dx, dy) = (i64::from(x1 - x0), i64::from(y1 - y0));
        let len = isqrt(dx.unsigned_abs().pow(2) + dy.unsigned_abs().pow(2)) as i64;
        if len == 0 {
            continue;
        }
        let mut at = 0i64;
        while at < len {
            let into = phase % period;
            let left = match into < on {
                true => on - into,
                false => period - into,
            };
            let take = left.min(len - at);
            let a = Point::new(x0 + (dx * at / len) as i32, y0 + (dy * at / len) as i32);
            let b = Point::new(
                x0 + (dx * (at + take) / len) as i32,
                y0 + (dy * (at + take) / len) as i32,
            );
            if into < on {
                match current {
                    Some(ref mut run) => {
                        if run.last() != Some(&a) {
                            run.push(a);
                        }
                        run.push(b);
                    }
                    None => current = Some(vec![a, b]),
                }
            } else if let Some(run) = current.take() {
                out.push(run);
            }
            at += take;
            phase += take;
        }
    }
    if let Some(run) = current.take() {
        out.push(run);
    }
    out.retain(|r| r.len() > 1);
    out
}

/// The magic of a table of names.
const NAMES: &[u8; 4] = b"LWN1";

/// The bits a coordinate of a name table has, `None` for no name table.
fn name_bits(table: &[u8]) -> Option<u32> {
    match table.get(..5) {
        Some(head) if head[..4] == *NAMES && (8..=24).contains(&head[4]) => {
            Some(u32::from(head[4]))
        }
        _ => None,
    }
}

/// The names of the table `table` ([`NAMES_50M`], or the M9's
/// [`NAMES_M9`]) that `view` shows, appended to `out` (1683): the names
/// whose shallowest zoom is at most the view's and whose point lies in
/// it, once for every copy of the world the point falls in. The reader
/// places nothing; the frontend hands them to the core's one rule. A table
/// cut short ends there and a kind this reader does not know is skipped,
/// never a panic. Past [`MAX_ZOOM`] it appends nothing: the table is
/// thinned for the zooms the outline draws ([`draw`]).
pub fn names(table: &[u8], view: &Viewport, out: &mut Vec<ViewLabel>) {
    if view.zoom() > MAX_ZOOM {
        return;
    }
    let _ = read_names(table, view, out);
}

fn read_names(table: &[u8], view: &Viewport, out: &mut Vec<ViewLabel>) -> Option<()> {
    let bits = name_bits(table)?;
    let mut at = 5usize;
    let count = usize::try_from(varint(table, &mut at)?).ok()?;
    let head = at;
    let offsets = table.get(head..head + count.checked_add(1)?.checked_mul(4)?)?;
    let get = |i: usize| -> Option<u32> {
        let b = offsets.get(i * 4..i * 4 + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let data_at = head + offsets.len();
    let data = table.get(data_at..data_at + usize::try_from(get(count)?).ok()?)?;
    let mut at = data_at + data.len();
    let points = varint(table, &mut at)?;
    let string = |i: u32| -> Option<&str> {
        let n = usize::try_from(i).ok()?;
        if n >= count {
            return None;
        }
        let (a, b) = (
            usize::try_from(get(n)?).ok()?,
            usize::try_from(get(n + 1)?).ok()?,
        );
        core::str::from_utf8(data.get(a..b)?).ok()
    };
    let (w, h) = view.size();
    let (left, top) = view.top_left_sub();
    let sub = i64::from(SUBPIXEL);
    let (vw, vh) = (i64::from(w) * sub, i64::from(h) * sub);
    let up = i32::from(view.zoom()) + 12 - bits as i32;
    let scale = |v: u32| -> i64 {
        match up >= 0 {
            true => i64::from(v) << up,
            false => i64::from(v) >> -up,
        }
    };
    let world = 1i64 << (12 + u32::from(view.zoom()));
    for _ in 0..points {
        let (kind, rank, min_zoom, _) = (
            *table.get(at)?,
            *table.get(at + 1)?,
            *table.get(at + 2)?,
            *table.get(at + 3)?,
        );
        at += 4;
        let (x, y) = (varint(table, &mut at)?, varint(table, &mut at)?);
        let string_at = varint(table, &mut at)?;
        let alternate = varint(table, &mut at)?;
        let Some(kind) = LabelKind::from_wire(kind) else {
            continue;
        };
        if min_zoom > view.zoom() {
            continue;
        }
        let Some(local) = string(string_at) else {
            continue;
        };
        let text = match alternate.checked_sub(1) {
            Some(a) => string(a).unwrap_or(local),
            None => local,
        };
        let (sx, sy) = (scale(x), scale(y) - top);
        for k in left.div_euclid(world)..=(left + vw - 1).div_euclid(world) {
            let (ax, ay) = (sx + k * world - left, sy);
            if !(0..vw).contains(&ax) || !(0..vh).contains(&ay) {
                continue;
            }
            out.push(ViewLabel {
                kind,
                rank,
                min_zoom,
                text: String::from(text),
                local: String::from(local),
                at: (x, y),
                anchor: (ax, ay),
                line: Vec::new(),
            });
        }
    }
    Some(())
}
