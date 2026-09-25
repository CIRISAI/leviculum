#![no_std]
//! How long a host's transmit mute may last on an LNode, and when the board
//! takes its own voice back.
//!
//! # The defect this crate closes (leviculum#410)
//!
//! `RadioConfigWire::radio_silent` is a mute with no end. The firmware drops
//! every outgoing frame while it is set, and the only two things that clear
//! it are a reset and a later config that says `radio_silent = false`. On
//! 2026-09-15 the periculum corpus ended on an RNode-only cell, the three
//! LNodes it had muted were never handed back, and the rig stood mute for
//! seven hours with `[MEDIA] lora=on`, a live receive loop and a climbing
//! transport counter.
//!
//! The harness has since been taught to hand the bench back. That is a
//! promise made by the process that issued the mute, and it is worth exactly
//! as much as that process's survival: a `kill -9`, a host reboot or an ssh
//! session that dies mid-run leaves a board holding a silence nobody
//! remembers issuing. A control frame that silences field hardware until
//! somebody power-cycles it **fails mute**. It has to fail safe.
//!
//! So the silence carries a lease. The board is mute for at most the lease,
//! counted from the last config that said silent; a harness that wants a
//! longer silence re-sends the config, which is what a harness that is still
//! alive does anyway.
//!
//! # Why a crate and not three lines in `lora.rs`
//!
//! `leviculum-nrf` cross-compiles to `thumbv7em-none-eabihf` and runs no host
//! tests, so a deadline expressed there is a deadline nothing can assert.
//! Two things here are decisions rather than I/O — what the default lease is,
//! and what "expired" means at the boundary millisecond — and both are the
//! kind that is wrong by one and fails silently. See
//! `docs/src/concepts/firmware-host-test-seam.md`.

/// The longest single scenario in the periculum hardware corpus, in seconds:
/// `hardware/lora_lnode_path_soak.toml`, `[test] timeout_secs = 4200`. The
/// runner-up is `lora_pn_board_offer_past_the_link.toml` at 3600.
///
/// This is the number [`DEFAULT_LEASE_S`] has to clear, and the reason is in
/// how the mute is issued: `runner::silence_unused_lnode` pushes
/// `radio_silent = true` at every discovered board the *current scenario*
/// did not bind, once per scenario. So the longest a muted board ever goes
/// without hearing from the harness again is one scenario — and a lease that
/// expires inside one would un-mute a board in the middle of somebody else's
/// measurement, which is the harm the mute exists to prevent.
///
/// Read off `hardware/*.toml` in the periculum tree on 2026-09-25;
/// `tests/default_lease.rs` states the check that keeps it honest.
pub const LONGEST_HARDWARE_SCENARIO_S: u32 = 4_200;

/// What the firmware uses when the host sends `0`, or sends the short frame
/// that predates the lease field: [`LONGEST_HARDWARE_SCENARIO_S`] plus half
/// again, 105 minutes.
///
/// `0` means *this*, and never "forever" — that is the whole point of the
/// default, and it is what lets today's periculum and today's `lnsd` keep
/// sending the frame they always sent and still get a board that recovers.
///
/// The margin is half the longest scenario rather than a round number
/// because what it has to cover is not the scenario's declared timeout but
/// its whole wall clock: the harness's container start, the flash, the
/// settle sleep and the teardown all fall between two consecutive silences
/// of the same board, and none of them is inside `timeout_secs`. Half of the
/// longest declared budget is more than the corpus has ever spent on that
/// overhead, and the cost of being generous here is bounded — it is a
/// recovery deadline, not a spacing.
pub const DEFAULT_LEASE_S: u16 = 6_300;

/// Milliseconds in a second, as the lease arithmetic spells it.
const MS_PER_S: u64 = 1_000;

/// The deadline on a host's transmit mute.
///
/// Not muted until [`grant`](Self::grant) says so, which is what a board that
/// nobody has silenced comes up as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MuteLease {
    /// When the current lease runs out, in the caller's millisecond clock.
    /// `None` is "no mute in force", which is both the initial state and
    /// what [`expired_at`](Self::expired_at) and [`clear`](Self::clear)
    /// leave behind.
    expires_at_ms: Option<u64>,
    /// The lease currently in force, in seconds — or, once it has run out,
    /// the one that just did. It outlives `expires_at_ms` on purpose: the
    /// expiry line has to be able to say *how long* the silence was for, and
    /// the only place that number still exists at that moment is here.
    lease_s: u16,
}

impl MuteLease {
    /// A board nobody has muted.
    pub const fn new() -> Self {
        Self {
            expires_at_ms: None,
            lease_s: 0,
        }
    }

    /// Mute the board for `lease_s` seconds from `now_ms`, or for
    /// [`DEFAULT_LEASE_S`] when `lease_s` is zero.
    ///
    /// Re-granting restarts the deadline rather than extending it, which is
    /// what makes "the harness re-sends the config" a working way to hold a
    /// board silent for longer than one lease.
    pub fn grant(&mut self, now_ms: u64, lease_s: u16) {
        let lease_s = if lease_s == 0 {
            DEFAULT_LEASE_S
        } else {
            lease_s
        };
        self.lease_s = lease_s;
        // Saturating for form's sake: a u16 of seconds is at most 65_535_000
        // ms, which cannot carry any plausible uptime past u64.
        self.expires_at_ms = Some(now_ms.saturating_add(u64::from(lease_s) * MS_PER_S));
    }

    /// Drop the mute outright, with no expiry to report.
    ///
    /// This is what a config carrying `radio_silent = false` does: the host
    /// has said the board may transmit, so there is no deadline left to wait
    /// out and nothing for [`expired_at`](Self::expired_at) to announce —
    /// the unmute has a line of its own.
    pub fn clear(&mut self) {
        self.expires_at_ms = None;
        self.lease_s = 0;
    }

    /// Is the board mute at `now_ms`?
    ///
    /// Strictly before the deadline: at exactly `grant_ms + lease_s * 1000`
    /// the lease is over. A lease of `n` seconds buys `n` seconds of silence
    /// and not a millisecond more, which is the only reading of "at most the
    /// lease" that a caller can hold this type to.
    pub fn is_muted(&self, now_ms: u64) -> bool {
        match self.expires_at_ms {
            Some(expires_at_ms) => now_ms < expires_at_ms,
            None => false,
        }
    }

    /// Has the lease just run out? `Some(deadline_ms)` exactly once per
    /// lease, on the first call at or after the deadline.
    ///
    /// Edge-triggered, and that is the whole reason it takes `&mut self`:
    /// the caller logs `LORA_MUTE_EXPIRED` from it, and the caller is a loop
    /// that comes round several times a second. A level-triggered answer
    /// would put that line in the log once per turn for the rest of the
    /// board's uptime, and every firmware line also goes into the 2 KiB
    /// post-crash tail.
    ///
    /// The deadline rather than `now_ms` is returned because the two differ
    /// by however long the loop was parked in a receive window — up to a
    /// minute — and the fact worth recording is when the silence was over,
    /// not when the board next looked.
    pub fn expired_at(&mut self, now_ms: u64) -> Option<u64> {
        match self.expires_at_ms {
            Some(expires_at_ms) if now_ms >= expires_at_ms => {
                self.expires_at_ms = None;
                Some(expires_at_ms)
            }
            _ => None,
        }
    }

    /// The lease in force, in seconds — or, after it ran out, the one that
    /// did. Zero when no mute was ever granted or the mute was cleared.
    pub fn lease_s(&self) -> u16 {
        self.lease_s
    }

    /// Whole seconds left on the lease at `now_ms`, rounded down; zero when
    /// the board is not mute.
    ///
    /// For the `[LORA] active config` line, where it answers the question a
    /// reader of a mute board actually has — not "is it silent" but "for how
    /// much longer".
    pub fn remaining_s(&self, now_ms: u64) -> u16 {
        match self.expires_at_ms {
            Some(expires_at_ms) if now_ms < expires_at_ms => {
                // Cannot exceed the granted lease, which is a u16.
                ((expires_at_ms - now_ms) / MS_PER_S) as u16
            }
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The boundary, both sides of it: muted at one millisecond before the
    /// deadline, free at the deadline itself.
    #[test]
    fn a_lease_holds_to_its_last_millisecond_and_not_past_it() {
        let mut lease = MuteLease::new();
        lease.grant(1_000, 60);
        assert!(lease.is_muted(1_000));
        assert!(lease.is_muted(1_000 + 60_000 - 1));
        assert!(!lease.is_muted(1_000 + 60_000));
        assert!(!lease.is_muted(1_000 + 60_001));
    }

    /// A board nobody muted transmits, and says nothing about a lease.
    #[test]
    fn an_ungranted_lease_mutes_nothing() {
        let mut lease = MuteLease::new();
        assert!(!lease.is_muted(0));
        assert!(!lease.is_muted(u64::MAX));
        assert_eq!(lease.lease_s(), 0);
        assert_eq!(lease.remaining_s(0), 0);
        assert_eq!(lease.expired_at(u64::MAX), None);
    }

    /// Zero is the firmware default, never "forever". This is the case every
    /// host that predates the field lands in.
    #[test]
    fn a_zero_lease_is_the_firmware_default() {
        let mut lease = MuteLease::new();
        lease.grant(0, 0);
        assert_eq!(lease.lease_s(), DEFAULT_LEASE_S);
        assert!(lease.is_muted(u64::from(DEFAULT_LEASE_S) * 1_000 - 1));
        assert!(!lease.is_muted(u64::from(DEFAULT_LEASE_S) * 1_000));
    }

    /// The expiry fires once. A loop that asks every turn gets one answer,
    /// not one per turn.
    #[test]
    fn an_expiry_is_reported_exactly_once() {
        let mut lease = MuteLease::new();
        lease.grant(5_000, 10);
        assert_eq!(lease.expired_at(5_000), None);
        assert_eq!(lease.expired_at(14_999), None);
        assert_eq!(lease.expired_at(15_000), Some(15_000));
        assert_eq!(lease.expired_at(15_001), None);
        assert_eq!(lease.expired_at(u64::MAX), None);
        assert!(!lease.is_muted(15_000));
        // The line the caller writes still knows how long the silence was.
        assert_eq!(lease.lease_s(), 10);
    }

    /// The deadline, not the moment the loop noticed it. The two differ by a
    /// whole receive window, and the log wants the former.
    #[test]
    fn the_expiry_reports_the_deadline_and_not_the_observation() {
        let mut lease = MuteLease::new();
        lease.grant(0, 60);
        // The loop was parked in a 60 s idle listen and comes back late.
        assert_eq!(lease.expired_at(118_000), Some(60_000));
    }

    /// Re-granting restarts the deadline. This is how a live harness keeps a
    /// board silent across a run longer than one lease.
    #[test]
    fn a_regrant_restarts_the_deadline_rather_than_extending_it() {
        let mut lease = MuteLease::new();
        lease.grant(0, 60);
        lease.grant(30_000, 60);
        assert!(lease.is_muted(89_999));
        assert!(!lease.is_muted(90_000));
        assert_eq!(lease.expired_at(90_000), Some(90_000));
    }

    /// A config that says the board may transmit ends the mute outright, and
    /// leaves no expiry for anyone to announce.
    #[test]
    fn clearing_ends_the_mute_without_an_expiry() {
        let mut lease = MuteLease::new();
        lease.grant(0, 900);
        lease.clear();
        assert!(!lease.is_muted(0));
        assert_eq!(lease.expired_at(u64::MAX), None);
        assert_eq!(lease.lease_s(), 0);
        assert_eq!(lease.remaining_s(0), 0);
    }

    /// What the status line prints, rounded down, and zero once the board can
    /// transmit again.
    #[test]
    fn the_remaining_seconds_count_down_and_stop_at_zero() {
        let mut lease = MuteLease::new();
        lease.grant(1_000, 900);
        assert_eq!(lease.remaining_s(1_000), 900);
        assert_eq!(lease.remaining_s(489_000), 412);
        assert_eq!(lease.remaining_s(900_999), 0);
        assert_eq!(lease.remaining_s(901_000), 0);
        assert_eq!(lease.remaining_s(u64::MAX), 0);
    }

    /// The arithmetic cannot overflow at the widest lease a `u16` can carry,
    /// at any uptime the board can reach. `u64` milliseconds is 584 million
    /// years; the saturating add is there so a caller passing `u64::MAX`
    /// gets a mute that ends rather than a panic in a release build's
    /// debug-assertions cousin.
    #[test]
    fn the_widest_lease_cannot_overflow_the_clock() {
        let mut lease = MuteLease::new();
        lease.grant(u64::MAX, u16::MAX);
        assert_eq!(lease.lease_s(), u16::MAX);
        assert!(lease.is_muted(u64::MAX - 1));
        assert!(!lease.is_muted(u64::MAX));

        // And at a plausible uptime, a year of milliseconds, nothing wraps.
        let a_year_ms = 365 * 24 * 60 * 60 * 1_000;
        lease.grant(a_year_ms, u16::MAX);
        assert!(lease.is_muted(a_year_ms + 65_534_999));
        assert!(!lease.is_muted(a_year_ms + 65_535_000));
    }
}
