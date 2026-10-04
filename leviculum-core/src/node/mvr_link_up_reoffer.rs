//! mvr: a new link carries the announces it missed (#383, the #434
//! lever).
//!
//! Seed 6 of the 2026-09-27 `ble_room_10` sweep, reduced to its
//! mechanism: a transport node relays an announce as a two-emission
//! ladder (`PATHFINDER_RETRIES = 1`) and then retires the entry. A peer
//! whose first link lands AFTER the retirement never hears the
//! announce, and nothing else ever re-emits it — in the room that made
//! missing the 7–8 bridge by 1.2 s a permanent lost route to node 6,
//! five probes red. The fix is the peer-up re-offer
//! (`Transport::reoffer_stored_announces_to_peer`): when a multi-peer
//! interface reports a peer's first link, the stored announces go to
//! that peer, on its link alone, as ordinary transit announces.
//!
//! Pinned here: the headline shape (ladder retired, link 1 s later,
//! the peer has the announce within ONE emission, and a second node
//! accepts those exact bytes as a path); the #168 exclusions (nothing
//! re-offered to the peer the path came through, nothing from a
//! non-transport node, nothing whose path entry expired); and the #402
//! cap (a re-offer over budget queues WITH its delivery hint and goes
//! out to the same one peer when the queue drains).
//!
//! Design table and the room replay:
//! `leviculum-nrf/ble-tx/tests/graph_formation.rs`,
//! `seed6_replay_design_table`.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::embedded_storage::EmbeddedStorage;
use crate::identity::Identity;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{HeaderType, Packet, PacketContext, PacketType, TransportType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Clock;
use crate::transport::{Action, InterfaceId, TickOutput};

/// Both boards' exact shape: `EmbeddedStorage`, transport enabled.
type EmbeddedNode = NodeCore<OsRng, MockClock, EmbeddedStorage>;

fn make_node() -> Box<EmbeddedNode> {
    NodeCoreBuilder::new().enable_transport(true).build_boxed(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        EmbeddedStorage::new(),
    )
}

fn add_iface(node: &mut EmbeddedNode, name: &'static str, id: u8) -> usize {
    let idx = node
        .transport
        .register_interface(Box::new(MockInterface::new(name, id)));
    node.set_interface_name(idx, String::from(name));
    idx
}

/// A third node's destination and its announce bytes (Header 1, wire
/// hops 0 — the shape a direct neighbour's announce has on the link).
fn origin_announce(node: &EmbeddedNode) -> (Destination, Vec<u8>) {
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .unwrap();
    let ts = node.transport().clock().now_ms();
    let ann = dest.announce(None, &mut OsRng, ts, ts / 1000).unwrap();
    let mut buf = [0u8; MTU];
    let len = ann.pack(&mut buf).unwrap();
    (dest, buf[..len].to_vec())
}

/// Every announce this output puts on the wire, as
/// `(target iface, delivery hint, parsed packet)`.
fn announces_out(out: &TickOutput) -> Vec<(Option<InterfaceId>, Option<[u8; 16]>, Packet)> {
    out.actions
        .iter()
        .filter_map(|action| {
            let (target, peer, data) = match action {
                Action::SendPacket { iface, data, peer } => (Some(*iface), *peer, data),
                Action::Broadcast { data, .. } => (None, None, data),
            };
            let p = Packet::unpack(data).ok()?;
            (p.flags.packet_type == PacketType::Announce).then_some((target, peer, p))
        })
        .collect()
}

/// Run the node's clock forward in 500 ms steps, draining every timer
/// action, until `horizon_ms` past now — long enough for a relay
/// ladder (two emissions, `PATHFINDER_G` apart, plus retry jitter) to
/// fire completely and retire.
fn drain_ladder(node: &mut EmbeddedNode, horizon_ms: u64) -> usize {
    let start = node.transport().clock().now_ms();
    let mut emitted = 0;
    let mut t = start;
    while t < start + horizon_ms {
        t += 500;
        node.transport().clock().set(t);
        let out = node.handle_timeout();
        emitted += announces_out(&out).len();
    }
    emitted
}

const P1: [u8; TRUNCATED_HASHBYTES] = [0x11; TRUNCATED_HASHBYTES];
const P2: [u8; TRUNCATED_HASHBYTES] = [0x22; TRUNCATED_HASHBYTES];

/// The headline: the ladder has retired, the link comes up later, and
/// the new peer still gets the announce — within the peer-up call
/// itself, addressed at that peer on that interface, as the ordinary
/// transit announce a second node accepts a path from.
#[test]
fn a_link_after_the_ladder_died_still_carries_the_stored_announce() {
    let mut node = make_node();
    let ble = add_iface(&mut node, "ble_nrf", 0);
    let _lora = add_iface(&mut node, "lora_sx1262", 1);

    let (dest, ann_bytes) = origin_announce(&node);

    // The announce arrives through peer P1's link and is relayed as the
    // normal ladder…
    let _ = node.handle_packet_from_peer(InterfaceId(ble), P1, &ann_bytes);
    assert!(node.has_path(dest.hash()), "premise: the path installed");
    let ladder = drain_ladder(&mut node, 30_000);
    assert!(
        ladder >= 1,
        "premise: the relay ladder fired ({ladder} emissions)"
    );
    // …and is retired: nothing further fires on its own.
    assert_eq!(
        drain_ladder(&mut node, 30_000),
        0,
        "premise: the ladder is retired, the scheduler is silent"
    );

    // One second later, P2's first link comes up.
    let now = node.transport().clock().now_ms() + 1_000;
    node.transport().clock().set(now);
    let out = node.handle_interface_peer_up(InterfaceId(ble), P2);

    let offers = announces_out(&out);
    assert_eq!(offers.len(), 1, "exactly the one stored announce");
    let (target, peer, packet) = &offers[0];
    assert_eq!(
        *target,
        Some(InterfaceId(ble)),
        "on the reporting interface"
    );
    assert_eq!(*peer, Some(P2), "addressed at the new peer's link alone");
    assert_eq!(packet.destination_hash, *dest.hash().as_bytes());
    assert_eq!(
        packet.flags.header_type,
        HeaderType::Type2,
        "a relayed announce names its relay"
    );
    assert_eq!(packet.flags.transport_type, TransportType::Transport);
    assert_eq!(
        packet.transport_id,
        Some(*node.transport().identity().hash())
    );
    assert_eq!(
        packet.hops, 1,
        "the stored hop count, not the cached wire byte"
    );
    assert_eq!(
        packet.context,
        PacketContext::None,
        "an ordinary announce — the peer may rebroadcast it onward"
    );

    // Closure half: a second node accepts those exact bytes as a path.
    let mut peer_node = make_node();
    let peer_ble = add_iface(&mut peer_node, "ble_nrf", 0);
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).unwrap();
    let _ = peer_node.handle_packet(InterfaceId(peer_ble), &buf[..len]);
    assert!(
        peer_node.has_path(dest.hash()),
        "the re-offered announce installs the path at the new peer"
    );
    let entry = peer_node
        .transport
        .get_path_clone(dest.hash().as_bytes())
        .unwrap();
    assert_eq!(entry.hops, 2, "one hop further than the re-offering relay");
}

/// The #168 bounce-back rule: the peer the path came through is never
/// offered it back. P1 relinking (lost, then up again) gets nothing —
/// the only stored entry routes through P1 itself.
#[test]
fn the_peer_the_path_came_through_is_not_offered_it_back() {
    let mut node = make_node();
    let ble = add_iface(&mut node, "ble_nrf", 0);

    let (_dest, ann_bytes) = origin_announce(&node);
    let _ = node.handle_packet_from_peer(InterfaceId(ble), P1, &ann_bytes);
    drain_ladder(&mut node, 30_000);

    let out = node.handle_interface_peer_up(InterfaceId(ble), P1);
    assert!(
        announces_out(&out).is_empty(),
        "a route through the asking peer is the #168 bounce-back"
    );
}

/// A non-transport node holds the path for itself and offers nothing:
/// it would not relay toward the destination, so advertising the route
/// would promise a service it does not render.
#[test]
fn a_non_transport_node_offers_nothing() {
    let mut node = NodeCoreBuilder::new().enable_transport(false).build_boxed(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        EmbeddedStorage::new(),
    );
    let ble = add_iface(&mut node, "ble_nrf", 0);

    let (dest, ann_bytes) = origin_announce(&node);
    let _ = node.handle_packet_from_peer(InterfaceId(ble), P1, &ann_bytes);
    assert!(node.has_path(dest.hash()), "premise: the path installed");
    drain_ladder(&mut node, 30_000);

    let out = node.handle_interface_peer_up(InterfaceId(ble), P2);
    assert!(announces_out(&out).is_empty());
}

/// An entry past its expiry is not offered: the periodic cleaner just
/// has not swept it yet, and a re-offer would advertise a route this
/// node would no longer trust itself (the same rule the peer-up pull
/// applies to the direct-entry guard).
#[test]
fn an_expired_entry_is_not_offered() {
    let mut node = make_node();
    let ble = add_iface(&mut node, "ble_nrf", 0);

    let (dest, ann_bytes) = origin_announce(&node);
    let _ = node.handle_packet_from_peer(InterfaceId(ble), P1, &ann_bytes);
    drain_ladder(&mut node, 30_000);

    // Age the entry past its expiry in place, cache intact.
    use crate::traits::Storage;
    let now = node.transport().clock().now_ms();
    let mut entry = node
        .transport
        .get_path_clone(dest.hash().as_bytes())
        .unwrap();
    entry.expires_ms = now - 1;
    node.transport
        .storage_mut()
        .set_path(*dest.hash().as_bytes(), entry);

    let out = node.handle_interface_peer_up(InterfaceId(ble), P2);
    assert!(announces_out(&out).is_empty());
}

/// The #402 cap paces re-offers like any relayed announce — and a
/// held-back re-offer keeps its delivery hint: when the queue drains,
/// the announce still goes to the one peer the link-up was for.
#[test]
fn a_capped_reoffer_queues_and_drains_to_the_same_peer() {
    let mut node = make_node();
    let ble = add_iface(&mut node, "ble_nrf", 0);

    // Two stored announces from two origins, both via P1.
    let (dest_a, ann_a) = origin_announce(&node);
    let (dest_b, ann_b) = origin_announce(&node);
    let _ = node.handle_packet_from_peer(InterfaceId(ble), P1, &ann_a);
    let _ = node.handle_packet_from_peer(InterfaceId(ble), P1, &ann_b);
    drain_ladder(&mut node, 40_000);

    // A tight cap: ~170-byte announces against 20 bit/s of announce
    // budget make the second re-offer wait minutes, not jitter.
    node.transport.register_interface_bitrate(ble, 1_000);
    assert!(node.transport.set_interface_announce_cap(ble, 2));

    let out = node.handle_interface_peer_up(InterfaceId(ble), P2);
    let immediate = announces_out(&out);
    assert_eq!(
        immediate.len(),
        1,
        "the cap lets exactly one re-offer through now"
    );
    assert_eq!(immediate[0].1, Some(P2));

    // The other is queued, not dropped: it drains, holdoff over, to the
    // SAME peer.
    let mut drained: Vec<(Option<[u8; 16]>, [u8; TRUNCATED_HASHBYTES])> = Vec::new();
    let start = node.transport().clock().now_ms();
    for step in 1..=40u64 {
        node.transport().clock().set(start + step * 10_000);
        let out = node.handle_timeout();
        drained.extend(
            announces_out(&out)
                .into_iter()
                .map(|(_, peer, p)| (peer, p.destination_hash)),
        );
    }
    assert_eq!(drained.len(), 1, "the held re-offer drains exactly once");
    assert_eq!(
        drained[0].0,
        Some(P2),
        "the delivery hint survived the queue"
    );
    let offered: Vec<[u8; TRUNCATED_HASHBYTES]> =
        std::vec![immediate[0].2.destination_hash, drained[0].1,];
    assert!(offered.contains(dest_a.hash().as_bytes()));
    assert!(offered.contains(dest_b.hash().as_bytes()));
}
