//! What the map's autoload loads for the area on screen (1292): one
//! scheduler, driven by the frames the frontend asks, that says which
//! regions the view needs now and which it needs next. The frontend only
//! downloads, in the order given (`crate::mapload::Loader::schedule`).
//!
//! - The screen itself at its zoom loads first (1527, Lew's rule: the
//!   exact area looked at, at the zoom looked at, before anything else):
//!   the box under the uncovered part of the screen, a half tile past it
//!   and rounded out to the tiles of the view's zoom, at that zoom and the
//!   one above it ([`screen_want`]). It is small, so it lands before the
//!   cell around it starts; then the cell follows. It is asked while the
//!   view moves too: a box the view left runs on (a load cannot stop),
//!   two loads at most run, and a box the view left that waits is
//!   dropped (`crate::mapload`), so the box of the view it stops on is
//!   never more than one load behind (measured against a speed gate,
//!   which brought the screen's first tile later, 1527).
//! - The view is cut into cells, the tiles of a zoom [`CELL_ABOVE`] steps
//!   above the view's, so a pan asks for the same boxes again and a region
//!   that landed is never asked for twice.
//! - A frame whose screen the regions leave uncovered by more than
//!   [`GAP_SHARE`] asks for the cells under the uncovered part at once,
//!   during the pan too: one decision every [`DECIDE_MS`] while it moves,
//!   counted from the last decision, so motion never restarts the wait;
//!   every frame at rest decides.
//! - The cells next to the screen, those in the pan's direction first, and
//!   while the view moves only those, come after the cells on screen.
//! - A region cut under an old layer profile holds nothing (1707); a
//!   screen over it asks for that region's own box and zooms
//!   (`renewals`, 1727), which land under its name and replace it, and
//!   no cell or screen's box is asked for over it meanwhile.
//! - A frame that shows the view of the last decision decides nothing
//!   again: a load that failed for want of internet is tried again by the
//!   next view the user moves to, not by a frame asked again.

use alloc::vec::Vec;

use leviculum_map::mercator::{lat_of_y, lon_of_x, project, unproject, LatLon, World};
use leviculum_map::region::{Bounds, Header};
use leviculum_map::viewport::Viewport;

use super::map::{LoadReason, MapWant, DEEPEST_LOAD, PLACE_ZOOMS, VIEW_BELOW};

/// A cell is a tile this many zooms above the view's: eight tiles of the
/// view across, about a phone screen and a half. No deeper than
/// [`CELL_DEEPEST`].
pub const CELL_ABOVE: u8 = 3;
/// The deepest cell: a z16 view's, about 3 km across in Europe, so a view
/// deeper than z16 asks for its cell as a z16 view does.
pub const CELL_DEEPEST: u8 = DEEPEST_LOAD + 1 - CELL_ABOVE;
/// The share of the screen the regions may leave uncovered before the
/// cells under it are asked for.
pub const GAP_SHARE: f64 = 0.10;
/// While the view moves, one decision this often.
pub const DECIDE_MS: u64 = 250;
/// Below this zoom nothing is asked for by itself: a box there is a
/// country, and the world outline draws.
pub const MIN_ZOOM: u8 = PLACE_ZOOMS.0;
/// Below this zoom only the cells on screen are asked for, none next to
/// them.
pub const AHEAD_MIN_ZOOM: u8 = 12;
/// How far past the screen, in view pixels, the cells next to it reach:
/// the prefetch's two tiles (1135).
pub const AHEAD_PX: u32 = 512;
/// Sample points across and down the screen the share is counted on.
const SAMPLES: u32 = 16;

/// A cell: its zoom and tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cell {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl Cell {
    fn of(at: World, z: u8) -> Self {
        let shift = 32 - u32::from(z);
        let cut = |v: u32| v.checked_shr(shift).unwrap_or(0);
        Self {
            z,
            x: cut(at.x),
            y: cut(at.y),
        }
    }

    /// Its corners in world units, west and north in, east and south out.
    fn span(&self) -> ((u64, u64), (u64, u64)) {
        let shift = 32 - u32::from(self.z);
        let edge = |v: u32| u64::from(v) << shift;
        (
            (edge(self.x), edge(self.x + 1)),
            (edge(self.y), edge(self.y + 1)),
        )
    }

    /// Its middle.
    pub fn centre(&self) -> World {
        let ((x0, x1), (y0, y1)) = self.span();
        World {
            x: ((x0 + x1) / 2).min(u64::from(u32::MAX)) as u32,
            y: ((y0 + y1) / 2).min(u64::from(u32::MAX)) as u32,
        }
    }

    /// Its box in degrees.
    pub fn bounds(&self) -> Bounds {
        let ((x0, x1), (y0, y1)) = self.span();
        let at = |v: u64| v.min(u64::from(u32::MAX)) as u32;
        Bounds {
            west: lon_of_x(at(x0)),
            south: lat_of_y(at(y1)),
            east: lon_of_x(at(x1)),
            north: lat_of_y(at(y0)),
        }
    }
}

/// The zoom the regions must reach for `view`: its own, no deeper than a
/// load asks for.
fn need_zoom(view: &Viewport) -> u8 {
    view.zoom().min(DEEPEST_LOAD)
}

/// Whether `h` is a region still arriving (1291): written in place, its
/// length 0 until the file is complete. Its box holds nothing yet for the
/// scheduler, so a screen under it stays asked for and the chip says it
/// loads until the file is whole (1527).
fn arriving(h: &Header) -> bool {
    h.streamed && h.len == 0
}

/// Whether `h` is a complete region cut under an old layer profile
/// (1707): its deepest zoom was simplified, which every deeper view
/// magnifies past a pixel, so it does not hold the screen; a view over it
/// asks for it again ([`renewals`]).
fn stale(h: &Header) -> bool {
    h.profile < leviculum_map::profile::CUT_2 && !arriving(h)
}

/// Whether `h` reaches `at` (unprojected) at `zoom`.
fn reaches(h: &Header, p: LatLon, zoom: u8) -> bool {
    let b = &h.bounds;
    h.min_zoom <= zoom
        && h.max_zoom >= zoom
        && (b.west..=b.east).contains(&p.lon_e7)
        && (b.south..=b.north).contains(&p.lat_e7)
}

/// Whether a region of `regions` holds `at` at `zoom`: a complete region
/// of the current profile.
fn held(regions: &[Header], at: World, zoom: u8) -> bool {
    let p = unproject(at);
    regions
        .iter()
        .any(|h| h.profile >= leviculum_map::profile::CUT_2 && !arriving(h) && reaches(h, p, zoom))
}

/// Whether `at` at `zoom` is held, or lies in an old profile's region
/// that [`renewals`] asks for again whole (1727): no cell and no
/// screen's box is asked for there, so the region's own load replaces it
/// under its own name (`mapload::auto_name`) instead of a second region
/// landing over it and the old one staying on disk.
fn covered(regions: &[Header], at: World, zoom: u8) -> bool {
    let p = unproject(at);
    held(regions, at, zoom) || regions.iter().any(|h| stale(h) && reaches(h, p, zoom))
}

/// The loads that renew the old profile's regions under the screen of
/// `view` (1727, the audit of 1707): each region's own box and zooms, its
/// deepest never lowered, so the load lands under the region's own name
/// and takes its place once it verifies (`crate::mapload`). An
/// autoloaded region is so replaced; a hand-placed one gets an
/// autoloaded copy beside it, which draws over it
/// (`leviculum_map::choice::drawn`, the newer profile on top).
fn renewals(view: &Viewport, regions: &[Header]) -> Vec<MapWant> {
    let zoom = need_zoom(view);
    let points: Vec<LatLon> = samples(view, SAMPLES).map(unproject).collect();
    let mut out: Vec<MapWant> = Vec::new();
    for h in regions.iter().filter(|h| stale(h)) {
        if !points.iter().any(|&p| reaches(h, p, zoom)) {
            continue;
        }
        let b = h.bounds;
        if out
            .iter()
            .any(|w| w.bounds == b && w.zooms == (h.min_zoom, h.max_zoom))
        {
            continue;
        }
        let mid = |a: i32, z: i32| i32::try_from((i64::from(a) + i64::from(z)) / 2).unwrap_or(a);
        out.push(MapWant {
            bounds: b,
            zooms: (h.min_zoom, h.max_zoom),
            floor: h.max_zoom,
            reason: LoadReason::View,
            centre: project(LatLon {
                lat_e7: mid(b.south, b.north),
                lon_e7: mid(b.west, b.east),
            }),
        });
    }
    out
}

/// The sample points of `view`: `n` by `n` pixel middles.
fn samples(view: &Viewport, n: u32) -> impl Iterator<Item = World> + '_ {
    let (w, h) = view.size();
    (0..n).flat_map(move |j| {
        (0..n).map(move |i| {
            let px = ((2 * i + 1) * w / (2 * n)) as i32;
            let py = ((2 * j + 1) * h / (2 * n)) as i32;
            view.to_world(px, py)
        })
    })
}

/// The share of `view`'s screen no region of `regions` holds at its zoom,
/// from 0 to 1.
pub fn uncovered(view: &Viewport, regions: &[Header]) -> f64 {
    let zoom = need_zoom(view);
    let gaps = samples(view, SAMPLES)
        .filter(|&at| !held(regions, at, zoom))
        .count();
    gaps as f64 / f64::from(SAMPLES * SAMPLES)
}

/// The cells under the uncovered points of `view`, nearest its centre
/// first.
fn gap_cells(view: &Viewport, regions: &[Header], n: u32) -> Vec<Cell> {
    let zoom = need_zoom(view);
    let z = view.zoom().saturating_sub(CELL_ABOVE).min(CELL_DEEPEST);
    let mut cells: Vec<Cell> = Vec::new();
    for at in samples(view, n) {
        if covered(regions, at, zoom) {
            continue;
        }
        let cell = Cell::of(at, z);
        if !cells.contains(&cell) {
            cells.push(cell);
        }
    }
    let centre = view.centre;
    cells.sort_by(|a, b| {
        let d = |c: &Cell| along(centre, c.centre(), (0.0, 0.0)).1;
        d(a).total_cmp(&d(b)).then(a.cmp(b))
    });
    cells
}

/// How far `to` lies from `from` in world units along `heading` (the
/// first, larger is further ahead) and in all (the second), x the short
/// way round the world.
fn along(from: World, to: World, heading: (f64, f64)) -> (f64, f64) {
    let dx = f64::from(to.x.wrapping_sub(from.x) as i32);
    let dy = f64::from(to.y) - f64::from(from.y);
    (dx * heading.0 + dy * heading.1, dx * dx + dy * dy)
}

/// The load asked for `cell` for a view at `zoom`: the cell's box, from
/// [`VIEW_BELOW`] zooms above the view's to [`DEEPEST_LOAD`], the deepest
/// lowered no further than the view's own (as [`super::map::want_view`]).
fn want(cell: &Cell, zoom: u8) -> MapWant {
    MapWant {
        bounds: cell.bounds(),
        zooms: (zoom.saturating_sub(VIEW_BELOW), DEEPEST_LOAD),
        floor: zoom,
        reason: LoadReason::View,
        centre: cell.centre(),
    }
}

/// The screen's own load (1527): the box under the uncovered samples of
/// `view`, each sample's patch whole, grown by half a tile of the view's
/// zoom and rounded out to that zoom's tile grid, so a pan inside a tile
/// asks for the same box again; at the view's zoom and the one above it,
/// for a zoom out, the deepest never lowered. `None` when nothing is
/// uncovered, or the box would cross the date line (the cells load it).
pub fn screen_want(view: &Viewport, regions: &[Header]) -> Option<MapWant> {
    let zoom = need_zoom(view);
    let (w, h) = view.size();
    let n = SAMPLES;
    let mut span: Option<(i64, i64, i64, i64)> = None;
    for j in 0..n {
        for i in 0..n {
            let px = i64::from((2 * i + 1) * w / (2 * n));
            let py = i64::from((2 * j + 1) * h / (2 * n));
            if covered(regions, view.to_world(px as i32, py as i32), zoom) {
                continue;
            }
            span = Some(match span {
                None => (px, py, px, py),
                Some((x0, y0, x1, y1)) => (x0.min(px), y0.min(py), x1.max(px), y1.max(py)),
            });
        }
    }
    let (x0, y0, x1, y1) = span?;
    // A sample stands for its patch; half a tile of the zoom loaded, in
    // view pixels, past that.
    let tile_px = 256i64 << (view.zoom() - zoom);
    let (gx, gy) = (
        i64::from(w / (2 * n)) + tile_px / 2,
        i64::from(h / (2 * n)) + tile_px / 2,
    );
    let px = |v: i64| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    let nw = Cell::of(view.to_world(px(x0 - gx), px(y0 - gy)), zoom);
    let se = Cell::of(view.to_world(px(x1 + gx), px(y1 + gy)), zoom);
    if se.x < nw.x || se.y < nw.y {
        return None;
    }
    let ((west, _), (north, _)) = nw.span();
    let ((_, east), (_, south)) = se.span();
    let at = |v: u64| v.min(u64::from(u32::MAX)) as u32;
    Some(MapWant {
        bounds: Bounds {
            west: lon_of_x(at(west)),
            south: lat_of_y(at(south)),
            east: lon_of_x(at(east)),
            north: lat_of_y(at(north)),
        },
        zooms: (zoom.saturating_sub(1), zoom),
        floor: zoom,
        reason: LoadReason::Screen,
        centre: view.centre,
    })
}

/// One decision: what the screen needs, in order, and what it needs next.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewWants {
    /// The screen's own load first ([`screen_want`], 1527), then the
    /// cells under the uncovered part of the screen, nearest its centre
    /// first; none when the gap is [`GAP_SHARE`] or less.
    pub here: Vec<MapWant>,
    /// The uncovered cells next to the screen, ahead of the pan first.
    pub ahead: Vec<MapWant>,
    /// The share of the screen uncovered.
    pub share: f64,
}

/// What `view` needs from the regions `regions`, the view moving along
/// `heading` (screen pixels a second, the way the centre goes) or not.
pub fn wants(
    view: &Viewport,
    regions: &[Header],
    moving: bool,
    heading: Option<(f64, f64)>,
) -> ViewWants {
    let zoom = need_zoom(view);
    let share = uncovered(view, regions);
    if view.zoom() < MIN_ZOOM {
        return ViewWants {
            share,
            ..ViewWants::default()
        };
    }
    let (renew, here_cells) = match share > GAP_SHARE {
        true => (renewals(view, regions), gap_cells(view, regions, SAMPLES)),
        false => (Vec::new(), Vec::new()),
    };
    let mut ahead_cells = Vec::new();
    if view.zoom() >= AHEAD_MIN_ZOOM {
        let (w, h) = view.size();
        let grown = Viewport::new(
            view.centre,
            view.zoom(),
            w.saturating_add(2 * AHEAD_PX),
            h.saturating_add(2 * AHEAD_PX),
        );
        let way = heading.unwrap_or((0.0, 0.0));
        let mut next: Vec<(Cell, (f64, f64))> = gap_cells(&grown, regions, 2 * SAMPLES)
            .into_iter()
            .filter(|c| !here_cells.contains(c))
            .map(|c| (c, along(view.centre, c.centre(), way)))
            .filter(|(_, (ahead, _))| !moving || heading.is_some() && *ahead > 0.0)
            .collect();
        next.sort_by(|a, b| {
            b.1 .0
                .total_cmp(&a.1 .0)
                .then(a.1 .1.total_cmp(&b.1 .1))
                .then(a.0.cmp(&b.0))
        });
        ahead_cells = next.into_iter().map(|(c, _)| c).collect();
    }
    let screen = match here_cells.is_empty() {
        true => None,
        false => screen_want(view, regions),
    };
    ViewWants {
        here: screen
            .into_iter()
            .chain(renew)
            .chain(here_cells.iter().map(|c| want(c, zoom)))
            .collect(),
        ahead: ahead_cells.iter().map(|c| want(c, zoom)).collect(),
        share,
    }
}

/// The scheduler of one map: when the last decision was, and for which
/// view.
#[derive(Clone, Debug, Default)]
pub struct ViewLoads {
    decided_ms: Option<u64>,
    decided: Option<(World, u8, (u32, u32))>,
    /// The regions of the last decision, and how many of them were
    /// complete: a region that finished arriving decides again (1527).
    regions: (usize, usize),
}

impl ViewLoads {
    pub fn new() -> Self {
        Self::default()
    }

    /// The decision for the frame of `view` at `now_ms`, or none: the
    /// view moves and the last decision is younger than [`DECIDE_MS`], or
    /// the view and the regions are those of the last decision.
    pub fn decide(
        &mut self,
        view: &Viewport,
        regions: &[Header],
        now_ms: u64,
        moving: bool,
        heading: Option<(f64, f64)>,
    ) -> Option<ViewWants> {
        let key = (view.centre, view.zoom(), view.size());
        let count = (
            regions.len(),
            regions.iter().filter(|h| !arriving(h)).count(),
        );
        if self.decided == Some(key) && self.regions == count {
            return None;
        }
        if moving
            && self
                .decided_ms
                .is_some_and(|at| now_ms.saturating_sub(at) < DECIDE_MS)
        {
            return None;
        }
        self.decided_ms = Some(now_ms);
        self.decided = Some(key);
        self.regions = count;
        Some(wants(view, regions, moving, heading))
    }

    /// Forget the last decision: the next frame decides (the regions were
    /// deleted, the autoload was switched on).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, x_of_lon as x_of, LatLon};

    /// A region of `bounds` at `zooms`, a header and nothing else.
    fn region(bounds: Bounds, zooms: (u8, u8)) -> Header {
        Header {
            bounds,
            min_zoom: zooms.0,
            max_zoom: zooms.1,
            profile: leviculum_map::profile::CUT_2,
            attribution: (0, 0),
            notice: (0, 0),
            tiles: 0,
            index: 0,
            len: 0,
            labels: None,
            buildings: None,
            pois: None,
            streamed: false,
        }
    }

    /// The cell of central Florence at z13 as a region.
    fn florence() -> (Header, World) {
        let at = project(LatLon::from_degrees(43.7756, 11.2600));
        let cell = Cell::of(at, 13);
        (region(cell.bounds(), (10, 15)), at)
    }

    #[test]
    fn a_held_view_wants_nothing() {
        let (r, at) = florence();
        let view = Viewport::new(Cell::of(at, 13).centre(), 16, 480, 320);
        let w = wants(&view, &[r], false, None);
        assert_eq!(w.share, 0.0);
        assert!(w.here.is_empty() && w.ahead.is_empty(), "{w:?}");
    }

    /// 1707: a region cut under the old profile 1 no longer holds the
    /// screen — its deepest zoom was simplified, which overzoom shows —
    /// so its box is asked for again and an autoloaded region is
    /// rewritten under its own name.
    #[test]
    fn an_old_profile_region_no_longer_holds_the_screen() {
        let (mut r, at) = florence();
        let view = Viewport::new(Cell::of(at, 13).centre(), 16, 480, 320);
        assert_eq!(uncovered(&view, &[r]), 0.0);
        r.profile = leviculum_map::profile::CUT_1;
        assert_eq!(uncovered(&view, &[r]), 1.0);
        // 1727: the region's own box and zooms, nothing else over it.
        let w = wants(&view, &[r], false, None);
        let asked: Vec<(Bounds, (u8, u8), u8)> = w
            .here
            .iter()
            .map(|w| (w.bounds, w.zooms, w.floor))
            .collect();
        assert_eq!(asked, [(r.bounds, (10, 15), 15)]);
    }

    #[test]
    fn a_view_past_the_region_wants_the_cell_under_the_gap() {
        let (r, at) = florence();
        let mut view = Viewport::new(Cell::of(at, 13).centre(), 16, 480, 320);
        // Four screens east: nothing held.
        view.pan(4 * 480, 0);
        let w = wants(&view, &[r], false, None);
        assert_eq!(w.share, 1.0);
        assert!(!w.here.is_empty());
        let first = w.here[0].bounds;
        let c = unproject(view.centre);
        assert!(
            (first.west..=first.east).contains(&c.lon_e7)
                && (first.south..=first.north).contains(&c.lat_e7),
            "the cell under the centre first: {first:?}"
        );
        // The screen itself first (1527): its box around the screen, at
        // the view's load zoom and the one above; then the cells.
        let screen = &w.here[0];
        assert_eq!(screen.reason, LoadReason::Screen);
        assert_eq!((screen.zooms, screen.floor), ((14, 15), 15));
        let vb = view.bounds();
        assert!(
            screen.bounds.holds(&vb),
            "the screen box holds the screen: {:?} {vb:?}",
            screen.bounds
        );
        for want in &w.here[1..] {
            assert_eq!(want.reason, LoadReason::View);
            assert_eq!(want.zooms, (12, 15));
            assert_eq!(want.floor, 15);
        }
    }

    /// The screen's box is the screen, a half tile past it, rounded out
    /// to the tiles of the zoom it loads (1527): far smaller than the cell
    /// it lies in, the same box for a pan inside a tile, and only the
    /// uncovered part of a half-held screen.
    #[test]
    fn the_screens_box_is_the_screen_at_its_zoom_not_the_cell() {
        let (r, at) = florence();
        let mut view = Viewport::new(Cell::of(at, 13).centre(), 16, 480, 320);
        view.pan(4 * 480, 0);
        let screen = screen_want(&view, &[r]).expect("uncovered");
        // z15 tiles: the screen is 240 x 160 of them in pixels, so with
        // a half tile each side and rounding out, at most 3 x 3 tiles of
        // the cell's 4 x 4 at z15 (a z13 cell is 4 z15 tiles across).
        let tile = |lon: i32| x_of(lon) >> (32 - 15);
        let across = tile(screen.bounds.east - 1) - tile(screen.bounds.west) + 1;
        assert!(across <= 3, "{across} tiles across");
        let cell = Cell::of(view.centre, 13).bounds();
        let area = |b: &Bounds| {
            (f64::from(b.east) - f64::from(b.west)) * (f64::from(b.north) - f64::from(b.south))
        };
        assert!(
            area(&screen.bounds) < area(&cell) / 1.7,
            "smaller than its cell"
        );
        // Panned 20 px: the same box asked again.
        let mut moved = view;
        moved.pan(20, 0);
        assert_eq!(
            screen_want(&moved, &[r]).map(|w| w.bounds),
            Some(screen.bounds)
        );
        // Half the screen held: the box covers the uncovered half only.
        let cell = Cell::of(at, 13);
        let ((_, x1), _) = cell.span();
        let unit = 1u64 << (32 - 16 - 8);
        // The phone's screen (1527x2099): half of it is more than a tile.
        let half = Viewport::new(
            World {
                x: x1 as u32,
                y: cell.centre().y,
            },
            16,
            1527,
            2099,
        );
        let part = screen_want(&half, &[r]).expect("half uncovered");
        let whole = screen_want(&half, &[]).expect("nothing held");
        assert_eq!(part.bounds.east, whole.bounds.east);
        assert!(
            part.bounds.west > whole.bounds.west,
            "the held half is not asked again: {:?} {:?}",
            part.bounds,
            whole.bounds
        );
        // Its west edge within a tile and a half of the held edge.
        assert!(u64::from(x_of(part.bounds.west)) + 3 * 256 * unit >= x1);
        // Held: nothing.
        let held = Viewport::new(Cell::of(at, 13).centre(), 16, 480, 320);
        assert_eq!(screen_want(&held, &[r]), None);
    }

    /// A region still arriving holds nothing for the scheduler (1527): the
    /// screen under it stays asked for, and its landing decides again.
    #[test]
    fn a_region_still_arriving_holds_nothing_yet() {
        let (r, at) = florence();
        let arriving = Header {
            streamed: true,
            len: 0,
            ..r
        };
        let view = Viewport::new(Cell::of(at, 13).centre(), 16, 480, 320);
        assert_eq!(uncovered(&view, &[arriving]), 1.0);
        let mut loads = ViewLoads::new();
        assert!(loads.decide(&view, &[arriving], 0, false, None).is_some());
        assert!(loads.decide(&view, &[arriving], 10, false, None).is_none());
        let whole = Header {
            len: 4096,
            ..arriving
        };
        let landed = loads.decide(&view, &[whole], 20, false, None);
        assert!(
            landed.is_some_and(|w| w.share == 0.0),
            "landed: decided again"
        );
    }

    #[test]
    fn a_small_gap_waits() {
        let (r, at) = florence();
        let cell = Cell::of(at, 13);
        let ((_, x1), _) = cell.span();
        // The view's east edge 5 % of the screen past the cell's.
        let w_px = 480u32;
        // World units a pixel at z16: 2^32 over 256 · 2^16 pixels.
        let unit = 1u64 << (32 - 16 - 8);
        let centre_x = x1 - u64::from(w_px / 2 - w_px / 20) * unit;
        let view = Viewport::new(
            World {
                x: centre_x as u32,
                y: cell.centre().y,
            },
            16,
            w_px,
            320,
        );
        let w = wants(&view, &[r], false, None);
        assert!(w.share > 0.0 && w.share <= GAP_SHARE, "{}", w.share);
        assert!(w.here.is_empty());
        assert!(!w.ahead.is_empty(), "the cell east is next");
    }

    #[test]
    fn ahead_goes_the_way_of_the_pan_and_moving_only_that_way() {
        let (r, at) = florence();
        let cell = Cell::of(at, 13);
        let ((_, x1), (y0, _)) = cell.span();
        // World units a pixel at z16: 2^32 over 256 · 2^16 pixels.
        let unit = 1u64 << (32 - 16 - 8);
        // Held, 400 pixels from the cell's east edge and 300 from its
        // north: the cells east, north-east and north are next.
        let view = Viewport::new(
            World {
                x: (x1 - 400 * unit) as u32,
                y: (y0 + 300 * unit) as u32,
            },
            16,
            480,
            320,
        );
        let east = (1000.0, 0.0);
        let w = wants(&view, &[r], false, Some(east));
        assert_eq!(w.share, 0.0);
        assert!(w.here.is_empty());
        let cells: Vec<Cell> = w.ahead.iter().map(|m| Cell::of(m.centre, 13)).collect();
        let (e, ne, n) = (
            Cell {
                x: cell.x + 1,
                ..cell
            },
            Cell {
                x: cell.x + 1,
                y: cell.y - 1,
                ..cell
            },
            Cell {
                y: cell.y - 1,
                ..cell
            },
        );
        assert_eq!(cells, [e, ne, n], "east first, the nearer of a tie first");
        let moving = wants(&view, &[r], true, Some(east));
        let cells: Vec<Cell> = moving
            .ahead
            .iter()
            .map(|m| Cell::of(m.centre, 13))
            .collect();
        assert_eq!(cells, [e, ne], "moving: only ahead");
        assert!(wants(&view, &[r], true, None).ahead.is_empty());
        let north = wants(&view, &[r], false, Some((0.0, -1000.0)));
        assert_eq!(Cell::of(north.ahead[0].centre, 13), n);
    }

    #[test]
    fn below_the_least_zoom_nothing_is_asked() {
        let view = Viewport::new(project(LatLon::from_degrees(43.0, 11.0)), 9, 480, 320);
        let w = wants(&view, &[], false, None);
        assert_eq!(w.share, 1.0);
        assert!(w.here.is_empty() && w.ahead.is_empty());
    }

    /// One decision every 250 ms while moving, counted from the last one,
    /// not restarted by the frames between; every new frame at rest
    /// decides; the same view again decides nothing.
    #[test]
    fn decisions_are_paced_and_not_restarted_by_motion() {
        let mut loads = ViewLoads::new();
        let mut view = Viewport::new(project(LatLon::from_degrees(43.0, 11.0)), 16, 480, 320);
        let mut decided = Vec::new();
        for i in 0..40u64 {
            view.pan(10, 0);
            if loads.decide(&view, &[], i * 16, true, None).is_some() {
                decided.push(i * 16);
            }
        }
        assert_eq!(decided, [0, 256, 512]);
        view.pan(10, 0);
        assert!(loads.decide(&view, &[], 640, false, None).is_some(), "rest");
        assert!(loads.decide(&view, &[], 700, false, None).is_none(), "same");
        let (r, _) = florence();
        assert!(
            loads.decide(&view, &[r], 710, false, None).is_some(),
            "a region landed"
        );
    }

    #[test]
    fn a_cells_box_is_its_tile() {
        let cell = Cell { z: 1, x: 1, y: 0 };
        let b = cell.bounds();
        assert_eq!(b.west, 0);
        assert!(b.east >= 1_799_999_990, "{b:?}");
        assert!(b.north > 850_000_000 && b.south.abs() < 10);
        let last = Cell {
            z: 13,
            x: 8191,
            y: 8191,
        };
        assert!(last.bounds().east >= 1_799_000_000);
    }
}
