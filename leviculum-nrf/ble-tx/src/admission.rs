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
//!
//! # The dial backoff
//!
//! [`DialBackoff`] (579) is the per-address hold after a failed dial,
//! asked after an admitted verdict by both the firmware and lnsd, whose
//! own scan path does not run [`admit`]: a transport failure holds the
//! address the flat [`DIAL_BACKOFF_BASE_MS`], one that names the peer as
//! no Columba peer, or one that will not handshake, doubles the hold up
//! to [`DIAL_BACKOFF_CAP_MS`].

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
/// It travels with the address since #496, and since #504 it answers
/// the question "does this peer redraw its address"
/// ([`rotating_address`], [`PeerAddr::rotating`]): a public address
/// never does, whatever its top bits.
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

    /// Whether its owner redraws this address, read with the type the
    /// report carried ([`rotating_address`]).
    #[must_use]
    pub fn rotating(&self) -> bool {
        rotating_address(self.value, Some(self.addr_type))
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

/// The retry pause after a dial that said nothing about its peer, and
/// the step the peer-failure backoff doubles from (579). It was the
/// whole backoff until then, on both stacks: the firmware's
/// `CENTRAL_RETRY_BACKOFF_MS` and lnsd's `SESSION_BACKOFF_MS`.
pub const DIAL_BACKOFF_BASE_MS: u64 = 5_000;

/// The ceiling of the peer-failure backoff (579).
///
/// Two minutes, from the scan cadence: a sighting arrives about every
/// 7 s and the fallback bound is [`SCAN_FALLBACK_AFTER_MS`] (30 s), so
/// a cap of four fallback bounds lets the doubling (10, 20, 40, 80 s)
/// run its whole course before it binds, and a peer that was wrongly
/// judged, a real phone caught mid-restart, is dialled again within
/// two minutes at worst, inside its own next advertising burst. It is
/// also [`DEAD_END_TTL_MS`], the board's other address hold, so no
/// address waits longer for being odd than for being a dead end. The
/// same span is the absence that forgets an address: one not sighted
/// for longer than the cap would have been dialled again by now anyway.
pub const DIAL_BACKOFF_CAP_MS: u64 = 120_000;

/// Room in the board's [`DialBackoff`]: one entry per address a dial
/// failed at, [`DEAD_END_SLOTS`] like the dead-end table it sits
/// beside. Overflow evicts the address sighted longest ago, which only
/// costs that address its run, an early re-dial, never a lost link.
pub const DIAL_BACKOFF_SLOTS: usize = DEAD_END_SLOTS;

/// What a dial's give-up says about the peer behind the address (579).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialFailure {
    /// The connection never came up, timed out in setup, or dropped:
    /// a property of the air, not of the peer. Books the flat
    /// [`DIAL_BACKOFF_BASE_MS`] and leaves the peer's run alone.
    Transport,
    /// Connected, and the peer serves no Columba GATT layout: it
    /// advertised the service UUID and is not a Columba peer (545 D10,
    /// a phone advertising the UUID alone).
    NotColumba,
    /// The layout is there, and the peer will not handshake: it refused
    /// our identity write, or (#548) it connected to us and never wrote
    /// its own within the bound.
    NoHandshake,
}

impl DialFailure {
    /// The token the log lines print.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::NotColumba => "not_columba",
            Self::NoHandshake => "no_handshake",
        }
    }

    /// Whether the failure is evidence about the peer, and so lengthens
    /// its backoff.
    #[must_use]
    pub const fn says_peer(self) -> bool {
        !matches!(self, Self::Transport)
    }
}

/// The pause the `strikes`-th peer failure in a row books: the base
/// doubled once per strike, so the first is already twice a transport
/// failure's, up to [`DIAL_BACKOFF_CAP_MS`]. Zero strikes is the flat
/// base.
#[must_use]
pub fn dial_backoff_ms(strikes: u8) -> u64 {
    DIAL_BACKOFF_BASE_MS
        .checked_shl(u32::from(strikes))
        .map_or(DIAL_BACKOFF_CAP_MS, |ms| ms.min(DIAL_BACKOFF_CAP_MS))
}

/// One address's run in a [`DialBackoff`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BackoffEntry {
    addr: u64,
    /// Peer failures in a row since the last success or absence.
    strikes: u8,
    /// No dial before this.
    until_ms: u64,
    /// The last sighting, or the failure that booked the entry.
    seen_ms: u64,
    /// Whether this booking's hold was announced ([`DialBackoff::announce`]).
    announced: bool,
}

/// What [`DialBackoff::note_failure`] booked, for the log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffBooked {
    /// No dial to the address before this.
    pub until_ms: u64,
    /// Peer failures in a row, this one included.
    pub strikes: u8,
}

/// The per-address dial backoff (579): which addresses a dial failed
/// at, how many times in a row the failure was the PEER's, and until
/// when the scanner leaves each alone.
///
/// Before it, both stacks paused a flat 5 s after every failed dial,
/// with no memory of the address: in 575's room a node with one free
/// central slot dialled a UUID-only advertiser every 7 to 14 s for the
/// whole cell. Now a failure that says "not a Columba peer" or "will
/// not handshake" doubles the address's pause up to
/// [`DIAL_BACKOFF_CAP_MS`]; a transport failure keeps the flat base. A
/// handshake with the address in either role clears it
/// ([`note_success`](Self::note_success)), and so does an absence from
/// the sightings longer than the cap, which [`sighted`](Self::sighted)
/// notices.
///
/// Bounded and allocation-free for the board, `N` entries; the host
/// gives it more room.
#[derive(Debug, Clone, Copy)]
pub struct DialBackoff<const N: usize> {
    entries: [Option<BackoffEntry>; N],
}

impl<const N: usize> Default for DialBackoff<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> DialBackoff<N> {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self { entries: [None; N] }
    }

    fn find(&self, addr: u64) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.is_some_and(|e| e.addr == addr))
    }

    /// Whether an entry is forgotten by absence at `now_ms`.
    fn absent(entry: &BackoffEntry, now_ms: u64) -> bool {
        now_ms.saturating_sub(entry.seen_ms) > DIAL_BACKOFF_CAP_MS
    }

    /// Book a failed dial at `addr`. A peer failure adds a strike and
    /// pauses [`dial_backoff_ms`] of the new count; a transport failure
    /// pauses the flat base and keeps the count. A run whose address
    /// was absent past the cap starts over first.
    pub fn note_failure(&mut self, addr: u64, failure: DialFailure, now_ms: u64) -> BackoffBooked {
        let prior = self
            .find(addr)
            .and_then(|i| self.entries[i])
            .filter(|e| !Self::absent(e, now_ms))
            .map_or(0, |e| e.strikes);
        let strikes = if failure.says_peer() {
            prior.saturating_add(1)
        } else {
            prior
        };
        let pause = if failure.says_peer() {
            dial_backoff_ms(strikes)
        } else {
            DIAL_BACKOFF_BASE_MS
        };
        let entry = BackoffEntry {
            addr,
            strikes,
            until_ms: now_ms.saturating_add(pause),
            seen_ms: now_ms,
            announced: false,
        };
        let slot = self.find(addr).unwrap_or_else(|| self.free_slot(now_ms));
        if let Some(cell) = self.entries.get_mut(slot) {
            *cell = Some(entry);
        }
        BackoffBooked {
            until_ms: entry.until_ms,
            strikes,
        }
    }

    /// An empty or forgotten slot, else the one sighted longest ago.
    fn free_slot(&self, now_ms: u64) -> usize {
        self.entries
            .iter()
            .position(|e| e.is_none_or(|e| Self::absent(&e, now_ms)))
            .unwrap_or_else(|| {
                self.entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| e.map_or(0, |e| e.seen_ms))
                    .map_or(0, |(i, _)| i)
            })
    }

    /// A handshake with `addr` completed, in either role: it is a
    /// Columba peer, its run is over.
    pub fn note_success(&mut self, addr: u64) {
        if let Some(i) = self.find(addr) {
            self.entries[i] = None;
        }
    }

    /// A sighting of `addr`: forgets the run if the address was absent
    /// past the cap, else refreshes its last sighting. Returns the
    /// deadline it is held until, `None` when it may be dialled.
    pub fn sighted(&mut self, addr: u64, now_ms: u64) -> Option<u64> {
        let i = self.find(addr)?;
        let entry = self.entries[i].as_mut()?;
        if Self::absent(entry, now_ms) {
            self.entries[i] = None;
            return None;
        }
        entry.seen_ms = now_ms;
        (entry.until_ms > now_ms).then_some(entry.until_ms)
    }

    /// The deadline `addr` is held until at `now_ms`, without counting
    /// as a sighting: the re-checks between a window's choice and its
    /// dial ask this.
    #[must_use]
    pub fn held_until(&self, addr: u64, now_ms: u64) -> Option<u64> {
        self.find(addr)
            .and_then(|i| self.entries[i])
            .filter(|e| !Self::absent(e, now_ms) && e.until_ms > now_ms)
            .map(|e| e.until_ms)
    }

    /// The hold at `addr`, once per booking: `Some` the first time it is
    /// asked while the address is held, `None` after that and whenever
    /// it is not held. A board's scan callback has no per-address log
    /// memory across passes; this is it, for the `rule=backoff` line.
    pub fn announce(&mut self, addr: u64, now_ms: u64) -> Option<BackoffBooked> {
        let until_ms = self.held_until(addr, now_ms)?;
        let entry = self.find(addr).and_then(|i| self.entries[i].as_mut())?;
        if entry.announced {
            return None;
        }
        entry.announced = true;
        Some(BackoffBooked {
            until_ms,
            strikes: entry.strikes,
        })
    }

    /// The peer failures in a row booked at `addr`, 0 when none.
    #[must_use]
    pub fn strikes(&self, addr: u64) -> u8 {
        self.find(addr)
            .and_then(|i| self.entries[i])
            .map_or(0, |e| e.strikes)
    }
}

/// Whether the peer behind a [`PhaseEvent`] is new to the current
/// phase — the "seen before" input of the reset rule, carried
/// explicitly so [`PhaseEvent::resets_strict`] stays a pure function
/// (#504). [`PhaseNeighbours`] is the table that answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Neighbour {
    /// Not linked in the current phase: a new neighbour.
    New,
    /// Already linked in the current phase.
    Seen,
    /// A connection on a ROTATING address, before any identity: the
    /// address is redrawn at will, so it cannot say whether the peer is
    /// new, and the identity decides at [`PhaseEvent::LinkAccepted`].
    Unkeyed,
}

/// Everything that restarts or leaves alone the strict phase — the
/// reset rule as an explicit input, so a test can state which outcome
/// does what (#412).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseEvent {
    /// A connection came up in either role, before any identity
    /// (`conn_link_up`), keyed by its address when that is a fixed one
    /// ([`PhaseNeighbours::meet_conn`]). A new fixed address resets: the
    /// rig showed the fallback firing in the 3 s gap between `BLE:
    /// connected` and the handshake, #375 §0.
    ConnUp(Neighbour),
    /// A connection went down (`conn_link_down`): a departure, not a
    /// new neighbour.
    ConnDown,
    /// An identity's link registered plainly (`LinkUp::Accepted`),
    /// keyed by the identity ([`PhaseNeighbours::meet_link`]).
    LinkAccepted(Neighbour),
    /// An identity's newer link displaced its older one
    /// (`LinkUp::Displaced`): a peer we hold a live link to,
    /// reconnecting, so never a new neighbour.
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
    /// Whether this event restarts the strict phase, as decided for
    /// #412 (#504): a NEW neighbour — a link from an identity, or a
    /// connection on a fixed address, the board had not linked in the
    /// current phase — and a strict target. A known peer's reconnect,
    /// in any of its three events, a teardown, a refusal and a fallback
    /// target leave the clock alone.
    ///
    /// Until #504 every link up and every teardown reset, so a phone
    /// that reconnected inside the bound held the board in strict mode
    /// for good.
    #[must_use]
    pub const fn resets_strict(self) -> bool {
        match self {
            Self::ConnUp(n) | Self::LinkAccepted(n) => matches!(n, Neighbour::New),
            Self::StrictTarget => true,
            Self::ConnDown | Self::LinkDisplaced | Self::LinkRefused | Self::FallbackTarget => {
                false
            }
        }
    }
}

/// The peers the board linked in the current phase, by identity, or by
/// address for a connection on a fixed address before its identity is
/// known: the table behind [`Neighbour`] (#504).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NeighbourKey {
    Identity([u8; 16]),
    Addr(u64),
}

/// The firmware's seen-set beside its strict-phase clock: room for one
/// key per link slot plus the departed ones a dead-end table holds,
/// `MAX_LINKS + DEAD_END_SLOTS` (12) on every board. Overflow evicts
/// the oldest key; an evicted peer that comes back reads as new and
/// buys one early reset, never a lost link.
pub const PHASE_NEIGHBOUR_SLOTS: usize = DEAD_END_SLOTS / 2 + DEAD_END_SLOTS;

/// Who the board linked since the phase last reached fallback (#504).
///
/// A phase in this sense runs from one fallback to the next: the table
/// is [`clear`](Self::clear)ed when [`ScanPhase::poll`] announces the
/// switch, so every neighbour can restart the strict clock at most once
/// per phase however often it reconnects, and the bound to the next
/// fallback is [`SCAN_FALLBACK_AFTER_MS`] past the last new neighbour.
/// Bounded and allocation-free: a ring of [`PHASE_NEIGHBOUR_SLOTS`] keys
/// in the firmware, the oldest overwritten first.
#[derive(Debug, Clone, Copy)]
pub struct PhaseNeighbours<const N: usize> {
    keys: [Option<NeighbourKey>; N],
    next: usize,
}

impl<const N: usize> Default for PhaseNeighbours<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> PhaseNeighbours<N> {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            keys: [None; N],
            next: 0,
        }
    }

    /// Forget every neighbour: the phase reached fallback.
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// A connection came up on `addr`: [`Neighbour::Unkeyed`] for a
    /// rotating address ([`PeerAddr::rotating`]), which is not recorded;
    /// otherwise whether the address is new, and it is recorded.
    pub fn meet_conn(&mut self, addr: PeerAddr) -> Neighbour {
        if addr.rotating() {
            return Neighbour::Unkeyed;
        }
        self.meet(NeighbourKey::Addr(addr.value))
    }

    /// An identity's link came up: whether the identity is new, and it
    /// is recorded.
    pub fn meet_link(&mut self, identity: &[u8; 16]) -> Neighbour {
        self.meet(NeighbourKey::Identity(*identity))
    }

    fn meet(&mut self, key: NeighbourKey) -> Neighbour {
        if self.keys.contains(&Some(key)) {
            return Neighbour::Seen;
        }
        if N > 0 {
            self.keys[self.next] = Some(key);
            self.next = (self.next + 1) % N;
        }
        Neighbour::New
    }
}

/// The strict-phase clock behind [`SCAN_FALLBACK_AFTER_MS`]: when the
/// current strict phase began and whether the switch to fallback was
/// already announced for this phase. Reset by every [`PhaseEvent`] that
/// [`PhaseEvent::resets_strict`] names; otherwise it runs whenever the
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

    /// Apply one event under the reset rule; `true` iff it restarted
    /// the phase.
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
    } else if let Some(until_ms) = (fallback && report.addr.rotating())
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
        registry.conn_up(0, BOARD_ABOVE, 0);
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
        registry.conn_up(0, PHONE_RPA, 0);
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
        phase.note(PhaseEvent::LinkAccepted(Neighbour::New), 40_000);
        assert_eq!(phase.poll(40_000).mode, ScanMode::Strict);
        let next = phase.poll(40_000 + SCAN_FALLBACK_AFTER_MS);
        assert!(next.announce_ms.is_some(), "a new phase announces again");
    }

    #[test]
    fn the_reset_rule_as_decided() {
        use Neighbour::{New, Seen, Unkeyed};
        for (event, resets) in [
            (PhaseEvent::ConnUp(New), true),
            (PhaseEvent::ConnUp(Seen), false),
            (PhaseEvent::ConnUp(Unkeyed), false),
            (PhaseEvent::ConnDown, false),
            (PhaseEvent::LinkAccepted(New), true),
            (PhaseEvent::LinkAccepted(Seen), false),
            (PhaseEvent::LinkAccepted(Unkeyed), false),
            (PhaseEvent::LinkDisplaced, false),
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
    fn a_neighbour_is_new_once_per_phase() {
        let mut seen: PhaseNeighbours<PHASE_NEIGHBOUR_SLOTS> = PhaseNeighbours::new();
        let board = PeerAddr {
            value: BOARD_ABOVE,
            addr_type: AddrType::RandomStatic,
        };
        assert_eq!(seen.meet_conn(board), Neighbour::New);
        assert_eq!(seen.meet_conn(board), Neighbour::Seen);
        assert_eq!(seen.meet_link(&identity(1)), Neighbour::New);
        assert_eq!(seen.meet_link(&identity(1)), Neighbour::Seen);
        // A rotating address says nothing and is not recorded; its
        // identity decides.
        let phone = PeerAddr {
            value: PHONE_RPA,
            addr_type: AddrType::RandomPrivateResolvable,
        };
        assert_eq!(seen.meet_conn(phone), Neighbour::Unkeyed);
        assert_eq!(seen.meet_conn(phone), Neighbour::Unkeyed);
        // A public address is a fixed key, whatever its top bits.
        let host = PeerAddr {
            value: 0x001A_7DDA_7113,
            addr_type: AddrType::Public,
        };
        assert_eq!(seen.meet_conn(host), Neighbour::New);
        assert_eq!(seen.meet_conn(host), Neighbour::Seen);
        // The phase reaching fallback forgets them all.
        seen.clear();
        assert_eq!(seen.meet_conn(board), Neighbour::New);
        assert_eq!(seen.meet_link(&identity(1)), Neighbour::New);
    }

    #[test]
    fn the_neighbour_table_evicts_the_oldest_key_when_full() {
        let mut seen: PhaseNeighbours<PHASE_NEIGHBOUR_SLOTS> = PhaseNeighbours::new();
        for byte in 0..PHASE_NEIGHBOUR_SLOTS as u8 {
            assert_eq!(seen.meet_link(&identity(byte)), Neighbour::New);
        }
        for byte in 0..PHASE_NEIGHBOUR_SLOTS as u8 {
            assert_eq!(seen.meet_link(&identity(byte)), Neighbour::Seen);
        }
        // One more evicts identity 0, the oldest; the rest stay.
        assert_eq!(seen.meet_link(&identity(0xEE)), Neighbour::New);
        assert_eq!(seen.meet_link(&identity(1)), Neighbour::Seen);
        assert_eq!(seen.meet_link(&identity(0)), Neighbour::New, "evicted");
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

    /// How many reconnects of a rotating phone the #412 replay runs.
    const RECONNECTS: u32 = 20;

    /// How often the rotating phone of the #412 replay reconnects: under
    /// the strict bound, as the issue's capture has it (five addresses
    /// in four minutes, so one reconnect every ~48 s at worst and every
    /// ~20 s in the busier stretches). 20 s is the busy stretch.
    const RECONNECT_EVERY_MS: u64 = 20_000;

    /// #412, read off columba.rs by 489: until #504 every non-refused
    /// `peer_link_up` outcome restarted the strict clock, displacement
    /// included, and so did the connection event that carries the
    /// reconnect and the old link's teardown. A rotating phone that
    /// reconnected inside the bound therefore kept a board's scanner in
    /// strict mode forever, and the fallback phase that would let it
    /// dial a board below it never came: `longest_strict_ms=19000`,
    /// `reached_fallback=false` before #504.
    ///
    /// Under the decided rule only a NEW neighbour resets. The phone is
    /// new once, at its first link; every reconnect after that is a
    /// known peer on a fresh rotating address, and the board reaches
    /// fallback one bound after that first link (`longest_strict_ms=
    /// 29000` at one-second sampling) and stays there.
    #[test]
    fn issue_412_a_rotating_peer_that_keeps_reconnecting_no_longer_holds_off_the_fallback() {
        let phone = identity(0x4F);
        let mut phase = ScanPhase::new();
        let mut seen: PhaseNeighbours<PHASE_NEIGHBOUR_SLOTS> = PhaseNeighbours::new();
        // The phone's first link, at time zero: a new neighbour.
        let addr0 = PeerAddr {
            value: PHONE_RPA,
            addr_type: AddrType::RandomPrivateResolvable,
        };
        assert!(!phase.note(PhaseEvent::ConnUp(seen.meet_conn(addr0)), 0));
        assert!(phase.note(PhaseEvent::LinkAccepted(seen.meet_link(&phone)), 0));
        let mut longest_strict_ms = 0;
        let mut reached_fallback_at = None;
        for k in 1..=RECONNECTS {
            let at = u64::from(k) * RECONNECT_EVERY_MS;
            // Sampled every second between reconnects, the way the scan
            // callback samples per advertising report; the firmware
            // clears the table when the poll announces fallback.
            for t in (at - RECONNECT_EVERY_MS..at).step_by(1_000) {
                let poll = phase.poll(t);
                if poll.announce_ms.is_some() {
                    seen.clear();
                }
                match poll.mode {
                    ScanMode::Strict => {
                        longest_strict_ms = longest_strict_ms.max(t - phase.since_ms);
                    }
                    ScanMode::Fallback => {
                        reached_fallback_at.get_or_insert(t);
                    }
                }
            }
            // One reconnect of the phone on a fresh rotating address:
            // its new connection, the displacement of its old link, the
            // old link's teardown.
            let addr = PeerAddr {
                value: PHONE_RPA + u64::from(k),
                addr_type: AddrType::RandomPrivateResolvable,
            };
            phase.note(PhaseEvent::ConnUp(seen.meet_conn(addr)), at);
            let _ = seen.meet_link(&phone);
            phase.note(PhaseEvent::LinkDisplaced, at);
            phase.note(PhaseEvent::ConnDown, at);
        }
        let reached_fallback = reached_fallback_at.is_some();
        println!(
            "issue_412 displacement: reconnects={RECONNECTS} every_ms={RECONNECT_EVERY_MS} \
             longest_strict_ms={longest_strict_ms} fallback_after_ms={SCAN_FALLBACK_AFTER_MS} \
             reached_fallback={reached_fallback} at_ms={reached_fallback_at:?}"
        );
        assert!(
            reached_fallback,
            "{RECONNECTS} reconnects of a rotating peer every {RECONNECT_EVERY_MS} ms kept the \
             strict phase alive: longest strict stretch {longest_strict_ms} ms against the \
             {SCAN_FALLBACK_AFTER_MS} ms bound, the fallback was never reached"
        );
        assert_eq!(longest_strict_ms, SCAN_FALLBACK_AFTER_MS - 1_000);
        assert_eq!(reached_fallback_at, Some(SCAN_FALLBACK_AFTER_MS));
        assert_eq!(
            phase.poll(u64::from(RECONNECTS) * RECONNECT_EVERY_MS).mode,
            ScanMode::Fallback
        );
    }

    /// The same phone whose old link expired before each reconnect, so
    /// the reconnect is a plain `LinkAccepted`: its identity is known in
    /// this phase and resets nothing; once the phase reached fallback
    /// and forgot it, it may reset exactly once more.
    #[test]
    fn a_known_identity_relinking_after_expiry_resets_at_most_once_per_phase() {
        let phone = identity(0x4F);
        let mut phase = ScanPhase::new();
        let mut seen: PhaseNeighbours<PHASE_NEIGHBOUR_SLOTS> = PhaseNeighbours::new();
        assert!(phase.note(PhaseEvent::LinkAccepted(seen.meet_link(&phone)), 0));
        phase.note(PhaseEvent::ConnDown, 15_000);
        assert!(!phase.note(PhaseEvent::LinkAccepted(seen.meet_link(&phone)), 20_000));
        let poll = phase.poll(SCAN_FALLBACK_AFTER_MS);
        assert_eq!(
            poll.mode,
            ScanMode::Fallback,
            "one bound after the first link"
        );
        assert!(poll.announce_ms.is_some());
        seen.clear();
        assert!(phase.note(PhaseEvent::LinkAccepted(seen.meet_link(&phone)), 40_000));
        assert!(!phase.note(PhaseEvent::LinkAccepted(seen.meet_link(&phone)), 60_000));
        assert_eq!(
            phase.poll(40_000 + SCAN_FALLBACK_AFTER_MS).mode,
            ScanMode::Fallback
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
    /// Red until #504 (`rotating_guess=true`, admission
    /// `Err(ChurnHeld)`): the type travelled in [`PeerAddr`] since #496
    /// but nothing read it. Now [`rotating_address`] reads the type
    /// when there is one, and a public address is never rotating.
    #[test]
    fn issue_412_a_public_address_with_a_low_oui_is_not_rotating() {
        // 00:1A:7D:DA:71:13 as the SoftDevice stores it, LSB first.
        let raw = [0x13, 0x71, 0xDA, 0x7D, 0x1A, 0x00];
        let addr = PeerAddr::from_raw(&raw, 0x00);
        assert_eq!(addr.addr_type, AddrType::Public);
        let guessed = addr.rotating();
        println!(
            "issue_412 addr_type: addr={:012x} type={:?} top_bits={:02b} rotating={guessed}",
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

    /// The counterpart of the public address above: a RANDOM address
    /// whose top bits are `01` is a resolvable private one, still
    /// rotating, and still held by filter six under the cooldown.
    #[test]
    fn a_random_resolvable_address_stays_rotating_and_held() {
        let raw = [0x34, 0x12, 0x00, 0x00, 0x00, 0x40];
        let addr = PeerAddr::from_raw(&raw, 0x02);
        assert_eq!(addr.addr_type, AddrType::RandomPrivateResolvable);
        assert_eq!(addr.value >> 46, 0b01);
        assert!(addr.rotating());
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
        assert!(matches!(a.outcome, Err(Exclusion::ChurnHeld { .. })));
    }

    /// 579: a peer that keeps failing as a peer books 10, 20, 40, 80 s
    /// and then the cap, and is held exactly that long.
    #[test]
    fn a_peer_failure_doubles_the_backoff_up_to_the_cap() {
        let mut b: DialBackoff<DIAL_BACKOFF_SLOTS> = DialBackoff::new();
        let mut now = 1_000;
        let mut booked = [0u64; 7];
        for slot in &mut booked {
            let at = b.note_failure(BOARD_ABOVE, DialFailure::NotColumba, now);
            *slot = at.until_ms - now;
            assert_eq!(
                b.held_until(BOARD_ABOVE, at.until_ms - 1),
                Some(at.until_ms)
            );
            assert_eq!(
                b.held_until(BOARD_ABOVE, at.until_ms),
                None,
                "free at the deadline"
            );
            // Sighted all along, dialled again right at the deadline.
            assert_eq!(b.sighted(BOARD_ABOVE, at.until_ms - 1), Some(at.until_ms));
            now = at.until_ms + 2_000;
        }
        assert_eq!(
            booked,
            [10_000, 20_000, 40_000, 80_000, 120_000, 120_000, 120_000]
        );
        assert_eq!(b.strikes(BOARD_ABOVE), 7);
        // The handshake failure counts the same as the missing layout.
        let mut h: DialBackoff<DIAL_BACKOFF_SLOTS> = DialBackoff::new();
        h.note_failure(PHONE_RPA, DialFailure::NoHandshake, 0);
        let second = h.note_failure(PHONE_RPA, DialFailure::NotColumba, 10_000);
        assert_eq!((second.strikes, second.until_ms), (2, 30_000));
    }

    /// 579: a transport failure books the flat 5 s, every time, and
    /// neither starts nor lengthens a run; after peer failures it keeps
    /// their count, so the next peer failure resumes the doubling.
    #[test]
    fn a_transport_failure_keeps_the_flat_five_seconds() {
        let mut b: DialBackoff<DIAL_BACKOFF_SLOTS> = DialBackoff::new();
        for i in 0..5u64 {
            let now = i * 10_000;
            let at = b.note_failure(BOARD_ABOVE, DialFailure::Transport, now);
            assert_eq!((at.until_ms, at.strikes), (now + DIAL_BACKOFF_BASE_MS, 0));
        }
        b.note_failure(BOARD_ABOVE, DialFailure::NotColumba, 60_000);
        let flat = b.note_failure(BOARD_ABOVE, DialFailure::Transport, 80_000);
        assert_eq!((flat.until_ms, flat.strikes), (85_000, 1));
        let next = b.note_failure(BOARD_ABOVE, DialFailure::NotColumba, 90_000);
        assert_eq!((next.until_ms, next.strikes), (110_000, 2));
    }

    /// 579: a handshake ends the run; the next failure starts from one
    /// strike again.
    #[test]
    fn a_handshake_resets_the_backoff() {
        let mut b: DialBackoff<DIAL_BACKOFF_SLOTS> = DialBackoff::new();
        for now in [0, 20_000, 50_000] {
            b.note_failure(BOARD_ABOVE, DialFailure::NoHandshake, now);
        }
        assert_eq!(b.strikes(BOARD_ABOVE), 3);
        b.note_success(BOARD_ABOVE);
        assert_eq!(b.strikes(BOARD_ABOVE), 0);
        assert_eq!(b.held_until(BOARD_ABOVE, 50_001), None);
        assert_eq!(b.sighted(BOARD_ABOVE, 50_001), None);
        let again = b.note_failure(BOARD_ABOVE, DialFailure::NoHandshake, 60_000);
        assert_eq!((again.until_ms, again.strikes), (70_000, 1));
        // Another address's run was never touched.
        b.note_failure(BOARD_BELOW, DialFailure::NotColumba, 60_000);
        b.note_success(BOARD_ABOVE);
        assert_eq!(b.strikes(BOARD_BELOW), 1);
    }

    /// 579: an address that stayed out of the sightings longer than the
    /// cap is a new address when it returns: its run is forgotten. One
    /// that kept advertising through its hold is not.
    #[test]
    fn an_absence_longer_than_the_cap_resets_the_backoff() {
        let mut b: DialBackoff<DIAL_BACKOFF_SLOTS> = DialBackoff::new();
        for now in [0, 20_000, 50_000, 100_000] {
            b.note_failure(BOARD_ABOVE, DialFailure::NotColumba, now);
        }
        // Held 80 s, until 180 s, and sighted throughout: still held.
        for t in (101_000..180_000).step_by(7_000) {
            assert_eq!(b.sighted(BOARD_ABOVE, t), Some(180_000));
        }
        assert_eq!(b.strikes(BOARD_ABOVE), 4);
        // Last sighted at 177 s; back exactly at the cap: still its run.
        let last = 177_000;
        assert_eq!(b.sighted(BOARD_ABOVE, last + DIAL_BACKOFF_CAP_MS), None);
        assert_eq!(b.strikes(BOARD_ABOVE), 4);
        // One more failure, then gone one millisecond past the cap.
        let at = b.note_failure(BOARD_ABOVE, DialFailure::NotColumba, 300_000);
        assert_eq!(at.strikes, 5);
        let back = 300_000 + DIAL_BACKOFF_CAP_MS + 1;
        assert_eq!(b.held_until(BOARD_ABOVE, back), None);
        assert_eq!(b.sighted(BOARD_ABOVE, back), None);
        assert_eq!(b.strikes(BOARD_ABOVE), 0, "forgotten");
        let fresh = b.note_failure(BOARD_ABOVE, DialFailure::NotColumba, back + 1_000);
        assert_eq!(fresh.strikes, 1);
        // And a failure booked after an unnoticed absence starts over
        // too, sighting or not.
        let mut q: DialBackoff<DIAL_BACKOFF_SLOTS> = DialBackoff::new();
        q.note_failure(BOARD_BELOW, DialFailure::NotColumba, 0);
        q.note_failure(BOARD_BELOW, DialFailure::NotColumba, 20_000);
        let late = q.note_failure(
            BOARD_BELOW,
            DialFailure::NotColumba,
            20_001 + DIAL_BACKOFF_CAP_MS,
        );
        assert_eq!(late.strikes, 1);
    }

    /// The hold is announced once per booking, not per sighting, and
    /// never for an address that may be dialled.
    #[test]
    fn a_backoff_hold_is_announced_once_per_booking() {
        let mut b: DialBackoff<DIAL_BACKOFF_SLOTS> = DialBackoff::new();
        assert_eq!(b.announce(BOARD_ABOVE, 0), None, "nothing booked");
        let first = b.note_failure(BOARD_ABOVE, DialFailure::NotColumba, 0);
        assert_eq!(b.announce(BOARD_ABOVE, 1_000), Some(first));
        for t in [2_000, 5_000, 9_999] {
            assert!(b.sighted(BOARD_ABOVE, t).is_some());
            assert_eq!(b.announce(BOARD_ABOVE, t), None, "once per booking");
        }
        let second = b.note_failure(BOARD_ABOVE, DialFailure::NotColumba, 12_000);
        assert_eq!(b.announce(BOARD_ABOVE, 12_500), Some(second));
        // A booking whose hold ran out unannounced stays silent.
        b.note_failure(BOARD_BELOW, DialFailure::Transport, 0);
        assert_eq!(b.announce(BOARD_BELOW, DIAL_BACKOFF_BASE_MS), None);
    }

    /// A full table evicts the address sighted longest ago, never a
    /// fresher run.
    #[test]
    fn a_full_backoff_table_evicts_the_stalest_address() {
        let mut b: DialBackoff<2> = DialBackoff::new();
        b.note_failure(1, DialFailure::NotColumba, 0);
        b.note_failure(2, DialFailure::NotColumba, 1_000);
        assert!(b.sighted(1, 2_000).is_some(), "1 is now the fresher");
        b.note_failure(3, DialFailure::NotColumba, 3_000);
        assert_eq!(b.strikes(2), 0, "2 was sighted longest ago");
        assert_eq!((b.strikes(1), b.strikes(3)), (1, 1));
    }

    #[test]
    fn the_constants_are_the_firmwares() {
        assert_eq!(DIAL_BACKOFF_BASE_MS, 5_000);
        assert_eq!(DIAL_BACKOFF_CAP_MS, 4 * SCAN_FALLBACK_AFTER_MS);
        assert_eq!(DIAL_BACKOFF_CAP_MS, DEAD_END_TTL_MS);
        assert_eq!(dial_backoff_ms(0), DIAL_BACKOFF_BASE_MS);
        assert_eq!(dial_backoff_ms(u8::MAX), DIAL_BACKOFF_CAP_MS);
        assert_eq!(DEAD_END_TTL_MS, 120_000);
        // Three incoming slots and one outgoing: `MAX_LINKS` is 4.
        assert_eq!(DEAD_END_SLOTS, 2 * (usize::from(PERIPH_SLOTS) + 1));
        // One key per link plus the departed ones: `MAX_LINKS` + 8.
        assert_eq!(
            PHASE_NEIGHBOUR_SLOTS,
            usize::from(PERIPH_SLOTS) + 1 + DEAD_END_SLOTS
        );
    }
}
