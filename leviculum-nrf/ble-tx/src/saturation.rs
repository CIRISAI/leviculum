//! What a saturated board drops, answers and ends (leviculum#413, census
//! tier B: rows 82, 87, 92, 107, 114, 123, 131 and 138).
//!
//! A stalled phone and a burst on four links fill the board's bounded
//! queues, and each queue's full arm used to decide inline, with its own
//! rule and in places without a trace. Under that load (#388 was a heap
//! failure under it) these arms decide what is lost first, so the rules
//! live here, one function per queue class, and the order is written
//! down once:
//!
//! 1. **A stalled peer's own link queue** fills first: the fan-out keeps
//!    feeding it while its pump cannot drain. The arriving packet is
//!    dropped on that link alone ([`link_queue_verdict`]); the healthy
//!    links keep theirs, and the session stays up, because a stall is
//!    the liveness clock's to judge, not the queue's.
//! 2. **The core's outbound queue** fills only when the fan-out task
//!    itself cannot run. The core is answered `BufferFull`
//!    ([`outbound_verdict`]), the one queue that pushes back, so the
//!    packet is the core's to retry or drop by its own rules.
//! 3. **The inbound queue** fills when the main loop stalls behind a
//!    burst from several links. A completed packet is dropped and counted
//!    ([`incoming_verdict`]); the peer is not told, as on any lossy
//!    carrier.
//! 4. **The peer-event queue** fills only past eight transitions the main
//!    loop has not read: a carrier-off teardown of four links plus their
//!    four relinks fits exactly. The arriving event is dropped, the oldest
//!    pending wins ([`peer_event_verdict`]).
//! 5. **The drain table** refuses a connection only while a torn-down
//!    link's slot is still held: the link ends on either role with no
//!    dial failure booked ([`drain_table_refusal`]).
//!
//! No arm here ends a live session: the board sheds packets and events,
//! never a link that is still answering. Only the refused claim ends a
//! connection, and that one never became a session.

use crate::{DialFailure, LinkRole};

/// A bounded queue's occupancy when an item arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fill {
    /// Items queued now.
    pub len: usize,
    /// The queue's depth.
    pub capacity: usize,
}

impl Fill {
    #[must_use]
    pub const fn new(len: usize, capacity: usize) -> Self {
        Self { len, capacity }
    }

    /// A queue at its depth: the next item does not fit.
    #[must_use]
    pub const fn full(capacity: usize) -> Self {
        Self::new(capacity, capacity)
    }

    #[must_use]
    pub const fn is_full(self) -> bool {
        self.len >= self.capacity
    }
}

/// What a queue does with an arriving item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueue {
    /// Queue it.
    Queue,
    /// Drop the arriving item; what is queued stays.
    DropArriving,
}

impl Enqueue {
    fn of(fill: Fill) -> Self {
        if fill.is_full() {
            Self::DropArriving
        } else {
            Self::Queue
        }
    }
}

/// Which way a peer transition went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerEventKind {
    /// The identity's first link came up.
    Up,
    /// The identity's last link went down.
    Lost,
}

/// A peer transition arriving at the main loop's event queue (census row
/// 82).
///
/// The oldest pending report wins, whichever way either went: a flap is
/// a `Lost` then an `Up` on one ordered queue, and dropping a queued
/// report to make room would reorder what is left. A dropped `Lost` ages
/// its paths out through ordinary expiry, a dropped `Up` waits for the
/// peer's periodic announce. The firmware logs each drop as
/// `BLE_PEER_EVENT_DROPPED`.
#[must_use]
pub fn peer_event_verdict(fill: Fill, kind: PeerEventKind) -> Enqueue {
    match kind {
        PeerEventKind::Up | PeerEventKind::Lost => Enqueue::of(fill),
    }
}

/// How a packet reaches a link's outbound queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkItem {
    /// The fan-out, from the core's outbound queue.
    Fanout,
    /// A displacement moving the old link's queue onto the new link.
    Moved,
}

/// A packet arriving at one link's outbound queue (census rows 87 and
/// 123).
///
/// The fan-out and a displacement's move answer alike: the arriving
/// packet is dropped on that link and the session goes on. The fan-out
/// logs it as `BLE_TX_FANOUT_DROP`, the displacement in its own line's
/// `dropped=`.
#[must_use]
pub fn link_queue_verdict(fill: Fill, item: LinkItem) -> Enqueue {
    match item {
        LinkItem::Fanout | LinkItem::Moved => Enqueue::of(fill),
    }
}

/// What the interface answers the core for one outbound packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outbound {
    /// The carrier is off: `Ok`, and the packet is counted in the
    /// carrier's drop run, not queued.
    Swallow,
    /// Queue it for the fan-out.
    Queue,
    /// The queue is full: `BufferFull`, with an `IFACE_FULL` line.
    BufferFull,
}

/// One outbound packet from the core (census rows 91 and 92).
///
/// The carrier comes first: a switched-off carrier swallows the packet
/// as `Ok` however full the queue is, because a media profile is the
/// operator's choice and not a fault the core should retry against.
/// With the carrier on, a full queue answers `BufferFull`.
#[must_use]
pub fn outbound_verdict(carrier: crate::CarrierState, fill: Fill) -> Outbound {
    if carrier.step(crate::CarrierPath::Outbound) != crate::CarrierStep::Proceed {
        return Outbound::Swallow;
    }
    if fill.is_full() {
        Outbound::BufferFull
    } else {
        Outbound::Queue
    }
}

/// A completed inbound packet arriving at the main loop's queue (census
/// rows 114 and 138, the peripheral's and the central's).
///
/// Both roles drop it when the queue is full: the receive path runs
/// inside the SoftDevice's event handler and cannot wait. The drop was
/// silent until this order; the firmware counts it in the
/// `BLE_COUNTERS` line's `rx_full=`.
#[must_use]
pub fn incoming_verdict(fill: Fill) -> Enqueue {
    Enqueue::of(fill)
}

/// What a connection the drain table refused does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimRefused {
    /// The link is disconnected.
    pub disconnects: bool,
    /// The dial failure booked against the peer's address, if any.
    pub books: Option<DialFailure>,
}

/// A connection the drain table has no slot for (census rows 107 and
/// 131).
///
/// `MAX_LINKS` slots cover every connection the SoftDevice accepts, so
/// the table refuses only while a torn-down link's slot is still held
/// by a session that has not yet run its teardown. The link ends on
/// either role: the central disconnects it, and until this order the
/// peripheral left that to the connection's drop, which disconnects the
/// same link a moment later. Nothing is booked against the peer, whose
/// address did nothing wrong.
#[must_use]
pub fn drain_table_refusal(role: LinkRole) -> ClaimRefused {
    match role {
        LinkRole::Central | LinkRole::Peripheral => ClaimRefused {
            disconnects: true,
            books: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CarrierState;
    use leviculum_heap_budget::{MAX_LINKS, QUEUE_DEPTH};

    /// The depth of the main loop's peer-event queue.
    const PEER_EVENT_DEPTH: usize = 8;

    /// A queue the test drives through a verdict function, counting what
    /// it took and what it dropped.
    struct Queue {
        len: usize,
        depth: usize,
        dropped: usize,
    }

    impl Queue {
        fn new(depth: usize) -> Self {
            Self {
                len: 0,
                depth,
                dropped: 0,
            }
        }

        fn fill(&self) -> Fill {
            Fill::new(self.len, self.depth)
        }

        fn offer(&mut self, verdict: Enqueue) {
            match verdict {
                Enqueue::Queue => self.len += 1,
                Enqueue::DropArriving => self.dropped += 1,
            }
        }
    }

    #[test]
    fn a_stalled_peer_loses_packets_on_its_own_link_and_the_healthy_links_lose_none() {
        // Four links, the phone on slot 0 has stopped draining, the
        // other three drain one packet per flood. A burst of twelve
        // floods.
        let mut links: Vec<Queue> = (0..MAX_LINKS).map(|_| Queue::new(QUEUE_DEPTH)).collect();
        let burst = 12;
        for _ in 0..burst {
            for link in &mut links {
                let verdict = link_queue_verdict(link.fill(), LinkItem::Fanout);
                link.offer(verdict);
            }
            for link in links.iter_mut().skip(1) {
                link.len -= 1;
            }
        }
        assert_eq!(links[0].len, QUEUE_DEPTH);
        assert_eq!(links[0].dropped, burst - QUEUE_DEPTH);
        for link in links.iter().skip(1) {
            assert_eq!(link.dropped, 0);
        }
    }

    #[test]
    fn a_displacement_into_a_full_queue_drops_what_does_not_fit_as_the_fanout_does() {
        // The old link held a full queue; the new link has already been
        // fed two packets by the fan-out.
        let mut new_link = Queue::new(QUEUE_DEPTH);
        new_link.len = 2;
        for _ in 0..QUEUE_DEPTH {
            let verdict = link_queue_verdict(new_link.fill(), LinkItem::Moved);
            new_link.offer(verdict);
        }
        assert_eq!(new_link.len, QUEUE_DEPTH);
        assert_eq!(new_link.dropped, 2);
        for len in 0..=QUEUE_DEPTH {
            let fill = Fill::new(len, QUEUE_DEPTH);
            assert_eq!(
                link_queue_verdict(fill, LinkItem::Moved),
                link_queue_verdict(fill, LinkItem::Fanout)
            );
        }
    }

    #[test]
    fn a_full_outbound_queue_answers_buffer_full_and_a_dark_carrier_answers_ok_first() {
        let on = CarrierState::from_active(true);
        let off = CarrierState::from_active(false);
        assert_eq!(
            outbound_verdict(on, Fill::new(QUEUE_DEPTH - 1, QUEUE_DEPTH)),
            Outbound::Queue
        );
        assert_eq!(
            outbound_verdict(on, Fill::full(QUEUE_DEPTH)),
            Outbound::BufferFull
        );
        for len in 0..=QUEUE_DEPTH {
            assert_eq!(
                outbound_verdict(off, Fill::new(len, QUEUE_DEPTH)),
                Outbound::Swallow
            );
        }
    }

    #[test]
    fn a_stalled_main_loop_drops_the_inbound_burst_past_the_queue_and_counts_it() {
        // Every link completes two packets while the main loop reads
        // nothing; both roles' receive paths ask the same function.
        let mut incoming = Queue::new(QUEUE_DEPTH);
        let mut rx_full = 0usize;
        for _ in 0..2 {
            for _ in 0..MAX_LINKS {
                let verdict = incoming_verdict(incoming.fill());
                if verdict == Enqueue::DropArriving {
                    rx_full += 1;
                }
                incoming.offer(verdict);
            }
        }
        assert_eq!(incoming.len, QUEUE_DEPTH);
        assert_eq!(rx_full, 2 * MAX_LINKS - QUEUE_DEPTH);
        assert_eq!(rx_full, incoming.dropped);
    }

    #[test]
    fn a_carrier_off_teardown_and_its_relinks_fit_the_peer_event_queue_and_the_next_is_dropped() {
        let mut events = Queue::new(PEER_EVENT_DEPTH);
        for kind in [PeerEventKind::Lost, PeerEventKind::Up] {
            for _ in 0..MAX_LINKS {
                let verdict = peer_event_verdict(events.fill(), kind);
                events.offer(verdict);
            }
        }
        assert_eq!(events.dropped, 0);
        for kind in [PeerEventKind::Lost, PeerEventKind::Up] {
            assert_eq!(
                peer_event_verdict(events.fill(), kind),
                Enqueue::DropArriving,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_refused_drain_claim_ends_the_link_on_both_roles_and_books_nothing() {
        for role in [LinkRole::Central, LinkRole::Peripheral] {
            assert_eq!(
                drain_table_refusal(role),
                ClaimRefused {
                    disconnects: true,
                    books: None,
                },
                "{role:?}"
            );
        }
    }

    #[test]
    fn a_queue_below_its_depth_takes_every_class() {
        let fill = Fill::new(QUEUE_DEPTH - 1, QUEUE_DEPTH);
        assert_eq!(incoming_verdict(fill), Enqueue::Queue);
        assert_eq!(link_queue_verdict(fill, LinkItem::Fanout), Enqueue::Queue);
        assert_eq!(link_queue_verdict(fill, LinkItem::Moved), Enqueue::Queue);
        assert_eq!(
            peer_event_verdict(
                Fill::new(PEER_EVENT_DEPTH - 1, PEER_EVENT_DEPTH),
                PeerEventKind::Up
            ),
            Enqueue::Queue
        );
    }
}
