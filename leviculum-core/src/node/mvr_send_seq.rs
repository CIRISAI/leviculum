//! mvr (+ciris): a Channel send's sequence is the one its delivery proof
//! names, across the 2^16 wrap.
//!
//!  - `send_on_link_seq` returns each envelope's sequence: consecutive, and
//!    wrapping 0xFFFE, 0xFFFF, 0, 1.
//!  - `LinkStats::delivered_through(seq)` turns true exactly when that
//!    envelope and every earlier one is proved, while later envelopes are
//!    still outstanding. The ring is send-ordered, so an out-of-order
//!    arrival proves nothing early.
//!  - the serial comparison holds on both sides of the wrap.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use super::send_seq::serial_after;
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::channel::Channel;
use crate::link::LinkId;
use crate::node::send::SendError;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, NoStorage};
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

fn add_iface(node: &mut EndpointNode, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    idx
}

fn action_data(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .collect()
}

fn deliver_all(target: &mut EndpointNode, iface: usize, packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for pkt in packets {
        out.extend(action_data(&target.handle_packet(InterfaceId(iface), &pkt)));
    }
    out
}

fn establish() -> (EndpointNode, EndpointNode, usize, usize, LinkId) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut responder =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["sendseq"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    responder.register_destination(dest);

    let mut initiator =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let r_iface = add_iface(&mut responder, "R_mesh");
    let i_iface = add_iface(&mut initiator, "I_mesh");
    let (link_id, _routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");
    let mut for_responder = action_data(&out);
    for _ in 0..8 {
        if for_responder.is_empty() {
            break;
        }
        let back = deliver_all(&mut responder, r_iface, for_responder);
        for_responder = deliver_all(&mut initiator, i_iface, back);
    }
    assert_eq!(initiator.active_link_count(), 1, "initiator link active");
    (initiator, responder, i_iface, r_iface, link_id)
}

/// Send one payload, skipping past channel pacing; its sequence and packets.
fn send_seq(node: &mut EndpointNode, link_id: &LinkId, payload: &[u8]) -> (u16, Vec<Vec<u8>>) {
    loop {
        match node.send_on_link_seq(link_id, payload) {
            Ok((seq, out)) => return (seq, action_data(&out)),
            Err(SendError::PacingDelay { ready_at_ms }) => {
                let now = node.transport().clock().now_ms();
                node.transport()
                    .clock()
                    .advance(ready_at_ms.saturating_sub(now) + 1);
            }
            Err(e) => panic!("unexpected send error: {e:?}"),
        }
    }
}

fn through(node: &EndpointNode, link_id: &LinkId, seq: u16) -> bool {
    node.link_stats(link_id)
        .expect("link held")
        .delivered_through(seq)
}

/// The returned sequences are the ones the proofs name, across the wrap, and
/// a later envelope still in flight does not hide an earlier one's proof.
#[test]
fn a_send_sequence_is_proved_by_its_own_delivery_across_the_wrap() {
    let (mut initiator, mut responder, i_iface, r_iface, link_id) = establish();
    {
        let link = initiator.links.get_mut(&link_id).expect("link");
        let rtt = link.rtt_ms();
        let ch = link.ensure_channel(rtt);
        ch.force_next_tx_sequence_for_test(0xFFFE);
        ch.set_window_bounds_for_test(8, 8, 8);
    }
    {
        // The receiver expects the same starting sequence, so 0xFFFE is new
        // to it (not an old envelope it would re-prove unseen).
        let link = responder.links.get_mut(&link_id).expect("responder link");
        let rtt = link.rtt_ms();
        link.ensure_channel(rtt)
            .set_next_rx_sequence_for_test(0xFFFE);
    }
    assert_eq!(
        initiator
            .link_stats(&link_id)
            .unwrap()
            .oldest_unacked_sequence(),
        None
    );

    let mut sent = Vec::new();
    for i in 0..4u8 {
        sent.push(send_seq(&mut initiator, &link_id, &[i; 64]));
    }
    let seqs: Vec<u16> = sent.iter().map(|(s, _)| *s).collect();
    assert_eq!(seqs, [0xFFFE, 0xFFFF, 0, 1], "consecutive across the wrap");
    assert_eq!(
        initiator.link_stats(&link_id).unwrap().last_sent_sequence(),
        Some(1),
        "the stats name the newest send"
    );
    assert_eq!(
        initiator
            .link_stats(&link_id)
            .unwrap()
            .oldest_unacked_sequence(),
        Some(0xFFFE)
    );
    for s in &seqs {
        assert!(!through(&initiator, &link_id, *s), "{s:#x} not yet proved");
    }

    // The first envelope only: it, and nothing after it, is proved.
    let proofs = deliver_all(&mut responder, r_iface, sent[0].1.clone());
    let _ = deliver_all(&mut initiator, i_iface, proofs);
    assert!(through(&initiator, &link_id, 0xFFFE));
    assert!(!through(&initiator, &link_id, 0xFFFF));
    assert!(!through(&initiator, &link_id, 0));

    // Sequence 0 arrives before 0xFFFF: the receiver holds it out of order
    // and proves nothing yet, so 0 is not delivered through.
    let early = deliver_all(&mut responder, r_iface, sent[2].1.clone());
    let _ = deliver_all(&mut initiator, i_iface, early);
    assert!(!through(&initiator, &link_id, 0), "0xFFFF is still missing");

    // 0xFFFF lands: both it and the 0 it unblocks are proved, 1 is not.
    let proofs = deliver_all(&mut responder, r_iface, sent[1].1.clone());
    let _ = deliver_all(&mut initiator, i_iface, proofs);
    assert!(through(&initiator, &link_id, 0xFFFF));
    assert!(through(&initiator, &link_id, 0), "past the wrap");
    assert!(!through(&initiator, &link_id, 1));
    assert_eq!(
        initiator
            .link_stats(&link_id)
            .unwrap()
            .oldest_unacked_sequence(),
        Some(1)
    );

    let proofs = deliver_all(&mut responder, r_iface, sent[3].1.clone());
    let _ = deliver_all(&mut initiator, i_iface, proofs);
    assert!(through(&initiator, &link_id, 1));
    assert_eq!(
        initiator
            .link_stats(&link_id)
            .unwrap()
            .oldest_unacked_sequence(),
        None
    );
}

/// The ring's front is the oldest unproved send, whatever order the proofs
/// come in, across the wrap.
#[test]
fn the_oldest_unacked_is_the_front_of_a_send_ordered_ring() {
    let mut ch = Channel::new();
    ch.force_next_tx_sequence_for_test(0xFFFE);
    ch.set_window_bounds_for_test(8, 8, 8);
    ch.set_pacing_for_test(0, 0);
    for i in 0..3u8 {
        ch.send_system_raw(0x0001, &[i; 8], 400, TEST_TIME_MS, 100)
            .expect("send");
    }
    assert_eq!(ch.oldest_unacked_sequence(), Some(0xFFFE));
    assert!(
        ch.mark_delivered(0, TEST_TIME_MS + 50, 100),
        "a later proof first"
    );
    assert_eq!(ch.oldest_unacked_sequence(), Some(0xFFFE));
    assert!(ch.mark_delivered(0xFFFE, TEST_TIME_MS + 60, 100));
    assert_eq!(ch.oldest_unacked_sequence(), Some(0xFFFF));
    assert!(ch.mark_delivered(0xFFFF, TEST_TIME_MS + 70, 100));
    assert_eq!(ch.oldest_unacked_sequence(), None);
}

#[test]
fn serial_order_holds_across_the_wrap() {
    assert!(serial_after(1, 0));
    assert!(serial_after(0, 0xFFFF));
    assert!(serial_after(2, 0xFFFE));
    assert!(!serial_after(0xFFFF, 0));
    assert!(!serial_after(7, 7));
    assert!(serial_after(0x7FFF, 0));
    assert!(!serial_after(0x8000, 0), "half the space away is not after");
}
