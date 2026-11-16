//! The geoid: how far mean sea level lies above the WGS84 ellipsoid, so a
//! GNSS height, which is a height above the ellipsoid, becomes a height
//! above sea level (`H = h - N`).
//!
//! The table is EGM2008, the US National Geospatial-Intelligence Agency's
//! Earth Gravitational Model 2008, public domain, taken from GeographicLib's
//! 5' grid and kept at every third post by `scripts/bake-geoid.py`: a post
//! every 15', 721 rows from 90 N to 90 S, 1440 columns from 0 E to
//! 359.75 E, 2,076,500 bytes ([`EGM2008_15`]). The posts are the source's
//! own, unsmoothed, at its 3 mm step.
//!
//! Format, little-endian: `LGD1`, u16 rows, u16 cols, u16 step in
//! arc-seconds, u16 reserved, i32 offset in micrometres, u32 scale in
//! micrometres per step, then `rows * cols` u16 posts, row by row from the
//! north, each row eastward from 0 E. A post's height is
//! `offset + scale * post` micrometres. [`Grid`] reads the bytes in place;
//! nothing is allocated.
//!
//! Between posts the height is bilinear in the four around the point.
//! Against the full 1' EGM2008, measured at every 15' cell centre, that is
//! off by 0.10 m rms, by under 0.8 m at 99.9 % of the earth, and by 2.7 m
//! at worst, over the Sierra Nevada de Santa Marta (`tests/geoid.rs`):
//! well inside what a GNSS height is worth.

/// The table, EGM2008 at 15'.
pub const EGM2008_15: &[u8] = include_bytes!("../data/egm2008-15.lgd");

const MAGIC: &[u8; 4] = b"LGD1";
const HEADER: usize = 20;
const ARCSEC_PER_DEGREE: f64 = 3600.0;

/// A geoid table read in place. [`Grid::new`] accepts only a whole-earth
/// table whose rows run pole to pole and whose columns go once round.
#[derive(Clone, Copy, Debug)]
pub struct Grid<'a> {
    posts: &'a [u8],
    rows: usize,
    cols: usize,
    step_deg: f64,
    offset_m: f64,
    scale_m: f64,
}

impl<'a> Grid<'a> {
    /// Reads the header, or `None` for bytes that are not a whole-earth
    /// `LGD1` table of the length its header names.
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        let header = bytes.get(..HEADER)?;
        if header.get(..4)? != MAGIC {
            return None;
        }
        let u16_at = |i: usize| -> Option<u16> {
            Some(u16::from_le_bytes([*header.get(i)?, *header.get(i + 1)?]))
        };
        let four_at = |i: usize| -> Option<[u8; 4]> { header.get(i..i + 4)?.try_into().ok() };
        let rows = usize::from(u16_at(4)?);
        let cols = usize::from(u16_at(6)?);
        let step = u32::from(u16_at(8)?);
        let offset_um = i32::from_le_bytes(four_at(12)?);
        let scale_um = u32::from_le_bytes(four_at(16)?);
        // Whole earth: rows from pole to pole inclusive, columns once round.
        if rows < 2
            || cols < 2
            || step == 0
            || (rows as u32 - 1).checked_mul(step)? != 180 * 3600
            || (cols as u32).checked_mul(step)? != 360 * 3600
        {
            return None;
        }
        let posts = bytes.get(HEADER..)?;
        if posts.len() != rows.checked_mul(cols)?.checked_mul(2)? {
            return None;
        }
        Some(Self {
            posts,
            rows,
            cols,
            step_deg: f64::from(step) / ARCSEC_PER_DEGREE,
            offset_m: f64::from(offset_um) / 1e6,
            scale_m: f64::from(scale_um) / 1e6,
        })
    }

    /// Rows, from 90 N southward.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Columns, from 0 E eastward.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// The raw post at a row and column, as the table stores it.
    pub fn post(&self, row: usize, col: usize) -> Option<u16> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        let i = (row * self.cols + col) * 2;
        Some(u16::from_le_bytes([
            *self.posts.get(i)?,
            *self.posts.get(i + 1)?,
        ]))
    }

    /// The height of a post in metres.
    pub fn post_m(&self, row: usize, col: usize) -> Option<f64> {
        Some(self.offset_m + self.scale_m * f64::from(self.post(row, col)?))
    }

    /// The geoid undulation N in metres at a point: bilinear between the
    /// four posts around it, the longitude taken round the earth, the
    /// latitude clamped to the poles. `None` for a NaN or infinite input.
    pub fn undulation(&self, lat_deg: f64, lon_deg: f64) -> Option<f64> {
        if !lat_deg.is_finite() || !lon_deg.is_finite() {
            return None;
        }
        let lat = lat_deg.clamp(-90.0, 90.0);
        let y = (90.0 - lat) / self.step_deg;
        // The last row is the south pole; a point on it interpolates in
        // the band above with a weight of one on its lower edge.
        let r0 = (libm::floor(y) as usize).min(self.rows - 2);
        let fy = y - r0 as f64;

        let mut lon = libm::fmod(lon_deg, 360.0);
        if lon < 0.0 {
            lon += 360.0;
        }
        let x = lon / self.step_deg;
        let c = libm::floor(x);
        let fx = x - c;
        let c0 = (c as usize) % self.cols;
        let c1 = (c0 + 1) % self.cols;

        let n00 = self.post_m(r0, c0)?;
        let n01 = self.post_m(r0, c1)?;
        let n10 = self.post_m(r0 + 1, c0)?;
        let n11 = self.post_m(r0 + 1, c1)?;
        let north = n00 + (n01 - n00) * fx;
        let south = n10 + (n11 - n10) * fx;
        Some(north + (south - north) * fy)
    }
}

/// The geoid undulation N in metres at a point, from [`EGM2008_15`]:
/// how far sea level lies above the WGS84 ellipsoid there. 0.0, no
/// correction, for a NaN or infinite input or should the built-in table
/// ever fail to read.
pub fn undulation(lat_deg: f64, lon_deg: f64) -> f64 {
    Grid::new(EGM2008_15)
        .and_then(|g| g.undulation(lat_deg, lon_deg))
        .unwrap_or(0.0)
}

/// A height above the WGS84 ellipsoid, as GNSS gives it, turned into a
/// height above sea level: `h - N`. The ellipsoid height comes back
/// unchanged where [`undulation`] falls back to 0.0.
pub fn height_above_sea_level(lat_deg: f64, lon_deg: f64, ellipsoid_height_m: f64) -> f64 {
    ellipsoid_height_m - undulation(lat_deg, lon_deg)
}

/// [`undulation`] for a position in microdegrees, in whole centimetres.
pub fn undulation_cm(lat_e6: i32, lon_e6: i32) -> i32 {
    let n = undulation(f64::from(lat_e6) / 1e6, f64::from(lon_e6) / 1e6);
    // |N| < 110 m everywhere, so the centimetres fit an i32 with room.
    libm::round(n * 100.0) as i32
}

/// [`height_above_sea_level`] in integers, as the apps store a position:
/// microdegrees and an ellipsoid height in centimetres, giving centimetres
/// above sea level. Saturates at the ends of `i32`.
pub fn height_above_sea_level_cm(lat_e6: i32, lon_e6: i32, alt_e2: i32) -> i32 {
    alt_e2.saturating_sub(undulation_cm(lat_e6, lon_e6))
}
