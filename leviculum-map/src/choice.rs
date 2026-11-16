//! Which region a view draws when several are installed, and how far a
//! frontend lets the reader zoom (leviculum 1062's report, items 4 and 7).
//!
//! A view draws one region or the world outline, never a patchwork: the
//! region of the highest max zoom whose box holds the view's centre and
//! whose zooms reach the view's ([`useful_zooms`]), ties by file name; the
//! outline where none does.

use core::ops::RangeInclusive;

use alloc::vec::Vec;

use crate::mercator::{unproject, LatLon, World, MAX_LAT_E7};
use crate::outline;
use crate::region::{Bounds, Header};
use crate::viewport::{Viewport, MAX_OVERZOOM, MAX_VIEW_ZOOM};

/// Metres per degree of latitude, near enough for a box's size.
const M_PER_DEGREE: f64 = 111_320.0;

impl Bounds {
    /// Whether `p` lies in the box, edges included; a box whose west is
    /// east of its east crosses the antimeridian.
    pub fn contains(&self, p: LatLon) -> bool {
        let lat = (self.south..=self.north).contains(&p.lat_e7);
        let lon = match self.west <= self.east {
            true => (self.west..=self.east).contains(&p.lon_e7),
            false => p.lon_e7 >= self.west || p.lon_e7 <= self.east,
        };
        lat && lon
    }

    /// The box `half_m` metres to each side of `at`, north and south and
    /// east and west, clamped to the projection's edges and wrapped at the
    /// antimeridian (a box across it has its west east of its east). Where
    /// a region is loaded around a place (loquax, 1089).
    pub fn around(at: LatLon, half_m: u32) -> Self {
        let lat = at.lat_e7.clamp(-MAX_LAT_E7, MAX_LAT_E7);
        let dlat = f64::from(half_m) / M_PER_DEGREE;
        let cos = libm::cos(f64::from(lat) / 1e7 * core::f64::consts::PI / 180.0);
        let dlon = (dlat / cos.max(1e-6)).min(179.9);
        let e7 = |d: f64| libm::round(d * 1e7) as i64;
        let wrap = |v: i64| ((v + 1_800_000_000).rem_euclid(3_600_000_000) - 1_800_000_000) as i32;
        let clamp = |v: i64| v.clamp(-i64::from(MAX_LAT_E7), i64::from(MAX_LAT_E7)) as i32;
        Self {
            west: wrap(i64::from(at.lon_e7) - e7(dlon)),
            south: clamp(i64::from(lat) - e7(dlat)),
            east: wrap(i64::from(at.lon_e7) + e7(dlon)),
            north: clamp(i64::from(lat) + e7(dlat)),
        }
    }

    /// Whether `inner` lies wholly in this box. A box across the
    /// antimeridian holds only a box equal to it.
    pub fn holds(&self, inner: &Bounds) -> bool {
        if self.west > self.east || inner.west > inner.east {
            return self == inner;
        }
        self.south <= inner.south
            && inner.north <= self.north
            && self.west <= inner.west
            && inner.east <= self.east
    }
}

/// The regions a view draws, as indices into `regions` (file name,
/// header), in the order they are drawn: every region whose box reaches
/// into the view and whose zooms reach the view's ([`useful_zooms`]),
/// shallower data first, then the older layer profile, then by file name,
/// so the deepest is drawn last, on top, and of two as deep the newer
/// profile (1727: a hand-placed profile 1 region under the profile 2 copy
/// the autoload brought for its box, `crate::profile`). Each is drawn into its own box (1077), so regions loaded
/// side by side join up (1089). Empty for the outline alone.
pub fn drawn<'a>(
    regions: impl IntoIterator<Item = (&'a str, &'a Header)>,
    view: &Viewport,
) -> Vec<usize> {
    let mut found: Vec<(u8, u8, &str, usize)> = regions
        .into_iter()
        .enumerate()
        .filter(|(_, (_, h))| useful_zooms(h).contains(&view.zoom()))
        .filter(|(_, (_, h))| !view.area_of(&h.bounds).is_zero_sized())
        .map(|(i, (name, h))| (h.max_zoom, h.profile, name, i))
        .collect();
    found.sort();
    found.into_iter().map(|(_, _, _, i)| i).collect()
}

/// The view zooms a region draws at: its shallowest, down to
/// [`MAX_OVERZOOM`] past its deepest.
pub fn useful_zooms(h: &Header) -> RangeInclusive<u8> {
    h.min_zoom..=h.max_zoom.saturating_add(MAX_OVERZOOM).min(MAX_VIEW_ZOOM)
}

/// The zooms a frontend offers with these regions installed: from the
/// whole world down to the deepest any region draws at, and at least to
/// the outline's [`outline::MAX_ZOOM`].
pub fn view_zooms<'a>(headers: impl IntoIterator<Item = &'a Header>) -> RangeInclusive<u8> {
    let deepest = headers
        .into_iter()
        .map(|h| *useful_zooms(h).end())
        .fold(outline::MAX_ZOOM, u8::max);
    0..=deepest
}

/// The deepest zoom a fit centred on `centre` goes to: the deepest max
/// zoom of the regions whose box holds it, their own data and not the
/// overzoom past it, which the reader reaches by zooming in; the outline's
/// [`outline::MAX_ZOOM`] where none does. A fit to one point would
/// otherwise land at the deepest view zoom, a few metres of plain colour
/// (1074).
pub fn fit_ceiling<'a>(headers: impl IntoIterator<Item = &'a Header>, centre: World) -> u8 {
    let at = unproject(centre);
    headers
        .into_iter()
        .filter(|h| h.bounds.contains(at))
        .map(|h| h.max_zoom)
        .max()
        .unwrap_or(outline::MAX_ZOOM)
}

/// The region a view at `centre` and `zoom` draws, as an index into
/// `regions` (file name, header); `None` for the outline.
pub fn choose<'a>(
    regions: impl IntoIterator<Item = (&'a str, &'a Header)>,
    centre: World,
    zoom: u8,
) -> Option<usize> {
    let at = unproject(centre);
    let mut best: Option<(usize, u8, &str)> = None;
    for (i, (name, h)) in regions.into_iter().enumerate() {
        if !h.bounds.contains(at) || !useful_zooms(h).contains(&zoom) {
            continue;
        }
        let better = match best {
            None => true,
            Some((_, max, held)) => h.max_zoom > max || (h.max_zoom == max && name < held),
        };
        if better {
            best = Some((i, h.max_zoom, name));
        }
    }
    best.map(|(i, _, _)| i)
}
