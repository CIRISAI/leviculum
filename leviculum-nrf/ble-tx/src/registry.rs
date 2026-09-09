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
//! second job, and the rule is the reference's: the old link wins unless
//! it has gone zombie — no real data for [`ZOMBIE_TIMEOUT_MS`] — in
//! which case the newcomer displaces it. That needs a clock, so the
//! registry carries one per slot ([`PeerRegistry::note_real_data`]),
//! fed by the caller's inbound path exactly as lnsd's `LinkTable` feeds
//! `last_real_data_ms`.
//!
//! The rules are pure and their failure modes are sequences (a flap, a
//! displacement, the runtime carrier-off teardown that drops every live
//! link at once), so they live here with the crate's other host-tested
//! state machines; the firmware wraps one instance in a
//! critical-section mutex and reports what the return values tell it to
//! (`leviculum_nrf::ble::columba`).

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
    last_real_ms: [u64; N],
}

/// How long an existing link may have carried no *real* data — anything
/// the peer sent that is not a keepalive — before a fresh link from the
/// same identity may displace it (Codeberg #376).
///
/// The reference's `_zombie_timeout` (`ble-reticulum@07d94130`
/// `BLEInterface.py`, `_zombie_timeout`, applied in
/// `_check_duplicate_identity`) and lnsd's `ZOMBIE_TIMEOUT_MS`
/// (`leviculum-std/src/interfaces/ble/links.rs`, `LinkTable::admit`)
/// hold the same 30 s, and lnsd now reads *this* constant so the two
/// Rust stacks cannot drift apart. The mechanism it names is real and
/// asymmetric: a degraded BLE link still passes 1-byte keepalives while
/// every larger write fails, so silence-on-data is the only evidence a
/// link is dead that a live connection handle cannot fake.
///
/// The rule is a *refusal* rule first: below this age the old link is
/// kept and the newcomer is dropped. The 2026-09-09 field T114 is why —
/// there the newcomer was our own fallback dial into a phone we were
/// already linked to, and displacing the phone's own working link cost
/// every announce and every telemetry proof for as long as the phone
/// stayed beside the board.
pub const ZOMBIE_TIMEOUT_MS: u64 = 30_000;

/// What registering a link amounted to (see [`PeerRegistry::link_up`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkUp {
    /// Registered. `first` iff this is the identity's FIRST live link —
    /// the caller reports a peer arrival exactly then.
    Accepted { first: bool },
    /// Registered, and the identity's OLD link on `old_slot` must be
    /// torn down by the caller: it had carried no real data for
    /// `old_age_ms` (at or beyond [`ZOMBIE_TIMEOUT_MS`]), so it is a
    /// zombie and the newcomer wins. Never an arrival — the peer was
    /// never gone.
    Displaced { old_slot: usize, old_age_ms: u64 },
    /// NOT registered: the identity's link on `old_slot` carried real
    /// data `old_age_ms` ago, inside [`ZOMBIE_TIMEOUT_MS`], so it is
    /// alive and keeps the peer. The caller drops THIS connection.
    Refused { old_slot: usize, old_age_ms: u64 },
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
            last_real_ms: [0; N],
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
    /// refuses the newcomer unless the old link has gone zombie.
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
    /// by the old link's freshness, never by its age or its role
    /// ([`ZOMBIE_TIMEOUT_MS`]): a link that carried real data recently
    /// is alive and keeps the peer, so the newcomer is
    /// [`LinkUp::Refused`] and nothing here changes; a link that has
    /// been silent on data for the full timeout is a zombie and is
    /// [`LinkUp::Displaced`]. Neither edge of a displacement is a peer
    /// transition — the peer was never gone — which is why `Displaced`
    /// carries no `first` flag.
    ///
    /// Re-registering the SAME slot is neither: the link the caller
    /// would tear down is the one it just kept.
    ///
    /// An accepted link starts its freshness clock here. The handshake
    /// (peripheral) or the identity read (central) that got us this far
    /// is itself data the peer delivered, and the reference counts it
    /// the same way (`ble-reticulum@07d94130` `BLEInterface.py`,
    /// `_handle_identity_handshake`: "the 16-byte handshake counts as
    /// real data").
    pub fn link_up(&mut self, slot: usize, peer: [u8; 16], now_ms: u64) -> LinkUp {
        let old = self
            .slots
            .iter()
            .position(|id| *id == Some(peer))
            .filter(|old| *old != slot);
        if let Some(old_slot) = old {
            let old_age_ms = now_ms.saturating_sub(self.last_real_ms[old_slot]);
            if old_age_ms < ZOMBIE_TIMEOUT_MS {
                return LinkUp::Refused {
                    old_slot,
                    old_age_ms,
                };
            }
            self.slots[slot] = Some(peer);
            self.last_real_ms[slot] = now_ms;
            return LinkUp::Displaced {
                old_slot,
                old_age_ms,
            };
        }
        let first = self.slots.iter().flatten().all(|id| *id != peer);
        self.slots[slot] = Some(peer);
        self.last_real_ms[slot] = now_ms;
        LinkUp::Accepted { first }
    }

    /// A slot's peer sent real data at `now_ms` — the freshness clock
    /// [`link_up`](Self::link_up)'s duplicate rule reads.
    ///
    /// "Real data" is a received frame that is not a keepalive, lnsd's
    /// and the reference's notion exactly: keepalives are excluded
    /// because the failure this guards against is the link that still
    /// passes 1-byte writes while every packet-sized one fails. The
    /// caller filters keepalives out on its inbound path and calls this
    /// for what remains — per FRAME, not per reassembled packet, so a
    /// long packet's fragments each count.
    pub fn note_real_data(&mut self, slot: usize, now_ms: u64) {
        self.last_real_ms[slot] = now_ms;
    }

    /// Clear a slot. `Some(identity)` iff that took the identity's LAST
    /// live link — the caller reports a peer loss exactly then. An
    /// unclaimed slot yields `None`: no link, no loss.
    pub fn link_down(&mut self, slot: usize) -> Option<[u8; 16]> {
        let identity = self.slots[slot].take()?;
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
    /// during a zombie displacement's hand-over, whose old link is
    /// already being torn down: [`Self::link_up`] registers the new slot
    /// BEFORE the old one is signalled, and the old session clears its
    /// registry entry before releasing its drain slot, so the window is a
    /// fan-out or two wide and both slots are live throughout it.
    pub fn slot_for(&self, peer: &[u8; 16]) -> Option<usize> {
        self.slots.iter().position(|id| id.as_ref() == Some(peer))
    }

    /// The number of DISTINCT live peer identities (Codeberg #365) —
    /// the value the main loop mirrors into the core as the
    /// interface's peer count. Distinct, not per-slot: during a zombie
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

    /// The number itself, pinned. Three implementations agree on 30 s —
    /// this constant, lnsd's re-export of it, and the reference's
    /// `_zombie_timeout` — and a phone that walks between them must
    /// meet one rule, so the value is not free to drift quietly.
    #[test]
    fn the_zombie_timeout_is_the_references_thirty_seconds() {
        assert_eq!(ZOMBIE_TIMEOUT_MS, 30_000);
    }

    #[test]
    fn the_first_link_of_an_identity_is_an_arrival_and_displaces_nothing() {
        let mut reg = PeerRegistry::<4>::new();
        assert_eq!(reg.link_up(0, A, 0), LinkUp::Accepted { first: true });
        assert_eq!(
            reg.link_up(1, B, 0),
            LinkUp::Accepted { first: true },
            "a different identity is its own arrival, no displacement"
        );
    }

    /// The 2026-09-09 field T114, as a unit: the phone's own link is
    /// carrying data, our fallback dial reaches the same identity from
    /// its rotated address, and the LIVE link keeps the peer. Before
    /// this rule the newcomer displaced it unconditionally and every
    /// announce and proof to that phone died.
    #[test]
    fn a_second_link_is_refused_while_the_old_one_is_fresh() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 1_000);
        reg.note_real_data(0, 5_000);
        assert_eq!(
            reg.link_up(1, A, 5_000 + ZOMBIE_TIMEOUT_MS - 1),
            LinkUp::Refused {
                old_slot: 0,
                old_age_ms: ZOMBIE_TIMEOUT_MS - 1
            }
        );
        assert_eq!(reg.slot_for(&A), Some(0), "the old link still holds it");
        assert!(
            reg.link_down(1).is_none(),
            "the refused slot was never registered"
        );
        assert_eq!(reg.link_down(0), Some(A), "and the old link is the peer");
    }

    /// The morning case 6d3e5d4 was written for, still covered: that
    /// link had been silent for minutes, so it is a zombie by this rule
    /// and the reconnect takes the peer over.
    #[test]
    fn a_second_link_displaces_an_old_one_that_has_gone_zombie() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 1_000);
        // Keepalives keep the connection handle alive but are not real
        // data, so the caller never touches the clock for them.
        assert_eq!(
            reg.link_up(1, A, 1_000 + 4 * ZOMBIE_TIMEOUT_MS),
            LinkUp::Displaced {
                old_slot: 0,
                old_age_ms: 4 * ZOMBIE_TIMEOUT_MS
            }
        );
        // Both slots hold the identity until the old session's teardown
        // clears its own entry — the hand-over window `slot_for`
        // documents; the peer is one peer throughout it.
        assert_eq!(reg.peer_count(), 1);
        assert_eq!(reg.link_down(0), None, "the zombie's death is churn");
        assert_eq!(reg.slot_for(&A), Some(1), "the new link holds the peer");
    }

    /// The boundary sits exactly at the constant: one millisecond under
    /// it the old link is alive, at it the old link is a zombie. Same
    /// comparison lnsd's `admit` makes, so a phone that walks between
    /// the two stacks meets one rule.
    #[test]
    fn the_freshness_boundary_is_the_zombie_timeout() {
        let mut fresh = PeerRegistry::<4>::new();
        fresh.link_up(0, A, 0);
        assert!(matches!(
            fresh.link_up(1, A, ZOMBIE_TIMEOUT_MS - 1),
            LinkUp::Refused { .. }
        ));

        let mut stale = PeerRegistry::<4>::new();
        stale.link_up(0, A, 0);
        assert!(matches!(
            stale.link_up(1, A, ZOMBIE_TIMEOUT_MS),
            LinkUp::Displaced { .. }
        ));
    }

    /// Real data slides the boundary: a link the caller keeps feeding
    /// can never be displaced, however old the link itself is.
    #[test]
    fn real_data_keeps_an_old_link_out_of_the_zombie_window() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 0);
        let mut t = 0;
        for _ in 0..10 {
            t += ZOMBIE_TIMEOUT_MS - 1;
            reg.note_real_data(0, t);
        }
        assert!(matches!(reg.link_up(1, A, t + 1), LinkUp::Refused { .. }));
        assert!(matches!(
            reg.link_up(1, A, t + ZOMBIE_TIMEOUT_MS),
            LinkUp::Displaced { .. }
        ));
    }

    /// The hand-over sequence end to end: after a displacement the old
    /// slot's teardown is churn (the identity still owns the new link),
    /// and only the new link's death is the peer loss.
    #[test]
    fn displacement_teardown_of_the_old_slot_is_churn_not_a_loss() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 0);
        assert!(matches!(
            reg.link_up(1, A, ZOMBIE_TIMEOUT_MS),
            LinkUp::Displaced { old_slot: 0, .. }
        ));
        assert_eq!(reg.link_down(0), None, "old link's death is churn");
        assert!(reg.is_linked(&A), "the new link carries the peer");
        assert_eq!(reg.link_down(1), Some(A), "new link's death is the loss");
    }

    /// A same-slot re-registration must not name its own slot: the
    /// caller would refuse — or tear down — the very connection it just
    /// kept. True on both sides of the freshness boundary.
    #[test]
    fn re_registering_the_same_slot_neither_refuses_nor_displaces() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, 0);
        assert_eq!(reg.link_up(0, A, 1), LinkUp::Accepted { first: false });
        assert_eq!(
            reg.link_up(0, A, 10 * ZOMBIE_TIMEOUT_MS),
            LinkUp::Accepted { first: false }
        );
    }

    /// A slot's freshness clock belongs to the link that holds it now:
    /// a fresh registration resets it, so a peer inheriting a slot whose
    /// previous tenant went silent is not instantly displaceable.
    #[test]
    fn a_new_link_starts_its_own_freshness_clock() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, B, 0);
        reg.link_down(0);
        // Slot 0 was silent for ages; A claims it now.
        let t = 10 * ZOMBIE_TIMEOUT_MS;
        reg.link_up(0, A, t);
        assert!(matches!(reg.link_up(1, A, t + 1), LinkUp::Refused { .. }));
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
        reg.link_up(1, A, ZOMBIE_TIMEOUT_MS);
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
        reg.link_up(1, A, ZOMBIE_TIMEOUT_MS);
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
        reg.link_up(3, A, ZOMBIE_TIMEOUT_MS);
        assert_eq!(reg.peer_count(), 1, "two links, one peer");
        reg.link_up(0, B, ZOMBIE_TIMEOUT_MS);
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
        reg.link_up(3, A, ZOMBIE_TIMEOUT_MS);
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
