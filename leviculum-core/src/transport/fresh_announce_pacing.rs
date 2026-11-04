//! Own announces on a fresh interface, spaced so a Python peer learns every
//! destination (order 1320).
//!
//! A child of `transport.rs`, so the pacer reaches the interface tables and
//! the action list without either being opened up.
//!
//! ## The peer behaviour this answers
//!
//! A Python node holds an announce for a destination it has no path to when
//! the receiving interface `should_ingress_limit()` (`RNS/Transport.py:1695-1706`).
//! An interface younger than `IC_NEW_TIME` (two hours) limits above
//! `IC_BURST_FREQ_NEW` = 3 Hz, and the frequency is `n / span` over the
//! arrival times once more than `IC_DEQUE_MIN_SAMPLE` = 2 samples are in
//! (`RNS/Interfaces/Interface.py:75-84, 145-165, 279-288`). A connection
//! Python spawned just now is such an interface, so the third announce we
//! put on it inside one second sets a burst, and a quiet connection never
//! collects the `IC_BURST_MIN_SAMPLES` = 6 samples that end it: the held
//! destination stays unknown (order 1317, the facade test
//! `a_file_travels_both_ways_with_python_lxmf`).
//!
//! This is a peer's quirk, not a medium's, and the announce cadence lives in
//! the core, so the pacing is here and not in an interface.
//!
//! ## The rule
//!
//! Every announce this node originates (hops 0) on an interface younger than
//! two hours leaves at least [`FRESH_ANNOUNCE_SPACING_MS`] after the previous
//! one on that interface. The first goes at once; the others wait in a
//! per-interface queue, in the order they were made, and the poll releases
//! them one per spacing. A broadcast reaches every other interface at once and
//! leaves the waiting interface out (`exclude_ifaces`), which gets its copy
//! from the queue. One slot per destination and peer: a newer emission of a
//! waiting destination replaces the bytes in place, as the announce-cap queue
//! does.
//!
//! The local IPC socket is not paced in either direction, toward a client
//! of ours or toward the shared instance we are a client of: the reference's
//! local interfaces never ingress-limit (`LocalInterface.py:137-138`), and a
//! client that announces thousands of destinations through its instance
//! (`lnpnd/tests/status_under_transport_load.rs`) must not wait a spacing
//! for each.
//!
//! ## The paths (orders 1320, 1358)
//!
//! Which announces a fresh interface that is not the IPC socket paces. The
//! gate is "this node speaks for the destination": hops 0 when the announce
//! leaves.
//!
//! | Path | Sender | Paced |
//! |---|---|---|
//! | own announce, broadcast | `send_on_all_interfaces` | yes |
//! | own announce, one interface or peer (#376) | `send_own_announce_on_interface` | yes |
//! | interface-up re-announce, own destinations and cached client announces | `Node::announce_local_destinations` | yes |
//! | own announce's second emission | scheduler, `forward_on_all`, hops 0 | yes |
//! | a local client's announce, forwarded | scheduler, `forward_on_all`, hops 0 | yes |
//! | a local client's path response for a pending request (#171) | scheduler, `forward_on_all`, hops 0 | yes |
//! | an announce relayed from a network interface | scheduler, `forward_on_all` or `broadcast_announce_with_caps`, hops above 0 | no |
//! | a targeted path response | scheduler, `push_announce` | no |
//! | anything on the IPC socket, either direction | | no |
//!
//! A local client's announce counts as the node's own because the Python
//! peer cannot tell them apart: it counts every arrival on its interface.
//! The core treats it so already: `incoming_hop_count` undoes the IPC hop,
//! the announce table stores it at hops 0, and its one emission meets the
//! hops-0 gate in `forward_on_all` (pins in `transport.rs`,
//! `local_client_announces_are_paced_on_a_fresh_network_interface`, and the
//! Python mvr `local_clients_announces_on_a_fresh_interface_reach_python`).
//!
//! Relayed announces (hops above 0) are not paced, though the reference's
//! ingress control counts them the same and would hold a third unknown
//! destination inside a second on a fresh interface. A Python transport node
//! relays a burst as it comes and its peer holds it the same way; holding
//! transit traffic here would make this node the slower relay, and a held
//! relayed destination is still found by path request. A targeted path
//! response answers a request: the requester has it in `path_requests` and
//! skips the ingress check for it (`RNS/Transport.py:1701`).
//!
//! Deviation rule: the wire is unchanged (the same announce bytes), the
//! semantics improve (a Python peer learns every destination, not the first
//! two), and the delay is at most one spacing per own destination.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

use super::{Transport, IC_NEW_TIME_MS, TRUNCATED_HASHBYTES};
use crate::hex_fmt::HexShort;
use crate::traits::{Clock, Storage};

/// The least time between two own announces on a fresh interface.
///
/// Python holds an unknown destination once `n / span` exceeds
/// `IC_BURST_FREQ_NEW` = 3 Hz over more than two samples
/// (`RNS/Interfaces/Interface.py:76, 82, 279-288`). With a uniform spacing
/// `d` the k-th announce arrives `(k - 1) d` after the first and must not be
/// above `k / 3` Hz, which is tightest at k = 3: `d` >= 500 ms. 500 ms sits
/// on the boundary itself (three samples over exactly one second measure
/// 3 Hz, and any jitter that brings the first closer to the third holds the
/// third), so the spacing keeps 100 ms beside it: three announces span
/// 1.2 s and measure 2.5 Hz, and every longer run measures less.
pub(crate) const FRESH_ANNOUNCE_SPACING_MS: u64 = 600;

/// The pacers of all interfaces, sorted by interface id.
///
/// A vec and not a `BTreeMap`: a board has at most three interfaces, and a
/// map with this value type was a set of B-tree monomorphs of its own, about
/// 1 KiB of the Pocket's flash (order 1326). Kept sorted, so every walk visits
/// the interfaces in the order the map did, and a lookup is a binary search
/// for the hosts with many spawned interfaces.
pub(crate) type Pacers = Vec<(usize, FreshAnnouncePacer)>;

/// The index of `id` in `pacers`, or where it would be inserted.
fn slot(pacers: &Pacers, id: usize) -> Result<usize, usize> {
    pacers.binary_search_by_key(&id, |(i, _)| *i)
}

/// One own announce waiting for its turn on a fresh interface.
#[derive(Debug, Clone)]
pub(crate) struct PacedAnnounce {
    /// The announce as it goes on the wire.
    pub raw: Vec<u8>,
    /// The announced destination, the slot's key with `peer`.
    pub dst: [u8; TRUNCATED_HASHBYTES],
    /// The peer behind the interface it is addressed to (#376), `None` for
    /// every live link.
    pub peer: Option<[u8; TRUNCATED_HASHBYTES]>,
}

/// The pacing state of one interface.
///
/// Created when the interface registers (`set_interface_name`), restarted
/// when it comes up again (`handle_interface_up`, a reconnect is a new
/// spawned interface at the peer), removed with the interface's other
/// announce bookkeeping (`remove_announce_freq_tracking`).
#[derive(Debug, Clone)]
pub(crate) struct FreshAnnouncePacer {
    /// When the interface came up; it is fresh for `IC_NEW_TIME_MS` from here.
    up_ms: u64,
    /// The earliest time the next own announce may leave on it.
    next_ms: u64,
    /// Own announces waiting, oldest first.
    pub(crate) queue: VecDeque<PacedAnnounce>,
}

impl FreshAnnouncePacer {
    fn new(now_ms: u64) -> Self {
        Self {
            up_ms: now_ms,
            next_ms: now_ms,
            queue: VecDeque::new(),
        }
    }

    fn fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.up_ms) < IC_NEW_TIME_MS
    }

    /// Whether an announce may leave now; when it may, the next one waits a
    /// spacing from now. Anything already queued goes first.
    fn admit(&mut self, now_ms: u64) -> bool {
        if !self.fresh(now_ms) && self.queue.is_empty() {
            return true;
        }
        if self.queue.is_empty() && now_ms >= self.next_ms {
            self.next_ms = now_ms + FRESH_ANNOUNCE_SPACING_MS;
            return true;
        }
        false
    }

    /// Queue an announce the interface could not take now, in the slot its
    /// destination and peer already hold if they hold one.
    fn enqueue(&mut self, announce: PacedAnnounce) {
        match self
            .queue
            .iter_mut()
            .find(|w| w.dst == announce.dst && w.peer == announce.peer)
        {
            Some(waiting) => waiting.raw = announce.raw,
            None => self.queue.push_back(announce),
        }
    }

    /// The next announce due at `now_ms`, if one is.
    fn release(&mut self, now_ms: u64) -> Option<PacedAnnounce> {
        if now_ms < self.next_ms {
            return None;
        }
        let next = self.queue.pop_front()?;
        self.next_ms = now_ms + FRESH_ANNOUNCE_SPACING_MS;
        Some(next)
    }
}

impl<C: Clock, S: Storage> Transport<C, S> {
    /// Start pacing `id` if it has no pacer yet (its registration).
    pub(super) fn register_fresh_pacer(&mut self, id: usize) {
        let now = self.clock.now_ms();
        let pacers = self.fresh_announce_pacers.get_or_insert_with(Box::default);
        if let Err(at) = slot(pacers, id) {
            pacers.insert(at, (id, FreshAnnouncePacer::new(now)));
        }
    }

    /// Restart `id`'s fresh window: the interface came up again, and the
    /// peer on the other side counts announces on a new interface from now.
    /// What is still queued keeps its order and its spacing.
    pub fn mark_interface_fresh(&mut self, id: usize) {
        let now = self.clock.now_ms();
        let pacers = self.fresh_announce_pacers.get_or_insert_with(Box::default);
        match slot(pacers, id) {
            Ok(at) => pacers[at].1.up_ms = now,
            Err(at) => pacers.insert(at, (id, FreshAnnouncePacer::new(now))),
        }
    }

    /// Forget `id`'s pacer and what waits in it (the interface is gone).
    pub(super) fn remove_fresh_pacer(&mut self, id: usize) {
        if let Some(pacers) = self.fresh_announce_pacers.as_mut() {
            if let Ok(at) = slot(pacers, id) {
                pacers.remove(at);
            }
            if pacers.is_empty() {
                self.fresh_announce_pacers = None;
            }
        }
    }

    /// Whether an own announce of `dst` may leave on `iface` now. When it
    /// may not, it is queued for the poll to release, and the caller must
    /// not send it.
    pub(super) fn admit_own_announce(
        &mut self,
        iface: usize,
        raw: &[u8],
        dst: [u8; TRUNCATED_HASHBYTES],
        peer: Option<[u8; TRUNCATED_HASHBYTES]>,
    ) -> bool {
        if self.is_local_client(iface) || self.shared_instance_interface == Some(iface) {
            return true;
        }
        let now = self.clock.now_ms();
        let Some(pacer) = self.fresh_announce_pacers.as_mut().and_then(|p| {
            let at = slot(p, iface).ok()?;
            Some(&mut p[at].1)
        }) else {
            return true;
        };
        if pacer.admit(now) {
            return true;
        }
        pacer.enqueue(PacedAnnounce {
            raw: raw.to_vec(),
            dst,
            peer,
        });
        let release_ms = pacer.next_ms;
        crate::tracing::debug!(
            event = "ANN_PACED",
            dst = %HexShort(&dst),
            iface = %self.iface_name(iface),
            release_ms = release_ms,
        );
        false
    }

    /// Put the own announce `raw` of `dst` on one interface, now or, on a
    /// fresh interface that took one less than a spacing ago, from its queue
    /// (order 1320). For the senders that address one interface: the
    /// interface-up re-announce and a targeted announce. The caller has
    /// recorded the packet hash for echo dedup, as for any
    /// `send_on_interface_to_peer`.
    pub fn send_own_announce_on_interface(
        &mut self,
        iface: usize,
        raw: &[u8],
        dst: [u8; TRUNCATED_HASHBYTES],
        peer: Option<[u8; TRUNCATED_HASHBYTES]>,
    ) {
        if self.admit_own_announce(iface, raw, dst, peer) {
            let _ = self.send_on_interface_to_peer(iface, raw, peer);
        }
    }

    /// The interfaces among those a broadcast of the own announce `raw`
    /// would reach that must not take it now; each has queued its copy.
    /// `skip` are the interfaces the broadcast leaves out already.
    pub(super) fn paced_broadcast_exclusions(
        &mut self,
        raw: &[u8],
        dst: [u8; TRUNCATED_HASHBYTES],
        skip: &[usize],
    ) -> Vec<usize> {
        let paced: Vec<usize> = self
            .fresh_announce_pacers
            .iter()
            .flat_map(|p| p.iter())
            .map(|(id, _)| *id)
            .filter(|id| !skip.contains(id))
            .collect();
        paced
            .into_iter()
            .filter(|&id| !self.admit_own_announce(id, raw, dst, None))
            .collect()
    }

    /// Release every queued own announce that is due (the poll's step).
    pub(super) fn drain_fresh_announce_queues(&mut self, now: u64) {
        let mut sends = Vec::new();
        for (iface, pacer) in self
            .fresh_announce_pacers
            .iter_mut()
            .flat_map(|p| p.iter_mut())
        {
            if let Some(next) = pacer.release(now) {
                sends.push((*iface, next));
            }
        }
        for (iface, next) in sends {
            crate::tracing::debug!(
                event = "ANN_TX",
                occasion = super::AnnounceTxOccasion::Local.as_str(),
                dst = %HexShort(&next.dst),
                hops = 0u8,
                iface = %self.iface_name(iface),
            );
            self.push_packet(iface, next.raw, None, next.peer);
        }
    }

    /// How many own announces wait in the fresh-interface queues.
    #[cfg(test)]
    pub(crate) fn fresh_announces_waiting(&self) -> usize {
        self.fresh_announce_pacers
            .iter()
            .flat_map(|p| p.iter())
            .map(|(_, p)| p.queue.len())
            .sum()
    }

    /// When the next queued own announce falls due, if one waits.
    pub(super) fn fresh_announce_deadline(&self) -> Option<u64> {
        self.fresh_announce_pacers
            .iter()
            .flat_map(|p| p.iter())
            .filter(|(_, p)| !p.queue.is_empty())
            .map(|(_, p)| p.next_ms)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::memory_storage::MemoryStorage;
    use crate::test_utils::{MockClock, TEST_TIME_MS};
    use crate::transport::TransportConfig;
    use rand_core::OsRng;

    fn transport_with(iface: usize) -> Transport<MockClock, MemoryStorage> {
        let mut t = Transport::new(
            TransportConfig::default(),
            MockClock::new(TEST_TIME_MS),
            MemoryStorage::with_defaults(),
            Identity::generate(&mut OsRng),
        );
        t.set_interface_name(iface, alloc::string::String::from("fresh"));
        t
    }

    /// A network interface just registered takes one own announce and
    /// queues the next.
    #[test]
    fn a_fresh_network_interface_queues_the_second_announce() {
        let mut t = transport_with(3);
        assert!(t.admit_own_announce(3, &[1], [1; TRUNCATED_HASHBYTES], None));
        assert!(!t.admit_own_announce(3, &[2], [2; TRUNCATED_HASHBYTES], None));
        assert_eq!(t.fresh_announces_waiting(), 1);
    }

    /// The IPC uplink to a shared instance is never paced: the instance's
    /// local interface does not ingress-limit (`LocalInterface.py:137-138`).
    #[test]
    fn the_uplink_to_a_shared_instance_is_not_paced() {
        let mut t = transport_with(3);
        t.set_shared_instance_interface(Some(3));
        for dst in 0..10u8 {
            assert!(t.admit_own_announce(3, &[dst], [dst; TRUNCATED_HASHBYTES], None));
        }
        assert_eq!(t.fresh_announces_waiting(), 0);
    }

    /// Nor is a local client's interface on the instance's side.
    #[test]
    fn a_local_clients_interface_is_not_paced() {
        let mut t = transport_with(3);
        t.set_local_client(3, true);
        for dst in 0..10u8 {
            assert!(t.admit_own_announce(3, &[dst], [dst; TRUNCATED_HASHBYTES], None));
        }
        assert_eq!(t.fresh_announces_waiting(), 0);
    }

    /// The table stays sorted by id whatever order interfaces come and go
    /// in, so the poll walks them in the order the map it replaced did.
    #[test]
    fn the_pacer_table_stays_sorted_by_interface() {
        let mut t = transport_with(5);
        t.register_fresh_pacer(2);
        t.register_fresh_pacer(7);
        t.register_fresh_pacer(5);
        t.mark_interface_fresh(1);
        let ids = |t: &Transport<MockClock, MemoryStorage>| -> Vec<usize> {
            t.fresh_announce_pacers
                .iter()
                .flat_map(|p| p.iter())
                .map(|(id, _)| *id)
                .collect()
        };
        assert_eq!(ids(&t), alloc::vec![1, 2, 5, 7]);
        t.remove_fresh_pacer(5);
        t.remove_fresh_pacer(4);
        assert_eq!(ids(&t), alloc::vec![1, 2, 7]);
        for id in [1, 2, 7] {
            t.remove_fresh_pacer(id);
        }
        assert!(t.fresh_announce_pacers.is_none());
    }

    fn announce(dst: u8) -> PacedAnnounce {
        PacedAnnounce {
            raw: alloc::vec![dst],
            dst: [dst; TRUNCATED_HASHBYTES],
            peer: None,
        }
    }

    #[test]
    fn the_first_goes_at_once_and_the_next_waits_a_spacing() {
        let mut p = FreshAnnouncePacer::new(1_000);
        assert!(p.admit(1_000));
        assert!(!p.admit(1_000));
        p.enqueue(announce(2));
        assert!(p.release(1_000 + FRESH_ANNOUNCE_SPACING_MS - 1).is_none());
        assert_eq!(
            p.release(1_000 + FRESH_ANNOUNCE_SPACING_MS)
                .map(|a| a.dst[0]),
            Some(2)
        );
    }

    #[test]
    fn a_queue_keeps_its_order_and_a_new_announce_waits_behind_it() {
        let mut p = FreshAnnouncePacer::new(0);
        assert!(p.admit(0));
        p.enqueue(announce(2));
        p.enqueue(announce(3));
        // Due by the clock, but two are waiting: the newcomer queues too.
        assert!(!p.admit(10 * FRESH_ANNOUNCE_SPACING_MS));
    }

    #[test]
    fn a_newer_emission_takes_its_destinations_slot() {
        let mut p = FreshAnnouncePacer::new(0);
        p.enqueue(announce(2));
        p.enqueue(announce(3));
        p.enqueue(PacedAnnounce {
            raw: alloc::vec![9],
            ..announce(2)
        });
        assert_eq!(p.queue.len(), 2);
        assert_eq!(p.queue[0].raw, alloc::vec![9]);
    }

    /// The rule against Python's arithmetic: no run of k >= 3 paced arrivals
    /// measures above 3 Hz (`n / span`, Interface.py:279-288).
    #[test]
    fn no_paced_run_measures_above_three_hertz() {
        for k in 3u64..=20 {
            let span_ms = (k - 1) * FRESH_ANNOUNCE_SPACING_MS;
            assert!(k * 1000 <= 3 * span_ms, "k={k} span={span_ms} ms");
        }
    }

    #[test]
    fn an_interface_two_hours_old_is_not_paced() {
        let mut p = FreshAnnouncePacer::new(0);
        assert!(p.admit(IC_NEW_TIME_MS));
        assert!(p.admit(IC_NEW_TIME_MS));
    }
}
