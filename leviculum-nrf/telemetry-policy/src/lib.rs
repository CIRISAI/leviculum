//! Telemetry send policy and target lifecycle (Codeberg #236).
//!
//! A telemetry producer states *when it wants to report* — a minimum
//! interval, a minimum distance moved, a maximum interval as a heartbeat,
//! an accuracy threshold and a settle time — and it holds no airtime
//! figure of its own. Duty cycle, spacing and back-off belong to the
//! interface (`docs/src/concepts/interface-isolation.md`,
//! `docs/src/concepts/regulatory-airtime.md`). Nothing in this crate
//! knows what a channel is, how long a frame takes to send, or that LoRa
//! exists.
//!
//! It is the pure part, next to `leviculum-gnss-time` and
//! `leviculum-gnss-presence` and for the same reason: the decision "report
//! now, and why" is a state machine over time, position and target state,
//! and a state machine that needs a radio to be exercised is a state
//! machine that is never exercised.
//!
//! Two things live here, because they are one decision:
//!
//! * **The cadence policy** — [`Profile`] bundles the parameter defaults,
//!   [`SendPolicy::poll`] answers "report now?" with a [`ReportReason`].
//! * **The target lifecycle** — a target is set by hash alone (the
//!   2026-08-22 UX decision on #236: users know the LXMF address, not the
//!   key), so the node must resolve the key over the air before it can
//!   encrypt anything. [`TargetState::AwaitingKey`] is that wait, stated
//!   rather than hidden, and the immediate report of the concept's
//!   observability rule fires on key arrival rather than on target
//!   setting.
//!
//! The configured target *is* the on-switch: no target is
//! [`TargetState::Off`] and that is the default, which removes the
//! on-without-target and off-with-target states entirely.

#![cfg_attr(not(test), no_std)]

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

/// Wire id of [`Profile::Tracker`], shared with the #238 control envelope
/// (`leviculum_core::envelope::TELEMETRY_PROFILE_TRACKER`).
pub const PROFILE_ID_TRACKER: u8 = 0x01;
/// Wire id of [`Profile::Station`]
/// (`leviculum_core::envelope::TELEMETRY_PROFILE_STATION`).
pub const PROFILE_ID_STATION: u8 = 0x02;
/// Wire id that clears the target instead of setting one
/// (`leviculum_core::envelope::TELEMETRY_PROFILE_OFF`). It is not a
/// [`Profile`]: "off" is the absence of a target, not a cadence.
pub const PROFILE_ID_OFF: u8 = 0x00;

/// Which cadence policy a target was configured with.
///
/// Profiles rather than individual knobs, because the two deployments
/// differ in kind and not in degree: a tracker is interesting when it
/// moves, a station is interesting when it is still alive. Both sets of
/// parameters remain individually addressable underneath
/// ([`PolicyParams`]) for the expert flags #236 describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Movement-driven: report when the node has moved far enough and not
    /// too recently, with a heartbeat as the fallback.
    Tracker,
    /// Slow heartbeat only, for a node that does not move. The default.
    Station,
}

impl Profile {
    /// The default profile: most users configure nothing beyond the
    /// address, and a node that does not move is the common case (a
    /// relay on a roof). A `station` misconfigured as a tracker reports
    /// nothing extra; a tracker misconfigured as a station under-reports
    /// but stays alive on the heartbeat — the failure is legible either
    /// way, and the cheaper one is the default.
    pub const DEFAULT: Self = Self::Station;

    /// Decode a wire profile id. [`PROFILE_ID_OFF`] is not a profile and
    /// yields `None`; the caller reads it as "clear the target".
    pub const fn from_wire(id: u8) -> Option<Self> {
        match id {
            PROFILE_ID_TRACKER => Some(Self::Tracker),
            PROFILE_ID_STATION => Some(Self::Station),
            _ => None,
        }
    }

    /// The wire id of this profile.
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Tracker => PROFILE_ID_TRACKER,
            Self::Station => PROFILE_ID_STATION,
        }
    }

    /// Short name for the structured `[TELEMETRY]` events.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tracker => "tracker",
            Self::Station => "station",
        }
    }

    /// The parameter defaults this profile bundles.
    pub const fn params(self) -> PolicyParams {
        match self {
            Self::Tracker => PolicyParams::TRACKER,
            Self::Station => PolicyParams::STATION,
        }
    }
}

/// The five cadence parameters, all of them policy and none of them
/// airtime.
///
/// Every duration is milliseconds because the firmware's monotonic clock
/// is; the distance is metres and the accuracy threshold is HDOP × 100,
/// which is what a GNSS receiver actually reports (see
/// [`Fix::hdop_e2`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyParams {
    /// Floor between two consecutive reports of any kind. A movement
    /// report that would land inside this window waits for it.
    pub min_interval_ms: u64,
    /// Ceiling between two consecutive reports: the heartbeat that keeps
    /// "stationary" distinguishable from "dead". Fires whether or not
    /// there is a position to report.
    pub max_interval_ms: u64,
    /// Metres the node must have moved since the last *reported* position
    /// before movement alone justifies a report. `0` disables the
    /// movement path entirely, which is what makes a station a station.
    pub min_distance_m: u32,
    /// Worst HDOP × 100 that still counts as a usable position. A fix
    /// above it is treated as no position at all: it is not reported, and
    /// it does not move the movement reference.
    pub max_hdop_e2: u16,
    /// How long after the first usable fix the movement path stays shut,
    /// so a cold start does not spend the channel on a drifting first
    /// fix. The heartbeat and the immediate report are exempt — the
    /// former carries no promise of precision, the latter is the
    /// operator's confirmation and the concept makes it unconditional.
    pub settle_ms: u64,
}

impl PolicyParams {
    /// Tracker defaults.
    ///
    /// One minute between reports and 50 m of movement is the shape of a
    /// person walking: at 1.4 m/s the distance gate opens after ~35 s and
    /// the interval gate then decides, so a walk reports about once a
    /// minute and a stationary rucksack falls back to the 15-minute
    /// heartbeat. HDOP 3.0 is the usual "good fix" line for consumer
    /// receivers; 60 s of settle covers a warm start's initial drift.
    pub const TRACKER: Self = Self {
        min_interval_ms: 60_000,
        max_interval_ms: 15 * 60_000,
        min_distance_m: 50,
        max_hdop_e2: 300,
        settle_ms: 60_000,
    };

    /// Station defaults.
    ///
    /// Hourly heartbeat and no movement path: `min_distance_m == 0`
    /// switches movement off, so a station reports on the clock alone
    /// even if it has a GNSS receiver and that receiver wanders. The
    /// interval floor equals the heartbeat, which is the honest way to
    /// say "this profile has exactly one cadence".
    pub const STATION: Self = Self {
        min_interval_ms: 60 * 60_000,
        max_interval_ms: 60 * 60_000,
        min_distance_m: 0,
        max_hdop_e2: 500,
        settle_ms: 30_000,
    };
}

// ---------------------------------------------------------------------------
// Target lifecycle
// ---------------------------------------------------------------------------

/// Where the configured target stands, and therefore whether anything can
/// be sent at all.
///
/// This is the honest three-state answer the 2026-08-22 UX decision
/// requires: hash-only configuration means the node may hold a perfectly
/// valid target it cannot yet encrypt to, and saying so beats waiting
/// silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    /// No target configured. Telemetry is off; this is the default.
    Off,
    /// A target hash is configured but its public key is not known yet.
    /// The node resolves it over the air (path request, or simply hearing
    /// the target's announce). Nothing can be sent from here.
    AwaitingKey,
    /// Key known: reports can be built and encrypted.
    Ready,
}

impl TargetState {
    /// Short name for the structured `[TELEMETRY]` events and the boot
    /// banner line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::AwaitingKey => "awaiting-key",
            Self::Ready => "ready",
        }
    }
}

/// Why a report is due. Carried into the `[TELEMETRY]` event so a log
/// tail answers "why did it send just then" without a second tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportReason {
    /// The one report a newly usable target owes the operator, whatever
    /// the cadence says. Fires when the key becomes known, which for a
    /// hash-only target is later than the moment it was set.
    Immediate,
    /// The node moved at least `min_distance_m` since the last reported
    /// position, and `min_interval_ms` has passed.
    Movement,
    /// `max_interval_ms` has passed since the last report. Fires with or
    /// without a position.
    Heartbeat,
}

impl ReportReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Movement => "movement",
            Self::Heartbeat => "heartbeat",
        }
    }
}

// ---------------------------------------------------------------------------
// Position
// ---------------------------------------------------------------------------

/// One position offered to the policy, in the same scaled-integer domain
/// the telemetry codec packs.
///
/// Degrees × 1e6 rather than floats: GNSS receivers deliver scaled
/// integers, the wire carries scaled integers, and a policy that decides
/// on floats would decide differently from the value it caused to be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fix {
    pub latitude_e6: i32,
    pub longitude_e6: i32,
    /// Horizontal dilution of precision × 100, as the receiver reported
    /// it. `None` when no HDOP-bearing sentence has been seen; the
    /// accuracy gate then cannot be applied and the fix is refused,
    /// because "we do not know how good this is" is not "it is good".
    pub hdop_e2: Option<u16>,
}

/// Metres per degree of latitude on WGS84, at the mean radius. The value
/// varies by about 1 % between equator and pole; the movement gate is a
/// threshold on a distance of tens of metres, so a 1 % model error is
/// three orders of magnitude below anything it decides.
const METRES_PER_DEGREE_LAT: i64 = 111_320;

/// cos(latitude) in Q15 at one-degree steps, 0°..=90°. Used to shrink a
/// longitude difference to metres. A table plus linear interpolation
/// rather than a trigonometric call: this crate has no `libm` and the
/// firmware has no FPU library beyond what the core provides, and the
/// interpolation error (< 1e-4 of the value) is again far below the
/// threshold's own resolution.
const COS_Q15: [i32; 91] = [
    32768, 32763, 32748, 32723, 32688, 32643, 32588, 32524, 32449, 32365, 32270, 32166, 32052,
    31928, 31795, 31651, 31499, 31336, 31164, 30983, 30792, 30592, 30382, 30163, 29935, 29698,
    29452, 29197, 28932, 28660, 28378, 28088, 27789, 27482, 27166, 26842, 26510, 26170, 25822,
    25466, 25102, 24730, 24351, 23965, 23571, 23170, 22763, 22348, 21926, 21498, 21063, 20622,
    20174, 19720, 19261, 18795, 18324, 17847, 17364, 16877, 16384, 15886, 15384, 14876, 14365,
    13848, 13328, 12803, 12275, 11743, 11207, 10668, 10126, 9580, 9032, 8481, 7927, 7371, 6813,
    6252, 5690, 5126, 4560, 3993, 3425, 2856, 2286, 1715, 1144, 572, 0,
];

/// cos(latitude) in Q15 for a latitude in degrees × 1e6, interpolated
/// between whole-degree table entries. Symmetric in the hemisphere and
/// clamped at the poles.
fn cos_lat_q15(latitude_e6: i32) -> i64 {
    let abs_e6 = (latitude_e6 as i64).abs().min(90_000_000);
    let whole = (abs_e6 / 1_000_000) as usize;
    if whole >= 90 {
        return 0;
    }
    let frac = abs_e6 % 1_000_000;
    let lo = COS_Q15[whole] as i64;
    let hi = COS_Q15[whole + 1] as i64;
    lo + (hi - lo) * frac / 1_000_000
}

/// Longitude difference in degrees × 1e6, taken the short way round so a
/// step across the antimeridian is one degree and not 359.
fn delta_lon_e6(a: i32, b: i32) -> i64 {
    let mut d = a as i64 - b as i64;
    if d > 180_000_000 {
        d -= 360_000_000;
    } else if d < -180_000_000 {
        d += 360_000_000;
    }
    d
}

/// Whether `a` and `b` are at least `min_m` metres apart.
///
/// Equirectangular approximation in millimetres: over the tens to
/// hundreds of metres a movement gate is set to, the error against the
/// great-circle distance is below the GNSS noise the gate exists to ride
/// out. Compares squares only after establishing that each component is
/// itself below the threshold, so the squares cannot overflow `i64` for
/// any threshold a firmware could hold.
fn moved_at_least(a: Fix, b: Fix, min_m: u32) -> bool {
    let threshold_mm = min_m as i64 * 1000;
    let dy_mm = (a.latitude_e6 as i64 - b.latitude_e6 as i64) * METRES_PER_DEGREE_LAT / 1000;
    let cos = cos_lat_q15((a.latitude_e6 / 2).saturating_add(b.latitude_e6 / 2));
    let dx_mm =
        delta_lon_e6(a.longitude_e6, b.longitude_e6) * METRES_PER_DEGREE_LAT / 1000 * cos / 32768;
    if dy_mm.abs() >= threshold_mm || dx_mm.abs() >= threshold_mm {
        return true;
    }
    dy_mm * dy_mm + dx_mm * dx_mm >= threshold_mm * threshold_mm
}

// ---------------------------------------------------------------------------
// The policy
// ---------------------------------------------------------------------------

/// What a telemetry-target control frame asks for, once its profile slot
/// has been read.
///
/// The wire form lives in `leviculum_core::envelope`; this is the same
/// decision expressed without a dependency on it, so the whole chain
/// "profile id in a frame → target state" is testable in one crate that
/// links nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetCommand {
    /// [`PROFILE_ID_OFF`]: forget the target. Telemetry off.
    Clear,
    /// Any other id: report to this target with that cadence. An id this
    /// firmware does not know arrives as [`Profile::DEFAULT`] — a newer
    /// host's cadence preference is not worth losing the destination
    /// over, and the state the node reports says which profile it runs.
    Set(Profile),
}

/// Read a control frame's profile slot.
pub const fn command_from_wire(profile_id: u8) -> TargetCommand {
    if profile_id == PROFILE_ID_OFF {
        TargetCommand::Clear
    } else {
        match Profile::from_wire(profile_id) {
            Some(profile) => TargetCommand::Set(profile),
            None => TargetCommand::Set(Profile::DEFAULT),
        }
    }
}

/// What applying a [`TargetCommand`] did, for the caller's event line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOutcome {
    /// A target was set or replaced; the state says whether it is usable
    /// yet ([`TargetState::Ready`]) or still resolving its key
    /// ([`TargetState::AwaitingKey`]).
    Set(TargetState),
    /// The target was cleared: telemetry off.
    Cleared,
}

/// The send policy and target lifecycle of one node.
///
/// Drive it with [`set_target`](Self::set_target) /
/// [`note_key_available`](Self::note_key_available) /
/// [`clear_target`](Self::clear_target) from the control channel, with
/// [`poll`](Self::poll) from the main loop, and confirm every report the
/// radio actually took with [`note_emitted`](Self::note_emitted) followed
/// by [`note_dispatch`](Self::note_dispatch).
///
/// The split between `poll` and the confirmation is deliberate: a report
/// that could not be built, could not be handed to transport, or was
/// handed over and then lost by the dispatch must not consume the
/// cadence, or a node would go quiet for a whole interval after one
/// failed attempt. The converse is just as deliberate and is enforced by
/// the attempt floor in [`poll`](Self::poll): a node that cannot deliver
/// must not retry faster than it would have reported, or one unreachable
/// target turns a reporter into a transmitter.
#[derive(Debug, Clone)]
pub struct SendPolicy {
    profile: Profile,
    params: PolicyParams,
    state: TargetState,
    /// Armed when the target became usable and not yet spent.
    immediate_pending: bool,
    /// When the first fix passing the accuracy gate was seen; the settle
    /// window is measured from here.
    first_fix_ms: Option<u64>,
    last_report_ms: Option<u64>,
    /// When a report was last *handed to transport*, whatever became of
    /// it. Separate from `last_report_ms` because the two answer different
    /// questions — "when did a report actually go out" versus "when did
    /// this node last spend airtime trying" — and only the second one can
    /// bound a retry. See [`poll`](SendPolicy::poll).
    last_attempt_ms: Option<u64>,
    last_reported_fix: Option<Fix>,
    /// A report handed to transport whose dispatch has not been settled
    /// yet. See [`note_emitted`](Self::note_emitted).
    pending: Option<PendingReport>,
}

/// A report that is out of the reporter's hands but not yet on the air.
#[derive(Debug, Clone, Copy)]
struct PendingReport {
    /// When it was emitted. The cadence anchors here rather than at
    /// settle time, so the confirmation's own latency does not shorten
    /// the next interval.
    now_ms: u64,
    /// The position it carried, `None` if it carried none.
    fix: Option<Fix>,
}

impl Default for SendPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl SendPolicy {
    /// A node with no target: telemetry off, default profile parameters
    /// loaded so a `set_target` that names no profile still has them.
    pub const fn new() -> Self {
        Self {
            profile: Profile::DEFAULT,
            params: Profile::DEFAULT.params(),
            state: TargetState::Off,
            immediate_pending: false,
            first_fix_ms: None,
            last_report_ms: None,
            last_attempt_ms: None,
            last_reported_fix: None,
            pending: None,
        }
    }

    pub const fn state(&self) -> TargetState {
        self.state
    }

    pub const fn profile(&self) -> Profile {
        self.profile
    }

    pub const fn params(&self) -> PolicyParams {
        self.params
    }

    /// Override individual parameters (the expert flags under the
    /// profile presets, #236). Applied on top of whatever profile is
    /// configured; a later [`set_target`](Self::set_target) reloads the
    /// profile defaults over them.
    pub fn set_params(&mut self, params: PolicyParams) {
        self.params = params;
    }

    /// Configure a target. `key_known` is the caller's answer to "do we
    /// already hold this destination's public key" — for a hash-only
    /// frame it is `false` and the node enters
    /// [`TargetState::AwaitingKey`].
    ///
    /// Returns the state it entered. Setting a target always restarts the
    /// cadence: the last report went to somebody else.
    ///
    /// It does *not* restart the attempt floor. Airtime is airtime whoever
    /// the recipient is, and an exception here would be an escape hatch —
    /// a host that re-sends its target frame on a timer would drive
    /// exactly the storm the floor exists to stop.
    pub fn set_target(&mut self, profile: Profile, key_known: bool) -> TargetState {
        self.profile = profile;
        self.params = profile.params();
        self.last_report_ms = None;
        self.last_reported_fix = None;
        if key_known {
            self.state = TargetState::Ready;
            self.immediate_pending = true;
        } else {
            self.state = TargetState::AwaitingKey;
            // Not armed yet: the immediate report cannot be sent without
            // the key, and arming it here would let it fire on a target
            // that is later cleared before the key ever arrives.
            self.immediate_pending = false;
        }
        self.state
    }

    /// Apply a control frame's decision in one call: the set/clear split
    /// and the key lookup that decides ready versus awaiting-key.
    ///
    /// `key_known` is the caller's answer to "do we already hold this
    /// destination's public key". It is ignored for
    /// [`TargetCommand::Clear`].
    pub fn apply(&mut self, command: TargetCommand, key_known: bool) -> TargetOutcome {
        match command {
            TargetCommand::Clear => {
                self.clear_target();
                TargetOutcome::Cleared
            }
            TargetCommand::Set(profile) => TargetOutcome::Set(self.set_target(profile, key_known)),
        }
    }

    /// The target's public key arrived over the air. Returns `true` if
    /// this was the transition out of [`TargetState::AwaitingKey`], which
    /// is the moment the immediate report is armed and the event worth
    /// logging.
    pub fn note_key_available(&mut self) -> bool {
        if self.state == TargetState::AwaitingKey {
            self.state = TargetState::Ready;
            self.immediate_pending = true;
            true
        } else {
            false
        }
    }

    /// The key went away again (a target changed to one we do not hold).
    /// Only meaningful from [`TargetState::Ready`].
    pub fn note_key_lost(&mut self) -> bool {
        if self.state == TargetState::Ready {
            self.state = TargetState::AwaitingKey;
            self.immediate_pending = false;
            true
        } else {
            false
        }
    }

    /// Clear the target: telemetry off. The cadence state goes with it,
    /// so a later target does not inherit a stale "last reported" from a
    /// different recipient.
    pub fn clear_target(&mut self) {
        self.state = TargetState::Off;
        self.immediate_pending = false;
        self.last_report_ms = None;
        self.last_reported_fix = None;
    }

    /// Whether this fix is good enough to be reported as a position.
    ///
    /// A fix with no HDOP at all is refused: the concept's "no fix, no
    /// position" rule extends to "no idea how good the fix is". The
    /// heartbeat still reports without a position, which is exactly the
    /// absence encoding the concept fixes.
    pub const fn position_is_reportable(&self, fix: Fix) -> bool {
        match fix.hdop_e2 {
            Some(hdop) => hdop <= self.params.max_hdop_e2,
            None => false,
        }
    }

    /// Ask whether a report is due now.
    ///
    /// `fix` is the current GNSS answer: `None` when the node has no fix
    /// (presence is not `Fix`, or the receiver is absent). A fix that
    /// fails the accuracy gate is treated exactly like `None` — it is not
    /// a position, so it neither reports nor moves the movement
    /// reference.
    ///
    /// Mutates only the settle anchor: the first usable fix starts the
    /// settle window whether or not anything is sent.
    ///
    /// # The attempt floor
    ///
    /// The first gate is not the cadence but `min_interval_ms` since the
    /// last *emission*, successful or not, and it sits ahead of every
    /// other path including the immediate report. It states one invariant:
    ///
    /// > Between any two emissions of a telemetry report, successful or
    /// > not, at least `min_interval_ms` of clock has passed.
    ///
    /// A lost dispatch consumes no cadence and no reading — that is
    /// deliberate and stays — but without a second clock the node then
    /// finds the same interval elapsed on the very next tick and re-emits
    /// at the tick rate. Measured on the bench: an announce-plus-report
    /// pair every 6.5 s against a 60 s policy, ~20 % channel occupancy
    /// from one node.
    ///
    /// A node that has emitted nothing yet has no floor to clear, which is
    /// what keeps the immediate report of a newly usable target immediate.
    pub fn poll(&mut self, now_ms: u64, fix: Option<Fix>) -> Option<ReportReason> {
        if self.state != TargetState::Ready {
            return None;
        }
        let usable = fix.filter(|f| self.position_is_reportable(*f));
        if usable.is_some() && self.first_fix_ms.is_none() {
            self.first_fix_ms = Some(now_ms);
        }
        if let Some(attempt) = self.last_attempt_ms {
            if now_ms.saturating_sub(attempt) < self.params.min_interval_ms {
                return None;
            }
        }
        if self.immediate_pending {
            return Some(ReportReason::Immediate);
        }
        let last = match self.last_report_ms {
            // Ready, nothing armed, nothing sent yet: the heartbeat
            // clock starts at the first poll rather than at boot, so a
            // target set long after boot does not fire instantly through
            // the heartbeat path.
            None => {
                self.last_report_ms = Some(now_ms);
                return None;
            }
            Some(last) => last,
        };
        let since = now_ms.saturating_sub(last);
        if since >= self.params.max_interval_ms {
            return Some(ReportReason::Heartbeat);
        }
        if self.params.min_distance_m == 0 || since < self.params.min_interval_ms {
            return None;
        }
        if let Some(first) = self.first_fix_ms {
            if now_ms.saturating_sub(first) < self.params.settle_ms {
                return None;
            }
        }
        match (usable, self.last_reported_fix) {
            // Nothing to compare against yet: the first usable fix after
            // the target became ready is itself the movement.
            (Some(_), None) => Some(ReportReason::Movement),
            (Some(now), Some(then)) if moved_at_least(now, then, self.params.min_distance_m) => {
                Some(ReportReason::Movement)
            }
            _ => None,
        }
    }

    /// Confirm that a report actually went out, with the position it
    /// carried (`None` when it carried none). Only this consumes the
    /// cadence and the armed immediate report.
    pub fn note_sent(&mut self, now_ms: u64, reported: Option<Fix>) {
        self.immediate_pending = false;
        self.last_report_ms = Some(now_ms);
        if reported.is_some() {
            self.last_reported_fix = reported;
        }
    }

    /// Note that a report has been *handed to transport* — built,
    /// encrypted, turned into actions — and is awaiting dispatch.
    ///
    /// This consumes nothing. Handing a packet to the core is not the
    /// same event as the interface taking it: on a board whose outbound
    /// queue is full, `send_single_packet` succeeds and the dispatch that
    /// follows drops the frame. Counting the first event as "sent" is how
    /// a report that never left the board still cost a whole cadence
    /// interval of silence (#344).
    ///
    /// It does, however, start the attempt floor: this is the moment
    /// airtime was spent, and [`poll`](Self::poll) refuses to emit again
    /// for `min_interval_ms` from here whatever the dispatch decides. That
    /// is the whole of the rate limit — the failure path adds nothing,
    /// because a floor that only the failure path raised would be a floor
    /// a caller could forget to raise.
    ///
    /// Pair it with [`note_dispatch`](Self::note_dispatch). Two
    /// `note_emitted` calls without a settle in between keep only the
    /// later one: the earlier report is gone either way, and the cadence
    /// belongs to the report that is actually in flight.
    pub fn note_emitted(&mut self, now_ms: u64, reported: Option<Fix>) {
        self.last_attempt_ms = Some(now_ms);
        self.pending = Some(PendingReport {
            now_ms,
            fix: reported,
        });
    }

    /// Settle the pending report against what the dispatch did with it.
    ///
    /// `delivered` is the dispatch's own verdict, not a guess from a log
    /// line: `true` consumes the cadence exactly as
    /// [`note_sent`](Self::note_sent) does, `false` consumes neither the
    /// cadence nor the reading, so the reading is still owed and goes out
    /// on the first tick past the attempt floor
    /// ([`poll`](Self::poll)) rather than on the very next one.
    ///
    /// Deliberately *not* a retry: nothing is re-sent here, nothing is
    /// queued, and the next attempt happens on the ordinary tick that was
    /// going to run anyway — the *reading* it carries is whatever the
    /// sensors say then, not the one that failed.
    ///
    /// Returns whether the report counted as sent — `false` also for a
    /// settle with nothing pending, so a caller cannot report success for
    /// a report it never emitted.
    pub fn note_dispatch(&mut self, delivered: bool) -> bool {
        match self.pending.take() {
            Some(report) if delivered => {
                self.note_sent(report.now_ms, report.fix);
                true
            }
            _ => false,
        }
    }

    /// Whether a report is emitted and not yet settled.
    pub const fn has_pending_report(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests;
