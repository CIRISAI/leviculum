//! One announce rule for every board (Codeberg #401).
//!
//! Before this module a board's announce cadence was a constant, and the
//! plan on the tracker was to pick it at flash time: a stationary profile
//! and a mobile one. That choice is gone, with its failure mode — a
//! hundred boards mounted on trees carrying the hiking profile, which
//! nobody notices until the channel is full. A board observes whether it
//! moves, so it can decide for itself.
//!
//! The rule, in the order a board applies it:
//!
//! 1. **Moving** — [`MOVING_ANNOUNCE_INTERVAL_MS`].
//! 2. **Not moving** — [`STILL_ANNOUNCE_INTERVAL_MS`]. Slow, never
//!    silent.
//! 3. **Movement resumes** — announce at once, do not wait for the tick.
//! 4. **A previously unknown neighbour is heard** — announce at once.
//! 5. **The PHY makes the cadence unaffordable** — stretch it, whatever
//!    states 1 to 4 say ([`DutyBudget`]).
//!
//! Both halves of the announce, the board's own delivery destination and
//! the propagation role, run off ONE [`AnnounceCadence`]. They are
//! decided together because a board whose own announce is withheld is
//! unreachable as a recipient while still usable as a mailbox, and
//! splitting the cadence between them would hide that asymmetry. The two
//! [`AnnounceSlot`]s keep separate deadlines only so the two frames do
//! not contend for the same airtime window at boot.
//!
//! The movement detector is not a second detector: it reads
//! [`leviculum_telemetry_policy::PolicyParams::TRACKER`] — `min_distance_m`,
//! `max_hdop_e2`, `settle_ms` — and calls that crate's own
//! [`moved_at_least`](leviculum_telemetry_policy::moved_at_least). The
//! thresholds exist once.

use leviculum_telemetry_policy::{moved_at_least, Fix, PolicyParams};

use crate::{Decision, Withheld};

// ---------------------------------------------------------------------------
// The two cadences
// ---------------------------------------------------------------------------

/// The fast cadence: a board that is moving announces every five minutes.
///
/// Five minutes is this project's existing discovery cadence on BLE and
/// the one the propagation role already ran on LoRa, so a moving board
/// advertises both halves at one rhythm rather than two. At the compiled
/// default PHY (SF8/BW125/CR4:5, 18-symbol preamble) the whole announce
/// set at this cadence is well under a tenth of the lawful budget, which
/// is why rule 5 does nothing there.
pub const MOVING_ANNOUNCE_INTERVAL_MS: u64 = 5 * 60 * 1_000;

/// The floor: a board that is not moving announces once per hour.
///
/// Slow, never silent. A node that is never heard cannot be reached at
/// all, and an hour of silence is what the field operator complained
/// about on 2026-09-09 — but that complaint is answered by the immediate
/// triggers (rules 3 and 4) rather than by a raised cadence, because a
/// raised cadence costs airtime on every board in the mesh while a
/// trigger costs one announce on the one board that needs it.
pub const STILL_ANNOUNCE_INTERVAL_MS: u64 = 60 * 60 * 1_000;

/// How long a board stays in the fast state without a new position
/// confirming it: fifteen minutes (decided on #401, 2026-09-15).
///
/// This is check 3 of the movement proof. Without it a board that moved
/// once and then lost the sky would announce every five minutes for the
/// rest of its life.
pub const MOVEMENT_CONFIRM_TIMEOUT_MS: u64 = 15 * 60 * 1_000;

/// How many consecutive usable fixes must lie past the distance
/// threshold before movement counts: check 1 of the proof.
///
/// Three, and not one, because the telemetry policy switches the
/// movement path off for a station on purpose (`min_distance_m = 0`,
/// and the comment there states why: a fixed board's position wanders
/// while the board stands still). With the configured distinction gone
/// this detector is the only safeguard left, so a single implausible fix
/// must not be able to spend the channel. Three consecutive samples in
/// the same direction is a walk; a receiver that flicks out and back
/// never reaches two.
pub const MOVEMENT_MIN_CONSECUTIVE_FIXES: u32 = 3;

/// The share of the lawful duty budget a board's OWN announces may
/// spend: one tenth.
///
/// The #402 announce cap governs TRANSIT announces at 2 % of interface
/// bandwidth and exempts locally originated ones by design, matching the
/// reference. That exemption is the hole this closes: it is the cap's own
/// arithmetic applied to the one class the cap deliberately lets through,
/// not a second mechanism.
///
/// Measured basis (#401, 2026-09-15): the full announce set costs about
/// 0.22 % duty at SF8 and about 4.47 % at SF12. In a 1 % sub-band the
/// second figure is four and a half times the whole lawful allowance
/// spent on introductions, before any payload. At a fast PHY the rule
/// changes nothing; at the slowest, five minutes becomes roughly half an
/// hour.
pub const OWN_ANNOUNCE_DUTY_SHARE: u64 = 10;

/// How often a board samples its position for the movement proof.
///
/// The tracker's own `min_interval_ms` (60 s), reused rather than chosen:
/// it is already this project's floor between two consecutive telemetry
/// emissions, and a detector sampled faster than the policy that consumes
/// it would only age the streak quicker. Three samples is then three
/// minutes to prove movement, comfortably inside the fast cadence it
/// switches on.
pub const MOVEMENT_SAMPLE_INTERVAL_MS: u64 = PolicyParams::TRACKER.min_interval_ms;

/// Denominator of the `lt_alock` u16 encoding a lawful duty allowance
/// arrives in (`fraction * 10000`, RNode `CMD_LT_ALOCK`). Named rather
/// than spelled, because the same literal appears in the interval
/// arithmetic and in every test that checks it.
pub const DUTY_E4_SCALE: u64 = 10_000;

// ---------------------------------------------------------------------------
// Rule 5: the budget, first, because it bounds all the others
// ---------------------------------------------------------------------------

/// What this board's own announce set costs on the carrier it is running,
/// and what the band in force allows.
///
/// Both numbers already exist on a board and already appear in its log
/// lines: the airtime per frame is the duty ledger's own arithmetic (the
/// same path `announce_cap_bitrate_bps` uses) and the allowance is the
/// long-term airtime lock the interface derived from its TX frequency.
/// Neither is re-derived here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DutyBudget {
    /// Airtime of one announce frame, milliseconds. `0` when no radio has
    /// been configured yet — nothing is transmitting, so nothing is
    /// bounded.
    pub frame_airtime_ms: u64,
    /// How many announce frames one tick of the cadence puts on the air:
    /// the board's own delivery destination, plus the propagation role
    /// when this boot runs one. The set, because the two halves are
    /// decided together and a budget for half of it would be a budget
    /// for a cadence nobody runs.
    pub frames_per_set: u32,
    /// The lawful allowance, in the `lt_alock` u16 encoding
    /// (`fraction * 10000`, so 1 % is 100 and 10 % is 1000). `0` is the
    /// encoding's own "unlimited": an out-of-band frequency with no
    /// explicit lock derives no allowance, and a budget cannot be built
    /// on an allowance nobody stated.
    pub lawful_duty_e4: u16,
}

impl DutyBudget {
    /// The shortest interval whose announce set fits
    /// `1 / OWN_ANNOUNCE_DUTY_SHARE` of the lawful allowance.
    ///
    /// `0` — "nothing to enforce" — when any input is missing.
    ///
    /// ```text
    /// set_ms / interval_ms  <=  (lawful_duty_e4 / 10_000) / SHARE
    /// interval_ms           >=  set_ms * 10_000 * SHARE / lawful_duty_e4
    /// ```
    #[must_use]
    pub const fn min_interval_ms(&self) -> u64 {
        if self.lawful_duty_e4 == 0 || self.frame_airtime_ms == 0 || self.frames_per_set == 0 {
            return 0;
        }
        let set_ms = self
            .frame_airtime_ms
            .saturating_mul(self.frames_per_set as u64);
        set_ms
            .saturating_mul(DUTY_E4_SCALE)
            .saturating_mul(OWN_ANNOUNCE_DUTY_SHARE)
            / self.lawful_duty_e4 as u64
    }

    /// The airtime one tick of the cadence costs, milliseconds.
    #[must_use]
    pub const fn set_airtime_ms(&self) -> u64 {
        self.frame_airtime_ms
            .saturating_mul(self.frames_per_set as u64)
    }
}

/// The stretch rule 5 is applying, when it is applying one: the
/// configured interval, the interval actually used, and the arithmetic
/// that forced it.
///
/// A board that is quieter than its configuration must say why, or the
/// next person measures a cadence that is not the one in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stretch {
    /// What rules 1 and 2 asked for.
    pub configured_ms: u64,
    /// What the board actually uses.
    pub effective_ms: u64,
    /// The budget that forced it.
    pub budget: DutyBudget,
}

// ---------------------------------------------------------------------------
// Rules 1 to 3: movement, PROVEN
// ---------------------------------------------------------------------------

/// Whether the board is moving, and it must be PROVEN rather than
/// measured once.
///
/// Three checks, and a reader who finds only two must not conclude the
/// third was forgotten:
///
/// * **Check 1 — several consecutive fixes, not one jump.**
///   [`MOVEMENT_MIN_CONSECUTIVE_FIXES`] usable fixes must all lie past
///   `min_distance_m` from the anchor before movement counts. The anchor
///   deliberately does NOT advance while the streak builds: a receiver
///   that flicks out and back resets the streak and leaves the anchor
///   where the board actually is, while a slow walk accumulates distance
///   against a fixed reference until it clears the threshold.
/// * **Check 2 — the accuracy gate.** A fix worse than `max_hdop_e2`, or
///   one carrying no HDOP at all, is not a position: it neither counts
///   nor moves the anchor. "We do not know how good this is" is not "it
///   is good".
/// * **Check 3 — an upper bound on the fast state.**
///   [`MOVEMENT_CONFIRM_TIMEOUT_MS`] after the last confirmation the
///   board is still again, whatever it was doing before.
///
/// `settle_ms` from the same parameter set is the cold-start guard: the
/// anchor tracks the receiver until the settle window is over, so a
/// drifting first fix cannot be the reference the whole deployment is
/// measured against.
#[derive(Debug, Clone, Copy)]
pub struct MovementDetector {
    params: PolicyParams,
    anchor: Option<Fix>,
    streak: u32,
    first_fix_ms: Option<u64>,
    confirmed_ms: Option<u64>,
}

impl Default for MovementDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl MovementDetector {
    /// A detector that has seen nothing: a board is still until it proves
    /// otherwise.
    ///
    /// The parameters are the telemetry policy's Tracker set verbatim
    /// (`min_distance_m = 50`, `max_hdop_e2 = 300`, `settle_ms = 60_000`).
    /// Not the Station set: that one has `min_distance_m = 0`, which
    /// switches the movement path off, and the whole point of #401 is
    /// that a board is not configured as one or the other.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            params: PolicyParams::TRACKER,
            anchor: None,
            streak: 0,
            first_fix_ms: None,
            confirmed_ms: None,
        }
    }

    /// The thresholds in force, for a caller that logs them.
    #[must_use]
    pub const fn params(&self) -> PolicyParams {
        self.params
    }

    /// Check 3, on its own: is the board in the fast state right now?
    #[must_use]
    pub const fn is_moving(&self, now_ms: u64) -> bool {
        match self.confirmed_ms {
            Some(at) => now_ms.saturating_sub(at) < MOVEMENT_CONFIRM_TIMEOUT_MS,
            None => false,
        }
    }

    /// Feed one sample. `None` is "no fix", which is not evidence of
    /// anything and leaves the streak alone — a board under a roof has
    /// not stopped moving, it has stopped being observed, and check 3
    /// already bounds what that costs.
    ///
    /// Returns `true` on the rising edge only: movement that has just
    /// been proven on a board that was still. That is rule 3, and the
    /// caller announces at once.
    pub fn poll(&mut self, now_ms: u64, fix: Option<Fix>) -> bool {
        let was_moving = self.is_moving(now_ms);
        // Check 2: the accuracy gate, before anything else looks at the
        // coordinates.
        let Some(fix) = fix.filter(|f| self.accuracy_ok(*f)) else {
            return false;
        };
        let first = *self.first_fix_ms.get_or_insert(now_ms);
        if now_ms.saturating_sub(first) < self.params.settle_ms {
            // Cold start: the anchor follows the receiver rather than
            // freezing on its first, drifting answer.
            self.anchor = Some(fix);
            return false;
        }
        let Some(anchor) = self.anchor else {
            self.anchor = Some(fix);
            return false;
        };
        // Check 1: consecutive displacement past the shared threshold.
        if moved_at_least(fix, anchor, self.params.min_distance_m) {
            self.streak = self.streak.saturating_add(1);
            if self.streak >= MOVEMENT_MIN_CONSECUTIVE_FIXES {
                self.streak = 0;
                self.anchor = Some(fix);
                self.confirmed_ms = Some(now_ms);
                return !was_moving;
            }
        } else {
            self.streak = 0;
        }
        false
    }

    const fn accuracy_ok(&self, fix: Fix) -> bool {
        match fix.hdop_e2 {
            Some(hdop) => hdop <= self.params.max_hdop_e2,
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// The two halves of the announce
// ---------------------------------------------------------------------------

/// Which announce a deadline belongs to.
///
/// One cadence, two emitters. The board's own destination and the
/// propagation role are decided together and differ in exactly two
/// respects, both stated here rather than at the call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnounceSlot {
    /// The board's own `lxmf.delivery` destination.
    Own,
    /// The `lxmf.propagation` role, when this boot runs one.
    Propagation,
}

impl AnnounceSlot {
    /// Whether a plausible wall clock is required before this announce
    /// may go out.
    ///
    /// The board's own announce is clock-gated: the emission timestamp is
    /// what a receiver ranks paths by, and one stamped from uptime
    /// poisons the receiver's path table for the whole life of the entry.
    /// The propagation role's announce is NOT, deliberately (#384 item
    /// 6): a peer treats a small timebase as merely old, creation is
    /// unconditional, and the first contact that announce invites is
    /// exactly what delivers the clock seed.
    #[must_use]
    pub const fn is_clock_gated(self) -> bool {
        matches!(self, Self::Own)
    }

    /// The `reason=` token of the `[ANNOUNCE] sent` line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Own => "periodic",
            Self::Propagation => "pn-periodic",
        }
    }
}

/// The deadline half of one slot: it owns WHEN, never HOW LONG — the
/// interval is handed in by [`AnnounceCadence`], which is what makes the
/// two slots share one cadence instead of two.
#[derive(Debug, Clone, Copy)]
pub struct PeriodicAnnounce {
    next_ms: Option<u64>,
    initial_delay_ms: u64,
    last_fired_ms: Option<u64>,
}

impl PeriodicAnnounce {
    /// Armed lazily: the first [`poll`](Self::poll) or
    /// [`wait_ms`](Self::wait_ms) sets the deadline `initial_delay_ms`
    /// after the clock it is given. Lazy so the whole cadence is a `const`
    /// value a board can hold in a static without a boot-time
    /// initialiser.
    #[must_use]
    pub const fn new(initial_delay_ms: u64) -> Self {
        Self {
            next_ms: None,
            initial_delay_ms,
            last_fired_ms: None,
        }
    }

    /// `None` before the deadline. At or after it, the decision — and the
    /// deadline moves either way, so a caller that ignores the answer
    /// still cannot spin.
    pub fn poll(&mut self, now_ms: u64, clock_ok: bool, interval_ms: u64) -> Option<Decision> {
        let due = self.arm(now_ms);
        if now_ms < due {
            return None;
        }
        if !clock_ok {
            self.next_ms = Some(now_ms.saturating_add(crate::NO_CLOCK_RETRY_MS));
            return Some(Decision::Withheld(Withheld::NoClock));
        }
        self.next_ms = Some(now_ms.saturating_add(interval_ms));
        self.last_fired_ms = Some(now_ms);
        Some(Decision::Announce)
    }

    /// Bring the deadline forward to the earliest moment the budget
    /// allows: now, or `min_spacing_ms` after the last announce that
    /// actually went out, whichever is later.
    ///
    /// Never pushes a deadline back. An immediate trigger is exactly one
    /// announce brought forward, not a cadence change: the tick it fires
    /// re-arms at the ordinary interval like any other.
    pub fn trigger(&mut self, now_ms: u64, min_spacing_ms: u64) {
        let earliest = match self.last_fired_ms {
            Some(last) => last.saturating_add(min_spacing_ms).max(now_ms),
            None => now_ms,
        };
        let due = self.arm(now_ms);
        if earliest < due {
            self.next_ms = Some(earliest);
        }
    }

    /// Hold this slot until `until_ms`, whatever the cadence says. The
    /// caller has a reason of its own (the propagation role refuses to
    /// invite uploads while its store is unmounted).
    pub fn defer(&mut self, now_ms: u64, until_ms: u64) {
        let due = self.arm(now_ms);
        if until_ms > due {
            self.next_ms = Some(until_ms);
        }
    }

    /// How long until the next poll is worth making. Zero means "now".
    #[must_use]
    pub fn wait_ms(&self, now_ms: u64) -> u64 {
        self.next_ms
            .unwrap_or_else(|| now_ms.saturating_add(self.initial_delay_ms))
            .saturating_sub(now_ms)
    }

    fn arm(&mut self, now_ms: u64) -> u64 {
        *self
            .next_ms
            .get_or_insert(now_ms.saturating_add(self.initial_delay_ms))
    }
}

/// How soon after boot the propagation role's first announce fires
/// (`NODE_ANNOUNCE_DELAY`, `reference/LXMF/LXMF/LXMRouter.py:41`).
///
/// Ten seconds ahead of the board's own
/// ([`crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS`]), because the two share
/// one half-duplex radio and two announces contending for the same
/// airtime window at boot is the collision the spacing exists to avoid.
/// One cadence, two deadlines, deliberately offset.
pub const PN_ANNOUNCE_INITIAL_DELAY_MS: u64 = 20 * 1_000;

/// The one cadence both halves of the announce obey.
///
/// Feed it positions ([`note_fix`](Self::note_fix)) and the carrier's
/// budget ([`set_budget`](Self::set_budget)); ask it for a slot's
/// deadline ([`wait_ms`](Self::wait_ms)) and poll that slot when the
/// deadline expires ([`poll`](Self::poll)). Rules 3 and 4 enter through
/// [`note_fix`](Self::note_fix)'s return value and
/// [`trigger`](Self::trigger).
#[derive(Debug, Clone, Copy)]
pub struct AnnounceCadence {
    movement: MovementDetector,
    budget: DutyBudget,
    own: PeriodicAnnounce,
    propagation: PeriodicAnnounce,
}

impl Default for AnnounceCadence {
    fn default() -> Self {
        Self::new()
    }
}

impl AnnounceCadence {
    /// A board that has not moved, on a carrier it knows nothing about
    /// yet: the floor, unstretched.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            movement: MovementDetector::new(),
            budget: DutyBudget {
                frame_airtime_ms: 0,
                frames_per_set: 0,
                lawful_duty_e4: 0,
            },
            own: PeriodicAnnounce::new(crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS),
            propagation: PeriodicAnnounce::new(PN_ANNOUNCE_INITIAL_DELAY_MS),
        }
    }

    /// Arm both slots relative to `now_ms`, which is boot.
    ///
    /// Idempotent and optional: a slot polled before it was armed arms
    /// itself against the clock of that first poll. The explicit call is
    /// what makes the boot offsets mean "after boot" rather than "after
    /// whenever the loop first got round to this slot", which on a board
    /// that spends its first seconds mounting flash is not the same
    /// moment.
    pub fn arm(&mut self, now_ms: u64) {
        self.own.arm(now_ms);
        self.propagation.arm(now_ms);
    }

    /// Tell the cadence what a frame costs and what the band allows.
    /// Returns `true` when this changed the budget, which is the edge a
    /// caller logs on.
    pub fn set_budget(&mut self, budget: DutyBudget) -> bool {
        if self.budget == budget {
            return false;
        }
        self.budget = budget;
        true
    }

    /// The budget in force.
    #[must_use]
    pub const fn budget(&self) -> DutyBudget {
        self.budget
    }

    /// Rules 1 and 2 alone: what the movement state asks for, before rule
    /// 5 has had its say.
    #[must_use]
    pub const fn configured_interval_ms(&self, now_ms: u64) -> u64 {
        if self.movement.is_moving(now_ms) {
            MOVING_ANNOUNCE_INTERVAL_MS
        } else {
            STILL_ANNOUNCE_INTERVAL_MS
        }
    }

    /// The interval actually used: rules 1 and 2, bounded by rule 5.
    #[must_use]
    pub const fn interval_ms(&self, now_ms: u64) -> u64 {
        let configured = self.configured_interval_ms(now_ms);
        let floor = self.budget.min_interval_ms();
        if floor > configured {
            floor
        } else {
            configured
        }
    }

    /// The stretch to log, or `None` when the configured cadence is
    /// affordable as it stands.
    #[must_use]
    pub const fn stretch(&self, now_ms: u64) -> Option<Stretch> {
        let configured = self.configured_interval_ms(now_ms);
        let effective = self.interval_ms(now_ms);
        if effective > configured {
            Some(Stretch {
                configured_ms: configured,
                effective_ms: effective,
                budget: self.budget,
            })
        } else {
            None
        }
    }

    /// Whether the board is in the fast state.
    #[must_use]
    pub const fn is_moving(&self, now_ms: u64) -> bool {
        self.movement.is_moving(now_ms)
    }

    /// The detector, for a caller that reports its thresholds.
    #[must_use]
    pub const fn movement(&self) -> &MovementDetector {
        &self.movement
    }

    /// Feed one position sample. Returns `true` when movement has just
    /// been proven on a board that was still — rule 3 — in which case
    /// both slots have already been brought forward.
    pub fn note_fix(&mut self, now_ms: u64, fix: Option<Fix>) -> bool {
        if !self.movement.poll(now_ms, fix) {
            return false;
        }
        self.trigger(now_ms);
        true
    }

    /// Rules 3 and 4: announce at once, on both halves.
    ///
    /// "At once" is bounded by rule 5 and by nothing else: on a carrier
    /// where the set is unaffordable the trigger waits out the budget's
    /// spacing rather than being dropped. It is one announce brought
    /// forward, never a cadence change.
    pub fn trigger(&mut self, now_ms: u64) {
        // The budget's spacing, and never less than the emission floor
        // this project already runs (`PolicyParams::TRACKER
        // .min_interval_ms`): a board that meets six new neighbours in a
        // minute owes the mesh one announce, not six. On a carrier whose
        // band derives no lawful allowance the budget's spacing is zero,
        // and the floor is then the only thing holding that case.
        let spacing = {
            let budget = self.budget.min_interval_ms();
            let floor = MOVEMENT_SAMPLE_INTERVAL_MS;
            if budget > floor {
                budget
            } else {
                floor
            }
        };
        self.own.trigger(now_ms, spacing);
        self.propagation.trigger(now_ms, spacing);
    }

    /// Poll one slot. `clock_ok` is the caller's plausibility answer; it
    /// is only consulted for a slot that is clock-gated
    /// ([`AnnounceSlot::is_clock_gated`]).
    pub fn poll(&mut self, slot: AnnounceSlot, now_ms: u64, clock_ok: bool) -> Option<Decision> {
        let interval = self.interval_ms(now_ms);
        let gate = clock_ok || !slot.is_clock_gated();
        self.slot_mut(slot).poll(now_ms, gate, interval)
    }

    /// Hold one slot until `until_ms` for a reason of the caller's own.
    pub fn defer(&mut self, slot: AnnounceSlot, now_ms: u64, until_ms: u64) {
        self.slot_mut(slot).defer(now_ms, until_ms);
    }

    /// How long the caller may sleep before polling this slot.
    #[must_use]
    pub fn wait_ms(&self, slot: AnnounceSlot, now_ms: u64) -> u64 {
        match slot {
            AnnounceSlot::Own => self.own.wait_ms(now_ms),
            AnnounceSlot::Propagation => self.propagation.wait_ms(now_ms),
        }
    }

    fn slot_mut(&mut self, slot: AnnounceSlot) -> &mut PeriodicAnnounce {
        match slot {
            AnnounceSlot::Own => &mut self.own,
            AnnounceSlot::Propagation => &mut self.propagation,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// **These are host tests because the rig cannot exercise the movement
/// path at all.** Both shipped images carry GNSS (`bsp-t114` pulls
/// `gnss`, the Pocket image likewise), but in the shielded measurement
/// room both boards report `valid=false sat=0`. A green hardware cell in
/// that room is therefore not evidence about #401: it says the board
/// booted and announced, not that it decided its cadence correctly.
/// Acceptance on hardware needs an injected position or a run outdoors.
#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> u64 {
        1_700_000_000_000
    }

    /// A fix at a given offset in metres from the reference point, with a
    /// good HDOP. Latitude only: one degree of latitude is
    /// `METRES_PER_DEGREE_LAT` everywhere, so a metre offset is exact
    /// without a cosine.
    fn fix_at(north_m: i64) -> Fix {
        Fix {
            latitude_e6: (north_m * 1_000_000 / 111_320) as i32,
            longitude_e6: 8_000_000,
            hdop_e2: Some(100),
        }
    }

    /// Walk the detector past the settle window with the board standing
    /// still, so later samples are judged rather than absorbed by the
    /// cold-start guard.
    fn settled(cadence: &mut AnnounceCadence) -> u64 {
        let settle = PolicyParams::TRACKER.settle_ms;
        cadence.arm(t0());
        assert!(!cadence.note_fix(t0(), Some(fix_at(0))));
        assert!(!cadence.note_fix(t0() + settle, Some(fix_at(0))));
        t0() + settle
    }

    /// Sample period the board runs the detector at: the tracker's own
    /// `min_interval_ms`, so the tests age the same way the firmware does.
    fn step() -> u64 {
        PolicyParams::TRACKER.min_interval_ms
    }

    // -- Rule 2: the floor ------------------------------------------------

    /// A board that never moves settles at the floor and stays there.
    #[test]
    fn a_board_that_never_moves_announces_at_the_floor() {
        let mut cadence = AnnounceCadence::new();
        let mut now = settled(&mut cadence);
        for _ in 0..60 {
            now += step();
            assert!(
                !cadence.note_fix(now, Some(fix_at(0))),
                "a board standing still never proves movement"
            );
        }
        assert!(!cadence.is_moving(now));
        assert_eq!(cadence.interval_ms(now), STILL_ANNOUNCE_INTERVAL_MS);
    }

    /// And it is never silent: the floor still fires.
    #[test]
    fn the_floor_is_slow_and_never_silent() {
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        let first = t0() + crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS;
        assert_eq!(cadence.poll(AnnounceSlot::Own, t0(), true), None);
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, first, true),
            Some(Decision::Announce)
        );
        assert_eq!(
            cadence.poll(
                AnnounceSlot::Own,
                first + STILL_ANNOUNCE_INTERVAL_MS - 1,
                true
            ),
            None
        );
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, first + STILL_ANNOUNCE_INTERVAL_MS, true),
            Some(Decision::Announce)
        );
    }

    // -- Rule 1: the fast cadence, and rule 3's bound on leaving it -------

    /// A board that moves takes the fast cadence.
    #[test]
    fn a_moving_board_takes_the_fast_cadence() {
        let mut cadence = AnnounceCadence::new();
        let mut now = settled(&mut cadence);
        let mut edges = 0;
        for n in 1..=MOVEMENT_MIN_CONSECUTIVE_FIXES as i64 {
            now += step();
            if cadence.note_fix(now, Some(fix_at(n * 100))) {
                edges += 1;
            }
        }
        assert_eq!(edges, 1, "movement is proven exactly once on the way up");
        assert!(cadence.is_moving(now));
        assert_eq!(cadence.interval_ms(now), MOVING_ANNOUNCE_INTERVAL_MS);
    }

    /// One that stops falls back after the stated bound, not before.
    #[test]
    fn a_board_that_stops_falls_back_after_the_bound_and_not_before() {
        let mut cadence = AnnounceCadence::new();
        let mut now = settled(&mut cadence);
        for n in 1..=MOVEMENT_MIN_CONSECUTIVE_FIXES as i64 {
            now += step();
            cadence.note_fix(now, Some(fix_at(n * 100)));
        }
        let confirmed = now;
        assert!(cadence.is_moving(confirmed));
        // The receiver goes quiet: no fix at all is not evidence of
        // stopping, so only the bound ends the fast state.
        assert_eq!(
            cadence.interval_ms(confirmed + MOVEMENT_CONFIRM_TIMEOUT_MS - 1),
            MOVING_ANNOUNCE_INTERVAL_MS,
            "still fast one millisecond before the bound"
        );
        assert_eq!(
            cadence.interval_ms(confirmed + MOVEMENT_CONFIRM_TIMEOUT_MS),
            STILL_ANNOUNCE_INTERVAL_MS,
            "and at the bound it is back on the floor"
        );
    }

    /// The fast state expires after fifteen minutes without confirmation
    /// even while the receiver keeps reporting the same place — the case
    /// a bound on silence alone would miss.
    #[test]
    fn the_fast_state_expires_without_a_confirming_position() {
        let mut cadence = AnnounceCadence::new();
        let mut now = settled(&mut cadence);
        for n in 1..=MOVEMENT_MIN_CONSECUTIVE_FIXES as i64 {
            now += step();
            cadence.note_fix(now, Some(fix_at(n * 100)));
        }
        let parked = fix_at(MOVEMENT_MIN_CONSECUTIVE_FIXES as i64 * 100);
        let deadline = now + MOVEMENT_CONFIRM_TIMEOUT_MS;
        while now < deadline {
            now += step();
            assert!(!cadence.note_fix(now, Some(parked)));
        }
        assert!(
            !cadence.is_moving(now),
            "fifteen minutes parked is not movement, whatever it was before"
        );
        assert_eq!(cadence.interval_ms(now), STILL_ANNOUNCE_INTERVAL_MS);
    }

    // -- Check 1: one jump is not movement, several fixes are ------------

    /// A single implausible jump does NOT count as movement. This is the
    /// check that protects a solar node from its own receiver, so both
    /// directions are asserted: the jump alone proves nothing...
    #[test]
    fn a_single_implausible_jump_is_not_movement() {
        let mut cadence = AnnounceCadence::new();
        let mut now = settled(&mut cadence);
        for _ in 0..20 {
            now += step();
            assert!(!cadence.note_fix(now, Some(fix_at(500))), "the jump");
            now += step();
            assert!(!cadence.note_fix(now, Some(fix_at(0))), "and back");
        }
        assert!(
            !cadence.is_moving(now),
            "twenty round trips of a wandering receiver are still not movement"
        );
        assert_eq!(cadence.interval_ms(now), STILL_ANNOUNCE_INTERVAL_MS);
    }

    /// ...and several consistent fixes do.
    #[test]
    fn several_consistent_fixes_are_movement() {
        let mut cadence = AnnounceCadence::new();
        let mut now = settled(&mut cadence);
        let mut proven_at = None;
        for n in 1..=MOVEMENT_MIN_CONSECUTIVE_FIXES as i64 {
            now += step();
            assert!(!cadence.is_moving(now), "not before the streak is complete");
            if cadence.note_fix(now, Some(fix_at(n * 100))) {
                proven_at = Some(n);
            }
        }
        assert_eq!(
            proven_at,
            Some(MOVEMENT_MIN_CONSECUTIVE_FIXES as i64),
            "proven on the last of the consecutive fixes, not before"
        );
    }

    /// A walk slower than the threshold per sample still counts, because
    /// the anchor does not creep after the walker: displacement
    /// accumulates against a fixed reference.
    #[test]
    fn a_walk_slower_than_the_threshold_per_sample_still_counts() {
        let mut cadence = AnnounceCadence::new();
        let mut now = settled(&mut cadence);
        let per_sample = (PolicyParams::TRACKER.min_distance_m / 2) as i64;
        let mut moving = false;
        for n in 1..=12i64 {
            now += step();
            moving |= cadence.note_fix(now, Some(fix_at(n * per_sample)));
        }
        assert!(moving, "25 m per sample for twelve samples is a walk");
    }

    // -- Check 2: the accuracy gate --------------------------------------

    /// A fix worse than the threshold is not a position: it neither
    /// proves movement nor moves the anchor.
    #[test]
    fn a_fix_that_fails_the_accuracy_gate_proves_nothing() {
        let mut cadence = AnnounceCadence::new();
        let mut now = settled(&mut cadence);
        let bad = Fix {
            hdop_e2: Some(PolicyParams::TRACKER.max_hdop_e2 + 1),
            ..fix_at(1_000)
        };
        for _ in 0..10 {
            now += step();
            assert!(!cadence.note_fix(now, Some(bad)));
        }
        assert!(!cadence.is_moving(now));
        // And the anchor was not moved by them: a good fix at the
        // original place is still "here".
        now += step();
        assert!(!cadence.note_fix(now, Some(fix_at(0))));
        assert!(!cadence.is_moving(now));
    }

    /// A fix with no HDOP at all is refused for the same reason: "we do
    /// not know how good this is" is not "it is good".
    #[test]
    fn a_fix_without_an_accuracy_figure_proves_nothing() {
        let mut cadence = AnnounceCadence::new();
        let mut now = settled(&mut cadence);
        for n in 1..=10i64 {
            now += step();
            let no_hdop = Fix {
                hdop_e2: None,
                ..fix_at(n * 100)
            };
            assert!(!cadence.note_fix(now, Some(no_hdop)));
        }
        assert!(!cadence.is_moving(now));
    }

    /// The cold-start guard: a receiver drifting inside `settle_ms`
    /// cannot prove movement, however far it claims to have gone.
    #[test]
    fn the_settle_window_absorbs_a_cold_start() {
        let mut cadence = AnnounceCadence::new();
        let settle = PolicyParams::TRACKER.settle_ms;
        let mut now = t0();
        for n in 0..6i64 {
            assert!(!cadence.note_fix(now, Some(fix_at(n * 400))));
            now += settle / 6;
        }
        assert!(!cadence.is_moving(now));
    }

    // -- Rules 3 and 4: exactly one immediate announce -------------------

    /// Movement resuming produces exactly one immediate announce, on both
    /// halves, and not a second cadence.
    #[test]
    fn movement_resuming_produces_one_immediate_announce() {
        let mut cadence = AnnounceCadence::new();
        let mut now = settled(&mut cadence);
        // Spend both slots' first announce so the deadlines are far away.
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, now, true),
            Some(Decision::Announce)
        );
        assert_eq!(
            cadence.poll(AnnounceSlot::Propagation, now, true),
            Some(Decision::Announce)
        );
        for n in 1..=MOVEMENT_MIN_CONSECUTIVE_FIXES as i64 {
            now += step();
            cadence.note_fix(now, Some(fix_at(n * 100)));
        }
        for slot in [AnnounceSlot::Own, AnnounceSlot::Propagation] {
            assert_eq!(
                cadence.poll(slot, now, true),
                Some(Decision::Announce),
                "{slot:?}: movement resumed, announce at once"
            );
            assert_eq!(
                cadence.poll(slot, now + 1, true),
                None,
                "{slot:?}: and exactly one, not a burst"
            );
            assert_eq!(
                cadence.wait_ms(slot, now),
                MOVING_ANNOUNCE_INTERVAL_MS,
                "{slot:?}: back on the ordinary fast tick afterwards"
            );
        }
    }

    /// A previously unknown neighbour produces exactly one immediate
    /// announce and NOT a raised cadence: the board is still still.
    #[test]
    fn a_new_neighbour_produces_one_announce_and_no_cadence_change() {
        let mut cadence = AnnounceCadence::new();
        let mut now = settled(&mut cadence);
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, now, true),
            Some(Decision::Announce)
        );
        now += 60_000;
        cadence.trigger(now);
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, now, true),
            Some(Decision::Announce)
        );
        assert_eq!(cadence.poll(AnnounceSlot::Own, now + 1, true), None);
        assert!(
            !cadence.is_moving(now),
            "a neighbour is a trigger, never a movement signal"
        );
        assert_eq!(
            cadence.interval_ms(now),
            STILL_ANNOUNCE_INTERVAL_MS,
            "and the cadence it returns to is still the floor"
        );
    }

    // -- Rule 5: the duty budget -----------------------------------------

    /// The budget is the cap's own arithmetic: the announce set may use
    /// one tenth of the lawful allowance, stated as the equation rather
    /// than as a frozen millisecond count.
    #[test]
    fn the_budget_is_one_tenth_of_the_lawful_allowance() {
        let budget = DutyBudget {
            frame_airtime_ms: 7_054,
            frames_per_set: 2,
            lawful_duty_e4: 1_000, // 10 %, ERC 70-03 h1.7
        };
        let interval = budget.min_interval_ms();
        // Duty actually spent at that interval, in the same e4 encoding.
        let spent_e4 = budget.set_airtime_ms() * DUTY_E4_SCALE / interval;
        assert_eq!(
            spent_e4,
            budget.lawful_duty_e4 as u64 / OWN_ANNOUNCE_DUTY_SHARE,
            "the announce set spends exactly one tenth of the allowance"
        );
    }

    /// Rule 5 does nothing at a fast PHY: the set is far below the
    /// threshold, so the configured cadence is the one in force.
    #[test]
    fn a_fast_phy_is_not_stretched() {
        let mut cadence = AnnounceCadence::new();
        // SF8/BW125/CR4:5, 18-symbol preamble, 183 B: 544 ms, from
        // `leviculum_core::rnode::packet_airtime_ms` itself.
        let budget = DutyBudget {
            frame_airtime_ms: 544,
            frames_per_set: 2,
            lawful_duty_e4: 1_000,
        };
        assert!(cadence.set_budget(budget));
        let now = t0();
        assert!(
            budget.min_interval_ms() < MOVING_ANNOUNCE_INTERVAL_MS,
            "the arithmetic, not the number: the set fits the fast cadence"
        );
        assert_eq!(cadence.interval_ms(now), STILL_ANNOUNCE_INTERVAL_MS);
        assert_eq!(cadence.stretch(now), None, "nothing to say, nothing said");
    }

    /// And it stretches at a slow one, for both cadences that are below
    /// the affordable minimum.
    #[test]
    fn a_slow_phy_stretches_the_fast_cadence() {
        let mut cadence = AnnounceCadence::new();
        // SF12/BW125/CR4:5, 18-symbol preamble, 183 B: 7054 ms, from the
        // same function. The set of two is 14.1 s.
        let budget = DutyBudget {
            frame_airtime_ms: 7_054,
            frames_per_set: 2,
            lawful_duty_e4: 1_000,
        };
        assert!(cadence.set_budget(budget));
        let mut now = settled(&mut cadence);
        for n in 1..=MOVEMENT_MIN_CONSECUTIVE_FIXES as i64 {
            now += step();
            cadence.note_fix(now, Some(fix_at(n * 100)));
        }
        assert!(cadence.is_moving(now));
        let stretch = cadence.stretch(now).expect("the fast cadence is stretched");
        assert_eq!(stretch.configured_ms, MOVING_ANNOUNCE_INTERVAL_MS);
        assert_eq!(stretch.effective_ms, budget.min_interval_ms());
        assert!(
            stretch.effective_ms > stretch.configured_ms,
            "five minutes does not fit a tenth of this band on this carrier"
        );
        assert_eq!(cadence.interval_ms(now), stretch.effective_ms);
    }

    /// The floor is not stretched when it is already affordable: the
    /// stretch is a bound, not a multiplier.
    #[test]
    fn an_affordable_floor_is_left_alone() {
        let mut cadence = AnnounceCadence::new();
        let budget = DutyBudget {
            frame_airtime_ms: 7_054,
            frames_per_set: 2,
            lawful_duty_e4: 1_000,
        };
        cadence.set_budget(budget);
        let now = t0();
        assert!(budget.min_interval_ms() < STILL_ANNOUNCE_INTERVAL_MS);
        assert_eq!(cadence.interval_ms(now), STILL_ANNOUNCE_INTERVAL_MS);
        assert_eq!(cadence.stretch(now), None);
    }

    /// A tighter band buys less: the same carrier in a 1 % sub-band
    /// stretches ten times as far. Asserted as the ratio, because that is
    /// what the rule says.
    #[test]
    fn a_tighter_band_stretches_proportionally() {
        let wide = DutyBudget {
            frame_airtime_ms: 7_054,
            frames_per_set: 2,
            lawful_duty_e4: 1_000,
        };
        let narrow = DutyBudget {
            lawful_duty_e4: 100,
            ..wide
        };
        assert_eq!(narrow.min_interval_ms(), wide.min_interval_ms() * 10);
    }

    /// No lawful allowance, nothing to enforce: an out-of-band frequency
    /// with no explicit airtime lock derives no budget, and a cadence
    /// must not be invented from one.
    #[test]
    fn an_unknown_allowance_enforces_nothing() {
        let budget = DutyBudget {
            frame_airtime_ms: 7_054,
            frames_per_set: 2,
            lawful_duty_e4: 0,
        };
        assert_eq!(budget.min_interval_ms(), 0);
        let mut cadence = AnnounceCadence::new();
        cadence.set_budget(budget);
        assert_eq!(cadence.interval_ms(t0()), STILL_ANNOUNCE_INTERVAL_MS);
        assert_eq!(cadence.stretch(t0()), None);
    }

    /// An unconfigured radio has no budget either: nothing is
    /// transmitting, so nothing is bounded.
    #[test]
    fn an_unconfigured_radio_enforces_nothing() {
        let budget = DutyBudget {
            frame_airtime_ms: 0,
            frames_per_set: 2,
            lawful_duty_e4: 1_000,
        };
        assert_eq!(budget.min_interval_ms(), 0);
    }

    /// The set is both halves: a board running the propagation role pays
    /// twice, and its affordable minimum is twice as far out.
    #[test]
    fn the_budget_counts_both_halves_of_the_announce() {
        let one = DutyBudget {
            frame_airtime_ms: 7_054,
            frames_per_set: 1,
            lawful_duty_e4: 1_000,
        };
        let both = DutyBudget {
            frames_per_set: 2,
            ..one
        };
        assert_eq!(both.min_interval_ms(), one.min_interval_ms() * 2);
    }

    /// Rule 5 bounds rules 3 and 4 too: an immediate trigger on an
    /// unaffordable carrier waits out the spacing rather than being
    /// dropped. Nothing is lost, the budget still holds.
    #[test]
    fn an_immediate_trigger_waits_out_the_budget_but_is_not_dropped() {
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        let budget = DutyBudget {
            frame_airtime_ms: 7_054,
            frames_per_set: 2,
            lawful_duty_e4: 1_000,
        };
        cadence.set_budget(budget);
        let spacing = budget.min_interval_ms();
        let first = t0() + crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS;
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, first, true),
            Some(Decision::Announce)
        );
        cadence.trigger(first + 1_000);
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, first + spacing - 1, true),
            None,
            "the trigger does not buy airtime the band does not allow"
        );
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, first + spacing, true),
            Some(Decision::Announce),
            "and it is not dropped: it fires the moment the budget allows"
        );
    }

    /// A burst of triggers is still one announce: six new neighbours in a
    /// minute owe the mesh one announce, not six. The floor holds even on
    /// a carrier whose band derives no lawful allowance.
    #[test]
    fn a_burst_of_triggers_is_one_announce() {
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        let first = t0() + crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS;
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, first, true),
            Some(Decision::Announce)
        );
        assert_eq!(cadence.budget().min_interval_ms(), 0, "no band, no budget");
        let mut announces = 0;
        for n in 1..=6u64 {
            let now = first + n * 1_000;
            cadence.trigger(now);
            if cadence.poll(AnnounceSlot::Own, now, true) == Some(Decision::Announce) {
                announces += 1;
            }
        }
        assert_eq!(announces, 0, "inside the emission floor, nothing fires");
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, first + MOVEMENT_SAMPLE_INTERVAL_MS, true),
            Some(Decision::Announce),
            "and the whole burst collapses into the one announce it owed"
        );
    }

    // -- The two slots ---------------------------------------------------

    /// Both halves follow one rule: same interval, same triggers, and the
    /// deadlines differ only by the deliberate boot offset that keeps the
    /// two frames out of one airtime window.
    #[test]
    fn both_halves_share_one_cadence_and_differ_only_at_boot() {
        let cadence = AnnounceCadence::new();
        assert_eq!(
            cadence.wait_ms(AnnounceSlot::Propagation, t0()),
            PN_ANNOUNCE_INITIAL_DELAY_MS
        );
        assert_eq!(
            cadence.wait_ms(AnnounceSlot::Own, t0()),
            crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS
        );
        const {
            assert!(
                crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS > PN_ANNOUNCE_INITIAL_DELAY_MS,
                "the two announces do not contend for the same window at boot"
            )
        };
    }

    /// The board's own announce is clock-gated; the propagation role's is
    /// not, and each says so for a reason recorded on the slot.
    #[test]
    fn only_the_own_announce_is_clock_gated() {
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        let own_due = t0() + crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS;
        let pn_due = t0() + PN_ANNOUNCE_INITIAL_DELAY_MS;
        assert_eq!(
            cadence.poll(AnnounceSlot::Propagation, pn_due, false),
            Some(Decision::Announce),
            "a clockless board still announces the role: the contact it invites carries the seed"
        );
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, own_due, false),
            Some(Decision::Withheld(Withheld::NoClock)),
            "an own announce stamped from uptime poisons the receiver's path table"
        );
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, own_due + crate::NO_CLOCK_RETRY_MS, true),
            Some(Decision::Announce),
            "and it is retried in a minute, not in an hour"
        );
    }

    /// A deferred slot stays deferred, and the other half is untouched:
    /// the propagation role's store gate is its own business.
    #[test]
    fn a_deferred_slot_holds_and_does_not_move_the_other() {
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        let pn_due = t0() + PN_ANNOUNCE_INITIAL_DELAY_MS;
        cadence.defer(AnnounceSlot::Propagation, pn_due, pn_due + 60_000);
        assert_eq!(cadence.poll(AnnounceSlot::Propagation, pn_due, true), None);
        assert_eq!(
            cadence.poll(AnnounceSlot::Propagation, pn_due + 60_000, true),
            Some(Decision::Announce)
        );
        assert_eq!(
            cadence.wait_ms(AnnounceSlot::Own, t0()),
            crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS
        );
    }

    /// The slot tokens are the log's vocabulary: pinned so a rename has
    /// to be deliberate.
    #[test]
    fn the_slot_tokens_are_stable() {
        assert_eq!(AnnounceSlot::Own.as_str(), "periodic");
        assert_eq!(AnnounceSlot::Propagation.as_str(), "pn-periodic");
    }
}
