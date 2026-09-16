//! What the board reports as its physical link (Codeberg #236).
//!
//! The radio measures rssi and snr on every reception and has done since
//! the driver existed; the Telemeter carries them
//! (`leviculum_lxmf::telemetry::PhysicalLink`); nothing assembled them
//! into a report. What was missing was not the numbers but the rule for
//! turning a stream of them into one reading, and that rule is policy —
//! the same argument that puts the cadence in this crate rather than in
//! the firmware, where no host test can reach it.
//!
//! # What the reading means
//!
//! **The last frame this board received, and nothing else.** Not a mean
//! over a window and not the best of one:
//!
//! * A mean mixes neighbours. On a mesh the previous frame is as likely
//!   to have come from a different peer at a different distance as from
//!   the same one, so the average of a window is the average of a
//!   population nobody asked about — and it moves *down* when a distant
//!   node joins, which reads on a viewer as the near link degrading.
//! * The best of a window answers "how good can it get", which is the
//!   question a link budget asks, not the one a status row asks.
//! * The last frame is what the references report under this name.
//!   `RNS.Link.rssi`/`.snr` are assigned from each received packet and
//!   keep the last one (`reference/Reticulum/RNS/Link.py:837-840`), and
//!   `RNodeInterface.r_stat_rssi`/`r_stat_snr` are the figures of the
//!   frame the KISS stat bytes arrived with (`RNodeInterface.py:877-880`).
//!   A Python-RNS peer on the same mesh therefore means by "rssi" what we
//!   mean by it.
//!
//! # When there is no reading
//!
//! A board that has heard nothing recently reports **no physical-link
//! sensor at all**, rather than the last thing it heard an hour ago.
//! [`FRESH_MS`] is that line, and [`reading`] is where it is drawn.
//!
//! # Quality
//!
//! `q` is emitted because there is a definition to emit, not because the
//! field exists: [`quality_percent`] is Reticulum's own SNR-to-quality
//! map (`RNodeInterface.py:882-890`), the number `rnstatus` prints for
//! an RNode. Inventing a second scale under the same name would make our
//! `q` and a Python peer's `q` two different quantities with one label.

use crate::PolicyParams;

/// One reception, as the radio's packet status measured it.
///
/// The spreading factor travels with the frame rather than being read at
/// report time: [`quality_percent`] is a function of the PHY as well as
/// the snr, and a board reconfigured between the reception and the report
/// would otherwise restate an old frame's quality on the new PHY's scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reception {
    /// Received signal strength of the frame, dBm.
    pub rssi_dbm: i16,
    /// Signal-to-noise ratio of the frame, whole dB.
    pub snr_db: i16,
    /// The spreading factor the receiver was running when it arrived.
    pub spreading_factor: u8,
    /// Monotonic milliseconds at which the frame was handed up.
    pub at_ms: u64,
}

/// The physical-link reading one report carries: the wire's three slots,
/// with `q` absent when the PHY has no defined quality scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub rssi_dbm: i16,
    pub snr_db: i16,
    /// Link quality, 0-100, per [`quality_percent`].
    pub quality_percent: Option<u8>,
}

/// How old the last reception may be and still be reported.
///
/// The tracker heartbeat, which is the *fastest* cadence any profile
/// reports on ([`PolicyParams::TRACKER`]). A reading older than that was
/// already carried by the previous report of a tracker, so repeating it
/// would say "still hearing this" about a window in which the board heard
/// nothing — which is exactly the stale reading this bound exists to
/// suppress. A station reports hourly and will therefore omit the sensor
/// unless it heard a frame in the last quarter of its own interval; on a
/// mesh whose neighbours announce at all, it will have.
pub const FRESH_MS: u64 = PolicyParams::TRACKER.max_interval_ms;

/// The reading a report should carry, or `None` for no sensor at all:
/// nothing has ever been received, or the last reception is older than
/// [`FRESH_MS`].
pub fn reading(last: Option<Reception>, now_ms: u64) -> Option<Reading> {
    let last = last?;
    // `saturating_sub`, so a reception stamped in the future (a clock the
    // caller read on the other side of a wake) counts as fresh rather
    // than as 584 million years old.
    if now_ms.saturating_sub(last.at_ms) > FRESH_MS {
        return None;
    }
    Some(Reading {
        rssi_dbm: last.rssi_dbm,
        snr_db: last.snr_db,
        quality_percent: quality_percent(last.snr_db, last.spreading_factor),
    })
}

/// Reticulum's own SNR-to-quality map, `RNodeInterface.py:882-890`:
///
/// ```text
/// q_snr_min = Q_SNR_MIN_BASE - (sf - 7) * Q_SNR_STEP
/// quality   = clamp(((snr - q_snr_min) / (Q_SNR_MAX - q_snr_min)) * 100, 0, 100)
/// ```
///
/// with `Q_SNR_MIN_BASE = -9`, `Q_SNR_MAX = 6`, `Q_SNR_STEP = 2`
/// (`RNodeInterface.py:124-126`). The floor drops 2 dB per spreading
/// factor because a higher SF decodes further below the noise, so the
/// same -5 dB is a poor link at SF7 and a good one at SF12.
///
/// `None` for a spreading factor outside 7-12, which is the range the
/// config validator admits (`leviculum_core::rnode`, SF5/SF6 are refused
/// for want of a carrier-detect threshold). A `q` computed for a PHY the
/// radio cannot be running would be a number without a link behind it.
///
/// **Whole percent, where the reference emits one decimal.** The decimal
/// there is carried by a snr in 0.25 dB steps; ours is whole dB out of
/// `packet_status_dbm`, so a tenth of a percent here would be precision
/// this board does not have. Rounding is half-up on a non-negative
/// numerator, the nearest integer to what the reference would print.
pub fn quality_percent(snr_db: i16, spreading_factor: u8) -> Option<u8> {
    if !(7..=12).contains(&spreading_factor) {
        return None;
    }
    const Q_SNR_MIN_BASE: i32 = -9;
    const Q_SNR_MAX: i32 = 6;
    const Q_SNR_STEP: i32 = 2;
    let snr_min = Q_SNR_MIN_BASE - (spreading_factor as i32 - 7) * Q_SNR_STEP;
    let span = Q_SNR_MAX - snr_min;
    let above = (snr_db as i32 - snr_min).clamp(0, span);
    Some(((above * 100 + span / 2) / span) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heard(snr_db: i16, at_ms: u64) -> Reception {
        Reception {
            rssi_dbm: -92,
            snr_db,
            spreading_factor: 8,
            at_ms,
        }
    }

    /// A board that has never received anything carries no sensor. This
    /// is the boot state of every board and the standing state of one
    /// whose antenna fell off.
    #[test]
    fn nothing_ever_received_is_no_sensor() {
        assert_eq!(reading(None, 900_000), None);
    }

    /// The freshness bound, at both sides of the edge. A reading exactly
    /// `FRESH_MS` old is still reported; one millisecond older is not.
    #[test]
    fn a_reception_older_than_the_window_is_no_sensor() {
        let last = Some(heard(2, 1_000));
        assert!(reading(last, 1_000 + FRESH_MS).is_some());
        assert_eq!(reading(last, 1_000 + FRESH_MS + 1), None);
    }

    /// The reported figures are the last frame's, not a blend with the
    /// frame before it: two receptions in the same window report the
    /// second one's numbers and nothing of the first's.
    #[test]
    fn the_reading_is_the_last_frame_and_not_a_mean() {
        let near = Reception {
            rssi_dbm: -40,
            snr_db: 10,
            spreading_factor: 8,
            at_ms: 1_000,
        };
        let far = Reception {
            rssi_dbm: -120,
            snr_db: -8,
            spreading_factor: 8,
            at_ms: 2_000,
        };
        let r = reading(Some(far), 3_000).unwrap();
        assert_eq!(r.rssi_dbm, far.rssi_dbm);
        assert_eq!(r.snr_db, far.snr_db);
        // And the other order, so the test cannot pass by reporting a
        // constant: the near frame's numbers, unaveraged with the far.
        let r = reading(Some(near), 3_000).unwrap();
        assert_eq!(r.rssi_dbm, -40);
        assert_eq!(r.snr_db, 10);
    }

    /// The reference's own worked values, computed by hand from
    /// `RNodeInterface.py:882-890` at SF8: `q_snr_min = -9 - 1*2 = -11`,
    /// span 17. Python prints 0.0, 64.7 and 100.0 for these three.
    #[test]
    fn quality_matches_the_reticulum_formula_at_sf8() {
        assert_eq!(quality_percent(-11, 8), Some(0));
        assert_eq!(quality_percent(0, 8), Some(65)); // 11/17 = 64.7 %
        assert_eq!(quality_percent(6, 8), Some(100));
    }

    /// The floor moves with the spreading factor, which is the whole
    /// point of the formula: the same -9 dB is unusable at SF7 and
    /// respectable at SF12.
    #[test]
    fn the_floor_drops_two_db_per_spreading_factor() {
        assert_eq!(quality_percent(-9, 7), Some(0));
        // SF12: q_snr_min = -9 - 5*2 = -19, span 25, (-9 + 19)/25 = 40 %.
        assert_eq!(quality_percent(-9, 12), Some(40));
    }

    /// Both ends clamp, as the reference's two `if` lines do: a signal
    /// below the floor is 0 and one above `Q_SNR_MAX` is 100, never a
    /// negative or a 137.
    #[test]
    fn quality_clamps_at_both_ends() {
        assert_eq!(quality_percent(-32, 7), Some(0));
        assert_eq!(quality_percent(32, 7), Some(100));
        for sf in 7..=12 {
            for snr in -40..=40 {
                let q = quality_percent(snr, sf).unwrap();
                assert!(q <= 100, "sf {sf} snr {snr} gave {q}");
            }
        }
    }

    /// A spreading factor the radio cannot be running yields no `q`, and
    /// the rssi and snr are still reported — the slot is nil-able on the
    /// wire precisely so a missing quality does not cost the two
    /// measurements beside it.
    #[test]
    fn an_unsupported_spreading_factor_drops_q_and_keeps_the_rest() {
        assert_eq!(quality_percent(0, 6), None);
        assert_eq!(quality_percent(0, 13), None);
        let last = Reception {
            rssi_dbm: -77,
            snr_db: 3,
            spreading_factor: 6,
            at_ms: 0,
        };
        let r = reading(Some(last), 1_000).unwrap();
        assert_eq!(r.rssi_dbm, -77);
        assert_eq!(r.snr_db, 3);
        assert_eq!(r.quality_percent, None);
    }
}
