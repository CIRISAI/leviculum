//! Channel frames with a deadline (+ciris, leviculum#94).
//!
//! The witness from the issue: under a stalled path, frames with a 200 ms
//! deadline are dropped once expired and never delivered late, and the
//! ordered stream behind them carries on.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::{FrameExpiry, NodeCore, NodeCoreBuilder, NodeEvent};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Clock;
use crate::transport::{Action, InterfaceId, TickOutput};

type Node = NodeCore<OsRng, MockClock, crate::traits::NoStorage>;

fn add_iface(node: &mut Node, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    idx
}

fn packets(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data.clone(),
        })
        .collect()
}

struct Pair {
    a: Node,
    b: Node,
    a_if: usize,
    b_if: usize,
    link: LinkId,
}

impl Pair {
    fn new() -> Self {
        let identity = Identity::generate(&mut OsRng);
        let key = identity.ed25519_verifying().to_bytes();
        let mut b: Node = NodeCoreBuilder::new().build(
            OsRng,
            MockClock::new(TEST_TIME_MS),
            crate::traits::NoStorage,
        );
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "mvrapp",
            &["timed"],
        )
        .unwrap();
        dest.set_accepts_links(true);
        let hash = *dest.hash();
        b.register_destination(dest);
        let mut a: Node = NodeCoreBuilder::new().build(
            OsRng,
            MockClock::new(TEST_TIME_MS),
            crate::traits::NoStorage,
        );
        let b_if = add_iface(&mut b, "B");
        let a_if = add_iface(&mut a, "A");
        let (link, _, out) = a.connect(hash, &key).expect("connect");
        let mut pair = Pair {
            a,
            b,
            a_if,
            b_if,
            link,
        };
        pair.a_to_b(packets(&out));
        assert_eq!(pair.a.active_link_count(), 1);
        assert_eq!(pair.b.active_link_count(), 1);
        pair
    }

    /// Carry packets A -> B and every answer back and forth until quiet;
    /// returns B's events.
    fn a_to_b(&mut self, mut to_b: Vec<Vec<u8>>) -> Vec<NodeEvent> {
        let mut b_events = Vec::new();
        for _ in 0..16 {
            if to_b.is_empty() {
                break;
            }
            let mut to_a = Vec::new();
            for p in to_b.drain(..) {
                let out = self.b.handle_packet(InterfaceId(self.b_if), &p);
                b_events.extend(out.events.clone());
                to_a.extend(packets(&out));
            }
            for p in to_a {
                let out = self.a.handle_packet(InterfaceId(self.a_if), &p);
                to_b.extend(packets(&out));
            }
        }
        b_events
    }

    fn advance(&mut self, ms: u64) -> (Vec<Vec<u8>>, Vec<NodeEvent>) {
        for n in [&mut self.a, &mut self.b] {
            let now = n.transport().clock().now_ms();
            n.transport().clock().set(now + ms);
        }
        let _ = self.b.handle_timeout();
        let out = self.a.handle_timeout();
        (packets(&out), out.events)
    }

    /// Send a timed frame, first letting the clocks run to the channel's
    /// next pacing slot if it is not open yet.
    fn send(&mut self, data: &[u8], ttl_ms: u32) -> Vec<Vec<u8>> {
        loop {
            match self.a.send_on_link_timed(&self.link, data, ttl_ms) {
                Ok(out) => return packets(&out),
                Err(crate::node::SendError::PacingDelay { ready_at_ms }) => {
                    for n in [&mut self.a, &mut self.b] {
                        n.transport().clock().set(ready_at_ms);
                    }
                }
                Err(e) => panic!("send: {e:?}"),
            }
        }
    }
}

fn received(events: &[NodeEvent]) -> Vec<Vec<u8>> {
    events
        .iter()
        .filter_map(|e| match e {
            NodeEvent::MessageReceived {
                data, msgtype: 0, ..
            } => Some(data.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_fresh_timed_frame_is_delivered_as_the_plain_message() {
    let mut pair = Pair::new();
    let pkts = pair.send(b"frame", 200);
    let events = pair.a_to_b(pkts);
    assert_eq!(received(&events), [b"frame".to_vec()]);
}

/// The frame is lost and its deadline passes before the retransmit: the
/// sender reports it expired and sends the marker in its place; the
/// receiver consumes the marker and delivers nothing, and the next frame
/// flows.
#[test]
fn a_lost_frame_past_its_deadline_is_replaced_not_resent() {
    let mut pair = Pair::new();
    let _lost = pair.send(b"stale", 200);
    let (retx, events) = pair.advance(5_000);
    assert!(events.iter().any(|e| matches!(
        e,
        NodeEvent::ChannelMessageExpired {
            expiry: FrameExpiry::Sender,
            ..
        }
    )));
    assert!(!retx.is_empty(), "the marker goes out on the retransmit");
    let b_events = pair.a_to_b(retx);
    assert!(received(&b_events).is_empty(), "nothing stale delivered");
    assert!(b_events.iter().any(|e| matches!(
        e,
        NodeEvent::ChannelMessageExpired {
            expiry: FrameExpiry::MarkedByPeer,
            ..
        }
    )));
    let pkts = pair.send(b"next", 200);
    assert_eq!(received(&pair.a_to_b(pkts)), [b"next".to_vec()]);
}

/// The witness: a 200 ms frame queued behind a stalled one arrives, waits
/// in the reorder buffer for the gap, and is dropped when the gap fills
/// (at the retransmit) instead of being delivered late.
#[test]
fn a_frame_that_waited_behind_a_gap_is_dropped_not_delivered_late() {
    let mut pair = Pair::new();
    // Calibrate the receiver's baseline with one prompt frame.
    let pkts = pair.send(b"f0", 200);
    assert_eq!(received(&pair.a_to_b(pkts)), [b"f0".to_vec()]);

    let _gap = pair.send(b"f1", 10_000); // lost, still worth resending
    let pkts = pair.send(b"f2", 200);
    let b_events = pair.a_to_b(pkts);
    assert!(received(&b_events).is_empty(), "f2 waits behind the gap");

    let mut retx = Vec::new();
    for _ in 0..40 {
        retx = pair.advance(250).0;
        if !retx.is_empty() {
            break;
        }
    }
    assert!(!retx.is_empty(), "f1 is retransmitted");
    let b_events = pair.a_to_b(retx);
    assert_eq!(received(&b_events), [b"f1".to_vec()], "f2 is not delivered");
    assert!(b_events.iter().any(|e| matches!(
        e,
        NodeEvent::ChannelMessageExpired {
            expiry: FrameExpiry::ArrivedLate { age_ms },
            ..
        } if *age_ms > 200
    )));
}

/// A frame over the Channel limit is `TooLarge`, plain or timed, and the link
/// stays up: a size error must not read as a dead link.
#[test]
fn an_oversized_send_is_too_large_not_a_failed_link() {
    let mut pair = Pair::new();
    let huge = std::vec![0u8; 100_000];
    assert_eq!(
        pair.a.send_on_link(&pair.link, &huge).unwrap_err(),
        crate::node::SendError::TooLarge
    );
    assert_eq!(
        pair.a
            .send_on_link_timed(&pair.link, &huge, 200)
            .unwrap_err(),
        crate::node::SendError::TooLarge
    );
    assert_eq!(pair.a.active_link_count(), 1);
}
