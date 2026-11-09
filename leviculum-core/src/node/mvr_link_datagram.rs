//! mvr: link datagrams are delivered unordered, once, with their sequence
//! number, and never proved (+ciris, leviculum#102).
//!
//! ## Mechanism
//!
//! A Channel frame behind a lost one waits for the gap to fill, so one loss
//! on live media became a burst of drops. [`NodeCore::send_unordered`] sends
//! a link DATA packet with context `Datagram` instead: link-encrypted
//! `[sequence u32][payload]`, no window, no receipt, no retransmit. The
//! receiver emits [`NodeEvent::LinkDatagramReceived`] as each one decrypts,
//! whatever order they come in, and sends nothing back even when the
//! destination proves everything.
//!
//! Sans-I/O: two `NodeCore`s over `MockInterface`, no radio, no timers.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::datagram::{DatagramStats, DATAGRAM_HEADER_LEN};
use crate::link::LinkId;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent, SendError};
use crate::packet::{Packet, PacketContext};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{Action, InterfaceId, TickOutput};

/// Real storage, so the transport's packet-hash dedup runs as in the field.
type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

fn add_iface(node: &mut Node, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    node.set_interface_hw_mtu(idx, 1400);
    idx
}

fn packets(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .collect()
}

struct Pair {
    sender: Node,
    receiver: Node,
    s_iface: usize,
    r_iface: usize,
    link_id: LinkId,
}

/// An established link from `sender` to a destination on `receiver` that
/// proves every plain packet, so a datagram that drew a proof would show.
fn linked_pair() -> Pair {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut receiver = NodeCoreBuilder::new().build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "av",
        &["datagram"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    receiver.register_destination(dest);
    let r_iface = add_iface(&mut receiver, "R");

    let mut sender = NodeCoreBuilder::new().build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    let s_iface = add_iface(&mut sender, "S");

    let (link_id, _, out) = sender.connect(dest_hash, &signing_key).unwrap();
    let mut to_receiver = packets(&out);
    for _ in 0..8 {
        if to_receiver.is_empty() {
            break;
        }
        let mut back = Vec::new();
        for raw in to_receiver {
            back.extend(packets(&receiver.handle_packet(InterfaceId(r_iface), &raw)));
        }
        to_receiver = Vec::new();
        for raw in back {
            to_receiver.extend(packets(&sender.handle_packet(InterfaceId(s_iface), &raw)));
        }
    }
    assert!(sender.link(&link_id).unwrap().is_active());
    assert!(receiver.link(&link_id).unwrap().is_active());
    Pair {
        sender,
        receiver,
        s_iface,
        r_iface,
        link_id,
    }
}

/// Send one datagram; return its sequence number and the single packet.
fn send(pair: &mut Pair, data: &[u8]) -> (u32, Vec<u8>) {
    let (seq, out) = pair.sender.send_unordered(&pair.link_id, data).unwrap();
    let mut sent = packets(&out);
    assert_eq!(sent.len(), 1, "one datagram, one packet");
    let raw = sent.pop().unwrap();
    assert_eq!(
        Packet::unpack(&raw).unwrap().context,
        PacketContext::Datagram
    );
    (seq, raw)
}

/// Deliver `raw` to the receiver: the datagrams it delivered and every
/// packet it sent back.
fn deliver(pair: &mut Pair, raw: &[u8]) -> (Vec<(u32, Vec<u8>)>, usize) {
    let out = pair.receiver.handle_packet(InterfaceId(pair.r_iface), raw);
    let got = out
        .events
        .iter()
        .filter_map(|e| match e {
            NodeEvent::LinkDatagramReceived {
                link_id,
                sequence,
                data,
            } if *link_id == pair.link_id => Some((*sequence, data.clone())),
            _ => None,
        })
        .collect();
    (got, packets(&out).len())
}

/// Out of order, with a gap: each datagram is delivered the moment it
/// arrives, carrying its own sequence number, and none draws a proof.
#[test]
fn datagrams_arrive_as_they_come_with_their_sequence_and_no_proof() {
    let mut pair = linked_pair();
    let sent: Vec<(u32, Vec<u8>)> = (0..4u8).map(|i| send(&mut pair, &[i; 100])).collect();
    assert_eq!(
        sent.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );

    // 1 is lost; 3 overtakes 2; 0 comes last.
    let mut delivered = Vec::new();
    for i in [3usize, 2, 0] {
        let (got, back) = deliver(&mut pair, &sent[i].1);
        assert_eq!(back, 0, "datagram {i} drew a packet back");
        delivered.extend(got);
    }
    assert_eq!(
        delivered,
        std::vec![
            (3, std::vec![3u8; 100]),
            (2, std::vec![2u8; 100]),
            (0, std::vec![0u8; 100])
        ]
    );
    assert_eq!(
        pair.sender.link_datagram_stats(&pair.link_id),
        Some(DatagramStats {
            sent: 4,
            sent_bytes: 400,
            ..DatagramStats::default()
        })
    );
    assert_eq!(
        pair.receiver.link_datagram_stats(&pair.link_id),
        Some(DatagramStats {
            received: 3,
            received_bytes: 300,
            ..DatagramStats::default()
        })
    );
    let _ = pair.s_iface;
}

/// The payload may fill the link MDU less the sequence number, and no more.
#[test]
fn the_datagram_mdu_is_the_link_mdu_less_the_sequence_number() {
    let mut pair = linked_pair();
    let mdu = pair.sender.link_datagram_mdu(&pair.link_id).unwrap();
    assert_eq!(
        mdu,
        pair.sender.link(&pair.link_id).unwrap().mdu() - DATAGRAM_HEADER_LEN
    );
    let (_, raw) = send(&mut pair, &std::vec![0x5A; mdu]);
    assert_eq!(
        deliver(&mut pair, &raw).0.len(),
        1,
        "a full datagram arrives"
    );
    assert!(matches!(
        pair.sender
            .send_unordered(&pair.link_id, &std::vec![0x5A; mdu + 1]),
        Err(SendError::TooLarge)
    ));
}

/// A datagram that does not decrypt is not delivered, counts as refused and
/// does not touch the link's liveness.
#[test]
fn a_tampered_datagram_is_refused() {
    let mut pair = linked_pair();
    let (_, mut raw) = send(&mut pair, b"frame");
    let last = raw.len() - 1;
    raw[last] ^= 0x01;
    let before = pair.receiver.link(&pair.link_id).unwrap().last_inbound_ms();
    pair.receiver.transport.clock().advance(1_000);
    let (got, back) = deliver(&mut pair, &raw);
    assert!(got.is_empty());
    assert_eq!(back, 0);
    assert_eq!(
        pair.receiver.link(&pair.link_id).unwrap().last_inbound_ms(),
        before,
        "a forged datagram is no evidence the peer is alive"
    );
    assert_eq!(
        pair.receiver
            .link_datagram_stats(&pair.link_id)
            .unwrap()
            .refused,
        1
    );
}

/// leviculum#102 review: a replay is refused even after the packet-hash
/// dedup has forgotten it, and refused before it counts as liveness.
#[test]
fn a_replay_after_the_dedup_forgets_it_is_refused_by_the_window() {
    let mut pair = linked_pair();
    let (seq, raw) = send(&mut pair, b"frame");
    assert_eq!(
        deliver(&mut pair, &raw).0,
        std::vec![(seq, b"frame".to_vec())]
    );

    // The dedup catches the immediate repeat.
    assert!(deliver(&mut pair, &raw).0.is_empty());
    let stats = pair.receiver.link_datagram_stats(&pair.link_id).unwrap();
    assert_eq!(
        (stats.received, stats.replayed),
        (1, 0),
        "dedup, not the window"
    );

    // Once the cache has turned over, only the window stands.
    pair.receiver.transport.storage_mut().clear_packet_hashes();
    let before = pair.receiver.link(&pair.link_id).unwrap().last_inbound_ms();
    pair.receiver.transport.clock().advance(1_000);
    let (got, back) = deliver(&mut pair, &raw);
    assert!(got.is_empty(), "a replay is not delivered");
    assert_eq!(back, 0);
    assert_eq!(
        pair.receiver.link(&pair.link_id).unwrap().last_inbound_ms(),
        before,
        "a replay is no evidence the peer is alive"
    );
    let stats = pair.receiver.link_datagram_stats(&pair.link_id).unwrap();
    assert_eq!((stats.received, stats.replayed), (1, 1));
}

/// leviculum#102 review: a datagram flood never takes the host's last
/// event slots from the Channel. Datagrams beyond the reserve are shed and
/// counted; the Channel message that follows is delivered, not held.
#[test]
fn a_datagram_flood_leaves_the_channel_its_room() {
    use crate::node::DATAGRAM_SINK_RESERVE;
    let mut pair = linked_pair();
    pair.receiver
        .set_channel_delivery_budget(Some(DATAGRAM_SINK_RESERVE + 2));
    let flood: Vec<Vec<u8>> = (0..10u8).map(|i| send(&mut pair, &[i; 50]).1).collect();
    let mut delivered = 0;
    for raw in &flood {
        delivered += deliver(&mut pair, raw).0.len();
    }
    assert_eq!(delivered, 2, "datagrams stop at the reserve");
    assert_eq!(
        pair.receiver
            .link_datagram_stats(&pair.link_id)
            .unwrap()
            .shed,
        8
    );

    let out = pair.sender.send_on_link(&pair.link_id, b"control").unwrap();
    let mut got_message = false;
    for raw in packets(&out) {
        let o = pair.receiver.handle_packet(InterfaceId(pair.r_iface), &raw);
        got_message |= o.events.iter().any(|e| {
            matches!(e, NodeEvent::MessageReceived { data, .. } if data.as_slice() == b"control")
        });
    }
    assert!(got_message, "the Channel message is delivered at once");
}

/// leviculum#102 review: at a full interface a datagram is shed and
/// counted, never queued for retry; a Channel frame refused the same way
/// is still handed back for retry.
#[test]
fn a_full_interface_sheds_datagrams_and_retries_the_channel() {
    use crate::transport::{dispatch_actions, DropReason};
    let mut pair = linked_pair();
    let (_, datagram) = send(&mut pair, b"frame");
    let channel = packets(&pair.sender.send_on_link(&pair.link_id, b"control").unwrap())
        .pop()
        .unwrap();
    let mut full = MockInterface::new("full", pair.s_iface as u8);
    full.reject_sends = true;
    let mut ifaces: Vec<&mut dyn crate::traits::Interface> = std::vec![&mut full];
    let iface = InterfaceId(pair.s_iface);
    let result = dispatch_actions(
        &mut ifaces,
        std::vec![
            Action::SendPacket {
                iface,
                data: datagram,
                peer: None,
            },
            Action::SendPacket {
                iface,
                data: channel.clone(),
                peer: None,
            },
        ],
        &std::collections::BTreeMap::new(),
    );
    assert_eq!(result.drops, std::vec![(iface, DropReason::DatagramShed)]);
    assert_eq!(result.retries.len(), 1, "only the Channel frame is retried");
    assert_eq!(result.retries[0].data, channel);

    pair.sender.record_dispatch_drops(&result);
    assert_eq!(pair.sender.transport.stats().drops_datagram_shed(), 1);
}
