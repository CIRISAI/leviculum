//! The fix snapshot the GNSS task publishes, and how each sentence folds
//! into it (leviculum#413, the stores section of
//! `docs/src/firmware/host-test-seams.md`).
//!
//! `leviculum-nrf/src/gnss.rs` pumps the UART through the presence machine
//! and publishes the snapshot; the fold itself is here, so a receiver that
//! drops its fix, reports a date with no position, or flips between valid
//! and invalid sentences is a host test over the real parser instead of a
//! board on a window sill.
//!
//! Time and position follow different rules on purpose. Position keeps its
//! last good value across sentences without a fix, so the display has
//! something to draw during a dip. Time does not: an RMC without a valid
//! fix clears `unix_secs`, because a stale time claim would seed the
//! calendar wrong ([`crate::SeedGate`] is only ever offered what a valid
//! RMC carried).

use nmea0183::{GPSQuality, GGA, RMC};

use crate::unix_secs_from_rmc_utc;

/// Compact GNSS fix snapshot: what the display, the telemetry reporter and
/// the calendar seed read. `valid` is the receiver's own validity flag (RMC
/// `Mode::is_valid` or GGA quality other than no fix), not a project-wide
/// policy.
///
/// `latitude` / `longitude` are decimal degrees with sign (negative = S/W,
/// positive = N/E), as nmea0183's `as_f64` returns them: it applies the
/// hemisphere itself, so the fold must not apply it a second time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GnssFix {
    /// True when the receiver reports a usable fix.
    pub valid: bool,
    /// Number of satellites in use, from the latest GGA.
    pub sat_in_use: u8,
    /// Latitude in decimal degrees (signed). `None` until the first valid
    /// sentence carries position data.
    pub latitude: Option<f64>,
    /// Longitude in decimal degrees (signed). Same convention as latitude.
    pub longitude: Option<f64>,
    /// RMC UTC date+time as unix seconds (Codeberg #166 item 1), set on
    /// every valid RMC and — unlike position — CLEARED on an invalid one:
    /// a stale time claim would seed the calendar wrong, while stale
    /// position only mis-renders. Consumers must treat it as "time as of
    /// this snapshot's publication", good to ~1 s (one RMC cadence).
    pub unix_secs: Option<u64>,
    /// Metres above mean sea level, from the latest GGA with a fix.
    /// Follows position: kept across non-valid sentences.
    pub altitude_m: Option<f32>,
    /// Horizontal dilution of precision, from the latest GGA with a fix
    /// (Codeberg #236). The only accuracy number standard NMEA carries,
    /// and therefore the one the telemetry accuracy gate is applied to —
    /// a position with no HDOP is a position of unknown quality and is
    /// not reported.
    pub hdop: Option<f32>,
    /// Ground speed in metres per second, from the latest valid RMC.
    pub speed_mps: Option<f32>,
    /// Course over ground in degrees, from the latest valid RMC. `None`
    /// while stationary — receivers stop reporting it, and inventing 0°
    /// would claim due north.
    pub bearing_deg: Option<f32>,
}

impl GnssFix {
    /// No sentence seen yet.
    pub const fn empty() -> Self {
        Self {
            valid: false,
            sat_in_use: 0,
            latitude: None,
            longitude: None,
            unix_secs: None,
            altitude_m: None,
            hdop: None,
            speed_mps: None,
            bearing_deg: None,
        }
    }

    /// Fold an RMC: a valid one sets position, time, speed and course; an
    /// invalid one clears only the time.
    ///
    /// A receiver that has lost its fix blanks position and speed and may
    /// keep the date; the parser keeps that sentence (the vendored
    /// nmea0183's second API change), so the time it no longer vouches for
    /// is cleared here instead of outliving the fix.
    pub fn fold_rmc(&mut self, rmc: &RMC) {
        let valid = rmc.mode.is_valid();
        self.valid = valid;
        // A stale time claim must not outlive the fix that made it:
        // position keeps last-good for the display, time does not. RMC UTC
        // is already leap-second-corrected by the receiver — converted
        // as-is, never via raw GPS time (#166, time-and-clocks.md).
        self.unix_secs = match (valid, &rmc.datetime) {
            (true, Some(datetime)) => unix_secs_from_rmc_utc(datetime),
            _ => None,
        };
        if let (true, Some(lat), Some(lon)) = (valid, &rmc.latitude, &rmc.longitude) {
            self.latitude = Some(lat.as_f64());
            self.longitude = Some(lon.as_f64());
            // Movement, for the telemetry location sensor (#236). Course is
            // absent while stationary on most receivers, so it is carried
            // as absent rather than as zero.
            self.speed_mps = rmc.speed.as_ref().map(|s| s.as_mps());
            self.bearing_deg = rmc.course.as_ref().map(|c| c.degrees);
        }
    }

    /// Fold a GGA: the satellite count always; validity is any quality but
    /// no fix; position, altitude and HDOP only with a fix that carries a
    /// position.
    ///
    /// A receiver searching for a fix blanks the position fields and still
    /// counts its satellites; the parser keeps that sentence (the vendored
    /// nmea0183's one API change), so its count and its lost fix land here.
    pub fn fold_gga(&mut self, gga: &GGA) {
        self.sat_in_use = gga.sat_in_use;
        let fix = !matches!(gga.gps_quality, GPSQuality::NoFix);
        self.valid = fix;
        if let (true, Some(lat), Some(lon)) = (fix, &gga.latitude, &gga.longitude) {
            self.latitude = Some(lat.as_f64());
            self.longitude = Some(lon.as_f64());
            self.altitude_m = gga.altitude.as_ref().map(|a| a.meters);
            // The accuracy gate's input (#236). Kept with position rather
            // than cleared with time: a stale HDOP mis-scales a position
            // that is itself stale, which is the same class of error, not a
            // new one. Set with the position it describes, so a fix whose
            // HDOP is blank is a position of unknown quality.
            self.hdop = gga.hdop;
        }
    }
}

impl Default for GnssFix {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nmea0183::{ParseResult, Parser};

    /// One complete sentence through the real parser, the pipeline the
    /// firmware's GNSS task runs. The checksum is computed here so the
    /// fixtures read as the receiver's fields.
    fn parse(body: &str) -> ParseResult {
        let sum = body.bytes().fold(0u8, |acc, b| acc ^ b);
        let line = format!("${body}*{sum:02X}\r\n");
        let mut parser = Parser::new();
        for b in line.bytes() {
            if let Some(result) = parser.parse_from_byte(b) {
                return result.unwrap_or_else(|e| panic!("{body:?}: {e}"));
            }
        }
        panic!("parser never completed {body:?}");
    }

    fn rmc(body: &str) -> RMC {
        match parse(body) {
            ParseResult::RMC(Some(rmc)) => rmc,
            other => panic!("expected RMC, got {other:?}"),
        }
    }

    fn gga(body: &str) -> GGA {
        match parse(body) {
            ParseResult::GGA(Some(gga)) => gga,
            other => panic!("expected GGA, got {other:?}"),
        }
    }

    const RMC_VALID: &str = "GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W,A";
    const RMC_SOUTH_WEST: &str = "GPRMC,123519,A,3352.000,S,15112.000,W,000.0,,230394,,,A";
    const RMC_VOID: &str = "GPRMC,123521,V,4807.038,N,01131.000,E,000.0,,230394,,,N";
    const GGA_FIX: &str = "GPGGA,123520,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,";
    const GGA_NO_FIX: &str = "GPGGA,123522,4807.038,N,01131.000,E,0,03,99.9,,M,,M,,";

    #[test]
    fn a_valid_rmc_sets_position_time_and_movement() {
        let mut fix = GnssFix::empty();
        fix.fold_rmc(&rmc(RMC_VALID));
        assert!(fix.valid);
        assert!((fix.latitude.unwrap() - 48.1173).abs() < 1e-4);
        assert!((fix.longitude.unwrap() - 11.516_666).abs() < 1e-4);
        assert_eq!(fix.unix_secs, Some(764_426_119));
        assert!(fix.speed_mps.unwrap() > 11.0);
        assert_eq!(fix.bearing_deg, Some(84.4));
    }

    #[test]
    fn south_and_west_are_negative_and_a_missing_course_stays_absent() {
        let mut fix = GnssFix::empty();
        fix.fold_rmc(&rmc(RMC_SOUTH_WEST));
        assert!(fix.latitude.unwrap() < -33.0, "{:?}", fix.latitude);
        assert!(fix.longitude.unwrap() < -151.0, "{:?}", fix.longitude);
        assert_eq!(fix.bearing_deg, None);
    }

    #[test]
    fn an_invalid_rmc_clears_the_time_and_keeps_the_position() {
        let mut fix = GnssFix::empty();
        fix.fold_rmc(&rmc(RMC_VALID));
        let before = fix;
        fix.fold_rmc(&rmc(RMC_VOID));
        assert!(!fix.valid);
        assert_eq!(
            fix.unix_secs, None,
            "a stale time claim must not seed the clock"
        );
        assert_eq!(fix.latitude, before.latitude);
        assert_eq!(fix.longitude, before.longitude);
        assert_eq!(fix.speed_mps, before.speed_mps);
    }

    #[test]
    fn an_invalid_rmc_on_an_unseeded_board_offers_the_gate_nothing() {
        let gate = crate::SeedGate::new();
        let mut fix = GnssFix::empty();
        fix.fold_rmc(&rmc(RMC_VOID));
        assert_eq!(gate.offer(fix.unix_secs), None);
        fix.fold_rmc(&rmc(RMC_VALID));
        assert_eq!(gate.offer(fix.unix_secs), Some(764_426_119));
    }

    /// What the firmware folds from one sentence: the presence machine
    /// forwards a GGA or an RMC only when the parser returns one
    /// (`leviculum-nrf/gnss-presence`, `ParseResult::GGA(Some(_))`,
    /// `ParseResult::RMC(Some(_))`).
    fn fold_sentence(fix: &mut GnssFix, body: &str) {
        match parse(body) {
            ParseResult::GGA(Some(gga)) => fix.fold_gga(&gga),
            ParseResult::RMC(Some(rmc)) => fix.fold_rmc(&rmc),
            _ => {}
        }
    }

    /// A receiver searching for a fix blanks the position fields (u-blox,
    /// MediaTek); the satellite count is the one number it still reports.
    const GGA_SEARCHING: &str = "GPGGA,092725.00,,,,,0,05,99.99,,,,,,";
    /// The same before the receiver knows the time or any precision.
    const GGA_COLD: &str = "GNGGA,,,,,,0,00,,,M,,M,,";

    #[test]
    fn a_gga_searching_for_a_fix_publishes_its_satellite_count() {
        let mut fix = GnssFix::empty();
        fold_sentence(&mut fix, RMC_VALID);
        fold_sentence(&mut fix, GGA_FIX);
        let before = fix;
        fold_sentence(&mut fix, GGA_SEARCHING);
        assert!(!fix.valid, "a searching receiver has no fix");
        assert_eq!(fix.sat_in_use, 5);
        assert_eq!(fix.latitude, before.latitude);
        assert_eq!(fix.hdop, before.hdop);
        fold_sentence(&mut fix, GGA_COLD);
        assert_eq!(fix.sat_in_use, 0);
    }

    /// A receiver that has lost its fix blanks position, speed and course
    /// and keeps the time and date it still knows (u-blox, MediaTek).
    const RMC_LOST_FIX: &str = "GPRMC,092725.00,V,,,,,,,110126,,,N";
    /// The same before the receiver knows the time or the date.
    const RMC_COLD: &str = "GNRMC,,V,,,,,,,,,,N";

    #[test]
    fn a_void_rmc_with_blank_fields_clears_the_time() {
        let mut fix = GnssFix::empty();
        fold_sentence(&mut fix, RMC_VALID);
        let before = fix;
        fold_sentence(&mut fix, RMC_LOST_FIX);
        assert!(!fix.valid, "a void RMC is no fix");
        assert_eq!(
            fix.unix_secs, None,
            "a receiver that lost its fix no longer vouches for the time"
        );
        assert_eq!(fix.latitude, before.latitude);
        assert_eq!(fix.longitude, before.longitude);
        assert_eq!(fix.speed_mps, before.speed_mps);

        fold_sentence(&mut fix, RMC_VALID);
        fold_sentence(&mut fix, RMC_COLD);
        assert_eq!(fix.unix_secs, None);
    }

    #[test]
    fn a_gga_with_a_fix_sets_position_altitude_and_hdop() {
        let mut fix = GnssFix::empty();
        fix.fold_gga(&gga(GGA_FIX));
        assert!(fix.valid);
        assert_eq!(fix.sat_in_use, 8);
        assert!((fix.latitude.unwrap() - 48.1173).abs() < 1e-4);
        assert_eq!(fix.altitude_m, Some(545.4));
        assert_eq!(fix.hdop, Some(0.9));
        assert_eq!(fix.unix_secs, None, "GGA carries no date and sets no time");
    }

    #[test]
    fn a_gga_without_a_fix_counts_satellites_and_keeps_the_rest() {
        let mut fix = GnssFix::empty();
        fix.fold_rmc(&rmc(RMC_VALID));
        fix.fold_gga(&gga(GGA_FIX));
        let before = fix;
        fix.fold_gga(&gga(GGA_NO_FIX));
        assert!(!fix.valid);
        assert_eq!(fix.sat_in_use, 3);
        assert_eq!(fix.latitude, before.latitude);
        assert_eq!(fix.altitude_m, before.altitude_m);
        assert_eq!(fix.hdop, before.hdop);
        assert_eq!(
            fix.unix_secs, before.unix_secs,
            "only an RMC clears the time"
        );
    }
}
