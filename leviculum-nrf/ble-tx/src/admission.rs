//! The scanner's dial admission (#412, #413): which advertiser a board
//! may put into its scan window, the address-keyed dead-end table that
//! is one of its inputs, and the strict-to-fallback clock that sets the
//! [`ScanMode`] the verdict is computed under.
//!
//! All three used to live in the firmware's `columba.rs`, reading
//! `embassy_time::Instant` directly, so the rule that decides whom a
//! board dials next could only be exercised on the rig. Here the clock
//! is a parameter (`now_ms`, milliseconds on any monotonic clock) and
//! the firmware keeps only the locks around its single instances and
//! the log lines; `tests/graph_formation.rs` drives the same
//! [`admit`] the firmware's scan callback does.
//!
//! # The six filters
//!
//! [`admit`] evaluates them in this order and names the first that
//! excludes the advertiser ([`Exclusion`]):
//!
//! 1. the verdict does not initiate ([`ConnectDecision::initiate`]);
//! 2. the address is a dead end ([`DeadEnds`]);
//! 3. a live connection was made on this address — Core Spec Vol 6
//!    Part B §4.5 dooms a second one ([`PeerRegistry::addr_linked`]);
//! 4. the advertised identity hint belongs to a peer we already hold
//!    ([`PeerRegistry::hint_linked`]), the same exclusion one level up,
//!    where a rotated address cannot hide;
//! 5. a FALLBACK verdict while the dial ledger's pause stands
//!    ([`DialLedger::fallback_held`], #412 part 2);
//! 6. a FALLBACK verdict to a ROTATING address while the churn ledger's
//!    cooldown stands ([`ChurnLedger::rotating_fallback_held`], #417).
//!
//! The order is load-bearing for the log lines, not for the answer: the
//! firmware says `BLE_DIAL_LEDGER held` and `BLE_CHURN_LEDGER held` only
//! when that filter is the one that decided, exactly as it did while the
//! chain was a short-circuiting `||`.

use crate::{
    rotating_address, should_initiate, ChurnLedger, ConnectDecision, DialLedger, PeerRegistry,
    ScanMode, IDENTITY_HINT_LEN, SCAN_FALLBACK_AFTER_MS,
};

/// How long a dead-end address is skipped: one that carried our own
/// identity, one whose duplicate handshake was refused for an actively
/// used old link (#360 — a dial that REPLACES the old link stays
/// connected and never lands here), or one a fallback dial could not
/// connect to. Sized to the RPA rotation timescale (minutes): the
/// address dies on its own at the peer's next rotation, this just stops
/// us re-dialling it every scan pass until then — the rig's
/// pre-#375-part-2 capture shows the alternative, a 5 s timeout dial at
/// the same address every ~20 s, forever.
///
/// The firmware's `DEAD_END_TTL` (a `Duration` of 120 s) until #496; in
/// milliseconds here because this crate has no clock type.
pub const DEAD_END_TTL_MS: u64 = 120_000;

/// Room for the addresses [`DEAD_END_TTL_MS`] talks about: a revealed
/// self-link plus a run of failed fallback targets (at most one failure
/// per ~13 s dial cycle, so eight slots bridge more than one TTL of
/// them). Overflow reuses the oldest entry — an early re-dial, not a
/// loss.
///
/// `2 * MAX_LINKS` on every board (`MAX_LINKS` is 4); the firmware
/// asserts that at compile time, so the two cannot drift.
pub const DEAD_END_SLOTS: usize = 8;

/// The address type an advertising report carries beside the six
/// bytes (Core Spec Vol 6 Part B §1.3; the SoftDevice's
/// `BLE_GAP_ADDR_TYPE_*`).
///
/// It travels with the address since #496 so the question "does this
/// peer redraw its address" can one day be answered from the type
/// rather than guessed from the top two bits ([`rotating_address`]).
/// Nothing in [`admit`] reads it yet — that is a later pass, after the
/// reviewer has read the `issue_412_` reds below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrType {
    /// An IEEE-assigned public address. Its top bits are the OUI's and
    /// carry no class at all.
    Public,
    /// A random static address, bound to the power cycle.
    RandomStatic,
    /// A resolvable private address, redrawn every few minutes.
    RandomPrivateResolvable,
    /// A non-resolvable private address, redrawn the same way.
    RandomPrivateNonResolvable,
    /// An anonymous advertisement: no address at all.
    Anonymous,
    /// A raw value the Core Spec does not define.
    Other(u8),
}

impl AddrType {
    /// The type from the SoftDevice's `ble_gap_addr_t::addr_type`.
    #[must_use]
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            0x00 => Self::Public,
            0x01 => Self::RandomStatic,
            0x02 => Self::RandomPrivateResolvable,
            0x03 => Self::RandomPrivateNonResolvable,
            0x7F => Self::Anonymous,
            other => Self::Other(other),
        }
    }
}

/// An advertiser's address as the scanner received it: the number the
/// v2.2 sort compares ([`crate::addr_value`]) and the type it came with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerAddr {
    /// The 48-bit value, as [`crate::addr_value`] reads the raw bytes.
    pub value: u64,
    /// The type the report carried.
    pub addr_type: AddrType,
}

impl PeerAddr {
    /// The address from the raw report: LSB-first bytes and the raw type.
    #[must_use]
    pub fn from_raw(addr_le: &[u8; 6], addr_type: u8) -> Self {
        Self {
            value: crate::addr_value(addr_le),
            addr_type: AddrType::from_raw(addr_type),
        }
    }
}

/// The addresses the scanner must not dial for a while, and when each
/// was condemned: a self-link revealed post-connect, an address whose
/// identity we already hold a LIVE link to (#376), and failed fallback
/// dials (#375 §0 — a fallback target that does not answer a
/// CONNECT_IND within 5 s is gone or unreachable by rule, and without
/// this entry the 5 s backoff re-dialled it forever).
///
/// The firmware's `RecentDeadEnds` until #496, with the clock moved out
/// into every call.
#[derive(Debug, Clone, Copy)]
pub struct DeadEnds {
    entries: [Option<(u64, u64)>; DEAD_END_SLOTS],
}

impl Default for DeadEnds {
    fn default() -> Self {
        Self::new()
    }
}

impl DeadEnds {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [None; DEAD_END_SLOTS],
        }
    }

    /// Condemn `addr` from `now_ms` for [`DEAD_END_TTL_MS`], reusing an
    /// expired entry or else the oldest one.
    pub fn note(&mut self, addr: u64, now_ms: u64) {
        let slot = self
            .entries
            .iter()
            .position(|e| e.is_none_or(|(_, at)| now_ms.saturating_sub(at) >= DEAD_END_TTL_MS))
            .unwrap_or_else(|| {
                self.entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| e.map_or(0, |(_, at)| at))
                    .map_or(0, |(i, _)| i)
            });
        self.entries[slot] = Some((addr, now_ms));
    }

    /// Whether `addr` is still condemned at `now_ms`.
    #[must_use]
    pub fn contains(&self, addr: u64, now_ms: u64) -> bool {
        self.entries
            .iter()
            .flatten()
            .any(|&(a, at)| a == addr && now_ms.saturating_sub(at) < DEAD_END_TTL_MS)
    }
}

/// Whether a dial that failed to CONNECT condemns its address: only a
/// fallback verdict does (#375 §0). A failed strict dial keeps only the
/// short retry backoff, because the sort will keep electing that peer
/// and a two-minute hole toward it would cost real links.
#[must_use]
pub fn connect_failure_condemns(decision: ConnectDecision) -> bool {
    decision == ConnectDecision::InitiateFallback
}

/// Everything that restarts or leaves alone the strict phase — the
/// reset rule as an explicit input, so a test can state which outcome
/// does what (#412).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseEvent {
    /// A connection came up in either role, before any identity
    /// (`conn_link_up`; the rig showed the fallback firing in the 3 s
    /// gap between `BLE: connected` and the handshake, #375 §0).
    ConnUp,
    /// A connection went down (`conn_link_down`): a board that just
    /// lost a link gets the full strict bound before it may dial
    /// against the sort.
    ConnDown,
    /// An identity's link registered plainly (`LinkUp::Accepted`).
    LinkAccepted,
    /// An identity's newer link displaced its older one
    /// (`LinkUp::Displaced`): a rotating peer reconnecting.
    LinkDisplaced,
    /// A duplicate was refused (`LinkUp::Refused`): no link came up.
    LinkRefused,
    /// The scan window closed on a STRICT verdict: the strict rule
    /// works here.
    StrictTarget,
    /// The scan window closed on a FALLBACK verdict: if that dial
    /// fails the board is still stranded and must stay in fallback.
    FallbackTarget,
}

impl PhaseEvent {
    /// Whether this event restarts the strict phase, as shipped: every
    /// link that came up or went down, whatever kind, and a strict
    /// target; a refusal and a fallback target leave the clock alone.
    #[must_use]
    pub const fn resets_strict(self) -> bool {
        match self {
            Self::ConnUp
            | Self::ConnDown
            | Self::LinkAccepted
            | Self::LinkDisplaced
            | Self::StrictTarget => true,
            Self::LinkRefused | Self::FallbackTarget => false,
        }
    }
}

/// The strict-phase clock behind [`SCAN_FALLBACK_AFTER_MS`]: when the
/// current strict phase began and whether the switch to fallback was
/// already announced for this phase. Reset by every [`PhaseEvent`] that
/// [`PhaseEvent::resets_strict`]; otherwise it runs whenever the
/// central is scanning, live links notwithstanding (the eager spec,
/// #375 part 3).
///
/// The firmware's `FALLBACK_CLOCK` tuple until #496.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanPhase {
    since_ms: u64,
    announced: bool,
}

impl Default for ScanPhase {
    fn default() -> Self {
        Self::new()
    }
}

/// What one [`ScanPhase::poll`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhasePoll {
    /// The mode the current strict phase has reached.
    pub mode: ScanMode,
    /// `Some(elapsed_ms)` exactly once per phase: the poll that first
    /// saw fallback, for the `BLE_SCAN_FALLBACK after_ms=` line.
    pub announce_ms: Option<u64>,
}

impl ScanPhase {
    /// A phase that began at boot, time zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            since_ms: 0,
            announced: false,
        }
    }

    /// Restart the strict phase at `now_ms`.
    pub fn reset(&mut self, now_ms: u64) {
        *self = Self {
            since_ms: now_ms,
            announced: false,
        };
    }

    /// Apply one event under the shipped reset rule; `true` iff it
    /// restarted the phase.
    pub fn note(&mut self, event: PhaseEvent, now_ms: u64) -> bool {
        let resets = event.resets_strict();
        if resets {
            self.reset(now_ms);
        }
        resets
    }

    /// The mode at `now_ms`, marking the phase announced the first time
    /// it reads fallback.
    pub fn poll(&mut self, now_ms: u64) -> PhasePoll {
        let elapsed_ms = now_ms.saturating_sub(self.since_ms);
        if elapsed_ms < SCAN_FALLBACK_AFTER_MS {
            return PhasePoll {
                mode: ScanMode::Strict,
                announce_ms: None,
            };
        }
        let announce_ms = (!self.announced).then_some(elapsed_ms);
        self.announced = true;
        PhasePoll {
            mode: ScanMode::Fallback,
            announce_ms,
        }
    }
}

/// The board's own side of the verdict: its v0.3.0 capability byte and
/// its current address value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    /// `LOCAL_CAPS` on the firmware.
    pub caps: u8,
    /// Our address as configured in the SoftDevice ([`crate::addr_value`]).
    pub addr: u64,
}

/// What one advertising report offers the admission, already parsed
/// ([`crate::parse_peer_advertisement`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvReport {
    /// The advertiser's address and type.
    pub addr: PeerAddr,
    /// The capability record, `None` when there was none.
    pub caps: Option<u8>,
    /// The identity hint, `None` when the record carried none.
    pub identity_hint: Option<[u8; IDENTITY_HINT_LEN]>,
}

/// The two identity-keyed ledgers [`admit`] consults for a fallback
/// verdict.
#[derive(Debug, Clone, Copy)]
pub struct Ledgers<'a> {
    /// The dial ledger (#412 part 2).
    pub dial: &'a DialLedger,
    /// The churn ledger (#417).
    pub churn: &'a ChurnLedger,
}

/// Which filter excluded an advertiser, in [`admit`]'s order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    /// Filter 1: the verdict does not initiate.
    NotInitiate,
    /// Filter 2: the address is in [`DeadEnds`].
    DeadEnd,
    /// Filter 3: a live connection was made on this address.
    AddrLinked,
    /// Filter 4: the identity hint names a peer we already hold.
    HintLinked,
    /// Filter 5: a fallback verdict under the dial ledger's pause,
    /// with its deadline for the `BLE_DIAL_LEDGER held` line.
    LedgerHeld {
        /// [`DialLedger::held_until_ms`].
        until_ms: u64,
    },
    /// Filter 6: a fallback verdict to a rotating address under the
    /// churn ledger's cooldown, with its deadline.
    ChurnHeld {
        /// [`ChurnLedger::held_until_ms`].
        until_ms: u64,
    },
}

/// One report's admission: the verdict the rule gave (logged on
/// `BLE_SCAN_DECISION` whatever came next) and whether the advertiser
/// may enter the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admission {
    /// [`should_initiate`]'s verdict under the given mode.
    pub decision: ConnectDecision,
    /// `Ok(())` admits the advertiser; `Err` names the filter.
    pub outcome: Result<(), Exclusion>,
}

impl Admission {
    /// Whether the advertiser may enter the window.
    #[must_use]
    pub fn admitted(&self) -> bool {
        self.outcome.is_ok()
    }
}

/// Decide one advertising report: the [`should_initiate`] verdict under
/// `mode`, then the six filters of the module docs in their order.
#[must_use]
pub fn admit<const N: usize>(
    local: Local,
    report: &AdvReport,
    mode: ScanMode,
    registry: &PeerRegistry<N>,
    dead_ends: &DeadEnds,
    ledgers: Ledgers<'_>,
    now_ms: u64,
) -> Admission {
    let peer_value = report.addr.value;
    let decision = should_initiate(local.caps, local.addr, report.caps, peer_value, mode);
    let fallback = decision == ConnectDecision::InitiateFallback;
    let outcome = if !decision.initiate() {
        Err(Exclusion::NotInitiate)
    } else if dead_ends.contains(peer_value, now_ms) {
        Err(Exclusion::DeadEnd)
    } else if registry.addr_linked(peer_value) {
        Err(Exclusion::AddrLinked)
    } else if registry.hint_linked(report.identity_hint) {
        Err(Exclusion::HintLinked)
    } else if let Some(until_ms) = fallback
        .then(|| {
            held_until(
                ledgers.dial.fallback_held(now_ms),
                ledgers.dial.held_until_ms(),
            )
        })
        .flatten()
    {
        Err(Exclusion::LedgerHeld { until_ms })
    } else if let Some(until_ms) = (fallback && rotating_address(peer_value))
        .then(|| {
            held_until(
                ledgers.churn.rotating_fallback_held(now_ms),
                ledgers.churn.held_until_ms(),
            )
        })
        .flatten()
    {
        Err(Exclusion::ChurnHeld { until_ms })
    } else {
        Ok(())
    };
    Admission { decision, outcome }
}

/// A ledger's hold as the firmware read it: held only while the ledger
/// says so AND it has a deadline to report.
fn held_until(held: bool, until_ms: Option<u64>) -> Option<u64> {
    held.then_some(until_ms).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChurnPolicy, LedgerPolicy, Origin, CAP_PERIPHERAL_ONLY, PERIPH_SLOTS};

    /// A board's static random address: top two bits `11`.
    const OURS: u64 = 0xC000_0000_5000;
    /// A static board above us: the sort lets us dial it.
    const BOARD_ABOVE: u64 = 0xC000_0000_9000;
    /// A static board below us: the sort makes us wait.
    const BOARD_BELOW: u64 = 0xC000_0000_1000;
    /// A resolvable private address (top bits `01`), below every board.
    const PHONE_RPA: u64 = 0x4000_0000_1234;
    const DUAL_ROLE: u8 = 0;
    const LOCAL: Local = Local {
        caps: DUAL_ROLE,
        addr: OURS,
    };

    fn report(value: u64, addr_type: AddrType, hint: Option<[u8; 4]>) -> AdvReport {
        AdvReport {
            addr: PeerAddr { value, addr_type },
            caps: None,
            identity_hint: hint,
        }
    }

    fn identity(byte: u8) -> [u8; 16] {
        [byte; 16]
    }

    /// The admission against an empty board: nothing linked, nothing
    /// condemned, nothing held.
    fn admit_clean(report: &AdvReport, mode: ScanMode) -> Admission {
        let registry: PeerRegistry<4> = PeerRegistry::new();
        admit(
            LOCAL,
            report,
            mode,
            &registry,
            &DeadEnds::new(),
            Ledgers {
                dial: &DialLedger::new(),
                churn: &ChurnLedger::new(),
            },
            1_000,
        )
    }

    /// A dial ledger whose pause stands at `now_ms`.
    fn held_dial_ledger(now_ms: u64) -> DialLedger {
        let policy = LedgerPolicy::MEASURED;
        let mut ledger = DialLedger::new();
        for _ in 0..policy.wasted_run {
            ledger.note(policy, &identity(7), None, now_ms);
        }
        assert!(ledger.fallback_held(now_ms), "the fixture arms the pause");
        ledger
    }

    /// A churn ledger whose cooldown stands at `now_ms`.
    fn held_churn_ledger(now_ms: u64) -> ChurnLedger {
        let policy = ChurnPolicy::MEASURED;
        let mut ledger = ChurnLedger::new();
        for _ in 0..policy.leave_run {
            ledger.note_leave(policy, &identity(9), now_ms);
        }
        assert!(
            ledger.rotating_fallback_held(now_ms),
            "the fixture arms the cooldown"
        );
        ledger
    }

    #[test]
    fn a_clean_board_admits_a_strict_and_a_fallback_verdict() {
        let strict = admit_clean(
            &report(BOARD_ABOVE, AddrType::RandomStatic, None),
            ScanMode::Strict,
        );
        assert_eq!(strict.decision, ConnectDecision::InitiateLowerAddress);
        assert_eq!(strict.outcome, Ok(()));
        let fallback = admit_clean(
            &report(BOARD_BELOW, AddrType::RandomStatic, None),
            ScanMode::Fallback,
        );
        assert_eq!(fallback.decision, ConnectDecision::InitiateFallback);
        assert!(fallback.admitted());
    }

    #[test]
    fn filter_one_a_verdict_that_waits_is_excluded() {
        let wait = admit_clean(
            &report(BOARD_BELOW, AddrType::RandomStatic, None),
            ScanMode::Strict,
        );
        assert_eq!(wait.decision, ConnectDecision::WaitPeerHasLowerAddress);
        assert_eq!(wait.outcome, Err(Exclusion::NotInitiate));
        let both_po = admit(
            Local {
                caps: CAP_PERIPHERAL_ONLY,
                addr: OURS,
            },
            &AdvReport {
                caps: Some(CAP_PERIPHERAL_ONLY),
                ..report(BOARD_ABOVE, AddrType::RandomStatic, None)
            },
            ScanMode::Fallback,
            &PeerRegistry::<4>::new(),
            &DeadEnds::new(),
            Ledgers {
                dial: &DialLedger::new(),
                churn: &ChurnLedger::new(),
            },
            0,
        );
        assert_eq!(both_po.outcome, Err(Exclusion::NotInitiate));
    }

    #[test]
    fn filter_two_a_dead_end_is_excluded_until_its_ttl_runs_out() {
        let mut dead = DeadEnds::new();
        dead.note(BOARD_ABOVE, 10_000);
        let registry: PeerRegistry<4> = PeerRegistry::new();
        let at = |now_ms| {
            admit(
                LOCAL,
                &report(BOARD_ABOVE, AddrType::RandomStatic, None),
                ScanMode::Strict,
                &registry,
                &dead,
                Ledgers {
                    dial: &DialLedger::new(),
                    churn: &ChurnLedger::new(),
                },
                now_ms,
            )
            .outcome
        };
        assert_eq!(at(10_000), Err(Exclusion::DeadEnd));
        assert_eq!(at(10_000 + DEAD_END_TTL_MS - 1), Err(Exclusion::DeadEnd));
        assert_eq!(at(10_000 + DEAD_END_TTL_MS), Ok(()));
    }

    #[test]
    fn filter_three_an_address_we_hold_a_connection_on_is_excluded() {
        let mut registry: PeerRegistry<4> = PeerRegistry::new();
        registry.conn_up(0, BOARD_ABOVE);
        let a = admit(
            LOCAL,
            &report(BOARD_ABOVE, AddrType::RandomStatic, None),
            ScanMode::Strict,
            &registry,
            &DeadEnds::new(),
            Ledgers {
                dial: &DialLedger::new(),
                churn: &ChurnLedger::new(),
            },
            0,
        );
        assert_eq!(a.outcome, Err(Exclusion::AddrLinked));
    }

    #[test]
    fn filter_four_a_hint_naming_a_linked_peer_is_excluded() {
        let peer = identity(0x5A);
        let mut registry: PeerRegistry<4> = PeerRegistry::new();
        let up = registry.link_up(0, peer, Origin::Outgoing, 247, 0);
        assert!(matches!(up, crate::LinkUp::Accepted { first: true }));
        let hint = crate::identity_hint(&peer);
        // The peer rotated: a new address the address filter cannot see.
        let a = admit(
            LOCAL,
            &report(BOARD_ABOVE, AddrType::RandomStatic, Some(hint)),
            ScanMode::Strict,
            &registry,
            &DeadEnds::new(),
            Ledgers {
                dial: &DialLedger::new(),
                churn: &ChurnLedger::new(),
            },
            0,
        );
        assert_eq!(a.outcome, Err(Exclusion::HintLinked));
        // No hint is never a match.
        let silent = admit(
            LOCAL,
            &report(BOARD_ABOVE, AddrType::RandomStatic, None),
            ScanMode::Strict,
            &registry,
            &DeadEnds::new(),
            Ledgers {
                dial: &DialLedger::new(),
                churn: &ChurnLedger::new(),
            },
            0,
        );
        assert_eq!(silent.outcome, Ok(()));
    }

    #[test]
    fn filter_five_the_dial_ledger_holds_a_fallback_verdict_and_never_a_strict_one() {
        let now_ms = 50_000;
        let dial = held_dial_ledger(now_ms);
        let registry: PeerRegistry<4> = PeerRegistry::new();
        let run = |value, mode| {
            admit(
                LOCAL,
                &report(value, AddrType::RandomStatic, None),
                mode,
                &registry,
                &DeadEnds::new(),
                Ledgers {
                    dial: &dial,
                    churn: &ChurnLedger::new(),
                },
                now_ms,
            )
            .outcome
        };
        let until_ms = now_ms + LedgerPolicy::MEASURED.pause_ms;
        assert_eq!(
            run(BOARD_BELOW, ScanMode::Fallback),
            Err(Exclusion::LedgerHeld { until_ms })
        );
        assert_eq!(run(BOARD_ABOVE, ScanMode::Fallback), Ok(()), "strict class");
        assert_eq!(run(BOARD_ABOVE, ScanMode::Strict), Ok(()));
    }

    #[test]
    fn filter_six_the_churn_ledger_holds_only_a_rotating_fallback_verdict() {
        let now_ms = 50_000;
        let churn = held_churn_ledger(now_ms);
        let registry: PeerRegistry<4> = PeerRegistry::new();
        let run = |value| {
            admit(
                LOCAL,
                &report(value, AddrType::RandomPrivateResolvable, None),
                ScanMode::Fallback,
                &registry,
                &DeadEnds::new(),
                Ledgers {
                    dial: &DialLedger::new(),
                    churn: &churn,
                },
                now_ms,
            )
            .outcome
        };
        let until_ms = now_ms + ChurnPolicy::MEASURED.cooldown_ms;
        assert_eq!(run(PHONE_RPA), Err(Exclusion::ChurnHeld { until_ms }));
        // A fixed-addressed fallback candidate passes.
        assert_eq!(run(BOARD_BELOW), Ok(()));
    }

    #[test]
    fn the_filters_name_the_first_that_excludes() {
        // A rotating fallback candidate that every filter from two to six
        // would exclude: the dead end is the one named.
        let now_ms = 50_000;
        let mut dead = DeadEnds::new();
        dead.note(PHONE_RPA, now_ms);
        let peer = identity(0x33);
        let mut registry: PeerRegistry<4> = PeerRegistry::new();
        registry.conn_up(0, PHONE_RPA);
        let _ = registry.link_up(0, peer, Origin::Incoming, 247, now_ms);
        let dial = held_dial_ledger(now_ms);
        let churn = held_churn_ledger(now_ms);
        let mut ledgers = Ledgers {
            dial: &dial,
            churn: &churn,
        };
        let r = report(
            PHONE_RPA,
            AddrType::RandomPrivateResolvable,
            Some(crate::identity_hint(&peer)),
        );
        let first = |dead: &DeadEnds, registry: &PeerRegistry<4>, ledgers: Ledgers<'_>| {
            admit(
                LOCAL,
                &r,
                ScanMode::Fallback,
                registry,
                dead,
                ledgers,
                now_ms,
            )
            .outcome
        };
        assert_eq!(first(&dead, &registry, ledgers), Err(Exclusion::DeadEnd));
        let clean_dead = DeadEnds::new();
        assert_eq!(
            first(&clean_dead, &registry, ledgers),
            Err(Exclusion::AddrLinked)
        );
        registry.conn_down(0);
        assert_eq!(
            first(&clean_dead, &registry, ledgers),
            Err(Exclusion::HintLinked)
        );
        let _ = registry.link_down(0);
        assert!(matches!(
            first(&clean_dead, &registry, ledgers),
            Err(Exclusion::LedgerHeld { .. })
        ));
        let free_dial = DialLedger::new();
        ledgers.dial = &free_dial;
        assert!(matches!(
            first(&clean_dead, &registry, ledgers),
            Err(Exclusion::ChurnHeld { .. })
        ));
    }

    #[test]
    fn dead_ends_reuse_an_expired_slot_then_the_oldest() {
        let mut dead = DeadEnds::new();
        for i in 0..DEAD_END_SLOTS as u64 {
            dead.note(0x100 + i, i * 1_000);
        }
        for i in 0..DEAD_END_SLOTS as u64 {
            assert!(dead.contains(0x100 + i, 7_000));
        }
        // A ninth address at a full table evicts the oldest (0x100).
        dead.note(0x200, 8_000);
        assert!(!dead.contains(0x100, 8_000), "the oldest was evicted");
        assert!(dead.contains(0x101, 8_000) && dead.contains(0x200, 8_000));
        // Once 0x101 has expired, the next note reuses its slot, and the
        // rest of the table survives.
        let after = 1_000 + DEAD_END_TTL_MS;
        dead.note(0x300, after);
        assert!(dead.contains(0x300, after));
        assert!(dead.contains(0x102, after) && dead.contains(0x200, after));
    }

    #[test]
    fn only_a_fallback_verdict_condemns_a_failed_connect() {
        assert!(connect_failure_condemns(ConnectDecision::InitiateFallback));
        assert!(!connect_failure_condemns(
            ConnectDecision::InitiateLowerAddress
        ));
        assert!(!connect_failure_condemns(
            ConnectDecision::InitiatePeripheralOnlyPeer
        ));
    }

    #[test]
    fn the_phase_reaches_fallback_after_the_bound_and_announces_once() {
        let mut phase = ScanPhase::new();
        phase.reset(1_000);
        let early = phase.poll(1_000 + SCAN_FALLBACK_AFTER_MS - 1);
        assert_eq!(early.mode, ScanMode::Strict);
        assert_eq!(early.announce_ms, None);
        let first = phase.poll(1_000 + SCAN_FALLBACK_AFTER_MS);
        assert_eq!(first.mode, ScanMode::Fallback);
        assert_eq!(first.announce_ms, Some(SCAN_FALLBACK_AFTER_MS));
        let again = phase.poll(1_000 + SCAN_FALLBACK_AFTER_MS + 5_000);
        assert_eq!(again.mode, ScanMode::Fallback);
        assert_eq!(again.announce_ms, None, "announced once per phase");
        phase.note(PhaseEvent::ConnDown, 40_000);
        assert_eq!(phase.poll(40_000).mode, ScanMode::Strict);
        let next = phase.poll(40_000 + SCAN_FALLBACK_AFTER_MS);
        assert!(next.announce_ms.is_some(), "a new phase announces again");
    }

    #[test]
    fn the_reset_rule_as_shipped() {
        for (event, resets) in [
            (PhaseEvent::ConnUp, true),
            (PhaseEvent::ConnDown, true),
            (PhaseEvent::LinkAccepted, true),
            (PhaseEvent::LinkDisplaced, true),
            (PhaseEvent::StrictTarget, true),
            (PhaseEvent::LinkRefused, false),
            (PhaseEvent::FallbackTarget, false),
        ] {
            assert_eq!(event.resets_strict(), resets, "{event:?}");
            let mut phase = ScanPhase::new();
            let _ = phase.poll(SCAN_FALLBACK_AFTER_MS);
            assert_eq!(phase.note(event, SCAN_FALLBACK_AFTER_MS), resets);
            let mode = phase.poll(SCAN_FALLBACK_AFTER_MS).mode;
            let expect = if resets {
                ScanMode::Strict
            } else {
                ScanMode::Fallback
            };
            assert_eq!(mode, expect, "{event:?}");
        }
    }

    #[test]
    fn the_address_type_travels_with_the_address() {
        let raw = [0x34, 0x12, 0x00, 0x00, 0x00, 0x40];
        let addr = PeerAddr::from_raw(&raw, 0x02);
        assert_eq!(addr.value, PHONE_RPA);
        assert_eq!(addr.addr_type, AddrType::RandomPrivateResolvable);
        assert_eq!(AddrType::from_raw(0x00), AddrType::Public);
        assert_eq!(AddrType::from_raw(0x01), AddrType::RandomStatic);
        assert_eq!(
            AddrType::from_raw(0x03),
            AddrType::RandomPrivateNonResolvable
        );
        assert_eq!(AddrType::from_raw(0x7F), AddrType::Anonymous);
        assert_eq!(AddrType::from_raw(0x42), AddrType::Other(0x42));
    }

    /// How many reconnects of a rotating phone the #412 red replays.
    const RECONNECTS: u32 = 20;

    /// How often the rotating phone of the #412 red reconnects: under
    /// the strict bound, as the issue's capture has it (five addresses
    /// in four minutes, so one reconnect every ~48 s at worst and every
    /// ~20 s in the busier stretches). 20 s is the busy stretch.
    const RECONNECT_EVERY_MS: u64 = 20_000;

    /// #412, read off columba.rs by 489: every non-refused `peer_link_up`
    /// outcome restarts the strict clock, displacement included, and so
    /// does the connection event that carries the reconnect and the old
    /// link's teardown. A rotating phone that reconnects inside the bound
    /// therefore keeps a board's scanner in strict mode forever, and the
    /// fallback phase that would let it dial a board below it never
    /// comes.
    ///
    /// The test states what the fix must deliver — the board reaches
    /// fallback within the replay — and is RED today by design.
    #[test]
    #[ignore = "issue 412 red: displacement by a rotating peer resets the strict clock"]
    fn issue_412_a_rotating_peer_that_keeps_reconnecting_holds_off_the_fallback() {
        let mut phase = ScanPhase::new();
        let mut longest_strict_ms = 0;
        let mut reached_fallback = false;
        for k in 1..=RECONNECTS {
            let at = u64::from(k) * RECONNECT_EVERY_MS;
            // Sampled every second between reconnects, the way the scan
            // callback samples per advertising report.
            for t in (at - RECONNECT_EVERY_MS..at).step_by(1_000) {
                if phase.poll(t).mode == ScanMode::Fallback {
                    reached_fallback = true;
                }
                longest_strict_ms = longest_strict_ms.max(t.saturating_sub(phase.since_ms));
            }
            // One reconnect of the phone: its new connection, the
            // displacement of its old link, the old link's teardown.
            phase.note(PhaseEvent::ConnUp, at);
            phase.note(PhaseEvent::LinkDisplaced, at);
            phase.note(PhaseEvent::ConnDown, at);
        }
        println!(
            "issue_412 displacement: reconnects={RECONNECTS} every_ms={RECONNECT_EVERY_MS} \
             longest_strict_ms={longest_strict_ms} fallback_after_ms={SCAN_FALLBACK_AFTER_MS} \
             reached_fallback={reached_fallback}"
        );
        assert!(
            reached_fallback,
            "{RECONNECTS} reconnects of a rotating peer every {RECONNECT_EVERY_MS} ms kept the \
             strict phase alive: longest strict stretch {longest_strict_ms} ms against the \
             {SCAN_FALLBACK_AFTER_MS} ms bound, the fallback was never reached"
        );
    }

    /// #412, the second reading find: the scanner drops the address type
    /// (`addr_value` keeps only the 48 bits), so [`rotating_address`]
    /// guesses the class from the top two bits. The pattern
    /// `window.rs` names as the residual: a PUBLIC address whose OUI
    /// begins in `0x00..=0x7F` — here a BlueZ host's `00:1A:7D` OUI —
    /// reads as rotating, so it is ordered behind the boards and falls
    /// under the churn cooldown, though a public address never rotates.
    ///
    /// RED today by design: the type travels in [`PeerAddr`] since this
    /// pass, but nothing reads it yet.
    #[test]
    #[ignore = "issue 412 red: a public address is classified by its top bits, not its type"]
    fn issue_412_a_public_address_with_a_low_oui_is_misclassified_as_rotating() {
        // 00:1A:7D:DA:71:13 as the SoftDevice stores it, LSB first.
        let raw = [0x13, 0x71, 0xDA, 0x7D, 0x1A, 0x00];
        let addr = PeerAddr::from_raw(&raw, 0x00);
        assert_eq!(addr.addr_type, AddrType::Public);
        let guessed = rotating_address(addr.value);
        println!(
            "issue_412 addr_type: addr={:012x} type={:?} top_bits={:02b} rotating_guess={guessed}",
            addr.value,
            addr.addr_type,
            addr.value >> 46
        );
        // The churn cooldown, filter six, is where the guess costs a dial.
        let now_ms = 50_000;
        let churn = held_churn_ledger(now_ms);
        let a = admit(
            LOCAL,
            &AdvReport {
                addr,
                caps: None,
                identity_hint: None,
            },
            ScanMode::Fallback,
            &PeerRegistry::<4>::new(),
            &DeadEnds::new(),
            Ledgers {
                dial: &DialLedger::new(),
                churn: &churn,
            },
            now_ms,
        );
        println!("issue_412 addr_type: admission={a:?}");
        assert!(
            !guessed,
            "a public address {:012x} reads as rotating from its top bits {:02b}",
            addr.value,
            addr.value >> 46
        );
        assert_eq!(
            a.outcome,
            Ok(()),
            "a public address fell under the rotating churn cooldown"
        );
    }

    #[test]
    fn the_constants_are_the_firmwares() {
        assert_eq!(DEAD_END_TTL_MS, 120_000);
        // Three incoming slots and one outgoing: `MAX_LINKS` is 4.
        assert_eq!(DEAD_END_SLOTS, 2 * (usize::from(PERIPH_SLOTS) + 1));
    }
}
