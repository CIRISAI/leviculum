//! The board's side of the cadence seam (#413).
//!
//! [`AnnounceCadence`](crate::AnnounceCadence) decides when an announce is
//! due. What it is fed, and what happens to its answer, used to be decided
//! inline in the board's announce scheduler (`leviculum-nrf/src/announce.rs`):
//! which destinations count as new neighbours, what position counts as
//! movement evidence, when the movement proof is sampled, and what a board
//! without a delivery destination does with a due slot. Each needed a
//! moving fix or thirty-three announcers to be seen on the rig; here each
//! is a function of what the board reads.
//!
//! # Where the open cadence decision plugs in
//!
//! How fast a board announces when it is still, when it moves, and what
//! counts as moving is not decided yet (300 s mobile, about an hour
//! stationary, a motion sensor instead of a GPS delta). Whatever is
//! decided lands in two places: the intervals, in
//! [`AnnounceCadence::configured_interval_ms`](crate::AnnounceCadence::configured_interval_ms),
//! and the evidence, in [`movement_fix`]. A motion sensor replaces the
//! input of `movement_fix`, sampled by [`MovementSampler`] and handed to
//! [`AnnounceCadence::note_fix`](crate::AnnounceCadence::note_fix); the
//! board's wiring does not change.

use leviculum_telemetry_policy::Fix;

use crate::{Decision, MOVEMENT_SAMPLE_INTERVAL_MS};

/// The destinations this boot has heard an announce from, as a fixed
/// table with no allocation: a board with no heap left must still be able
/// to answer "have I heard this one before".
#[derive(Debug, Clone)]
pub struct NeighbourTable<const N: usize> {
    slots: [Option<[u8; 16]>; N],
    next: usize,
}

impl<const N: usize> Default for NeighbourTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> NeighbourTable<N> {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [None; N],
            next: 0,
        }
    }

    /// Record a neighbour. `true` when it was not already known, which is
    /// the edge rule 4 fires on. Overflow displaces in round-robin order,
    /// the cheapest bound that never denies a genuinely new neighbour: the
    /// displaced one buys one more announce when it is heard again.
    pub fn insert(&mut self, destination: [u8; 16]) -> bool {
        if N == 0 {
            return true;
        }
        if self.slots.contains(&Some(destination)) {
            return false;
        }
        self.slots[self.next] = Some(destination);
        self.next = (self.next + 1) % N;
        true
    }
}

/// What the board's GNSS receiver last said, as the movement proof reads
/// it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GnssReading {
    /// The receiver's own validity flag for the last sentence.
    pub valid: bool,
    /// Decimal degrees; the receiver keeps the last good value across
    /// non-valid sentences.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Horizontal dilution of precision.
    pub hdop: Option<f32>,
}

/// The board's position as movement evidence, or `None` when there is
/// nothing to judge.
///
/// `None` in three cases, and each is deliberate:
///
/// * a user has pinned a fixed position — that pin is an assertion that
///   this board does not move and it replaces the sensor entirely
///   ([`leviculum_telemetry_policy::choose_position`]), so the sensor is
///   not consulted behind the user's back;
/// * no GNSS receiver in this binary, or no reading yet (`gnss` is
///   `None`) — a board that cannot observe its position has not proved
///   movement, and the floor is the safe answer;
/// * the receiver reports `valid=false`, or a coordinate is missing — the
///   receiver keeps the last good coordinates across non-valid sentences
///   so the display has something to render, and a stale position offered
///   as evidence is how a board invents a jump it never made.
#[must_use]
pub fn movement_fix(fixed_pinned: bool, gnss: Option<GnssReading>) -> Option<Fix> {
    if fixed_pinned {
        return None;
    }
    let reading = gnss?;
    if !reading.valid {
        return None;
    }
    Fix::from_degrees(reading.latitude, reading.longitude, reading.hdop)
}

/// When the movement proof is next owed a position sample.
///
/// Separate from the announce deadline because the two are hours apart
/// on a still board: a detector polled only when an announce is due would
/// never see the board start moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovementSampler {
    next_sample_ms: u64,
}

impl MovementSampler {
    /// The first sample one interval after `now_ms` (boot).
    #[must_use]
    pub const fn new(now_ms: u64) -> Self {
        Self {
            next_sample_ms: now_ms.saturating_add(MOVEMENT_SAMPLE_INTERVAL_MS),
        }
    }

    /// Whether a sample is due at `now_ms`; when it is, the next one is
    /// booked one interval from now.
    pub fn take(&mut self, now_ms: u64) -> bool {
        if now_ms < self.next_sample_ms {
            return false;
        }
        self.next_sample_ms = now_ms.saturating_add(MOVEMENT_SAMPLE_INTERVAL_MS);
        true
    }

    /// How long until the next sample is due.
    #[must_use]
    pub const fn wait_ms(&self, now_ms: u64) -> u64 {
        self.next_sample_ms.saturating_sub(now_ms)
    }
}

/// What the board's own periodic slot does with a polled decision.
///
/// The slot is polled first and the destination checked second, so a
/// board with no delivery destination still spends its due slot: the
/// deadline moves on and nothing is sent, rather than the slot standing
/// due and being polled again on every wake.
///
/// Returns the decision with the destination it is for, or `None` when
/// either is missing.
#[must_use]
pub fn own_announce<D>(polled: Option<Decision>, destination: Option<D>) -> Option<(Decision, D)> {
    Some((polled?, destination?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnnounceCadence, AnnounceSlot, MOVING_ANNOUNCE_INTERVAL_MS};
    use leviculum_telemetry_policy::PolicyParams;

    /// The board's table size (`NEIGHBOUR_SLOTS` in `leviculum-nrf/src/announce.rs`).
    const BOARD_SLOTS: usize = 32;

    fn dest(n: u8) -> [u8; 16] {
        [n; 16]
    }

    #[test]
    fn a_known_neighbour_buys_nothing() {
        let mut table = NeighbourTable::<BOARD_SLOTS>::new();
        assert!(table.insert(dest(1)));
        assert!(!table.insert(dest(1)));
    }

    #[test]
    fn a_thirty_third_neighbour_displaces_the_first_round_robin() {
        let mut table = NeighbourTable::<BOARD_SLOTS>::new();
        for n in 0..33u8 {
            assert!(table.insert(dest(n)), "neighbour {n} is new");
        }
        // 32 is in slot 0, where 0 was; 1..=32 are all still known.
        for n in 1..33u8 {
            assert!(!table.insert(dest(n)), "neighbour {n} is still known");
        }
        assert!(table.insert(dest(0)), "the displaced first buys again");
        assert!(table.insert(dest(1)), "and displaced the second in turn");
    }

    fn walking(north_m: f64) -> Option<GnssReading> {
        Some(GnssReading {
            valid: true,
            latitude: Some(53.0 + north_m / 111_320.0),
            longitude: Some(9.0),
            hdop: Some(1.0),
        })
    }

    #[test]
    fn a_fixed_pin_offers_no_movement_evidence_whatever_the_receiver_says() {
        assert_eq!(movement_fix(true, walking(0.0)), None);
        assert_eq!(movement_fix(true, None), None);
    }

    #[test]
    fn a_board_without_gnss_offers_no_movement_evidence() {
        assert_eq!(movement_fix(false, None), None);
    }

    #[test]
    fn an_invalid_fix_offers_no_evidence_even_with_coordinates_kept() {
        let stale = walking(0.0).map(|r| GnssReading { valid: false, ..r });
        assert_eq!(movement_fix(false, stale), None);
    }

    #[test]
    fn a_valid_fix_without_latitude_or_longitude_offers_none() {
        let reading = walking(0.0).map(|r| GnssReading {
            latitude: None,
            ..r
        });
        assert_eq!(movement_fix(false, reading), None);
        let reading = walking(0.0).map(|r| GnssReading {
            longitude: None,
            ..r
        });
        assert_eq!(movement_fix(false, reading), None);
    }

    #[test]
    fn a_valid_fix_is_evidence_with_its_hdop() {
        let fix = movement_fix(false, walking(0.0));
        assert_eq!(fix.map(|f| f.hdop_e2), Some(Some(100)));
        assert_eq!(fix.map(|f| f.longitude_e6), Some(9_000_000));
    }

    #[test]
    fn the_movement_proof_is_sampled_once_per_interval() {
        let t0 = 1_000;
        let mut sampler = MovementSampler::new(t0);
        assert!(!sampler.take(t0));
        assert_eq!(sampler.wait_ms(t0), MOVEMENT_SAMPLE_INTERVAL_MS);
        let due = t0 + MOVEMENT_SAMPLE_INTERVAL_MS;
        assert!(!sampler.take(due - 1));
        assert!(sampler.take(due));
        assert!(!sampler.take(due), "booked one interval on");
        // A late wake books from the moment it was taken, not the old slot.
        let late = due + MOVEMENT_SAMPLE_INTERVAL_MS + 7_000;
        assert!(sampler.take(late));
        assert_eq!(sampler.wait_ms(late), MOVEMENT_SAMPLE_INTERVAL_MS);
    }

    /// The board's loop: every sample feeds the cadence, and a resume
    /// brings the own slot forward to the sample that proved it.
    #[test]
    fn a_resume_after_movement_announces_at_the_sample_that_proved_it() {
        let t0 = 1_700_000_000_000u64;
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0);
        let mut sampler = MovementSampler::new(t0);
        let mut now = t0;
        // Stand still past the settle window, spending the first announce.
        while now < t0 + PolicyParams::TRACKER.settle_ms + MOVEMENT_SAMPLE_INTERVAL_MS {
            now += MOVEMENT_SAMPLE_INTERVAL_MS;
            assert!(sampler.take(now));
            assert!(!cadence.note_fix(now, movement_fix(false, walking(0.0))));
            cadence.poll(AnnounceSlot::Own, now, true);
        }
        assert_eq!(cadence.poll(AnnounceSlot::Own, now, true), None);
        // Walk 100 m a sample until the proof holds.
        let mut resumed_at = None;
        for step in 1..=10 {
            now += MOVEMENT_SAMPLE_INTERVAL_MS;
            assert!(sampler.take(now));
            let fix = movement_fix(false, walking(step as f64 * 100.0));
            if cadence.note_fix(now, fix) {
                resumed_at = Some(now);
                break;
            }
        }
        let resumed_at = resumed_at.expect("a walk proves movement");
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, resumed_at, true),
            Some(Decision::Announce),
            "the resume announces at once, not at the next tick"
        );
        assert_eq!(
            cadence.wait_ms(AnnounceSlot::Own, resumed_at),
            MOVING_ANNOUNCE_INTERVAL_MS
        );
    }

    /// The same walk behind a fixed pin proves nothing.
    #[test]
    fn a_pinned_board_that_is_carried_never_resumes() {
        let t0 = 1_700_000_000_000u64;
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0);
        let mut now = t0;
        for step in 0..30 {
            now += MOVEMENT_SAMPLE_INTERVAL_MS;
            assert!(!cadence.note_fix(now, movement_fix(true, walking(step as f64 * 100.0))));
        }
        assert!(!cadence.is_moving(now));
    }

    #[test]
    fn a_board_without_a_delivery_destination_spends_its_slot_and_sends_nothing() {
        let t0 = 1_700_000_000_000u64;
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0);
        let due = t0 + crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS;
        let polled = cadence.poll(AnnounceSlot::Own, due, true);
        assert_eq!(polled, Some(Decision::Announce));
        assert_eq!(own_announce::<u8>(polled, None), None);
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, due + 1, true),
            None,
            "the slot is spent, not left due"
        );
        assert_eq!(
            own_announce(polled, Some(7u8)),
            Some((Decision::Announce, 7))
        );
        assert_eq!(own_announce(None, Some(7u8)), None);
    }
}
