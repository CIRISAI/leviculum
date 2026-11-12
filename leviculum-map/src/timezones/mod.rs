//! The world's time zones: their outlines, which zone a point lies in,
//! where a view writes each zone's time, and the clock rules (order 1684).
//!
//! # Outlines
//!
//! timezone-boundary-builder's release 2026f, the "same since now" variant
//! with the oceans: 63 zones, each the zones that keep the same time since
//! the release merged under the IANA name of the most populous (Berlin
//! lies in `Europe/Paris`, the open sea in the nautical zone it keeps, or
//! in the land zone that keeps the same time). OpenStreetMap data under the
//! ODbL 1.0, so a view that draws them shows [`ATTRIBUTION`]. Baked by
//! `scripts/bake-timezones.py`, which cleans the source into a gapless
//! coverage and simplifies the shared borders once each (GEOS coverage
//! simplification, tolerance 2 grid units), into three tables at the world
//! outline's 18-bit grid:
//!
//! - [`RINGS`] (`LWO2`, read by [`outline::rings`]): every polygon of
//!   every zone, the zones in name order, each outer ring followed by its
//!   holes; 3,004 rings, 1,334 of them holes, 598,780 points, 1,007,112
//!   bytes.
//! - [`BORDERS`] (`LWL2`, [`outline::lines`]): the borders between zones,
//!   each once, and the stretches of the antimeridian where the zones on
//!   either side differ; 2,909 lines, 302,268 points, 503,524 bytes. A
//!   frontend strokes them with [`outline::bounds`] and [`BOUNDARIES`].
//! - [`ZONES`] (`LWZ1`): the zone names, index to IANA name, and per ring
//!   its zone and, for an outer ring big enough to hold one, its label's
//!   anchor (the polygon's pole of inaccessibility) and the shallowest
//!   zoom whose circle around it is 20 pixels wide; 9,515 bytes.
//!
//! [`zone_at`] answers the zone of a point without allocating: a ring's
//! box first, then even-odd over the rings of each zone. [`labels`] gives
//! a view the anchors it shows as [`ViewLabel`]s of kind
//! [`LabelKind::TimeZone`], the IANA name as their text, for the frontend
//! to replace with the zone's time; [`zone_rings`] and [`zones_in_view`]
//! give it the rings to shade.
//!
//! # Clocks
//!
//! On Linux and Android the rules come from the system's tzdata:
//! `system_zone_bytes` (feature `std`, in `system`) reads a zone's TZif file, from
//! `/usr/share/zoneinfo` or from Android's packed file ([`packed`]), and
//! [`tzif::offset_at`] reads the offset at an instant from it. A board has
//! no file system and keeps [`RULES`] instead, the POSIX TZ string of
//! every zone above as the build host's tzdata gives it in its TZif
//! footer (`scripts/gen-tz-table.py`), which [`board_offset`] evaluates
//! ([`posix`]).
//!
//! The board's known limit: a footer only describes the rules after a
//! zone's last explicit transition. A zone whose current rules a POSIX
//! string cannot express is right on the board only outside the
//! transitions its TZif file lists one by one: `Africa/Casablanca`'s
//! Ramadan changes were such (Morocco keeps +00 for good since
//! 2026-09-20, so the outline merges it into `Africa/Abidjan`), and so is
//! a one-off exception. With tzdata 2026f, the table's, the one zone of
//! the outline affected is `America/Havana`: Cuba stays on -04 from
//! 2026-11-01 through 2027-11-07, while the board keeps its lasting rule
//! and shows -05 in that time. `tests/timezones.rs` holds the board to the
//! TZif files of the same tzdata every hour of 2026 and finds exactly
//! that.

pub mod packed;
pub mod posix;
#[cfg(feature = "std")]
pub mod system;
pub mod tzif;

use alloc::string::String;
use alloc::vec::Vec;

pub use packed::{packed_version, packed_zone};
pub use posix::{Offset, PosixTz};
#[cfg(feature = "std")]
pub use system::system_zone_bytes;

use crate::blend::Blend;
use crate::labels::{LabelKind, ViewLabel, SUBPIXEL};
use crate::mercator::{project, LatLon};
use crate::outline::{self, varint, Place, Ring};
use crate::viewport::Viewport;

/// The zones' polygons, `LWO2`.
pub const RINGS: &[u8] = include_bytes!("../../data/timezones.lwo");

/// The borders between zones, each once, `LWL2`.
pub const BORDERS: &[u8] = include_bytes!("../../data/timezones.lwl");

/// The zone names and what each ring of [`RINGS`] belongs to, `LWZ1`.
pub const ZONES: &[u8] = include_bytes!("../../data/timezones.lwz");

/// The board's clock rules: two comment lines, the second
/// `# tzdata VERSION`, then `NAME POSIX-TZ` a line, a zone of [`ZONES`]
/// each.
pub const RULES: &str = include_str!("../../data/tz-rules.txt");

/// The attribution line a view that draws the zones carries: the data is
/// OpenStreetMap's under the ODbL 1.0, through timezone-boundary-builder.
pub const ATTRIBUTION: &str =
    "Time zones: © OpenStreetMap contributors, timezone-boundary-builder (ODbL)";

/// The borders alone, for [`outline::bounds`].
pub const BOUNDARIES: outline::World<'static> = outline::World {
    land: &[],
    lakes: &[],
    rivers: &[],
    ice: &[],
    shelves: &[],
    borders: BORDERS,
    disputed: &[],
};

/// The magic of a zone table.
const MAGIC: &[u8; 4] = b"LWZ1";
/// A ring without a label.
const NO_LABEL: u8 = 255;

/// What the zone table says of one ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingZone {
    /// The zone's index, [`zone_name`].
    pub zone: u16,
    /// The label's anchor in table units and the shallowest zoom that
    /// shows it; `None` for a hole or a polygon too small for one.
    pub label: Option<((u32, u32), u8)>,
}

/// The bits a coordinate has, and where the names start.
fn head(table: &[u8]) -> Option<(u32, usize)> {
    let h = table.get(..5)?;
    (h[..4] == *MAGIC && (8..=24).contains(&h[4])).then_some((u32::from(h[4]), 5))
}

/// The zone names of a zone table, in index order, and where the ring
/// records start.
fn names(table: &[u8]) -> Option<(impl Iterator<Item = &str>, usize)> {
    let (_, mut at) = head(table)?;
    let count = varint(table, &mut at)?;
    let start = at;
    for _ in 0..count {
        let len = varint(table, &mut at)? as usize;
        core::str::from_utf8(table.get(at..at + len)?).ok()?;
        at += len;
    }
    let end = at;
    let mut at = start;
    let iter = (0..count).map_while(move |_| {
        let len = varint(table, &mut at)? as usize;
        let s = core::str::from_utf8(table.get(at..at + len)?).ok()?;
        at += len;
        Some(s)
    });
    Some((iter, end))
}

/// How many zones the table has.
pub fn zone_count() -> usize {
    names(ZONES).map_or(0, |(n, _)| n.count())
}

/// The IANA name of zone `index`.
pub fn zone_name(index: u16) -> Option<&'static str> {
    names(ZONES)?.0.nth(usize::from(index))
}

/// The index of zone `name`.
pub fn zone_index(name: &str) -> Option<u16> {
    let i = names(ZONES)?.0.position(|n| n == name)?;
    u16::try_from(i).ok()
}

/// What [`ZONES`] says of each ring of [`RINGS`], in order.
pub fn ring_zones() -> impl Iterator<Item = RingZone> {
    ring_zones_of(ZONES)
}

fn ring_zones_of(table: &[u8]) -> impl Iterator<Item = RingZone> + '_ {
    let mut at = names(table).map(|(_, end)| end);
    let count = at.as_mut().and_then(|a| varint(table, a)).unwrap_or(0);
    let mut at = at.unwrap_or(table.len());
    (0..count).map_while(move |_| {
        let zone = u16::try_from(varint(table, &mut at)?).ok()?;
        let zoom = *table.get(at)?;
        at += 1;
        let label = match zoom {
            NO_LABEL => None,
            z => Some(((varint(table, &mut at)?, varint(table, &mut at)?), z)),
        };
        Some(RingZone { zone, label })
    })
}

/// Every ring of [`RINGS`] with its zone's index.
pub fn zone_rings() -> impl Iterator<Item = (u16, Ring<'static>)> {
    ring_zones()
        .zip(outline::rings(RINGS))
        .map(|(z, r)| (z.zone, r))
}

/// The index of the zone `(lat, lon)` (degrees) lies in; see [`zone_at`].
pub fn zone_index_at(lat: f64, lon: f64) -> Option<u16> {
    let bits = outline::bits(RINGS)?;
    let shift = 32 - bits;
    let p = project(LatLon::from_degrees(lat, lon));
    // The table's last row and column are its edge, 180 E and the
    // projection's south; a point past them is read just inside.
    let edge = (((1i64 << bits) - 1) << shift) - 1;
    let (px, py) = (i64::from(p.x).min(edge), i64::from(p.y).min(edge));
    let (tx, ty) = (px >> shift, py >> shift);
    let mut current: Option<u16> = None;
    let mut inside = false;
    for (zone, ring) in zone_rings() {
        if current != Some(zone) {
            if inside {
                return current;
            }
            current = Some(zone);
        }
        let [x0, y0, x1, y1] = ring.bbox;
        if tx < i64::from(x0) || tx > i64::from(x1) || ty < i64::from(y0) || ty > i64::from(y1) {
            continue;
        }
        let mut prev: Option<(i64, i64)> = None;
        let mut first = None;
        for (x, y) in ring.points() {
            let b = (i64::from(x) << shift, i64::from(y) << shift);
            first.get_or_insert(b);
            if let Some(a) = prev {
                inside ^= crosses(a, b, px, py);
            }
            prev = Some(b);
        }
        if let (Some(a), Some(b)) = (prev, first) {
            inside ^= crosses(a, b, px, py);
        }
    }
    match inside {
        true => current,
        false => None,
    }
}

/// Whether the ray east from `(px, py)` crosses the edge `a`-`b`, the
/// edge's lower end in, its upper end out.
fn crosses(a: (i64, i64), b: (i64, i64), px: i64, py: i64) -> bool {
    if (a.1 > py) == (b.1 > py) {
        return false;
    }
    // px < a.x + (py - a.y) (b.x - a.x) / (b.y - a.y), without dividing.
    let lhs = i128::from(px - a.0) * i128::from(b.1 - a.1);
    let rhs = i128::from(py - a.1) * i128::from(b.0 - a.0);
    match b.1 > a.1 {
        true => lhs < rhs,
        false => lhs > rhs,
    }
}

/// The IANA name of the zone `(lat, lon)` (degrees) lies in, by the
/// outline: the name of the merged zone, so Berlin answers
/// `Europe/Paris`. A latitude past the projection's 85.05 degrees is read
/// at its edge. `None` only where the simplified outline leaves a hole,
/// which a gapless coverage does not.
pub fn zone_at(lat: f64, lon: f64) -> Option<&'static str> {
    zone_name(zone_index_at(lat, lon)?)
}

/// Whether a box of table units, `bits` a coordinate, touches `view` in
/// any copy of the world.
fn box_in_view(bbox: [u32; 4], bits: u32, view: &Viewport) -> bool {
    let up = i32::from(view.zoom()) + 12 - bits as i32;
    let scale = |v: u32| -> i64 {
        match up >= 0 {
            true => i64::from(v) << up,
            false => i64::from(v) >> -up,
        }
    };
    let (w, h) = view.size();
    let (left, top) = view.top_left_sub();
    let (vw, vh) = (i64::from(w) * SUBPIXEL, i64::from(h) * SUBPIXEL);
    let world = 1i64 << (12 + u32::from(view.zoom()));
    let [x0, y0, x1, y1] = bbox;
    if scale(y1) - top < 0 || scale(y0) - top > vh {
        return false;
    }
    (left.div_euclid(world)..=(left + vw - 1).div_euclid(world)).any(|k| {
        let dx = k * world - left;
        scale(x1) + dx >= 0 && scale(x0) + dx <= vw
    })
}

/// The indices of the zones with a ring in `view`, each once, in index
/// order, appended to `out`.
pub fn zones_in_view(view: &Viewport, out: &mut Vec<u16>) {
    let Some(bits) = outline::bits(RINGS) else {
        return;
    };
    for (zone, ring) in zone_rings() {
        if out.last() != Some(&zone) && box_in_view(ring.bbox, bits, view) {
            out.push(zone);
        }
    }
}

/// The time zone labels `view` shows, appended to `out`: one for every
/// polygon whose anchor lies in the view and whose zoom the view reaches,
/// once for every copy of the world it falls in, of kind
/// [`LabelKind::TimeZone`], its `text` and `local` the zone's IANA name,
/// which the frontend replaces with the zone's local time. The frontend
/// places them by the core's one rule with its other names. A zone whose
/// anchors all lie outside a deep view gets no label there.
pub fn labels(view: &Viewport, out: &mut Vec<ViewLabel>) {
    labels_at(Place::Flat(view), out);
}

/// [`labels`] through the blended world projection (1710): each zone's
/// anchor by the blend, the world once, a zone on the globe's far side
/// not set.
pub fn labels_view(blend: &Blend, out: &mut Vec<ViewLabel>) {
    labels_at(Place::Curved(blend), out);
}

fn labels_at(place: Place<'_>, out: &mut Vec<ViewLabel>) {
    let Some(bits) = head(ZONES).map(|(b, _)| b) else {
        return;
    };
    let (w, h) = place.size();
    let (vw, vh) = (i64::from(w) * SUBPIXEL, i64::from(h) * SUBPIXEL);
    for rz in ring_zones() {
        let Some(((x, y), min_zoom)) = rz.label else {
            continue;
        };
        if min_zoom > place.zoom() {
            continue;
        }
        let Some(name) = zone_name(rz.zone) else {
            continue;
        };
        if place.hidden(bits, x, y) {
            continue;
        }
        for k in place.copies(vw) {
            let (ax, ay) = place.map(bits, x, y, k);
            if !(0..vw).contains(&ax) || !(0..vh).contains(&ay) {
                continue;
            }
            out.push(ViewLabel {
                kind: LabelKind::TimeZone,
                rank: 0,
                min_zoom,
                text: String::from(name),
                local: String::from(name),
                at: (x, y),
                anchor: (ax, ay),
                line: Vec::new(),
            });
        }
    }
}

/// The tzdata version [`RULES`] was generated from, `2026f` today.
pub fn board_tzdata_version() -> Option<&'static str> {
    RULES
        .lines()
        .find_map(|l| l.strip_prefix("# tzdata "))
        .map(str::trim)
}

/// The board's POSIX TZ string for zone `name`.
pub fn board_rule(name: &str) -> Option<&'static str> {
    RULES.lines().filter(|l| !l.starts_with('#')).find_map(|l| {
        let (n, tz) = l.split_once(' ')?;
        (n == name).then_some(tz)
    })
}

/// The board's local time of zone `name` at `unix` seconds since the
/// epoch, by [`RULES`]: the offset, DST flag and abbreviation. Wrong
/// inside the explicit transitions of a zone whose rules a POSIX string
/// cannot express (see the module's documentation).
pub fn board_time(name: &str, unix: i64) -> Option<Offset<'static>> {
    posix::offset_at(board_rule(name)?, unix)
}

/// The board's UTC offset in seconds of zone `name` at `unix`; see
/// [`board_time`].
pub fn board_offset(name: &str, unix: i64) -> Option<i32> {
    Some(board_time(name, unix)?.utc_offset)
}
