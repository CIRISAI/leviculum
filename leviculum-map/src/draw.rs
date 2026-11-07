//! One view drawn from its tiles through `leviculum-mapfill`.
//!
//! A tile's features are written in class order, but a view of several
//! tiles cannot be drawn tile after tile: geometry reaches past its tile's
//! edge (the source tiles carry a buffer), and the second tile's areas
//! would paint over the first tile's roads there. So the draw goes class by
//! class across all the tiles of the view, the order a single fixture of
//! the whole view had in the measurement. That needs every tile of the view
//! at hand at once, a few tens of kilobytes at z15 under profile 1.
//!
//! The lines go in three layers, tunnels under the ground's roads under
//! bridges, and under the PC finish ([`Style::PC`]) each layer strokes its
//! casings first, so the casings of a junction join under its fills and a
//! bridge's casing covers the road it crosses.
//!
//! Building footprints come from the region's building sections, not
//! from the blobs (1191): drawn after the areas, so over water and under
//! every line and name, at view zoom [`BUILDINGS_FROM`] and deeper where
//! the style asks for them ([`Style::buildings`]).
//!
//! The line widths follow the view zoom ([`crate::profile::line_widths`]),
//! and [`Ink::Lines`] draws the lines alone in one colour, for a terminal
//! that has one colour a cell and no fill (braille).

use alloc::vec::Vec;
use core::iter::Peekable;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::primitives::Rectangle;
use leviculum_mapfill::coverage::{Clip, Coverage};
use leviculum_mapfill::fill::Scratch;
use leviculum_mapfill::fixture::{Feature, Features, Fixture};
use leviculum_mapfill::render::{Clock, Look, Pass, Placement, RenderStats, Work};
use leviculum_mapfill::style::{Class, Profile, Style, AREAS};

use crate::buildings::{Bbox, Buildings, BUILDING_ZOOM};
use crate::region::{check_tile, Entry, Region, RegionError, Slot, Storage, TileId};
use crate::viewport::{ViewTile, Viewport};

/// The view zoom buildings are drawn from. At z15 they are 3 to 8 px long
/// and cost a fifth to a quarter more on the dearest frame; at z16 the
/// phone shows them as CoMaps shows its z14.5 (the study of 1191,
/// section 2, "Zoom gate").
pub const BUILDINGS_FROM: u8 = 16;

/// The most footprints a z15 tile's building section may hold and be
/// drawn outlined. A tile over it is drawn as blocks: every footprint
/// filled, no outline, so touching buildings merge into their block, as
/// Protomaps' merged z14 blocks look. The outline is about half the cost
/// of a building (the study of 1191, section 2), so this bounds the
/// dearest tiles at the cost of their fills. Counted over the whole
/// section, not over the part a view shows, so a tile looks the same in
/// the tile cache's 256 pixel tiles and in a view drawn whole. Frankfurt's
/// z15 tiles hold 876 footprints at the 90th percentile, 1,554 at the
/// 99th and 2,228 at most, Berlin's 1,426 at most: Frankfurt's densest
/// tiles are the ones it draws as blocks.
pub const BUILDING_CAP: u32 = 2048;

/// A tile of the view with its geometry and, where the view draws them,
/// its building footprints.
#[derive(Clone, Copy, Debug)]
pub struct DrawTile<'a> {
    pub fixture: Fixture<'a>,
    pub tile: ViewTile,
    /// The tile's building section; `None` where it has none or the view
    /// does not draw them ([`draws_buildings`]).
    pub buildings: Option<Buildings<'a>>,
}

/// Whether a view at zoom `zoom` drawn in `ink` and `style` draws
/// building footprints.
pub fn draws_buildings(style: Style, ink: Ink, zoom: u8) -> bool {
    style.buildings && ink == Ink::Colour && zoom >= BUILDINGS_FROM
}

/// How a view is inked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    /// Areas filled, lines in the colour table.
    Colour,
    /// Lines only, all in this colour, on the background.
    Lines(Rgb565),
}

impl Ink {
    /// The mapfill look of this ink for a view at zoom `z` in `style`.
    pub fn look(self, z: u8, style: Style) -> Look {
        let look = Look::of(style, crate::profile::line_widths(z));
        match self {
            Self::Colour => look,
            Self::Lines(c) => Look {
                mono: Some(c),
                ..look
            },
        }
    }
}

/// Which of the three line layers a feature is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer {
    Tunnel,
    Ground,
    Bridge,
}

impl Layer {
    fn of(f: &Feature<'_>) -> Self {
        match (f.tunnel(), f.bridge()) {
            (true, _) => Self::Tunnel,
            (false, true) => Self::Bridge,
            (false, false) => Self::Ground,
        }
    }
}

/// Draw `tiles` under `profile` into `target`, which is cleared first, in
/// `ink` at the widths of the view's zoom, in the M9's finish
/// ([`Style::M9`]). `scratch` and `work` as
/// `leviculum_mapfill::render::render` takes them.
pub fn draw<D, C>(
    tiles: &[DrawTile<'_>],
    profile: Profile,
    ink: Ink,
    target: &mut D,
    scratch: &mut Scratch<'_>,
    work: &mut Work<'_>,
    clock: &mut C,
) -> Result<RenderStats, D::Error>
where
    D: Coverage<Color = Rgb565>,
    C: Clock,
{
    let all = target.bounding_box();
    draw_in(tiles, all, profile, ink, target, scratch, work, clock)
}

/// [`draw_in`] in `style`, with every line `percent` per cent as wide
/// ([`Look::scaled`]): a GUI on a dense screen draws at the screen's own
/// pixels and passes its scale factor here.
#[allow(clippy::too_many_arguments)]
pub fn draw_in_scaled<D, C>(
    tiles: &[DrawTile<'_>],
    area: Rectangle,
    profile: Profile,
    ink: Ink,
    style: Style,
    percent: u32,
    target: &mut D,
    scratch: &mut Scratch<'_>,
    work: &mut Work<'_>,
    clock: &mut C,
) -> Result<RenderStats, D::Error>
where
    D: Coverage<Color = Rgb565>,
    C: Clock,
{
    let target = &mut Clip::new(target, &area);
    // Every tile of one view is at the view's zoom less its overzoom.
    let zoom = tiles
        .first()
        .map_or(0, |t| t.tile.id.z.saturating_add(t.tile.overzoom));
    let look = ink.look(zoom, style).scaled(percent);
    let mut pass = Pass::begin(target, scratch, look, clock)?;
    let mut cursors: Vec<_> = tiles
        .iter()
        .map(|t| {
            let at = Placement {
                shift: t.fixture.shift() + t.tile.overzoom as i8,
                origin: t.tile.origin,
            };
            (at, t.fixture.features().peekable())
        })
        .collect();
    let (areas, lines) = Class::ALL.split_at(AREAS);
    for &class in areas {
        for (at, features) in &mut cursors {
            // `<=`: a blob out of class order still has every feature drawn.
            while let Some(f) = features.next_if(|f| f.class <= class) {
                pass.feature(&f, *at, profile, target, work, clock)?;
            }
        }
    }
    if draws_buildings(style, ink, zoom) {
        for t in tiles {
            if let Some(b) = t.buildings {
                buildings(&mut pass, t.tile, b, target, work, clock)?;
            }
        }
    }
    // Every cursor now stands at its tile's first line. One walk keeps the
    // tunnels and bridges aside, few in any view, in the order the lines
    // are drawn in; the ground's lines are walked again for each phase. A
    // walk per layer and phase nearly doubled the instructions of
    // `Features::next` on the M9 view of Frankfurt at z15 (callgrind, 1099:
    // 7.5 M over 20 draws against 3.9 M at one walk; 5.9 M this way).
    let (mut tunnels, mut bridges) = (Vec::new(), Vec::new());
    walk(cursors.clone(), lines, |at, f| {
        match Layer::of(&f) {
            Layer::Tunnel => tunnels.push((at, f)),
            Layer::Bridge => bridges.push((at, f)),
            Layer::Ground => {}
        }
        Ok(())
    })?;
    let cased = look.casing > 0 && look.mono.is_none();
    if cased {
        for (at, f) in &tunnels {
            pass.casing(f, *at, profile, target, work, clock)?;
        }
    }
    for (at, f) in &tunnels {
        pass.feature(f, *at, profile, target, work, clock)?;
    }
    if cased {
        walk(cursors.clone(), lines, |at, f| match Layer::of(&f) {
            Layer::Ground => pass.casing(&f, at, profile, target, work, clock),
            _ => Ok(()),
        })?;
    }
    walk(cursors, lines, |at, f| match Layer::of(&f) {
        Layer::Ground => pass.feature(&f, at, profile, target, work, clock),
        _ => Ok(()),
    })?;
    if cased {
        for (at, f) in &bridges {
            pass.casing(f, *at, profile, target, work, clock)?;
        }
    }
    for (at, f) in &bridges {
        pass.feature(f, *at, profile, target, work, clock)?;
    }
    Ok(pass.stats)
}

/// Draw the footprints of the z15 tile `tile` that its part of the view
/// shows: every cell whose box meets that part, each footprint culled by
/// its box; outlined unless the section holds more than [`BUILDING_CAP`].
fn buildings<D, C>(
    pass: &mut Pass<'_, '_>,
    tile: ViewTile,
    b: Buildings<'_>,
    target: &mut D,
    work: &mut Work<'_>,
    clock: &mut C,
) -> Result<(), D::Error>
where
    D: Coverage<Color = Rgb565>,
    C: Clock,
{
    if tile.id.z != BUILDING_ZOOM {
        return Ok(());
    }
    // A section's units are a z15 tile's at extent 4096, one sub-pixel
    // each at 256 pixels a tile; the overzoom doubles them per step.
    let at = Placement {
        shift: tile.overzoom as i8,
        origin: tile.origin,
    };
    let (vl, vt, vr, vb) = pass.view();
    let unit = |v: i32, o: i32| {
        let u = (v - o) >> tile.overzoom.min(31);
        u.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    };
    let part = Bbox {
        x0: unit(vl, at.origin.x),
        y0: unit(vt, at.origin.y),
        x1: unit(vr, at.origin.x),
        y1: unit(vb, at.origin.y),
    };
    let outline = b.len() <= BUILDING_CAP;
    if !outline {
        pass.stats.blocks += 1;
    }
    for cell in b.touching(part) {
        for f in cell.footprints() {
            let Ok(f) = f else {
                // A corrupt record ends its cell; the rest is drawn.
                pass.stats.overflows += 1;
                break;
            };
            let bb = f.bbox;
            let rings = f.rings().map(|r| r.points());
            let bbox = (
                i32::from(bb.x0),
                i32::from(bb.y0),
                i32::from(bb.x1),
                i32::from(bb.y1),
            );
            pass.building(rings, bbox, at, outline, target, work, clock)?;
        }
    }
    Ok(())
}

/// Hand `visit` every feature left in `cursors`, class by class of
/// `classes` across the tiles: the order a single fixture of the whole
/// view would hold them in.
fn walk<'a, E>(
    mut cursors: Vec<(Placement, Peekable<Features<'a>>)>,
    classes: &[Class],
    mut visit: impl FnMut(Placement, Feature<'a>) -> Result<(), E>,
) -> Result<(), E> {
    for &class in classes {
        for (at, features) in &mut cursors {
            // `<=`: a blob out of class order still has every feature drawn.
            while let Some(f) = features.next_if(|f| f.class <= class) {
                visit(*at, f)?;
            }
        }
    }
    Ok(())
}

/// [`draw`] into the part `area` of `target` alone, which is cleared
/// first; the rest of `target` is left as it is. A region's view goes into
/// its box, [`Region::area`], so its edge is the same place on the earth
/// at every zoom, and the frontend draws the outline under the rest
/// (1077). The filler's clip is `area`, so a tile outside it costs its
/// bounding box test and no more.
#[allow(clippy::too_many_arguments)]
pub fn draw_in<D, C>(
    tiles: &[DrawTile<'_>],
    area: Rectangle,
    profile: Profile,
    ink: Ink,
    target: &mut D,
    scratch: &mut Scratch<'_>,
    work: &mut Work<'_>,
    clock: &mut C,
) -> Result<RenderStats, D::Error>
where
    D: Coverage<Color = Rgb565>,
    C: Clock,
{
    draw_in_scaled(
        tiles,
        area,
        profile,
        ink,
        Style::M9,
        100,
        target,
        scratch,
        work,
        clock,
    )
}

impl<S: Storage> Region<S> {
    /// The pixels of `view` this region's box covers, which [`draw_in`]
    /// draws its tiles into: whether the view covers it whole is
    /// `area == view's size`, and where it does not, the outline goes
    /// under the rest.
    pub fn area(&self, view: &Viewport) -> Rectangle {
        view.area_of(&self.header().bounds)
    }
}

impl<S: Storage> Region<S> {
    /// The blobs `view` needs that this region holds, each placed, and how
    /// many of the view's tiles have not arrived (a version 4 file still
    /// arriving, 1291). A tile not arrived is stood in for by the nearest
    /// coarser one that has ([`Region::stand_in`]), placed over it and
    /// listed once however many tiles it stands in for.
    pub fn view_entries(
        &self,
        view: &Viewport,
    ) -> Result<(Vec<(ViewTile, Entry)>, usize), RegionError> {
        let mut out: Vec<(ViewTile, Entry)> = Vec::new();
        let mut pending = 0;
        let mut ups: Vec<(ViewTile, Entry)> = Vec::new();
        for tile in view.tiles(self.zooms()) {
            match self.find_slot(tile.id)? {
                Slot::Here(e) => out.push((tile, e)),
                Slot::Empty => {}
                Slot::Pending => {
                    pending += 1;
                    let Some((e, dz)) = self.stand_in(tile.id)? else {
                        continue;
                    };
                    let Some(up) = tile.up(dz) else {
                        continue;
                    };
                    if !ups.iter().any(|(t, _)| t.id == up.id) {
                        ups.push((up, e));
                    }
                }
            }
        }
        // The stand-ins first: the tiles that are there draw over them in
        // each class.
        ups.extend(out);
        Ok((ups, pending))
    }
}

impl<'a> Region<&'a [u8]> {
    /// The tiles `view` needs that this region holds, borrowed in place;
    /// a tile not arrived yet stood in for as [`Region::view_entries`]
    /// says.
    pub fn view(&self, view: &Viewport) -> Result<Vec<DrawTile<'a>>, RegionError> {
        let (entries, _) = self.view_entries(view)?;
        let mut out = Vec::new();
        for (tile, e) in entries {
            if let Some(fixture) = self.fixture_of(&e)? {
                out.push(DrawTile {
                    fixture,
                    tile,
                    buildings: None,
                });
            }
        }
        Ok(out)
    }
}

impl<S: Storage> Region<S> {
    /// Read the tiles `view` needs that this region holds into `out`, one
    /// buffer each, for storage that is not addressable memory: a file, a
    /// card. `out` is cleared first; its buffers are reused across calls.
    /// [`draw_tiles`] then borrows them for [`draw`]. How many of the
    /// view's tiles have not arrived yet, each stood in for as
    /// [`Region::view_entries`] says: 0 for a whole file.
    pub fn read_view(
        &self,
        view: &Viewport,
        out: &mut Vec<(ViewTile, Vec<u8>)>,
    ) -> Result<usize, RegionError> {
        let mut spare: Vec<Vec<u8>> = out.drain(..).map(|(_, b)| b).collect();
        let (entries, pending) = self.view_entries(view)?;
        for (tile, e) in entries {
            let mut buf = spare.pop().unwrap_or_default();
            self.read_tile(&e, &mut buf)?;
            out.push((tile, buf));
        }
        Ok(pending)
    }
}

impl<'a> Region<&'a [u8]> {
    /// Give each z15 tile of `tiles` its building section, borrowed in
    /// place: what a view that draws buildings ([`draws_buildings`])
    /// asks after [`Region::view`]. A region older than version 3 has
    /// none.
    pub fn with_buildings(&self, tiles: &mut [DrawTile<'a>]) -> Result<(), RegionError> {
        for t in tiles.iter_mut() {
            if t.tile.id.z == BUILDING_ZOOM {
                t.buildings = self.buildings(t.tile.id)?;
            }
        }
        Ok(())
    }
}

impl<S: Storage> Region<S> {
    /// Read the building sections of the z15 tiles of `tiles` into `out`,
    /// one buffer each, for storage that is not addressable memory, as
    /// [`Region::read_view`] reads the blobs; `out` is cleared first and
    /// its buffers reused. [`attach_buildings`] then hands them to the
    /// tiles. Only a view that draws buildings reads them, so the M9 and
    /// the terminal never read a byte of them.
    pub fn read_buildings(
        &self,
        tiles: &[DrawTile<'_>],
        out: &mut Vec<(TileId, Vec<u8>)>,
    ) -> Result<(), RegionError> {
        let mut spare: Vec<Vec<u8>> = out.drain(..).map(|(_, b)| b).collect();
        for t in tiles.iter().filter(|t| t.tile.id.z == BUILDING_ZOOM) {
            if let Some(e) = self.find_buildings(t.tile.id)? {
                let mut buf = spare.pop().unwrap_or_default();
                self.read_tile(&e, &mut buf)?;
                out.push((t.tile.id, buf));
            }
        }
        Ok(())
    }
}

/// Give each tile of `tiles` its building section from `read`, as
/// [`Region::read_buildings`] read them, the head checked.
pub fn attach_buildings<'a>(
    tiles: &mut [DrawTile<'a>],
    read: &'a [(TileId, Vec<u8>)],
) -> Result<(), RegionError> {
    for (id, bytes) in read {
        let b = Buildings::parse(bytes).map_err(|e| RegionError::Buildings(*id, e))?;
        for t in tiles.iter_mut().filter(|t| t.tile.id == *id) {
            t.buildings = Some(b);
        }
    }
    Ok(())
}

/// The tiles [`Region::read_view`] read, checked and ready to draw.
pub fn draw_tiles(read: &[(ViewTile, Vec<u8>)]) -> Result<Vec<DrawTile<'_>>, RegionError> {
    read.iter()
        .map(|(tile, buf)| {
            Ok(DrawTile {
                fixture: check_tile(tile.id, buf)?,
                tile: *tile,
                buildings: None,
            })
        })
        .collect()
}
