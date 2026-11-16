//! Magnetic declination and the rest of the main field from the World
//! Magnetic Model WMM2025 (NOAA NCEI and the British Geological Survey,
//! public domain), so a map can turn a compass bearing into a true one.
//!
//! The evaluation follows the WMM2025 technical report (Chulliat et al.,
//! 2025, <https://doi.org/10.25923/prbc-s316>) and the code NCEI ships
//! with it: the geodetic position goes to geocentric spherical coordinates
//! on the WGS84 ellipsoid, the Gauss coefficients are moved from the epoch
//! by their secular variation, the field is summed over Schmidt
//! semi-normalised associated Legendre functions to degree 12, and the
//! spherical components are rotated back to the geodetic frame. At a
//! geographic pole, where the east component's division by the cosine of
//! the latitude fails, the east component is summed over the order-one
//! terms alone, as the report's code does.
//!
//! The coefficients are a generated table, [`coefficients`], baked from
//! NCEI's `WMM.COF` by `scripts/gen-wmm.py`. Every row of NCEI's two
//! published WMM2025 test tables is checked in `tests/declination.rs`.
//!
//! The model is valid from 2025.0 to 2030.0. Outside that span the
//! functions still compute, extrapolating the secular variation linearly
//! from the epoch, and the error grows with the distance; nothing is
//! clamped and no error is returned. Heights are in kilometres above the
//! WGS84 ellipsoid; the model is specified from 1 km below it to 850 km
//! above.
//!
//! `no_std`, no allocation: the work is four tables of 91 `f64` on the
//! stack (the moved coefficients and the Legendre functions with their
//! derivatives), about 3 KiB, and some thousand multiplications.

#[rustfmt::skip]
pub mod coefficients;

use coefficients::{EPOCH, G, G_DOT, H, H_DOT, LEN, N_MAX};

/// WGS84 semi-major axis, km.
const WGS84_A: f64 = 6378.137;
/// WGS84 flattening.
const WGS84_F: f64 = 1.0 / 298.257_223_563;
/// The geomagnetic reference radius of the model, km.
const RE: f64 = 6371.2;

/// The first year of the model's validity.
pub const VALID_FROM: f64 = 2025.0;
/// The end of the model's validity.
pub const VALID_UNTIL: f64 = 2030.0;

/// The magnetic field at one place and time, geodetic frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    /// North component, nT.
    pub x: f64,
    /// East component, nT.
    pub y: f64,
    /// Down component, nT.
    pub z: f64,
    /// Horizontal intensity, nT.
    pub h: f64,
    /// Total intensity, nT.
    pub f: f64,
    /// Inclination (dip), degrees, positive down.
    pub i: f64,
    /// Declination, degrees, east of true north positive.
    pub d: f64,
    /// Grid variation, degrees, only where the polar stereographic grid
    /// is the reference, at 55 degrees of latitude and beyond either way;
    /// `None` between. Normalised to (-180, 180].
    pub gv: Option<f64>,
    /// The yearly change of each element.
    pub sv: SecularVariation,
}

/// The yearly change of the field's elements.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SecularVariation {
    /// nT per year.
    pub x: f64,
    /// nT per year.
    pub y: f64,
    /// nT per year.
    pub z: f64,
    /// nT per year.
    pub h: f64,
    /// nT per year.
    pub f: f64,
    /// Degrees per year.
    pub i: f64,
    /// Degrees per year.
    pub d: f64,
}

/// The declination at a place and time, degrees, east positive.
///
/// `lat_deg` and `lon_deg` are geodetic WGS84 degrees,
/// `height_km_above_ellipsoid` the height above the WGS84 ellipsoid in km,
/// `decimal_year` the time as [`decimal_year`] makes it. See the module
/// documentation for validity.
pub fn declination(
    lat_deg: f64,
    lon_deg: f64,
    height_km_above_ellipsoid: f64,
    decimal_year: f64,
) -> f64 {
    field(lat_deg, lon_deg, height_km_above_ellipsoid, decimal_year).d
}

/// The whole field at a place and time; arguments as for [`declination`].
pub fn field(
    lat_deg: f64,
    lon_deg: f64,
    height_km_above_ellipsoid: f64,
    decimal_year: f64,
) -> Field {
    let lat = lat_deg.to_radians();
    let lon = lon_deg.to_radians();

    // Geodetic to geocentric spherical on WGS84.
    let e2 = WGS84_F * (2.0 - WGS84_F);
    let (sin_lat, cos_lat) = (libm::sin(lat), libm::cos(lat));
    let rc = WGS84_A / libm::sqrt(1.0 - e2 * sin_lat * sin_lat);
    let xp = (rc + height_km_above_ellipsoid) * cos_lat;
    let zp = (rc * (1.0 - e2) + height_km_above_ellipsoid) * sin_lat;
    let r = libm::sqrt(xp * xp + zp * zp);
    let phig = libm::asin(zp / r);

    let dt = decimal_year - EPOCH;
    let g: [f64; LEN] = core::array::from_fn(|k| G[k] + dt * G_DOT[k]);
    let h: [f64; LEN] = core::array::from_fn(|k| H[k] + dt * H_DOT[k]);

    let sph = Spherical::new(r, phig, lon);
    let main = sph.sum(&g, &h);
    let dot = sph.sum(&G_DOT, &H_DOT);

    // Rotate from the geocentric to the geodetic frame.
    let psi = phig - lat;
    let (sin_psi, cos_psi) = (libm::sin(psi), libm::cos(psi));
    let rot = |b: [f64; 3]| {
        [
            b[0] * cos_psi - b[2] * sin_psi,
            b[1],
            b[0] * sin_psi + b[2] * cos_psi,
        ]
    };
    let [x, y, z] = rot(main);
    let [xd, yd, zd] = rot(dot);

    let hh = libm::sqrt(x * x + y * y);
    let ff = libm::sqrt(hh * hh + z * z);
    let d = libm::atan2(y, x).to_degrees();
    let i = libm::atan2(z, hh).to_degrees();

    let hd = (x * xd + y * yd) / hh;
    let fd = (x * xd + y * yd + z * zd) / ff;
    let dd = ((x * yd - y * xd) / (hh * hh)).to_degrees();
    let id = ((hh * zd - z * hd) / (ff * ff)).to_degrees();

    let gv = if lat_deg >= 55.0 {
        Some(normalise_180(d - lon_deg))
    } else if lat_deg <= -55.0 {
        Some(normalise_180(d + lon_deg))
    } else {
        None
    };

    Field {
        x,
        y,
        z,
        h: hh,
        f: ff,
        i,
        d,
        gv,
        sv: SecularVariation {
            x: xd,
            y: yd,
            z: zd,
            h: hd,
            f: fd,
            i: id,
            d: dd,
        },
    }
}

/// The decimal year of a calendar date at 00:00 UTC, as the WMM code
/// counts it: the year plus the days before the date over the days of the
/// year. `None` for a month outside 1 to 12 or a day the month lacks.
pub fn decimal_year(year: i32, month: u32, day: u32) -> Option<f64> {
    const DAYS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let idx = usize::try_from(month.checked_sub(1)?).ok()?;
    let in_month = *DAYS.get(idx)? + u32::from(leap && month == 2);
    if day == 0 || day > in_month {
        return None;
    }
    let before: u32 = DAYS[..idx].iter().sum::<u32>() + u32::from(leap && month > 2);
    let in_year = if leap { 366.0 } else { 365.0 };
    Some(f64::from(year) + f64::from(before + day - 1) / in_year)
}

fn normalise_180(a: f64) -> f64 {
    let mut a = a % 360.0;
    if a > 180.0 {
        a -= 360.0;
    } else if a <= -180.0 {
        a += 360.0;
    }
    a
}

/// What a summation needs of a place: the radius powers, the longitude
/// harmonics and the Legendre functions.
struct Spherical {
    /// (RE / r)^(n + 2).
    radius_power: [f64; N_MAX + 1],
    cos_ml: [f64; N_MAX + 1],
    sin_ml: [f64; N_MAX + 1],
    p: [f64; LEN],
    dp: [f64; LEN],
    sin_phi: f64,
    cos_phi: f64,
}

impl Spherical {
    fn new(r: f64, phig: f64, lon: f64) -> Self {
        let mut radius_power = [0.0; N_MAX + 1];
        let ratio = RE / r;
        let mut pow = ratio * ratio;
        for rp in radius_power.iter_mut() {
            *rp = pow;
            pow *= ratio;
        }
        let (sin_l, cos_l) = (libm::sin(lon), libm::cos(lon));
        let mut cos_ml = [0.0; N_MAX + 1];
        let mut sin_ml = [0.0; N_MAX + 1];
        cos_ml[0] = 1.0;
        for m in 1..=N_MAX {
            cos_ml[m] = cos_ml[m - 1] * cos_l - sin_ml[m - 1] * sin_l;
            sin_ml[m] = cos_ml[m - 1] * sin_l + sin_ml[m - 1] * cos_l;
        }
        let sin_phi = libm::sin(phig);
        let (p, dp) = legendre(sin_phi);
        Spherical {
            radius_power,
            cos_ml,
            sin_ml,
            p,
            dp,
            sin_phi,
            cos_phi: libm::cos(phig),
        }
    }

    /// North, east and down in the geocentric frame for one coefficient
    /// set (the field, or its secular variation).
    fn sum(&self, g: &[f64; LEN], h: &[f64; LEN]) -> [f64; 3] {
        let (mut bx, mut by, mut bz) = (0.0, 0.0, 0.0);
        for n in 1..=N_MAX {
            let rp = self.radius_power[n];
            for m in 0..=n {
                let k = n * (n + 1) / 2 + m;
                let gc = g[k] * self.cos_ml[m] + h[k] * self.sin_ml[m];
                let gs = g[k] * self.sin_ml[m] - h[k] * self.cos_ml[m];
                bz -= rp * gc * (n + 1) as f64 * self.p[k];
                by += rp * gs * m as f64 * self.p[k];
                bx -= rp * gc * self.dp[k];
            }
        }
        if libm::fabs(self.cos_phi) > 1.0e-10 {
            by /= self.cos_phi;
        } else {
            by = self.pole_east(g, h);
        }
        [bx, by, bz]
    }

    /// The east component at a geographic pole: the limit of
    /// m P(n, m) / cos(phi) is nonzero for m = 1 alone, so the sum runs over
    /// the order-one terms with that limit, as the report's code does.
    fn pole_east(&self, g: &[f64; LEN], h: &[f64; LEN]) -> f64 {
        let mut ps = [0.0; N_MAX + 1];
        ps[0] = 1.0;
        let mut norm1 = 1.0;
        let mut by = 0.0;
        for n in 1..=N_MAX {
            let k = n * (n + 1) / 2 + 1;
            let nf = n as f64;
            let norm2 = norm1 * (2.0 * nf - 1.0) / nf;
            let norm3 = norm2 * libm::sqrt(2.0 * nf / (nf + 1.0));
            norm1 = norm2;
            if n == 1 {
                ps[n] = ps[n - 1];
            } else {
                let kk = ((nf - 1.0) * (nf - 1.0) - 1.0) / ((2.0 * nf - 1.0) * (2.0 * nf - 3.0));
                ps[n] = self.sin_phi * ps[n - 1] - kk * ps[n - 2];
            }
            by += self.radius_power[n]
                * (g[k] * self.sin_ml[1] - h[k] * self.cos_ml[1])
                * ps[n]
                * norm3;
        }
        by
    }
}

/// Schmidt semi-normalised associated Legendre functions of `x = sin(phi)`
/// and their derivatives with respect to latitude, indexed n(n+1)/2 + m:
/// the Gauss-normalised recursion, then the ratio to Schmidt's
/// normalisation, as the report's code computes them for low degrees.
fn legendre(x: f64) -> ([f64; LEN], [f64; LEN]) {
    let mut p = [0.0; LEN];
    let mut dp = [0.0; LEN];
    p[0] = 1.0;
    let z = libm::sqrt((1.0 - x) * (1.0 + x));
    for n in 1..=N_MAX {
        for m in 0..=n {
            let k = n * (n + 1) / 2 + m;
            if n == m {
                let k1 = (n - 1) * n / 2 + m - 1;
                p[k] = z * p[k1];
                dp[k] = z * dp[k1] + x * p[k1];
            } else if n == 1 && m == 0 {
                let k1 = (n - 1) * n / 2 + m;
                p[k] = x * p[k1];
                dp[k] = x * dp[k1] - z * p[k1];
            } else {
                let k1 = (n - 2) * (n - 1) / 2 + m;
                let k2 = (n - 1) * n / 2 + m;
                if m + 2 > n {
                    p[k] = x * p[k2];
                    dp[k] = x * dp[k2] - z * p[k2];
                } else {
                    let kk =
                        ((n - 1) * (n - 1) - m * m) as f64 / ((2 * n - 1) * (2 * n - 3)) as f64;
                    p[k] = x * p[k2] - kk * p[k1];
                    dp[k] = x * dp[k2] - z * p[k2] - kk * dp[k1];
                }
            }
        }
    }
    let mut norm = [0.0; LEN];
    norm[0] = 1.0;
    for n in 1..=N_MAX {
        let k = n * (n + 1) / 2;
        let k1 = (n - 1) * n / 2;
        norm[k] = norm[k1] * (2 * n - 1) as f64 / n as f64;
        for m in 1..=n {
            let k = n * (n + 1) / 2 + m;
            let two = if m == 1 { 2 } else { 1 };
            norm[k] = norm[k - 1] * libm::sqrt(((n - m + 1) * two) as f64 / (n + m) as f64);
        }
    }
    for n in 1..=N_MAX {
        for m in 0..=n {
            let k = n * (n + 1) / 2 + m;
            p[k] *= norm[k];
            // The derivative with respect to latitude, not co-latitude.
            dp[k] = -dp[k] * norm[k];
        }
    }
    (p, dp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_year_counts_days_before_the_date() {
        assert_eq!(decimal_year(2025, 1, 1), Some(2025.0));
        assert_eq!(decimal_year(2025, 7, 2), Some(2025.0 + 182.0 / 365.0));
        assert_eq!(decimal_year(2028, 7, 2), Some(2028.0 + 183.0 / 366.0));
        assert_eq!(decimal_year(2028, 2, 29), Some(2028.0 + 59.0 / 366.0));
        assert_eq!(decimal_year(2027, 2, 29), None);
        assert_eq!(decimal_year(2027, 13, 1), None);
        assert_eq!(decimal_year(2027, 0, 1), None);
        assert_eq!(decimal_year(2027, 4, 31), None);
        assert_eq!(decimal_year(2027, 4, 0), None);
    }

    #[test]
    fn grid_variation_wraps_into_half_open_range() {
        assert_eq!(normalise_180(308.78), 308.78 - 360.0);
        assert_eq!(normalise_180(-180.0), 180.0);
        assert_eq!(normalise_180(180.0), 180.0);
    }

    #[test]
    fn the_pole_gives_a_finite_field() {
        let f = field(90.0, 0.0, 0.0, 2025.0);
        assert!(f.x.is_finite() && f.y.is_finite() && f.z.is_finite());
        assert!(f.d.is_finite());
    }
}
