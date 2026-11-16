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
/// propagation role are decided together and differ in exactly three
/// respects, all stated here rather than at the call sites: the boot
/// offset, the clock gate, and the start pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnounceSlot {
    /// The board's own `lxmf.delivery` destination.
    Own,
    /// The `lxmf.propagation` role, when this boot runs one.
    Propagation,
    /// The board's `lev.position`, when it shares its position (order
    /// 1301, [`crate::PositionShare`]).
    Position,
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
    ///
    /// The position announce is clock-gated for the board's own reason and
    /// one more: its fix age is counted from the emission, so a receiver
    /// dates the fix by the announce's own timestamp.
    pub const fn is_clock_gated(self) -> bool {
        matches!(self, Self::Own | Self::Position)
    }

    /// How long after this slot's first announce the second follows,
    /// instead of the cadence's interval; `None` for a slot whose
    /// interval runs from its first.
    ///
    /// The propagation role announces twice at start, as `lxmd` does:
    /// the router's own announce [`PN_ANNOUNCE_INITIAL_DELAY_MS`] after
    /// start and the daemon's deferred start announce
    /// [`PN_START_ANNOUNCE_GAP_MS`] later (`lnpnd` runs the same pair off
    /// the same constants). On a board this matters more than on a host:
    /// a role whose one boot announce dies in a collision is an invisible
    /// propagation node until the cadence fires, which on a board that
    /// does not move is the hourly floor. The board's own announce has no
    /// pair; it is clock-gated, and a withheld one retries in a minute.
    #[must_use]
    pub const fn start_gap_ms(self) -> Option<u64> {
        match self {
            Self::Own | Self::Position => None,
            Self::Propagation => Some(PN_START_ANNOUNCE_GAP_MS),
        }
    }

    /// The `reason=` token of the `[ANNOUNCE] sent` line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Own => "periodic",
            Self::Propagation => "pn-periodic",
            Self::Position => "position-periodic",
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

    /// Record an announce the caller sent outside the deadline (a button
    /// press) as this slot's latest, and keep the next scheduled one at
    /// least `min_spacing_ms` away from it.
    ///
    /// Not a re-arm: a deadline already further out stays where it is, so
    /// a press does not push the cadence back by a whole interval.
    pub fn book(&mut self, now_ms: u64, min_spacing_ms: u64) {
        self.last_fired_ms = Some(now_ms);
        self.defer(now_ms, now_ms.saturating_add(min_spacing_ms));
    }

    /// The earliest moment an announce `min_spacing_ms` after this slot's
    /// last may go out: `now_ms` when none has, or the spacing has passed.
    #[must_use]
    pub fn earliest_ms(&self, now_ms: u64, min_spacing_ms: u64) -> u64 {
        match self.last_fired_ms {
            Some(last) => last.saturating_add(min_spacing_ms).max(now_ms),
            None => now_ms,
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

    /// Whether an announce has gone out from this slot since boot.
    #[must_use]
    pub const fn has_fired(&self) -> bool {
        self.last_fired_ms.is_some()
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

/// How long after the propagation role's first start announce the second
/// follows ([`AnnounceSlot::start_gap_ms`]): `lxmd` runs its deferred
/// start jobs `DEFFERED_JOBS_DELAY` = 10 s after start
/// (`reference/LXMF/LXMF/Utilities/lxmd.py:33`, `:501-503`), and that
/// announce sleeps the same `NODE_ANNOUNCE_DELAY` as the router's own
/// (`reference/LXMF/LXMF/LXMRouter.py:338-341`), so the two leave 10 s
/// apart. One constant for both stacks: `lnpnd` derives its own from it.
pub const PN_START_ANNOUNCE_GAP_MS: u64 = 10 * 1_000;

/// What [`AnnounceCadence::manual`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualAnnounce {
    /// Send both announces now; the cadence has booked them.
    Now {
        /// The position slot was booked with them: send the position
        /// announce too, after the other two. `false` when the press did
        /// not ask for it, or when the slot's own spacing since its last
        /// announce has not passed, in which case the slot was brought
        /// forward to that moment instead.
        position: bool,
    },
    /// The budget does not allow the set yet; every slot the press asked
    /// for fires on its own in `wait_ms`.
    Deferred {
        /// How long until the budget allows it.
        wait_ms: u64,
    },
}

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
    position: PeriodicAnnounce,
    /// The fix the last position announce carried, which the 25 m advance
    /// measures from.
    position_sent: Option<Fix>,
    /// The CHTM's hour-long channel load, hundredths of a percent, which
    /// thins the position announce ([`crate::thinned_interval_ms`]).
    channel_load_e4: u16,
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
            position: PeriodicAnnounce::new(crate::POSITION_ANNOUNCE_INITIAL_DELAY_MS),
            position_sent: None,
            channel_load_e4: 0,
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
        self.position.arm(now_ms);
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
        let spacing = self.trigger_spacing_ms();
        self.own.trigger(now_ms, spacing);
        self.propagation.trigger(now_ms, spacing);
        self.position.trigger(now_ms, spacing);
    }

    /// The spacing a brought-forward announce keeps from the last one: the
    /// budget's, never below the trigger floor. What [`trigger`]
    /// (Self::trigger) applies, named for the position advance.
    const fn trigger_spacing_ms(&self) -> u64 {
        let budget = self.budget.min_interval_ms();
        if budget > MOVEMENT_SAMPLE_INTERVAL_MS {
            budget
        } else {
            MOVEMENT_SAMPLE_INTERVAL_MS
        }
    }

    /// Tell the cadence the channel load the CHTM measured over the last
    /// hour (`channel_load_long`, hundredths of a percent).
    pub fn set_channel_load(&mut self, channel_load_long_e4: u16) {
        self.channel_load_e4 = channel_load_long_e4;
    }

    /// The position announce's interval: the cadence's, thinned by the
    /// channel load ([`crate::thinned_interval_ms`]).
    #[must_use]
    pub const fn position_interval_ms(&self, now_ms: u64) -> u64 {
        crate::thinned_interval_ms(self.interval_ms(now_ms), self.channel_load_e4)
    }

    /// A position announce carrying `fix` went out: the 25 m advance
    /// measures from here.
    pub fn note_position_sent(&mut self, fix: Fix) {
        self.position_sent = Some(fix);
    }

    /// The 25 m advance (order 1301): a fix that moved
    /// [`crate::POSITION_ADVANCE_DISTANCE_M`] from the last position
    /// announced brings the next position announce forward to the earliest
    /// moment the budget allows, stretched by the channel's thinning like
    /// the interval itself. Never later than it already was, never earlier
    /// than that spacing after the last one. Returns whether it moved the
    /// deadline, the edge a caller logs.
    ///
    /// Before the first position announce there is nothing to measure
    /// from, and the boot deadline stands.
    pub fn advance_position(&mut self, now_ms: u64, fix: Fix) -> bool {
        let Some(sent) = self.position_sent else {
            return false;
        };
        if !moved_at_least(fix, sent, crate::POSITION_ADVANCE_DISTANCE_M) {
            return false;
        }
        let spacing = crate::thinned_interval_ms(self.trigger_spacing_ms(), self.channel_load_e4);
        let before = self.position.wait_ms(now_ms);
        self.position.trigger(now_ms, spacing);
        self.position.wait_ms(now_ms) < before
    }

    /// Somebody asked for both announces now (a button press, Codeberg
    /// #459): the board's one place a manual announce is booked.
    ///
    /// Bounded by rule 5 and by nothing else. When the carrier's budget
    /// allows the set now, both slots record it as their latest announce,
    /// so the next trigger and the next tick are spaced from it, and
    /// [`ManualAnnounce::Now`] tells the caller to send. When the budget's
    /// spacing since the last announce has not passed, nothing is sent and
    /// both slots are brought forward to the moment it has, so the press
    /// is answered by the cadence's own tick as early as the band allows,
    /// never dropped and never paid for twice.
    ///
    /// The spacing is the budget's alone, without the trigger floor
    /// [`trigger`](Self::trigger) adds: the caller has its own rate limit
    /// ([`crate::BUTTON_ANNOUNCE_MIN_INTERVAL_MS`]), and the floor exists
    /// for triggers that arrive in bursts nobody chose.
    ///
    /// `position` is the caller's answer to "would a position announce go
    /// out now": the share resolves to owner or everyone and there is a
    /// fix (order 1316). When it is `true` the position slot is booked in
    /// the same press, under the same spacing, after the delivery half:
    /// the delivery and propagation halves decide whether the press goes
    /// out now, and the position joins them when its own last announce is
    /// at least the spacing ago. When it is not, the slot is brought
    /// forward to that moment, like a deferred press, so the position is
    /// still answered by its own tick as early as the band allows. When
    /// `position` is `false` the slot is untouched, and a press does what
    /// it did before order 1316.
    pub fn manual(&mut self, now_ms: u64, position: bool) -> ManualAnnounce {
        let spacing = self.budget.min_interval_ms();
        let earliest = self
            .own
            .earliest_ms(now_ms, spacing)
            .max(self.propagation.earliest_ms(now_ms, spacing));
        if earliest > now_ms {
            self.own.trigger(now_ms, spacing);
            self.propagation.trigger(now_ms, spacing);
            if position {
                self.position.trigger(now_ms, spacing);
            }
            return ManualAnnounce::Deferred {
                wait_ms: earliest - now_ms,
            };
        }
        self.own.book(now_ms, spacing);
        self.propagation.book(now_ms, spacing);
        if !position {
            return ManualAnnounce::Now { position: false };
        }
        if self.position.earliest_ms(now_ms, spacing) > now_ms {
            self.position.trigger(now_ms, spacing);
            return ManualAnnounce::Now { position: false };
        }
        self.position.book(now_ms, spacing);
        ManualAnnounce::Now { position: true }
    }

    /// Poll one slot. `clock_ok` is the caller's plausibility answer; it
    /// is only consulted for a slot that is clock-gated
    /// ([`AnnounceSlot::is_clock_gated`]).
    ///
    /// The deadline after a slot's FIRST announce is its start gap
    /// ([`AnnounceSlot::start_gap_ms`]) when it has one, bounded by rule 5
    /// like any other: on a carrier where the set does not fit the gap,
    /// the second start announce waits out the budget's spacing. Derived
    /// from whether the slot has fired, so the pair costs no state.
    pub fn poll(&mut self, slot: AnnounceSlot, now_ms: u64, clock_ok: bool) -> Option<Decision> {
        let interval = match slot.start_gap_ms() {
            Some(gap) if !self.slot(slot).has_fired() => {
                let budget = self.budget.min_interval_ms();
                if budget > gap {
                    budget
                } else {
                    gap
                }
            }
            _ if slot == AnnounceSlot::Position => self.position_interval_ms(now_ms),
            _ => self.interval_ms(now_ms),
        };
        let gate = clock_ok || !slot.is_clock_gated();
        self.slot_mut(slot).poll(now_ms, gate, interval)
    }

    /// Hold one slot until `until_ms` for a reason of the caller's own.
    pub fn defer(&mut self, slot: AnnounceSlot, now_ms: u64, until_ms: u64) {
        self.slot_mut(slot).defer(now_ms, until_ms);
    }

    /// The slot's announce may not go out for a reason of the caller's own
    /// (the propagation role's store did not mount): if it is due, push it
    /// out to `now_ms + retry_ms` and say so; if it is not, change nothing.
    ///
    /// Checked instead of [`poll`](Self::poll), so a withheld announce
    /// spends nothing: the deadline is moved by the retry, never consumed
    /// by an announce that did not happen, and the first poll after the
    /// cause clears announces at once. Returns whether this call withheld
    /// one, the moment the caller reports it.
    pub fn withhold(&mut self, slot: AnnounceSlot, now_ms: u64, retry_ms: u64) -> bool {
        if self.wait_ms(slot, now_ms) > 0 {
            return false;
        }
        self.defer(slot, now_ms, now_ms.saturating_add(retry_ms));
        true
    }

    /// How long the caller may sleep before polling this slot.
    #[must_use]
    pub fn wait_ms(&self, slot: AnnounceSlot, now_ms: u64) -> u64 {
        self.slot(slot).wait_ms(now_ms)
    }

    fn slot(&self, slot: AnnounceSlot) -> &PeriodicAnnounce {
        match slot {
            AnnounceSlot::Own => &self.own,
            AnnounceSlot::Propagation => &self.propagation,
            AnnounceSlot::Position => &self.position,
        }
    }

    fn slot_mut(&mut self, slot: AnnounceSlot) -> &mut PeriodicAnnounce {
        match slot {
            AnnounceSlot::Own => &mut self.own,
            AnnounceSlot::Propagation => &mut self.propagation,
            AnnounceSlot::Position => &mut self.position,
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

    // -- The manual announce (#459) --------------------------------------

    fn a_lora_budget() -> DutyBudget {
        DutyBudget {
            frame_airtime_ms: 7_054,
            frames_per_set: 2,
            lawful_duty_e4: 1_000,
        }
    }

    /// A press on an affordable carrier announces now and is booked: the
    /// next tick and the next trigger are spaced from it, so a press
    /// right before a scheduled announce does not put two sets on the air
    /// inside the budget's spacing.
    #[test]
    fn a_manual_announce_is_booked_against_the_budget() {
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        let budget = a_lora_budget();
        cadence.set_budget(budget);
        let spacing = budget.min_interval_ms();
        let press = t0() + 1_000;
        assert_eq!(
            cadence.manual(press, false),
            ManualAnnounce::Now { position: false }
        );
        for slot in [AnnounceSlot::Own, AnnounceSlot::Propagation] {
            assert_eq!(
                cadence.wait_ms(slot, press),
                spacing,
                "the boot announce is held one spacing behind the press ({slot:?})"
            );
        }
        cadence.trigger(press + 1);
        assert_eq!(
            cadence.wait_ms(AnnounceSlot::Own, press + 1),
            spacing - 1,
            "a trigger after the press is spaced from it, not from boot"
        );
    }

    /// A press inside the budget's spacing sends nothing itself and brings
    /// both slots forward to the moment the budget allows: answered once,
    /// by the tick, never dropped.
    #[test]
    fn a_manual_announce_inside_the_spacing_is_deferred_not_dropped() {
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        let budget = a_lora_budget();
        cadence.set_budget(budget);
        let spacing = budget.min_interval_ms();
        let first = t0() + 1_000;
        assert_eq!(
            cadence.manual(first, false),
            ManualAnnounce::Now { position: false }
        );
        let second = first + 10_000;
        assert_eq!(
            cadence.manual(second, false),
            ManualAnnounce::Deferred {
                wait_ms: spacing - 10_000
            }
        );
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, first + spacing - 1, true),
            None
        );
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, first + spacing, true),
            Some(Decision::Announce)
        );
    }

    /// A press is not a re-arm: a deadline already further out than the
    /// spacing stays where it was.
    #[test]
    fn a_manual_announce_does_not_push_the_cadence_back() {
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        let first = t0() + crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS;
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, first, true),
            Some(Decision::Announce)
        );
        let due = cadence.wait_ms(AnnounceSlot::Own, first);
        assert_eq!(
            cadence.manual(first + 60_000, false),
            ManualAnnounce::Now { position: false }
        );
        assert_eq!(cadence.wait_ms(AnnounceSlot::Own, first), due);
    }

    /// Order 1316: a double press on a board that shares its position and
    /// has a fix books the position slot with the other two, so the
    /// position goes out on the same press and the next position tick is
    /// spaced from it. Positive control: the same press without the
    /// position (sharing off, or no fix) leaves the slot on its boot
    /// deadline, so the booking assertion is not vacuous.
    #[test]
    fn a_double_press_with_position_shared_books_the_position_slot() {
        let budget = a_lora_budget();
        let spacing = budget.min_interval_ms();
        let press = t0() + 1_000;
        let boot_due = crate::POSITION_ANNOUNCE_INITIAL_DELAY_MS - 1_000;
        assert!(
            spacing > boot_due,
            "the booking is visible over the boot deadline"
        );

        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        cadence.set_budget(budget);
        assert_eq!(
            cadence.manual(press, true),
            ManualAnnounce::Now { position: true }
        );
        for slot in [
            AnnounceSlot::Own,
            AnnounceSlot::Propagation,
            AnnounceSlot::Position,
        ] {
            assert_eq!(
                cadence.wait_ms(slot, press),
                spacing,
                "every slot the press sent is booked against the budget ({slot:?})"
            );
        }

        let mut control = AnnounceCadence::new();
        control.arm(t0());
        control.set_budget(budget);
        assert_eq!(
            control.manual(press, false),
            ManualAnnounce::Now { position: false }
        );
        assert_eq!(
            control.wait_ms(AnnounceSlot::Position, press),
            boot_due,
            "control: a press without the position leaves its slot alone"
        );
    }

    /// Order 1316: with sharing off the double press books the delivery and
    /// propagation slots and nothing else, as it did before. Positive
    /// control: a position announce sent inside the spacing keeps the
    /// position out of a press that asks for it (rule 5 spaces the slot
    /// from its own last announce), and the slot is brought forward to the
    /// moment the band allows instead of being dropped.
    #[test]
    fn a_double_press_with_sharing_off_books_only_the_two_halves() {
        let budget = a_lora_budget();
        let spacing = budget.min_interval_ms();
        let press = t0() + 1_000;

        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        cadence.set_budget(budget);
        let position_due = cadence.wait_ms(AnnounceSlot::Position, press);
        assert_eq!(
            cadence.manual(press, false),
            ManualAnnounce::Now { position: false }
        );
        for slot in [AnnounceSlot::Own, AnnounceSlot::Propagation] {
            assert_eq!(cadence.wait_ms(slot, press), spacing, "{slot:?} booked");
        }
        assert_eq!(
            cadence.wait_ms(AnnounceSlot::Position, press),
            position_due,
            "sharing off: the position slot is untouched"
        );

        // Control: the position went out recently on its own tick, the two
        // halves did not, and a press asks for all three.
        let mut control = AnnounceCadence::new();
        control.arm(t0());
        control.set_budget(budget);
        let sent = t0() + crate::POSITION_ANNOUNCE_INITIAL_DELAY_MS;
        assert_eq!(
            control.poll(AnnounceSlot::Position, sent, true),
            Some(Decision::Announce)
        );
        let late_press = sent + 10_000;
        assert_eq!(
            control.manual(late_press, true),
            ManualAnnounce::Now { position: false },
            "control: the position is spaced from its own last announce"
        );
        assert_eq!(
            control.wait_ms(AnnounceSlot::Position, late_press),
            spacing - 10_000,
            "control: and brought forward to the moment the band allows"
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

    /// The propagation role announces twice at boot, as `lxmd` does
    /// (lnpnd's `the_node_announces_twice_at_start_then_on_the_interval`
    /// is the host half): the router's announce
    /// [`PN_ANNOUNCE_INITIAL_DELAY_MS`] after boot, the deferred start
    /// announce [`PN_START_ANNOUNCE_GAP_MS`] after it, then the board's
    /// cadence from the second. A board whose one boot announce dies in a
    /// collision is otherwise an invisible propagation node until the
    /// floor fires, an hour later on a board that does not move.
    #[test]
    fn the_propagation_role_announces_twice_at_boot_then_on_the_cadence() {
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        let first = t0() + PN_ANNOUNCE_INITIAL_DELAY_MS;
        let second = first + PN_START_ANNOUNCE_GAP_MS;
        let mut announced = Vec::new();
        let mut now = t0();
        let end = second + STILL_ANNOUNCE_INTERVAL_MS;
        while now <= end {
            if cadence.poll(AnnounceSlot::Propagation, now, true) == Some(Decision::Announce) {
                announced.push(now - t0());
            }
            now += 1_000;
        }
        assert_eq!(
            announced,
            vec![
                first - t0(),
                second - t0(),
                second - t0() + STILL_ANNOUNCE_INTERVAL_MS
            ],
            "router announce, deferred start announce, then the floor from the second (ms after boot)"
        );
    }

    /// The board's own announce is not part of the start pair: it fires
    /// once at boot and then on the cadence, as before.
    #[test]
    fn the_own_announce_has_no_start_pair() {
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        let first = t0() + crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS;
        assert_eq!(
            cadence.poll(AnnounceSlot::Own, first, true),
            Some(Decision::Announce)
        );
        assert_eq!(
            cadence.wait_ms(AnnounceSlot::Own, first),
            STILL_ANNOUNCE_INTERVAL_MS
        );
    }

    /// The second start announce is still an announce rule 5 bounds: on a
    /// carrier where the set does not fit the gap, it waits out the
    /// budget's spacing instead.
    #[test]
    fn the_start_gap_waits_out_the_budget() {
        let mut cadence = AnnounceCadence::new();
        // The slow carrier of `a_slow_phy_stretches_the_fast_cadence`.
        let budget = DutyBudget {
            frame_airtime_ms: 7_054,
            frames_per_set: 2,
            lawful_duty_e4: 1_000,
        };
        assert!(cadence.set_budget(budget));
        assert!(budget.min_interval_ms() > PN_START_ANNOUNCE_GAP_MS);
        cadence.arm(t0());
        let first = t0() + PN_ANNOUNCE_INITIAL_DELAY_MS;
        assert_eq!(
            cadence.poll(AnnounceSlot::Propagation, first, true),
            Some(Decision::Announce)
        );
        assert_eq!(
            cadence.wait_ms(AnnounceSlot::Propagation, first),
            budget.min_interval_ms()
        );
    }

    /// The board's own announce, the role's two start announces and
    /// nothing else at boot: no two of them due at the same moment, so
    /// none contends with another for one airtime window.
    #[test]
    fn no_two_boot_announces_share_a_deadline() {
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        let mut boot = Vec::new();
        let mut now = t0();
        while now <= t0() + MOVEMENT_SAMPLE_INTERVAL_MS {
            for slot in [AnnounceSlot::Own, AnnounceSlot::Propagation] {
                if cadence.poll(slot, now, true) == Some(Decision::Announce) {
                    boot.push(now - t0());
                }
            }
            now += 1_000;
        }
        assert_eq!(
            boot.len(),
            3,
            "the role's start pair and the own announce: {boot:?}"
        );
        boot.sort_unstable();
        assert!(
            boot.windows(2)
                .all(|pair| pair[1] - pair[0] >= PN_START_ANNOUNCE_GAP_MS),
            "each boot announce is at least the pair's own gap from the next: {boot:?}"
        );
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

    /// The propagation role whose store did not mount (the board's
    /// `tick_announce`, leviculum#413; no rig cell unmounts the store). A
    /// tick before the slot is due withholds nothing; the due tick is
    /// pushed a retry out, and every tick inside the retry is quiet, so
    /// the caller's one log line is not repeated per pass; the store
    /// mounting inside the retry announces at the retry's end, the
    /// deadline never having been consumed.
    #[test]
    fn an_unmounted_store_withholds_the_due_announce_one_retry_at_a_time() {
        const RETRY_MS: u64 = 60_000;
        let mut cadence = AnnounceCadence::new();
        cadence.arm(t0());
        let pn_due = t0() + PN_ANNOUNCE_INITIAL_DELAY_MS;
        assert!(!cadence.withhold(AnnounceSlot::Propagation, pn_due - 1, RETRY_MS));
        assert_eq!(cadence.wait_ms(AnnounceSlot::Propagation, pn_due - 1), 1);
        assert!(cadence.withhold(AnnounceSlot::Propagation, pn_due, RETRY_MS));
        assert_eq!(cadence.wait_ms(AnnounceSlot::Propagation, pn_due), RETRY_MS);
        assert!(!cadence.withhold(AnnounceSlot::Propagation, pn_due + RETRY_MS - 1, RETRY_MS));
        assert!(cadence.withhold(AnnounceSlot::Propagation, pn_due + RETRY_MS, RETRY_MS));
        assert_eq!(
            cadence.poll(AnnounceSlot::Propagation, pn_due + 2 * RETRY_MS, true),
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
        assert_eq!(AnnounceSlot::Position.as_str(), "position-periodic");
    }

    // -- The position announce (order 1301) -------------------------------

    /// What one scripted run of the position slot put on the air.
    struct Track {
        /// When each position announce went out.
        sent: std::vec::Vec<u64>,
        /// When each delivery announce went out.
        own: std::vec::Vec<u64>,
    }

    /// Drive the cadence the way `leviculum_nrf::announce` does, from boot
    /// for `minutes`: a movement sample and a 25 m advance once a minute,
    /// both slots polled every ten seconds, a position announce suppressed
    /// while `track` says there is no fix. `track(minute)` is the board's
    /// position, metres north of the start, or `None` for no fix.
    fn run(
        cadence: &mut AnnounceCadence,
        minutes: u64,
        track: impl Fn(u64) -> Option<i64>,
    ) -> Track {
        let mut out = Track {
            sent: std::vec::Vec::new(),
            own: std::vec::Vec::new(),
        };
        cadence.arm(t0());
        let mut now = t0();
        let end = t0() + minutes * 60_000;
        while now < end {
            let minute = (now - t0()) / 60_000;
            let fix = track(minute).map(fix_at);
            if (now - t0()).is_multiple_of(step()) {
                cadence.note_fix(now, fix);
                if let Some(fix) = fix {
                    cadence.advance_position(now, fix);
                }
            }
            if cadence.poll(AnnounceSlot::Own, now, true) == Some(Decision::Announce) {
                out.own.push(now);
            }
            match fix {
                None => {
                    cadence.withhold(AnnounceSlot::Position, now, crate::POSITION_NO_FIX_RETRY_MS);
                }
                Some(fix) => {
                    if cadence.poll(AnnounceSlot::Position, now, true) == Some(Decision::Announce) {
                        cadence.note_position_sent(fix);
                        out.sent.push(now);
                    }
                }
            }
            now += 10_000;
        }
        out
    }

    /// The scripted day: twenty minutes still, forty walking north at
    /// 80 m a minute, sixty still again, ten without a fix, twenty still.
    fn day(minute: u64) -> Option<i64> {
        match minute {
            0..=19 => Some(0),
            20..=59 => Some((minute as i64 - 19) * 80),
            60..=119 => Some(40 * 80),
            120..=129 => None,
            _ => Some(40 * 80),
        }
    }

    fn gaps(at: &[u64]) -> std::vec::Vec<u64> {
        at.windows(2).map(|w| w[1] - w[0]).collect()
    }

    /// The cadence under a scripted fix track, at our LoRa defaults on the
    /// 10 % sub-band: delivery (554 ms) plus the sealed position (two
    /// frames) is three frames a set.
    #[test]
    fn the_position_announce_rides_the_cadence_under_a_scripted_walk() {
        let mut cadence = AnnounceCadence::new();
        cadence.set_budget(DutyBudget {
            frame_airtime_ms: 554,
            frames_per_set: 3,
            lawful_duty_e4: 1_000,
        });
        // 554 * 3 * 10_000 * 10 / 1_000: the budget's spacing.
        let spacing = 166_200;
        assert_eq!(cadence.budget().min_interval_ms(), spacing);
        let track = run(&mut cadence, 150, day);
        let s = &track.sent;

        // Boot: the first position announce 50 s after boot, ten after the
        // delivery announce.
        assert_eq!(
            track.own[0],
            t0() + crate::PERIODIC_ANNOUNCE_INITIAL_DELAY_MS
        );
        assert_eq!(s[0], t0() + crate::POSITION_ANNOUNCE_INITIAL_DELAY_MS);
        // Standing still: nothing more until the board moves. The first
        // sample of the walk is 80 m from the last position announced, so
        // the 25 m advance brings the next one forward at once, two
        // minutes before the movement proof (three samples) would.
        let walk_start = t0() + 20 * 60_000;
        assert_eq!(s[1], walk_start, "the advance acts on the first fix: {s:?}");
        // Walking 80 m a minute, every advance waits out the budget's
        // spacing and no more than one poll beyond it.
        let walk: std::vec::Vec<u64> = s
            .iter()
            .copied()
            .filter(|t| *t >= walk_start && *t < t0() + 60 * 60_000)
            .collect();
        for gap in gaps(&walk) {
            assert!(gap >= spacing, "never inside the cap's spacing: {gap}");
            assert!(gap <= spacing + 10_000, "the 25 m advance: {gap}");
        }
        assert_eq!(walk.len(), 15, "{walk:?}");
        // Stopped: the fast cadence runs out its fifteen minutes, then the
        // hourly floor; no position announce in the no-fix window.
        let no_fix = (t0() + 120 * 60_000)..(t0() + 130 * 60_000);
        assert!(s.iter().all(|t| !no_fix.contains(t)), "{s:?}");
        let after_stop: std::vec::Vec<u64> = s
            .iter()
            .copied()
            .filter(|t| *t >= t0() + 60 * 60_000)
            .collect();
        // Fifteen minutes of the fast cadence, then the hourly floor.
        assert_eq!(after_stop.len(), 5, "{after_stop:?}");
        assert!(
            gaps(&after_stop)
                .iter()
                .all(|g| *g >= MOVING_ANNOUNCE_INTERVAL_MS),
            "{after_stop:?}"
        );
    }

    /// The cap arithmetic: over the scripted walk the board's own
    /// announces, delivery and sealed position together, stay inside the
    /// tenth of the lawful duty its own announces may spend.
    #[test]
    fn the_walk_stays_inside_the_own_announce_share() {
        let mut cadence = AnnounceCadence::new();
        cadence.set_budget(DutyBudget {
            frame_airtime_ms: 554,
            frames_per_set: 3,
            lawful_duty_e4: 1_000,
        });
        let track = run(&mut cadence, 60, |minute| Some(minute as i64 * 80));
        // The airtimes `leviculum-lxmf/tests/lev_position_airtime.rs` pins.
        let airtime_ms = track.own.len() as u64 * 554 + track.sent.len() as u64 * 913;
        // 10 % lawful, a tenth of it: 1 % of the hour, 36 s.
        let share_ms = 3_600_000 / 100;
        assert!(
            airtime_ms <= share_ms,
            "{airtime_ms} ms of own announces in an hour ({} delivery, {} position)",
            track.own.len(),
            track.sent.len()
        );
    }

    /// With the channel at twice the announce cap the position announce
    /// takes twice the interval; the delivery announce is not thinned.
    #[test]
    fn a_full_channel_thins_the_position_and_nothing_else() {
        let mut cadence = AnnounceCadence::new();
        let now = settled(&mut cadence);
        cadence.set_channel_load(400);
        assert_eq!(cadence.interval_ms(now), STILL_ANNOUNCE_INTERVAL_MS);
        assert_eq!(
            cadence.position_interval_ms(now),
            STILL_ANNOUNCE_INTERVAL_MS
        );
        let mut moving = AnnounceCadence::new();
        let mut now = settled(&mut moving);
        for n in 1..=MOVEMENT_MIN_CONSECUTIVE_FIXES as i64 {
            now += step();
            moving.note_fix(now, Some(fix_at(n * 100)));
        }
        moving.set_channel_load(400);
        assert_eq!(moving.interval_ms(now), MOVING_ANNOUNCE_INTERVAL_MS);
        assert_eq!(
            moving.position_interval_ms(now),
            2 * MOVING_ANNOUNCE_INTERVAL_MS
        );
        // A board walking through a full channel advances no sooner than
        // the thinned spacing either.
        let track = run(&mut moving, 60, |minute| Some(minute as i64 * 80));
        for gap in gaps(&track.sent[1..]) {
            assert!(gap >= 2 * MOVEMENT_SAMPLE_INTERVAL_MS, "{gap}");
        }
    }

    /// A fix that has not moved 25 m does not bring the position forward,
    /// and before the first position announce nothing does.
    #[test]
    fn the_advance_needs_25_m_and_a_first_announce() {
        let mut cadence = AnnounceCadence::new();
        let now = settled(&mut cadence);
        assert!(!cadence.advance_position(now, fix_at(1_000)));
        assert_eq!(
            cadence.poll(AnnounceSlot::Position, now, true),
            Some(Decision::Announce)
        );
        cadence.note_position_sent(fix_at(0));
        assert!(!cadence.advance_position(now, fix_at(20)));
        assert!(cadence.advance_position(now, fix_at(30)));
        // Already brought forward: a second fix past 25 m moves nothing.
        assert!(!cadence.advance_position(now, fix_at(40)));
    }
}
