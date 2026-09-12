//! The per-interface peer-link registry: which 16-byte identity each
//! live link slot belongs to, and the first/last-link rules that decide
//! when a link transition is a *peer* transition (Codeberg #365).
//!
//! A Reticulum BLE interface is one broadcast domain carrying several
//! links, and the core is only told about peers, not links: the
//! identity's FIRST link up is a peer arrival (the main loop pulls the
//! peer's delivery path), the identity's LAST link down is a peer loss
//! (the main loop culls the paths via that peer). A same-identity link
//! on another slot — a peer whose rotated address reconnected while its
//! old link was still up — is registry churn on both edges and must
//! report neither.
//!
//! Which of the two links that peer keeps is [`PeerRegistry::link_up`]'s
//! second job, and the rule is [`judge_duplicate`]: the NEWER connection
//! wins, unless the old link is demonstrably in active use — real
//! payload within [`LINK_ACTIVE_DATA_MS`] — in which case the newcomer
//! is refused (#360). The registry carries two clocks per slot: the
//! liveness clock ([`PeerRegistry::note_heard`]), fed by the caller's
//! inbound path for EVERY frame, keepalives included, exactly as lnsd's
//! `LinkTable` feeds `last_heard_ms` — the expiry sweeps read it through
//! [`PeerRegistry::silence_ms`] to tear a link down that has stopped
//! answering altogether — and the payload clock
//! ([`PeerRegistry::note_data`]), fed for non-keepalive frames only,
//! which is the one input the duplicate rule consults.
//!
//! The rules are pure and their failure modes are sequences (a flap, a
//! displacement, the runtime carrier-off teardown that drops every live
//! link at once), so they live here with the crate's other host-tested
//! state machines; the firmware wraps one instance in a
//! critical-section mutex and reports what the return values tell it to
//! (`leviculum_nrf::ble::columba`).

use leviculum_core::framing::ble::KEEPALIVE_INTERVAL_MS;

/// One interface's live links, indexed by the link's drain-table slot —
/// the same index that selects its outbound queue, so the drain table,
/// the fan-out and this registry can never disagree about which links
/// exist.
///
/// Two facts per slot, learned at different moments: the CONNECTION
/// address ([`conn_up`](Self::conn_up)), known the instant the link
/// exists in either role, and the peer IDENTITY
/// ([`link_up`](Self::link_up)), known only after the handshake or the
/// characteristic read. The address side exists for the scanner
/// (#375 §0): Core Spec Vol 6 Part B §4.5 permits only one connection
/// between two device addresses — an initiator "shall not send a
/// connection request to an advertiser it is already connected to",
/// and an advertiser "shall ignore" one from a device it is connected
/// to — so a dial to a live link's address can never succeed and must
/// be excluded BEFORE it spends five seconds timing out on the air.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRegistry<const N: usize> {
    slots: [Option<[u8; 16]>; N],
    addrs: [Option<u64>; N],
    last_heard_ms: [u64; N],
    /// When the slot's link last delivered real payload — a fragment
    /// frame, never a keepalive and never the handshake — `None` until
    /// it first does. The duplicate rule's one input (#360).
    last_data_ms: [Option<u64>; N],
}

/// A link that has delivered NOTHING — no packet fragment, no
/// keepalive — for this long is dead, and is torn down: it is the
/// EXPIRY bound, on both stacks (lnsd's `LinkTable::expire`, the
/// firmware session's `link_silent` arm), and since #382 it decides
/// nothing else. [`judge_duplicate`] does not read it — a duplicate is
/// decided by the old link's payload recency against
/// [`LINK_ACTIVE_DATA_MS`], never by this bound — and a link that stops
/// answering is removed by the sweep, whether or not anybody dials it.
///
/// Three missed keepalives at the protocol's 15 s cadence
/// (`leviculum_core::framing::ble::KEEPALIVE_INTERVAL_MS`). One
/// constant, imported by lnsd rather than duplicated, so the two stacks
/// cannot disagree about when a link is over.
///
/// It replaced a 30 s clock that measured PAYLOAD silence only
/// (Codeberg #382). The measurement that killed that clock: over 14.1 h
/// beside a Columba phone (`ble-accept-rns/lnsd.log`, 2026-08-30) the
/// gaps between received non-keepalive packets from a peer that was
/// demonstrably present throughout ran to a median of 51 s, a 90th
/// percentile of 182 s and a maximum of 5590 s, and 502 links outlived
/// 45 s without one byte of payload. Payload silence is what an idle
/// phone looks like; it is not evidence of anything. Keepalives are,
/// and the same log shows them arriving: only 2 of those 502 links were
/// ever closed by the silence timer.
pub const LINK_TIMEOUT_MS: u64 = 3 * KEEPALIVE_INTERVAL_MS;

/// Who opened the connection whose duplicate identity is being judged.
///
/// Since #360 it is NOT an input to [`judge_duplicate`] — the rule is
/// the same in both roles, and the function's signature is where that
/// is enforced. It survives as the `origin=` token every duplicate log
/// line still carries, so a capture shows which direction a decision
/// fired in even though the direction no longer decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The PEER connected to us (we are the peripheral; on lnsd,
    /// `Role::Peripheral`).
    Incoming,
    /// WE dialled the peer (we are the central; on lnsd,
    /// `Role::Central`).
    Outgoing,
}

/// What to do with a second connection carrying an identity we already
/// hold a live link to (see [`judge_duplicate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Duplicate {
    /// Keep the old link, drop the newcomer.
    Refuse,
    /// Tear the old link down; the newcomer takes the peer over.
    Displace,
}

/// A duplicate handshake is refused only when the old link carried real
/// payload within this window; otherwise the newer connection wins
/// ([`judge_duplicate`], #360).
///
/// One keepalive interval, `leviculum_core::framing::ble`'s
/// `KEEPALIVE_INTERVAL_MS` (15 s), and the choice is argued on both
/// bounds:
///
/// - **Why payload and not any frame.** A phone that rotates its
///   address abandons the old connection mid-interval, so the abandoned
///   link's last KEEPALIVE can be arbitrarily recent — the 2026-09-12
///   field T114 refused the rotated phone's replacement at
///   `old_silence_ms=8562` and `9674`, both inside one keepalive
///   interval, and the phone then had no link at all until the 45 s
///   expiry, ~45 s of every ~90 s rotation cycle (#360). A healthy idle
///   link and a rotated-away one are indistinguishable on the any-frame
///   clock at decision time; on the payload clock the rotated-away link
///   fails immediately, because it never delivers payload again.
/// - **Why one interval and not the reference's 30 s.** Every second of
///   this window extends the peer's linkless outage when a rotation
///   happens to follow payload closely — the abandoned link then blocks
///   its own replacement for the window's remainder. One interval is
///   the protocol's own liveness quantum, the unit `LINK_TIMEOUT_MS`
///   is already three of; a second free parameter (the reference's
///   `_zombie_timeout = 30.0`, `ble-reticulum` `BLEInterface.py`, has
///   no stated derivation) would be one more number the stacks could
///   drift on.
/// - **Why not zero.** Payload within the window is proof the old link
///   is delivering RIGHT NOW — the one state whose displacement costs
///   something the expiry would not also cost, a transfer in flight cut
///   for a connection that adds no reachability. Fragments of an active
///   transfer arrive well inside one interval, so the window covers
///   exactly that state.
pub const LINK_ACTIVE_DATA_MS: u64 = KEEPALIVE_INTERVAL_MS;

/// Decide a duplicate identity from how recently the OLD link carried
/// real payload, and from nothing else — the same rule in both roles
/// and on both stacks (#360): `old_data_silence_ms` is the time since
/// the old link's last non-keepalive frame, `None` when it never
/// carried one.
///
/// **The newer connection wins by default.** That is what the peers
/// run: Columba's BLE driver accepts the newer connection of an
/// identity it already holds once the old one is no longer active
/// (reference `ble-reticulum` `BLEInterface.py`,
/// `_check_duplicate_identity` — a stale or "zombie" old connection
/// never blocks the new one). The board-side field failure this fixes
/// (#360, 2026-09-12) was the opposite choice: the phone rotated its
/// address, its old connection to us went silent, our own dial of the
/// new address learned the same identity and was refused
/// (`origin=outgoing old_silence_ms=9674`), and the phone was linkless
/// until the old link's 45 s expiry — every ~90 s rotation cycle. A
/// peer's rotated-away link never speaks again, so keeping it AT ALL
/// is 45 s of dead air bought for nothing.
///
/// **The one refusal left** is an old link in demonstrable active use:
/// real payload within [`LINK_ACTIVE_DATA_MS`] (see there for the
/// bound's argument). Displacing a link mid-transfer for a connection
/// that adds no reachability is the 13bea3e5 field cost — the phone's
/// working link killed every ~95 s by our own blind fallback dial —
/// and a genuine duplicate dial is exactly a dial that lands while the
/// old link works. Keepalives do not count: an abandoned link's last
/// keepalive can be arbitrarily recent, so any keepalive-based refusal
/// re-creates the 45 s outage above.
///
/// **A dead link is still cleared — by expiry, not here.** The liveness
/// clock ([`PeerRegistry::note_heard`], fed per inbound FRAME including
/// the 1-byte keepalive since 381fa5a0) is read by two sweeps that tear
/// a link down once it has delivered neither payload nor keepalive for
/// [`LINK_TIMEOUT_MS`]: lnsd's `LinkTable::expire`
/// (`leviculum-std/src/interfaces/ble/links.rs`), driven from the
/// interface tick in the same module's `mod.rs`, which disconnects the
/// device and reports the peer lost; and the firmware's per-session
/// silence arm `link_silent` (`leviculum-nrf/src/ble/columba.rs`),
/// which disconnects and reports through the same `peer_link_down`
/// range loss takes. The duplicate rule handles only the case the
/// expiry is too slow for: the peer is HERE, on a new connection,
/// asking to be reachable now.
///
/// Who opened the connection ([`Origin`]) is deliberately absent from
/// the signature: the 2026-09-09 pair of field failures showed each
/// direction-based answer wrong in one direction, and #360 showed the
/// direction-only rule wrong again. Every duplicate log line still
/// carries `origin=` — the capture shows the direction, the rule does
/// not read it.
pub const fn judge_duplicate(old_data_silence_ms: Option<u64>) -> Duplicate {
    match old_data_silence_ms {
        Some(ms) if ms < LINK_ACTIVE_DATA_MS => Duplicate::Refuse,
        _ => Duplicate::Displace,
    }
}

/// `old_data_silence_ms=` as the duplicate lines print it: the number,
/// or `never` when the old link has not carried one payload frame.
/// Its own token rather than a sentinel number, because every number in
/// that position is also a real answer (the same argument as the
/// firmware's `FreeSlots` token in its `BLE_SCAN_DECISION` line).
pub struct DataSilence(pub Option<u64>);

impl core::fmt::Display for DataSilence {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(ms) => write!(f, "{ms}"),
            None => f.write_str("never"),
        }
    }
}

/// What registering a link amounted to (see [`PeerRegistry::link_up`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkUp {
    /// Registered. `first` iff this is the identity's FIRST live link —
    /// the caller reports a peer arrival exactly then.
    Accepted { first: bool },
    /// Registered, and the identity's OLD link on `old_slot` must be
    /// torn down by the caller. Never an arrival — the peer was never
    /// gone. `old_silence_ms` is how long that link had delivered
    /// nothing at all (reported, not consulted);
    /// `old_data_silence_ms` how long since it carried real payload —
    /// the input [`judge_duplicate`] decided on, `None` for never.
    Displaced {
        old_slot: usize,
        old_silence_ms: u64,
        old_data_silence_ms: Option<u64>,
    },
    /// NOT registered: the identity's existing link on `old_slot`
    /// carried real payload within [`LINK_ACTIVE_DATA_MS`]
    /// (`old_data_silence_ms`, the consulted input) and keeps the peer;
    /// `old_silence_ms` is reported beside it. The caller drops THIS
    /// connection.
    Refused {
        old_slot: usize,
        old_silence_ms: u64,
        old_data_silence_ms: Option<u64>,
    },
}

impl<const N: usize> Default for PeerRegistry<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> PeerRegistry<N> {
    /// No links.
    pub const fn new() -> Self {
        Self {
            slots: [None; N],
            addrs: [None; N],
            last_heard_ms: [0; N],
            last_data_ms: [None; N],
        }
    }

    /// Register a slot's connection address — at the connection event,
    /// in either role, before any identity is known.
    pub fn conn_up(&mut self, slot: usize, addr_value: u64) {
        self.addrs[slot] = Some(addr_value);
    }

    /// Clear a slot's connection address at teardown (the counterpart
    /// of [`conn_up`](Self::conn_up); identity clearing is
    /// [`link_down`](Self::link_down)'s job).
    pub fn conn_down(&mut self, slot: usize) {
        self.addrs[slot] = None;
    }

    /// Whether a live connection was made on this address — the
    /// scanner's pre-dial exclusion (see the struct docs). Addresses
    /// rotate, so a linked PEER can still reappear under a fresh
    /// address this check cannot know — and the identity behind an
    /// advertisement is unknowable before connecting, so that residual
    /// dial is inherent. It is resolved post-connect by
    /// [`link_up`](Self::link_up)'s identity duplicate rule, which
    /// refuses such a dial outright.
    pub fn addr_linked(&self, addr_value: u64) -> bool {
        self.addrs.iter().flatten().any(|a| *a == addr_value)
    }

    /// Register a slot's peer, at `now_ms`.
    ///
    /// With no other link from that identity this is a plain
    /// [`LinkUp::Accepted`], `first` iff it is the identity's FIRST live
    /// link — the caller reports a peer arrival exactly then.
    ///
    /// A second connection from an identity we already hold is decided
    /// by [`judge_duplicate`] from the old link's payload recency: the
    /// newer connection displaces the old link unless that link carried
    /// real payload within [`LINK_ACTIVE_DATA_MS`], which refuses the
    /// newcomer instead ([`LinkUp::Refused`]). The rule is the same in
    /// both roles — who opened the connection is not a parameter here,
    /// which is what enforces that (#360). The old link's any-frame
    /// silence is still MEASURED and carried on both variants — it is
    /// the number a capture checks the decision against — but only the
    /// payload clock is consulted. Neither edge of a displacement is a
    /// peer transition — the peer was never gone — which is why
    /// `Displaced` carries no `first` flag.
    ///
    /// Re-registering the SAME slot is neither: the link the caller
    /// would tear down is the one it just kept.
    ///
    /// An accepted link starts its liveness clock here, and its payload
    /// clock at `never`: the handshake (peripheral) or the identity
    /// read (central) that got us this far proves the peer is present,
    /// not that this link carries traffic — a link that has only
    /// handshaked must not outrank the next handshake of the same
    /// identity.
    pub fn link_up(&mut self, slot: usize, peer: [u8; 16], now_ms: u64) -> LinkUp {
        let old = self
            .slots
            .iter()
            .position(|id| *id == Some(peer))
            .filter(|old| *old != slot);
        if let Some(old_slot) = old {
            // The any-frame silence is measured for the log line on
            // both branches and consulted on neither; the payload
            // silence is the rule's input (#360).
            let old_silence_ms = now_ms.saturating_sub(self.last_heard_ms[old_slot]);
            let old_data_silence_ms =
                self.last_data_ms[old_slot].map(|last| now_ms.saturating_sub(last));
            if judge_duplicate(old_data_silence_ms) == Duplicate::Refuse {
                return LinkUp::Refused {
                    old_slot,
                    old_silence_ms,
                    old_data_silence_ms,
                };
            }
            self.slots[slot] = Some(peer);
            self.last_heard_ms[slot] = now_ms;
            self.last_data_ms[slot] = None;
            return LinkUp::Displaced {
                old_slot,
                old_silence_ms,
                old_data_silence_ms,
            };
        }
        let first = self.slots.iter().flatten().all(|id| *id != peer);
        self.slots[slot] = Some(peer);
        self.last_heard_ms[slot] = now_ms;
        self.last_data_ms[slot] = None;
        LinkUp::Accepted { first }
    }

    /// A slot's peer delivered a frame at `now_ms` — the liveness clock
    /// the expiry reads through [`silence_ms`](Self::silence_ms), and
    /// the one [`link_up`](Self::link_up) reports (never consults) with
    /// each duplicate decision.
    ///
    /// EVERY inbound frame, keepalives included (Codeberg #382). A
    /// keepalive is the one thing a peer with nothing to say still
    /// sends, so excluding it made a quiet peer indistinguishable from
    /// a departed one; the failure the old payload-only clock guarded
    /// against — a degraded link that still passes 1-byte writes while
    /// packet-sized ones fail — is now caught by the peer itself, which
    /// reconnects and displaces the link as an incoming duplicate.
    ///
    /// Called per FRAME, not per reassembled packet, so a long packet's
    /// fragments each count and a packet that never finishes
    /// reassembling still proves the link delivers.
    pub fn note_heard(&mut self, slot: usize, now_ms: u64) {
        self.last_heard_ms[slot] = now_ms;
    }

    /// A slot's peer delivered a real PAYLOAD frame at `now_ms` — a
    /// fragment, never the 1-byte keepalive — which feeds BOTH clocks:
    /// payload is also liveness, and it is additionally the active-use
    /// evidence [`judge_duplicate`] consults (#360). The caller's
    /// inbound path calls this instead of
    /// [`note_heard`](Self::note_heard) for every frame at or above the
    /// fragment-header size, per frame and before reassembly, like the
    /// liveness clock and for the same reason.
    pub fn note_data(&mut self, slot: usize, now_ms: u64) {
        self.last_heard_ms[slot] = now_ms;
        self.last_data_ms[slot] = Some(now_ms);
    }

    /// How long the slot's link has delivered nothing at all — no
    /// payload, no keepalive — at `now_ms`. `None` while the slot holds
    /// no identity: a connection that has not handshaked yet has no
    /// liveness clock to read, and the slot's previous tenant's clock
    /// is not it.
    ///
    /// The expiry sweep's input. lnsd's `LinkTable::expire` makes the
    /// same subtraction over its own rows; the firmware has no sweep
    /// task, so its per-session silence arm asks the registry directly
    /// (`leviculum_nrf::ble::columba::link_silent`). A link at or past
    /// [`LINK_TIMEOUT_MS`] here is torn down — that, not a displacement,
    /// is what clears a dead link since #382.
    pub fn silence_ms(&self, slot: usize, now_ms: u64) -> Option<u64> {
        self.slots[slot]?;
        Some(now_ms.saturating_sub(self.last_heard_ms[slot]))
    }

    /// Clear a slot. `Some(identity)` iff that took the identity's LAST
    /// live link — the caller reports a peer loss exactly then. An
    /// unclaimed slot yields `None`: no link, no loss.
    pub fn link_down(&mut self, slot: usize) -> Option<[u8; 16]> {
        let identity = self.slots[slot].take()?;
        self.last_data_ms[slot] = None;
        self.slots
            .iter()
            .flatten()
            .all(|id| *id != identity)
            .then_some(identity)
    }

    /// Whether the identity holds a live link — the central path's
    /// duplicate check (BLE addresses rotate, identities do not).
    pub fn is_linked(&self, peer: &[u8; 16]) -> bool {
        self.slots.iter().flatten().any(|id| id == peer)
    }

    /// The slot of a live link to this peer, if it holds one (Codeberg
    /// #376) — the fan-out's peer-to-link map.
    ///
    /// A peer holding two links is still one peer, and either link
    /// reaches it, so the lowest slot is returned. Two links exist only
    /// during a displacement's hand-over, whose old link is
    /// already being torn down: [`Self::link_up`] registers the new slot
    /// BEFORE the old one is signalled, and the old session clears its
    /// registry entry before releasing its drain slot, so the window is a
    /// fan-out or two wide and both slots are live throughout it.
    pub fn slot_for(&self, peer: &[u8; 16]) -> Option<usize> {
        self.slots.iter().position(|id| id.as_ref() == Some(peer))
    }

    /// The number of DISTINCT live peer identities (Codeberg #365) —
    /// the value the main loop mirrors into the core as the
    /// interface's peer count. Distinct, not per-slot: during a
    /// displacement's hand-over (#376) one peer briefly holds two links,
    /// and it is still one peer.
    pub fn peer_count(&self) -> usize {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().map(|id| (i, id)))
            .filter(|(i, id)| {
                self.slots[..*i]
                    .iter()
                    .flatten()
                    .all(|earlier| earlier != *id)
            })
            .count()
    }
}

/// What the core's #376 delivery hint made of one outbound packet
/// (`leviculum_nrf::ble::tx_fanout_task`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxFanout {
    /// No hint — a broadcast (announce, path request, anything the core
    /// did not address at a named peer). Copied into every live link's
    /// queue, which is what a Reticulum interface owes a broadcast: one
    /// `try_send` reaches every peer on the medium, exactly as one LoRa
    /// transmission reaches every listener.
    Flood,
    /// Hinted, and the peer holds a live link: queue on that slot alone.
    Route(usize),
    /// Hinted at a peer with NO live link here. The packet is DROPPED,
    /// not flooded.
    ///
    /// The hint exists because the core routed these bytes at that one
    /// neighbour. The remaining links are not a route to it: flooding
    /// them spends their airtime on a packet they must forward or drop,
    /// and a neighbour that forwards it re-creates exactly the relayed
    /// duplicate the hint removes (the 2026-09-09 desk failure, where a
    /// telemetry report addressed to the phone reached it twice, once
    /// through the other board). The peer's disappearance is separately
    /// reported to the core as a peer loss (Codeberg #365), which culls
    /// the paths via it, so the next packet for that destination is
    /// routed afresh — over another interface or after a fresh path
    /// request — instead of sprayed at links that cannot deliver it.
    NoLink,
}

/// Map the core's delivery hint onto this interface's links (see
/// [`TxFanout`]).
///
/// Pure, so the decision is host-tested; the firmware's fan-out task
/// supplies the registry and executes the answer.
pub fn plan_fanout<const N: usize>(
    registry: &PeerRegistry<N>,
    peer: Option<&[u8; 16]>,
) -> TxFanout {
    match peer {
        None => TxFanout::Flood,
        Some(peer) => match registry.slot_for(peer) {
            Some(slot) => TxFanout::Route(slot),
            None => TxFanout::NoLink,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 16] = [0xaa; 16];
    const B: [u8; 16] = [0xbb; 16];

    /// The number itself, pinned. It is the link timeout both
    /// interfaces expire on — three missed keepalives — not a second
    /// constant beside it, and since #382 it is an expiry bound only:
    /// [`judge_duplicate`] cannot reach it.
    #[test]
    fn the_dead_link_bound_is_the_link_timeout_itself() {
        assert_eq!(LINK_TIMEOUT_MS, 45_000);
        assert_eq!(LINK_TIMEOUT_MS, 3 * KEEPALIVE_INTERVAL_MS);
    }

    /// The refusal window, pinned: one keepalive interval on the
    /// PAYLOAD clock (#360) — the same protocol quantum the expiry
    /// bound is three of, not a second free parameter.
    #[test]
    fn the_active_data_window_is_one_keepalive_interval() {
        assert_eq!(LINK_ACTIVE_DATA_MS, 15_000);
        assert_eq!(LINK_ACTIVE_DATA_MS, KEEPALIVE_INTERVAL_MS);
    }

    #[test]
    fn the_first_link_of_an_identity_is_an_arrival_and_displaces_nothing() {
        let mut reg = PeerRegistry::<4>::new();
        assert_eq!(reg.link_up(0, A, 0), LinkUp::Accepted { first: true });
        assert_eq!(
            reg.link_up(1, B, 0),
            LinkUp::Accepted { first: true },
            "a different identity is its own arrival"
        );
    }

    /// The #360 minimal reproducer, the 2026-09-12 field T114 as a
    /// unit: the phone rotated its address and abandoned its old
    /// connection, which by then had been silent for ~8 s; the same
    /// identity handshakes on a new connection. The old rule refused
    /// (`origin=outgoing old_silence_ms=8562`) and the phone was
    /// linkless until the 45 s expiry — ~45 s of every ~90 s rotation
    /// cycle. Required: the NEW connection wins, the old link is torn
    /// down as replaced, and the caller's counters follow.
    #[test]
    fn a_rotated_phones_new_connection_replaces_its_silent_old_link() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 1_000);
        // The phone's last sign of life on the old link, then rotation.
        reg.note_heard(0, 2_000);
        assert_eq!(
            reg.link_up(1, A, 10_000),
            LinkUp::Displaced {
                old_slot: 0,
                old_silence_ms: 8_000,
                old_data_silence_ms: None,
            },
            "8 s of silence and no payload in flight: the newcomer wins"
        );
        assert_eq!(reg.peer_count(), 1, "one peer throughout the hand-over");
        assert_eq!(reg.link_down(0), None, "the old link's death is churn");
        assert_eq!(reg.slot_for(&A), Some(1), "the new link holds the peer");
    }

    /// The same rotation with payload in the link's history: payload
    /// OLDER than the window does not save the old link either — only
    /// payload within [`LINK_ACTIVE_DATA_MS`] does.
    #[test]
    fn stale_payload_does_not_save_a_rotated_away_link() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 0);
        reg.note_data(0, 60_000);
        // Keepalives kept it out of the expiry's reach, then rotation.
        reg.note_heard(0, 100_000);
        assert_eq!(
            reg.link_up(1, A, 108_000),
            LinkUp::Displaced {
                old_slot: 0,
                old_silence_ms: 8_000,
                old_data_silence_ms: Some(48_000),
            },
            "payload 48 s ago is history, not active use"
        );
    }

    /// The one refusal left (#360): the old link carried real payload
    /// within one keepalive interval — a transfer demonstrably in
    /// flight — so the second connection is a genuine duplicate dial
    /// (our blind fallback dial finding the peer's rotated
    /// advertisement, the 13bea3e5 field failure) and is sent away.
    #[test]
    fn a_duplicate_of_an_actively_used_link_is_refused() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 1_000);
        reg.note_data(0, 20_000);
        assert_eq!(
            reg.link_up(1, A, 25_000),
            LinkUp::Refused {
                old_slot: 0,
                old_silence_ms: 5_000,
                old_data_silence_ms: Some(5_000),
            },
            "payload 5 s ago: the old link is in use and keeps the peer"
        );
        assert_eq!(reg.slot_for(&A), Some(0), "the old link still holds it");
        assert!(
            reg.link_down(1).is_none(),
            "the refused slot was never registered"
        );
        assert_eq!(reg.link_down(0), Some(A), "and the old link is the peer");
    }

    /// The window's edges, on the rule directly and through the
    /// registry: payload at the bound is already history (`<`, not
    /// `<=`), one millisecond inside it still refuses, and `never`
    /// always displaces.
    #[test]
    fn the_duplicate_rule_reads_payload_recency_and_nothing_else() {
        assert_eq!(judge_duplicate(None), Duplicate::Displace);
        assert_eq!(judge_duplicate(Some(0)), Duplicate::Refuse);
        assert_eq!(
            judge_duplicate(Some(LINK_ACTIVE_DATA_MS - 1)),
            Duplicate::Refuse
        );
        assert_eq!(
            judge_duplicate(Some(LINK_ACTIVE_DATA_MS)),
            Duplicate::Displace
        );
        assert_eq!(judge_duplicate(Some(u64::MAX)), Duplicate::Displace);

        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 0);
        reg.note_data(0, 1_000);
        assert!(matches!(
            reg.link_up(1, A, 1_000 + LINK_ACTIVE_DATA_MS - 1),
            LinkUp::Refused { .. }
        ));
        assert!(matches!(
            reg.link_up(1, A, 1_000 + LINK_ACTIVE_DATA_MS),
            LinkUp::Displaced { .. }
        ));
    }

    /// Keepalives are liveness, not active use (#360): they keep a link
    /// out of the EXPIRY's reach forever, and they never refuse the
    /// identity's next handshake — an abandoned link's last keepalive
    /// can be arbitrarily recent (the phone rotates mid-interval), so a
    /// keepalive-based refusal would re-create the 45 s field outage.
    #[test]
    fn keepalives_hold_off_the_expiry_but_never_refuse_a_replacement() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 0);
        let mut t = 0;
        for _ in 0..100 {
            t += KEEPALIVE_INTERVAL_MS;
            reg.note_heard(0, t);
            assert!(
                reg.silence_ms(0, t).is_some_and(|s| s < LINK_TIMEOUT_MS),
                "the expiry never comes for a link that keeps answering"
            );
        }
        assert_eq!(
            reg.link_up(1, A, t + 1),
            LinkUp::Displaced {
                old_slot: 0,
                old_silence_ms: 1,
                old_data_silence_ms: None,
            },
            "a keepalive one millisecond ago is no reason to refuse"
        );
    }

    /// The handshake itself is not payload: a link that has only just
    /// handshaked must not outrank the same identity's NEXT handshake,
    /// or a phone whose first connection came up half-broken could
    /// never replace it inside the window.
    #[test]
    fn a_fresh_handshake_alone_does_not_refuse_the_next_one() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 1_000);
        assert!(matches!(
            reg.link_up(1, A, 1_001),
            LinkUp::Displaced {
                old_slot: 0,
                old_data_silence_ms: None,
                ..
            }
        ));
    }

    /// The expiry's own input, which is the mechanism item 2 of #382
    /// hands the dead-link job to: no identity, no clock; a registered
    /// link starts at zero and ages from what it last delivered.
    #[test]
    fn silence_is_reported_only_for_a_slot_that_holds_a_link() {
        let mut reg = PeerRegistry::<4>::new();
        assert_eq!(
            reg.silence_ms(0, 10_000),
            None,
            "an un-handshaked connection has no liveness clock to read"
        );
        reg.link_up(0, A, 10_000);
        assert_eq!(reg.silence_ms(0, 10_000), Some(0));
        assert_eq!(
            reg.silence_ms(0, 10_000 + LINK_TIMEOUT_MS),
            Some(LINK_TIMEOUT_MS)
        );
        reg.note_heard(0, 10_000 + LINK_TIMEOUT_MS);
        assert_eq!(reg.silence_ms(0, 10_000 + LINK_TIMEOUT_MS), Some(0));
        reg.link_down(0);
        assert_eq!(
            reg.silence_ms(0, 99_000),
            None,
            "and a freed slot's stale clock is nobody's evidence"
        );
    }

    /// A same-slot re-registration must not name its own slot: the
    /// caller would refuse — or tear down — the very connection it just
    /// kept. True in both directions and on both sides of the boundary.
    #[test]
    fn re_registering_the_same_slot_neither_refuses_nor_displaces() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 0);
        assert_eq!(reg.link_up(0, A, 1), LinkUp::Accepted { first: false });
        assert_eq!(
            reg.link_up(0, A, 10 * LINK_TIMEOUT_MS),
            LinkUp::Accepted { first: false }
        );
    }

    /// A slot's clocks belong to the link that holds it now: a fresh
    /// registration resets both, so a link inheriting a slot whose
    /// previous tenant carried payload moments ago is not protected by
    /// that tenant's activity — nor aged by its silence.
    #[test]
    fn a_new_link_starts_its_own_clocks() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, B, 0);
        reg.note_data(0, 1_000);
        reg.link_down(0);
        // B's payload was 1 ms ago; A claims the slot now.
        reg.link_up(0, A, 1_001);
        assert_eq!(
            reg.link_up(1, A, 1_002),
            LinkUp::Displaced {
                old_slot: 0,
                old_silence_ms: 1,
                old_data_silence_ms: None,
            },
            "the previous tenant's payload clock is nobody's evidence"
        );
    }

    #[test]
    fn the_last_link_of_an_identity_is_a_loss() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 0);
        assert_eq!(reg.link_down(0), Some(A));
    }

    #[test]
    fn a_non_last_link_down_is_churn_not_a_loss() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 0);
        reg.link_up(1, A, LINK_TIMEOUT_MS);
        assert_eq!(reg.link_down(0), None);
        assert!(reg.is_linked(&A));
        assert_eq!(reg.link_down(1), Some(A));
    }

    #[test]
    fn an_unclaimed_slot_going_down_reports_nothing() {
        let mut reg = PeerRegistry::<4>::new();
        assert_eq!(reg.link_down(2), None);
    }

    #[test]
    fn the_duplicate_check_tracks_liveness() {
        let mut reg = PeerRegistry::<4>::new();
        assert!(!reg.is_linked(&A));
        reg.link_up(0, A, 0);
        assert!(reg.is_linked(&A));
        reg.link_down(0);
        assert!(!reg.is_linked(&A));
    }

    /// The runtime carrier-off teardown: `--set-media ble=off` makes
    /// every connection task drop its own link, in whatever order the
    /// executor reaches them. Every linked identity must yield exactly
    /// one loss — that is what feeds one `PeerEvent::Lost` per peer to
    /// the core's path cull, the same report range loss produces.
    #[test]
    fn dropping_every_claimed_slot_yields_one_loss_per_identity() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 0);
        reg.link_up(2, B, 0);
        let losses: Vec<[u8; 16]> = (0..4).filter_map(|slot| reg.link_down(slot)).collect();
        assert_eq!(losses, vec![A, B]);
        assert!(!reg.is_linked(&A));
        assert!(!reg.is_linked(&B));
    }

    /// Same teardown mid-displacement-hand-over: two links, one
    /// identity. One loss, not two — the peer left once.
    #[test]
    fn a_displaced_identity_is_lost_once_when_all_slots_drop() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 0);
        reg.link_up(1, A, LINK_TIMEOUT_MS);
        let losses: Vec<[u8; 16]> = (0..4).filter_map(|slot| reg.link_down(slot)).collect();
        assert_eq!(losses, vec![A]);
    }

    /// The #372 shape: every slot filled by a different identity — a
    /// phone and three boards. Each is an arrival, the count says four,
    /// and losing one leaves the other three linked and untouched.
    #[test]
    fn n_identities_fill_n_slots_and_one_loss_leaves_n_minus_one() {
        const C: [u8; 16] = [0xcc; 16];
        const D: [u8; 16] = [0xdd; 16];
        let mut reg = PeerRegistry::<4>::new();
        for (slot, id) in [A, B, C, D].into_iter().enumerate() {
            assert_eq!(
                reg.link_up(slot, id, 0),
                LinkUp::Accepted { first: true },
                "each identity's first link"
            );
        }
        assert_eq!(reg.peer_count(), 4);

        assert_eq!(reg.link_down(1), Some(B), "B's only link is a loss");
        assert_eq!(reg.peer_count(), 3);
        for id in [A, C, D] {
            assert!(reg.is_linked(&id), "the others are untouched");
        }
        assert!(!reg.is_linked(&B));
    }

    /// The #375 §0 exclusion input: a connection's address is known
    /// from the connection event, before any identity arrives, and
    /// clears at teardown.
    #[test]
    fn conn_addresses_are_tracked_from_connect_to_teardown() {
        let mut reg = PeerRegistry::<4>::new();
        assert!(!reg.addr_linked(0xC0DE));

        // The rig's exact gap: connected, identity not yet presented.
        reg.conn_up(1, 0xC0DE);
        assert!(reg.addr_linked(0xC0DE), "excluded before any dial");
        assert!(!reg.addr_linked(0xBEEF));

        // Identity arrives; the address side is unaffected.
        assert_eq!(reg.link_up(1, A, 0), LinkUp::Accepted { first: true });
        assert!(reg.addr_linked(0xC0DE));

        // Teardown clears both facts independently.
        assert_eq!(reg.link_down(1), Some(A));
        assert!(
            reg.addr_linked(0xC0DE),
            "identity gone, connection fact still set"
        );
        reg.conn_down(1);
        assert!(!reg.addr_linked(0xC0DE));
    }

    /// Two live connections on different slots: clearing one leaves the
    /// other's address linked.
    #[test]
    fn one_teardown_leaves_the_other_connection_linked() {
        let mut reg = PeerRegistry::<4>::new();
        reg.conn_up(0, 0x1111);
        reg.conn_up(2, 0x2222);
        reg.conn_down(0);
        assert!(!reg.addr_linked(0x1111));
        assert!(reg.addr_linked(0x2222));
    }

    /// The mirrored peer count is DISTINCT identities: a displaced
    /// identity on two slots is one peer, and a slot gap does not
    /// confuse the count.
    #[test]
    fn peer_count_is_distinct_identities_across_slot_gaps() {
        let mut reg = PeerRegistry::<4>::new();
        assert_eq!(reg.peer_count(), 0);
        reg.link_up(1, A, 0);
        assert_eq!(reg.peer_count(), 1);
        // The displacement hand-over: same identity on a second slot.
        reg.link_up(3, A, LINK_TIMEOUT_MS);
        assert_eq!(reg.peer_count(), 1, "two links, one peer");
        reg.link_up(0, B, LINK_TIMEOUT_MS);
        assert_eq!(reg.peer_count(), 2);
        reg.link_down(1);
        assert_eq!(reg.peer_count(), 2, "A still holds slot 3");
        reg.link_down(3);
        assert_eq!(reg.peer_count(), 1);
    }

    /// Without a hint the fan-out is a flood, whatever the registry
    /// holds: a broadcast owes every peer on the medium a copy.
    #[test]
    fn a_packet_without_a_hint_floods_every_live_link() {
        let mut reg = PeerRegistry::<4>::new();
        assert_eq!(plan_fanout(&reg, None), TxFanout::Flood, "no links");
        reg.link_up(0, A, 0);
        reg.link_up(1, B, 0);
        assert_eq!(plan_fanout(&reg, None), TxFanout::Flood);
    }

    /// With a hint the packet goes on the hinted peer's link and on no
    /// other — the whole point of #376 part 2.
    #[test]
    fn a_hinted_packet_takes_only_that_peers_link() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 0);
        reg.link_up(2, B, 0);
        assert_eq!(plan_fanout(&reg, Some(&A)), TxFanout::Route(0));
        assert_eq!(plan_fanout(&reg, Some(&B)), TxFanout::Route(2));
    }

    /// A peer holding two links (the displacement hand-over window) is
    /// one peer: either link reaches it, and the decision picks one
    /// rather than duplicating the packet across both.
    #[test]
    fn a_peer_with_two_links_gets_the_packet_once() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(1, A, 0);
        reg.link_up(3, A, LINK_TIMEOUT_MS);
        assert_eq!(plan_fanout(&reg, Some(&A)), TxFanout::Route(1));
    }

    /// The peer walked out between the core's routing decision and this
    /// fan-out: DROP, never a fallback flood. See [`TxFanout::NoLink`].
    #[test]
    fn a_hint_for_a_peer_with_no_link_drops_instead_of_flooding() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, B, 0);
        assert_eq!(
            plan_fanout(&reg, Some(&A)),
            TxFanout::NoLink,
            "A is gone; B's link is not a route to A"
        );
        // And with nothing live at all it is still a drop, not a flood.
        reg.link_down(0);
        assert_eq!(plan_fanout(&reg, Some(&A)), TxFanout::NoLink);
    }

    /// The peer's LAST link died: `slot_for` must not keep naming the
    /// slot the teardown released.
    #[test]
    fn slot_for_forgets_a_slot_at_teardown() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(2, A, 0);
        assert_eq!(reg.slot_for(&A), Some(2));
        reg.link_down(2);
        assert_eq!(reg.slot_for(&A), None);
    }
}
