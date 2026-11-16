//! The viewport: a centre, an integer zoom and a size in pixels, and what
//! follows from them: where a world point lands, which tiles a view needs
//! and where each one is placed.
//!
//! Screen positions are sub-pixels (1/16 pixel, `leviculum-mapfill`'s
//! unit) from the view's top-left corner. A world point is quantised to the
//! zoom's sub-pixel grid before it is placed, the same way for a pin and
//! for a tile corner, so a pin and the map under it cannot drift apart by
//! rounding. The view centre sits at sub-pixel `(width * 8, height * 8)`.
//!
//! x wraps: a point is placed at its copy nearest the centre, and the tiles
//! of a view that crosses the antimeridian are taken modulo the zoom's
//! width. y does not wrap; rows outside the projection are not selected.

use alloc::vec::Vec;
use core::ops::RangeInclusive;

use embedded_graphics::prelude::{Point, Size};
use embedded_graphics::primitives::Rectangle;

use crate::mercator::{unproject, x_of_lon, y_of_lat, World};
use crate::region::{Bounds, TileId};

/// The deepest view zoom: a sub-pixel is one world unit there.
pub const MAX_VIEW_ZOOM: u8 = 20;
/// How many zooms past a region's deepest the view may go and still draw
/// it, each doubling the scale. Eight keeps every placed vertex inside the
/// filler's coordinate limit (`region::check_tile` bounds the vertices).
pub const MAX_OVERZOOM: u8 = 8;

const SUB: i64 = leviculum_mapfill::SUBPIXEL as i64;

/// A view of the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    pub centre: World,
    zoom: u8,
    width: u32,
    height: u32,
}

/// One tile a view needs, and where it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewTile {
    pub id: TileId,
    /// View zoom minus tile zoom: extra left shift on top of the tile's own.
    pub overzoom: u8,
    /// The tile's top-left corner in sub-pixels from the view's.
    pub origin: Point,
}

impl ViewTile {
    /// The tile `dz` zooms coarser that holds this one, placed so that its
    /// part over this tile lands where this tile would; `None` past
    /// [`MAX_OVERZOOM`]. What stands in for a tile not arrived yet
    /// (`Region::stand_in`, 1291).
    pub fn up(&self, dz: u8) -> Option<Self> {
        let overzoom = self.overzoom.checked_add(dz)?;
        if overzoom > MAX_OVERZOOM || dz > self.id.z {
            return None;
        }
        let size = 1i32 << (12 + u32::from(self.overzoom));
        let id = TileId {
            z: self.id.z - dz,
            x: self.id.x >> dz,
            y: self.id.y >> dz,
        };
        let (ix, iy) = (self.id.x - (id.x << dz), self.id.y - (id.y << dz));
        Some(Self {
            id,
            overzoom,
            origin: Point::new(
                self.origin.x - ix as i32 * size,
                self.origin.y - iy as i32 * size,
            ),
        })
    }
}

impl Viewport {
    /// A view of `width` x `height` pixels (each at least 1) at `zoom`
    /// (clamped to [`MAX_VIEW_ZOOM`]).
    pub fn new(centre: World, zoom: u8, width: u32, height: u32) -> Self {
        Self {
            centre,
            zoom: zoom.min(MAX_VIEW_ZOOM),
            width: width.max(1),
            height: height.max(1),
        }
    }

    pub fn zoom(&self) -> u8 {
        self.zoom
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// A different zoom about the same centre.
    pub fn set_zoom(&mut self, zoom: u8) {
        self.zoom = zoom.min(MAX_VIEW_ZOOM);
    }

    /// A different size about the same centre, for a resized pane.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
    }

    /// World units per sub-pixel, as a shift.
    fn sub_shift(&self) -> u32 {
        u32::from(MAX_VIEW_ZOOM - self.zoom)
    }

    /// The world's width in sub-pixels at this zoom.
    fn world_sub(&self) -> i64 {
        1i64 << (12 + u32::from(self.zoom))
    }

    /// The centre on this zoom's sub-pixel grid.
    fn centre_sub(&self) -> (i64, i64) {
        let s = self.sub_shift();
        (i64::from(self.centre.x >> s), i64::from(self.centre.y >> s))
    }

    /// The view's top-left corner in sub-pixels of the world at this zoom,
    /// x not wrapped: it may be negative or past the world's width, which
    /// is `1 << (12 + zoom)`. A drawer that places whole copies of the
    /// world (the outline) starts here.
    pub fn top_left_sub(&self) -> (i64, i64) {
        let (cx, cy) = self.centre_sub();
        (
            cx - i64::from(self.width) * SUB / 2,
            cy - i64::from(self.height) * SUB / 2,
        )
    }

    /// Where `w` lands, in sub-pixels from the view's top-left corner; x
    /// is the copy of `w` nearest the centre.
    pub fn to_screen_sub(&self, w: World) -> (i64, i64) {
        let s = self.sub_shift();
        let (cx, cy) = self.centre_sub();
        let world = self.world_sub();
        let dx = (i64::from(w.x >> s) - cx + world / 2).rem_euclid(world) - world / 2;
        let dy = i64::from(w.y >> s) - cy;
        (
            i64::from(self.width) * SUB / 2 + dx,
            i64::from(self.height) * SUB / 2 + dy,
        )
    }

    /// The pixel `w` lands in, which may be outside the view.
    pub fn to_screen(&self, w: World) -> (i64, i64) {
        let (x, y) = self.to_screen_sub(w);
        (x.div_euclid(SUB), y.div_euclid(SUB))
    }

    /// Whether `w` lands on a pixel of the view: left and top edges in,
    /// right and bottom edges out.
    pub fn contains(&self, w: World) -> bool {
        let (x, y) = self.to_screen(w);
        (0..i64::from(self.width)).contains(&x) && (0..i64::from(self.height)).contains(&y)
    }

    /// The world point at the top-left corner of pixel `(x, y)` of the
    /// view, for a click.
    pub fn to_world(&self, x: i32, y: i32) -> World {
        let s = self.sub_shift();
        let dx = (i64::from(x) * SUB - i64::from(self.width) * SUB / 2) << s;
        let dy = (i64::from(y) * SUB - i64::from(self.height) * SUB / 2) << s;
        World {
            x: (i64::from(self.centre.x) + dx).rem_euclid(1 << 32) as u32,
            y: (i64::from(self.centre.y) + dy).clamp(0, i64::from(u32::MAX)) as u32,
        }
    }

    /// The view's box in degrees, from its top-left corner to its
    /// bottom-right; the whole world's longitudes when the view is as wide
    /// as the world or wider. What a load of the visible area asks for
    /// (loquax, 1089).
    pub fn bounds(&self) -> Bounds {
        let nw = unproject(self.to_world(0, 0));
        let (w, h) = (self.width as i32, self.height as i32);
        let se = unproject(self.to_world(w, h));
        let wide = (i64::from(self.width) * SUB) << self.sub_shift() >= 1i64 << 32;
        let (west, east) = match wide {
            true => (-1_800_000_000, 1_800_000_000),
            false => (nw.lon_e7, se.lon_e7),
        };
        Bounds {
            west,
            south: se.lat_e7,
            east,
            north: nw.lat_e7,
        }
    }

    /// A different zoom about pixel `(x, y)` of the view: the world point
    /// at that pixel's top-left corner stays there, as a wheel under the
    /// pointer expects. y stops at the edges as [`pan`](Self::pan) does.
    pub fn zoom_about(&mut self, zoom: u8, x: i32, y: i32) {
        let held = self.to_world(x, y);
        self.set_zoom(zoom);
        let s = self.sub_shift();
        let dx = (i64::from(x) * SUB - i64::from(self.width) * SUB / 2) << s;
        let dy = (i64::from(y) * SUB - i64::from(self.height) * SUB / 2) << s;
        self.centre = World {
            x: (i64::from(held.x) - dx).rem_euclid(1 << 32) as u32,
            y: (i64::from(held.y) - dy).clamp(0, i64::from(u32::MAX)) as u32,
        };
    }

    /// The shallowest zoom at which the world, `256 << zoom` pixels
    /// across, is at least `width` wide: no view at that zoom or deeper
    /// shows a second copy of the world beside the first (Lew 2026-10-29,
    /// 1131). Height does not count: a world shorter than the view is
    /// centred in it ([`one_world`](Self::one_world)).
    pub fn min_zoom(width: u32) -> u8 {
        let mut zoom = 0;
        while zoom < MAX_VIEW_ZOOM && (256u64 << zoom) < u64::from(width) {
            zoom += 1;
        }
        zoom
    }

    /// The view as every frontend shows it: one world at most. The zoom is
    /// at least [`min_zoom`](Self::min_zoom) of the width, so x shows the
    /// world once and wraps only by moving it; y keeps the view inside
    /// the projection (Mercator's 85.05 degrees north and south), and a
    /// world shorter than the view sits in its middle.
    pub fn one_world(mut self) -> Self {
        self.set_zoom(self.zoom.max(Self::min_zoom(self.width)));
        let half = (i64::from(self.height) * SUB / 2) << self.sub_shift();
        let full = 1i64 << 32;
        self.centre.y = match 2 * half >= full {
            true => 1 << 31,
            false => i64::from(self.centre.y).clamp(half, full - half) as u32,
        };
        self
    }

    /// Move the centre by whole pixels; x wraps, y stops at the edges.
    pub fn pan(&mut self, dx: i32, dy: i32) {
        let px = 24 - u32::from(self.zoom);
        let (dx, dy) = (i64::from(dx) << px, i64::from(dy) << px);
        self.centre = World {
            x: (i64::from(self.centre.x) + dx).rem_euclid(1 << 32) as u32,
            y: (i64::from(self.centre.y) + dy).clamp(0, i64::from(u32::MAX)) as u32,
        };
    }

    /// The pixels of the view that the box `b` covers: those whose centre
    /// lies inside it, the filler's rule, so the edge is within half a
    /// pixel of the box at every zoom. Empty when the box is out of view.
    /// A region is drawn into this and no further (1077): its tiles cover
    /// the box rounded out to each zoom's tile grid, wider and lopsided at
    /// the shallow zooms, and drawn whole they moved the edge on zoom.
    ///
    /// x is the copy of the box that holds the view's centre, or the one
    /// nearest it; a view wider than the world shows that copy only.
    pub fn area_of(&self, b: &Bounds) -> Rectangle {
        let s = self.sub_shift();
        let world = self.world_sub();
        let (xw, xe) = (x_of_lon(b.west), x_of_lon(b.east));
        let span_e7 = (i64::from(b.east) - i64::from(b.west)).rem_euclid(3_600_000_000);
        let span = match b.west < b.east && span_e7 == 0 {
            // West 180 W and east 180 E: the whole world.
            true => world,
            false => (i64::from(xe >> s) - i64::from(xw >> s)).rem_euclid(world),
        };
        let (mut west, north) = self.to_screen_sub(World {
            x: xw,
            y: y_of_lat(b.north),
        });
        let (_, south) = self.to_screen_sub(World {
            x: xw,
            y: y_of_lat(b.south),
        });
        let mid = i64::from(self.width) * SUB / 2;
        if west > mid && west - world + span > mid {
            west -= world;
        }
        // The pixels whose centre, at sub-pixel 16 n + 8, is in [a, b).
        let pixels = |a: i64, b: i64, of: u32| {
            let first = |v: i64| (v - SUB / 2 + SUB - 1).div_euclid(SUB);
            let max = i64::from(of);
            let (p0, p1) = (first(a).clamp(0, max), first(b).clamp(0, max));
            (p0 as i32, (p1 - p0).max(0) as u32)
        };
        let (x, w) = pixels(west, west + span, self.width);
        let (y, h) = pixels(north, south, self.height);
        Rectangle::new(Point::new(x, y), Size::new(w, h))
    }

    /// The view that shows every point in `points` with `margin` pixels to
    /// spare on each side, at the deepest zoom in `zooms` that does; the
    /// shallowest when none does. `None` for no points.
    ///
    /// The box around the points is the narrowest one in x, which for
    /// points on both sides of the antimeridian is the one across it.
    pub fn fit(
        points: &[World],
        width: u32,
        height: u32,
        margin: u32,
        zooms: RangeInclusive<u8>,
    ) -> Option<Self> {
        let first = points.first()?;
        let mut xs: Vec<u32> = points.iter().map(|p| p.x).collect();
        xs.sort_unstable();
        // The widest gap between neighbours, the wrap-around one included,
        // is where the box does not go.
        let n = xs.len();
        let (mut gap, mut after) = (0u64, 0usize);
        for i in 0..n {
            let next = xs[(i + 1) % n];
            let g = if i + 1 == n {
                (1u64 << 32) - u64::from(xs[i]) + u64::from(next)
            } else {
                u64::from(next) - u64::from(xs[i])
            };
            if g > gap {
                gap = g;
                after = (i + 1) % n;
            }
        }
        let span_x = (1u64 << 32) - gap;
        let west = xs[after];
        let (mut north, mut south) = (first.y, first.y);
        for p in points {
            north = north.min(p.y);
            south = south.max(p.y);
        }
        let span_y = u64::from(south - north);
        let centre = World {
            x: west.wrapping_add((span_x / 2) as u32),
            y: north + (span_y / 2) as u32,
        };
        let room_w = u64::from(width.saturating_sub(2 * margin).max(1));
        let room_h = u64::from(height.saturating_sub(2 * margin).max(1));
        let (lo, hi) = (*zooms.start(), (*zooms.end()).min(MAX_VIEW_ZOOM));
        let mut zoom = lo;
        for z in (lo..=hi).rev() {
            // Pixels the box spans, plus one for the pixel the far edge
            // falls in, must fit in the room.
            let px = 24 - u32::from(z);
            if (span_x >> px) + 2 <= room_w && (span_y >> px) + 2 <= room_h {
                zoom = z;
                break;
            }
        }
        Some(Self::new(centre, zoom, width, height))
    }

    /// The tiles of a region holding `zooms` that this view needs, each
    /// placed. The tile zoom is the view's, clamped to the region's
    /// deepest; a view shallower than the region's shallowest, or more
    /// than [`MAX_OVERZOOM`] past its deepest, needs none. Rows outside
    /// the projection are skipped; columns wrap.
    pub fn tiles(&self, zooms: RangeInclusive<u8>) -> Vec<ViewTile> {
        let mut out = Vec::new();
        let (lo, hi) = (*zooms.start(), *zooms.end());
        if self.zoom < lo || lo > hi {
            return out;
        }
        let tz = self.zoom.min(hi);
        let overzoom = self.zoom - tz;
        if overzoom > MAX_OVERZOOM {
            return out;
        }
        let size = 1i64 << (12 + u32::from(overzoom));
        let (cx, cy) = self.centre_sub();
        let left = cx - i64::from(self.width) * SUB / 2;
        let top = cy - i64::from(self.height) * SUB / 2;
        let right = left + i64::from(self.width) * SUB;
        let bottom = top + i64::from(self.height) * SUB;
        let grid = 1i64 << tz;
        let (tx0, tx1) = (left.div_euclid(size), (right - 1).div_euclid(size));
        let ty0 = top.div_euclid(size).max(0);
        let ty1 = (bottom - 1).div_euclid(size).min(grid - 1);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                out.push(ViewTile {
                    id: TileId {
                        z: tz,
                        x: tx.rem_euclid(grid) as u32,
                        y: ty as u32,
                    },
                    overzoom,
                    origin: Point::new((tx * size - left) as i32, (ty * size - top) as i32),
                });
            }
        }
        out
    }
}

/// A view at a zoom between the steps (1134): the zoom a pinch left, in
/// thousandths, so it stays where the fingers left it. Its data comes from
/// one integer zoom ([`tile_zoom`](Self::tile_zoom), the step nearest),
/// drawn at that step and scaled by [`scale_at`](Self::scale_at) on the
/// way to the screen; names and pins are placed at the exact scale.
///
/// Positions are pixels as `f64` from the view's top left corner, a
/// pixel's centre at `n + 0.5`. At a whole zoom the view is a [`Viewport`]'s
/// ([`whole`](Self::whole)), and every frontend that keeps integer steps
/// keeps using that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactView {
    pub centre: World,
    zoom_milli: u32,
    width: u32,
    height: u32,
}

/// Thousandths in a zoom step.
pub const MILLI: u32 = 1000;

impl ExactView {
    /// A view of `width` x `height` pixels (each at least 1) at
    /// `zoom_milli` thousandths of a zoom step, clamped to
    /// [`MAX_VIEW_ZOOM`].
    pub fn new(centre: World, zoom_milli: u32, width: u32, height: u32) -> Self {
        Self {
            centre,
            zoom_milli: zoom_milli.min(u32::from(MAX_VIEW_ZOOM) * MILLI),
            width: width.max(1),
            height: height.max(1),
        }
    }

    /// The zoom in thousandths.
    pub fn zoom_milli(&self) -> u32 {
        self.zoom_milli
    }

    /// The zoom as a real number.
    pub fn zoom(&self) -> f64 {
        f64::from(self.zoom_milli) / f64::from(MILLI)
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// A different size about the same centre.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
    }

    /// The step nearest the zoom, a half rounding up: the data's zoom. The
    /// nearest rather than the one below keeps the scale between 0.71 and
    /// 1.41, so a line is never stretched to twice its drawn width.
    pub fn tile_zoom(&self) -> u8 {
        let z = (self.zoom_milli + MILLI / 2) / MILLI;
        u8::try_from(z).unwrap_or(MAX_VIEW_ZOOM).min(MAX_VIEW_ZOOM)
    }

    /// How many screen pixels one pixel of zoom `z` is: `2^(zoom - z)`.
    pub fn scale_at(&self, z: u8) -> f64 {
        libm::exp2(self.zoom() - f64::from(z))
    }

    /// World units a screen pixel spans.
    pub fn unit(&self) -> f64 {
        libm::exp2(24.0 - self.zoom())
    }

    /// Where `w` lands in pixels from the top left corner; x is the copy
    /// nearest the centre.
    pub fn to_screen(&self, w: World) -> (f64, f64) {
        let dx = i64::from(w.x.wrapping_sub(self.centre.x) as i32);
        let dy = i64::from(w.y) - i64::from(self.centre.y);
        let u = self.unit();
        (
            f64::from(self.width) / 2.0 + dx as f64 / u,
            f64::from(self.height) / 2.0 + dy as f64 / u,
        )
    }

    /// The world point at `(x, y)` pixels from the top left corner; x
    /// wraps, y stops at the projection's edges.
    pub fn to_world(&self, x: f64, y: f64) -> World {
        let u = self.unit();
        let dx = libm::round((x - f64::from(self.width) / 2.0) * u) as i64;
        let dy = libm::round((y - f64::from(self.height) / 2.0) * u) as i64;
        World {
            x: (i64::from(self.centre.x) + dx).rem_euclid(1 << 32) as u32,
            y: (i64::from(self.centre.y) + dy).clamp(0, i64::from(u32::MAX)) as u32,
        }
    }

    /// Move the centre by whole pixels; x wraps, y stops at the edges.
    pub fn pan(&mut self, dx: i32, dy: i32) {
        let c = self.to_world(
            f64::from(self.width) / 2.0 + f64::from(dx),
            f64::from(self.height) / 2.0 + f64::from(dy),
        );
        self.centre = c;
    }

    /// A different zoom about the point `(x, y)`: the world point there
    /// stays there, as under a pinch's fingers.
    pub fn zoom_about(&mut self, zoom_milli: u32, x: f64, y: f64) {
        let held = self.to_world(x, y);
        self.zoom_milli = zoom_milli.min(u32::from(MAX_VIEW_ZOOM) * MILLI);
        let u = self.unit();
        let dx = libm::round((x - f64::from(self.width) / 2.0) * u) as i64;
        let dy = libm::round((y - f64::from(self.height) / 2.0) * u) as i64;
        self.centre = World {
            x: (i64::from(held.x) - dx).rem_euclid(1 << 32) as u32,
            y: (i64::from(held.y) - dy).clamp(0, i64::from(u32::MAX)) as u32,
        };
    }

    /// The view as every frontend shows it, [`Viewport::one_world`]'s rule
    /// at the exact zoom: no shallower than the width's whole
    /// [`Viewport::min_zoom`], y inside the projection.
    pub fn one_world(mut self) -> Self {
        let floor = u32::from(Viewport::min_zoom(self.width)) * MILLI;
        self.zoom_milli = self.zoom_milli.max(floor);
        let half = f64::from(self.height) / 2.0 * self.unit();
        let full = (1u64 << 32) as f64;
        self.centre.y = match 2.0 * half >= full {
            true => 1 << 31,
            false => (f64::from(self.centre.y)).clamp(half, full - half) as u32,
        };
        self
    }

    /// At a whole zoom, the view moved to the nearest whole pixel of it,
    /// where a [`Viewport`] of that zoom draws the same pixels however it
    /// is cut; between the steps, the view.
    pub fn on_whole_pixel(mut self) -> Self {
        if !self.zoom_milli.is_multiple_of(MILLI) {
            return self;
        }
        let shift = 24 - (self.zoom_milli / MILLI).min(24);
        let half = (1u64 << shift) >> 1;
        let snap = |v: u32| (((u64::from(v) + half) >> shift) << shift) as u32;
        let last = (u32::MAX >> shift) << shift;
        self.centre = World {
            x: snap(self.centre.x),
            y: if self.centre.y > last {
                last
            } else {
                snap(self.centre.y)
            },
        };
        self
    }

    /// The view at the whole zoom, when it is one.
    pub fn whole(&self) -> Option<Viewport> {
        self.zoom_milli.is_multiple_of(MILLI).then(|| {
            Viewport::new(
                self.centre,
                (self.zoom_milli / MILLI) as u8,
                self.width,
                self.height,
            )
        })
    }

    /// An integer view at zoom `z` over at least this view's area, the
    /// same centre, a pixel more on each side unless `z` is the view's own
    /// zoom, where it is the view: what the data of a step is looked up in
    /// (the names a view holds, the region it loads).
    pub fn at_zoom(&self, z: u8) -> Viewport {
        if self.zoom_milli == u32::from(z) * MILLI {
            return Viewport::new(self.centre, z, self.width, self.height);
        }
        let s = self.scale_at(z);
        let side = |v: u32| (libm::ceil(f64::from(v) / s) as u32).saturating_add(2);
        Viewport::new(self.centre, z, side(self.width), side(self.height))
    }
}

impl From<Viewport> for ExactView {
    fn from(v: Viewport) -> Self {
        let (w, h) = v.size();
        Self::new(v.centre, u32::from(v.zoom()) * MILLI, w, h)
    }
}
