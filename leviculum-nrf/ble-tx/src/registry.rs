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
//! report neither; since #376 it additionally DISPLACES the old link:
//! [`PeerRegistry::link_up`] names the old slot and the caller tears
//! that connection down, so the peer keeps exactly its newest link.
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
}

/// What registering a link amounted to (see [`PeerRegistry::link_up`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkUp {
    /// The identity's FIRST live link — report a peer arrival.
    pub first: bool,
    /// The slot of the identity's OLD link, which the caller must tear
    /// down: the newest connection wins (#376).
    pub displaced: Option<usize>,
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
    /// address this check cannot know; that residual connection is
    /// resolved post-connect by [`link_up`](Self::link_up)'s identity
    /// duplicate rule, which since #376 displaces the OLD link rather
    /// than refusing the new one.
    pub fn addr_linked(&self, addr_value: u64) -> bool {
        self.addrs.iter().flatten().any(|a| *a == addr_value)
    }

    /// Register a slot's peer.
    ///
    /// `first` iff this is the identity's FIRST live link — the caller
    /// reports a peer arrival exactly then. `displaced` names the slot
    /// of the identity's OLD link, if one exists (#376): a second
    /// connection from an identity we already hold means the peer moved
    /// to the new connection, so the caller keeps THIS link and tears
    /// the old one down. Neither edge of that hand-over is a peer
    /// transition — the peer was never gone. Re-registering the same
    /// slot displaces nothing.
    pub fn link_up(&mut self, slot: usize, peer: [u8; 16]) -> LinkUp {
        let first = self.slots.iter().flatten().all(|id| *id != peer);
        let displaced = self
            .slots
            .iter()
            .position(|id| *id == Some(peer))
            .filter(|old| *old != slot);
        self.slots[slot] = Some(peer);
        LinkUp { first, displaced }
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

    /// The number of DISTINCT live peer identities (Codeberg #365) —
    /// the value the main loop mirrors into the core as the
    /// interface's peer count. Distinct, not per-slot: during the
    /// displacement hand-over (#376) one peer briefly holds two links,
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

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 16] = [0xaa; 16];
    const B: [u8; 16] = [0xbb; 16];

    #[test]
    fn the_first_link_of_an_identity_is_an_arrival_and_displaces_nothing() {
        let mut reg = PeerRegistry::<4>::new();
        assert_eq!(
            reg.link_up(0, A),
            LinkUp {
                first: true,
                displaced: None
            }
        );
        assert_eq!(
            reg.link_up(1, B),
            LinkUp {
                first: true,
                displaced: None
            },
            "a different identity is its own arrival, no displacement"
        );
    }

    #[test]
    fn a_second_link_of_the_same_identity_is_churn_and_displaces_the_old() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A);
        // The peer's rotated address reconnected while the old link was
        // still up: churn (no arrival report), and the OLD slot is
        // named for teardown — the newest connection wins (#376).
        assert_eq!(
            reg.link_up(1, A),
            LinkUp {
                first: false,
                displaced: Some(0)
            }
        );
    }

    /// The hand-over sequence end to end: after a displacement the old
    /// slot's teardown is churn (the identity still owns the new link),
    /// and only the new link's death is the peer loss.
    #[test]
    fn displacement_teardown_of_the_old_slot_is_churn_not_a_loss() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A);
        assert_eq!(reg.link_up(1, A).displaced, Some(0));
        assert_eq!(reg.link_down(0), None, "old link's death is churn");
        assert!(reg.is_linked(&A), "the new link carries the peer");
        assert_eq!(reg.link_down(1), Some(A), "new link's death is the loss");
    }

    /// A same-slot re-registration must not name its own slot: the
    /// caller would tear down the very connection it just kept.
    #[test]
    fn re_registering_the_same_slot_displaces_nothing() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A);
        assert_eq!(
            reg.link_up(0, A),
            LinkUp {
                first: false,
                displaced: None
            }
        );
    }

    #[test]
    fn the_last_link_of_an_identity_is_a_loss() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A);
        assert_eq!(reg.link_down(0), Some(A));
    }

    #[test]
    fn a_non_last_link_down_is_churn_not_a_loss() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A);
        reg.link_up(1, A);
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
        reg.link_up(0, A);
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
        reg.link_up(0, A);
        reg.link_up(2, B);
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
        reg.link_up(0, A);
        reg.link_up(1, A);
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
            assert!(reg.link_up(slot, id).first, "each identity's first link");
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
        assert!(reg.link_up(1, A).first);
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
        reg.link_up(1, A);
        assert_eq!(reg.peer_count(), 1);
        // The displacement hand-over: same identity on a second slot.
        reg.link_up(3, A);
        assert_eq!(reg.peer_count(), 1, "two links, one peer");
        reg.link_up(0, B);
        assert_eq!(reg.peer_count(), 2);
        reg.link_down(1);
        assert_eq!(reg.peer_count(), 2, "A still holds slot 3");
        reg.link_down(3);
        assert_eq!(reg.peer_count(), 1);
    }
}
