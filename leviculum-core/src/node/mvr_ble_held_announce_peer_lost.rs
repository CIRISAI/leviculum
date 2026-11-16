//! mvr: a held announce dies with the peer it came through (Codeberg #480).
//!
//! ## The field failure this reproduces
//!
//! Nightly `ble_room_20`, 2026-10-31, node 19 of the room: over its fresh
//! link to node 2 it heard node 5's announce at hops 2 while the ingress
//! burst limit was active, so the announce was held (`ANN_HELD`) with
//! `peer = node 2`. Later copies arrived at hops 4 through other links, and
//! the hold kept the shorter one (the 2026-09-18 deviation in
//! `Transport::hold_announce`). Node 2 then dropped the link;
//! `handle_interface_peer_lost` culled the path table ("removed 0 paths")
//! and left the held entry alone. Ten seconds later the tick released it
//! with the dead link as `ingress_peer`: `PATH_ADD hops=2 next_hop=node 2`,
//! a next hop with no link, rebroadcast to the room. Three probes to node 5
//! died at node 19 as `BLE_TX_ROUTE_MISS`, and since no real announce ever
//! reached node 19 below 3 hops the dead route was never replaced.
//!
//! ## What the fix has to deliver
//!
//! The hold keeps ONE copy per destination, so once the shorter copy from
//! the lost peer is kept, the longer copy from the live peer is gone. The
//! end state the hold must reach is the one a node reaches without the
//! hold: the hops-2 path via P installed, the hops-4 copy of the same
//! emission refused as no better, P lost, the path culled. Nothing via P,
//! nothing at all for D until D announces again or a path request asks.
//! A held copy that came through a peer still up is untouched.
//!
//! ## Shape
//!
//! 1 node, 1 shared BLE interface, deterministic, sub-second wall clock.
//! A noise peer trips the burst, D's announce is held, a peer is lost, the
//! tick releases what is left. Observable: D's path after the release.

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
use crate::packet::{HeaderType, Packet, TransportType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::InterfaceId;

type EmbeddedNode = NodeCore<OsRng, MockClock, EmbeddedStorage>;

/// The neighbour whose burst trips the limit, the peer the short copy
/// comes through (node 2 in the field run) and a peer that stays up.
const NOISE: [u8; TRUNCATED_HASHBYTES] = [0xD4; TRUNCATED_HASHBYTES];
const P: [u8; TRUNCATED_HASHBYTES] = [0xF7; TRUNCATED_HASHBYTES];
const Q: [u8; TRUNCATED_HASHBYTES] = [0x3C; TRUNCATED_HASHBYTES];

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

/// A fresh destination and one direct announce for it.
fn announce(app: &'static str) -> (crate::DestinationHash, Vec<u8>) {
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &[app],
    )
    .unwrap();
    let dest_hash = *dest.hash();
    let ann = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .unwrap();
    let mut buf = [0u8; MTU];
    let len = ann.pack(&mut buf).unwrap();
    (dest_hash, buf[..len].to_vec())
}

/// The same emission as relayed by `via`: HEADER_2, `via` as
/// `transport_id`, `wire_hops` on the wire (the receipt adds one).
fn relayed(raw: &[u8], via: [u8; TRUNCATED_HASHBYTES], wire_hops: u8) -> Vec<u8> {
    let mut p = Packet::unpack(raw).unwrap();
    p.flags.header_type = HeaderType::Type2;
    p.flags.transport_type = TransportType::Transport;
    p.hops = wire_hops;
    p.transport_id = Some(via);
    let mut buf = [0u8; MTU];
    let len = p.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

/// Spend the burst limit: a neighbour announcing many destinations at
/// 10 Hz, above the 3 Hz a new interface tolerates.
fn trip_the_burst(node: &mut EmbeddedNode, ble: usize) {
    for _ in 0..12 {
        let (_, raw) = announce("noise");
        let _ = node.handle_packet_from_peer(InterfaceId(ble), NOISE, &raw);
        node.transport.clock().advance(100);
    }
}

/// Tick three minutes of node time: every hold is released by then
/// (one per 5 s after a 15 s penalty, thirteen holds at most here).
fn release_all(node: &mut EmbeddedNode) {
    for _ in 0..360 {
        node.transport.clock().advance(500);
        let _ = node.handle_timeout();
    }
}

/// Feed `copies` of D's announce, each as (peer, wire hops), while the
/// burst is active, and assert every one was held, not installed.
fn hold_copies(
    node: &mut EmbeddedNode,
    ble: usize,
    copies: &[([u8; TRUNCATED_HASHBYTES], u8)],
) -> crate::DestinationHash {
    let (dest, raw) = announce("d");
    for (peer, wire_hops) in copies {
        let _ = node.handle_packet_from_peer(
            InterfaceId(ble),
            *peer,
            &relayed(&raw, *peer, *wire_hops),
        );
        assert!(
            node.transport.path(dest.as_bytes()).is_none(),
            "D's announce must be HELD by the burst limit, not installed on \
             arrival; without the hold this test measures nothing"
        );
    }
    dest
}

/// THE #480 shape: the kept copy came through P, a longer copy came
/// through Q, P is lost before the release. No path may name P.
#[test]
fn a_held_announce_dies_with_the_peer_it_came_through() {
    let mut node = make_node();
    let ble = add_iface(&mut node, "ble_0", 0);
    trip_the_burst(&mut node, ble);
    let dest = hold_copies(&mut node, ble, &[(P, 1), (Q, 3)]);

    let _ = node.handle_interface_peer_lost(InterfaceId(ble), P);
    release_all(&mut node);

    let path = node.transport.path(dest.as_bytes());
    assert!(
        path.is_none_or(|p| p.next_hop != Some(P) && p.via_peer != Some(P)),
        "a held announce released after its peer was lost installed a path \
         through that peer (next_hop={:?} via_peer={:?}): every packet to D \
         dies as BLE_TX_ROUTE_MISS (#480, ble_room_20 node 19)",
        path.map(|p| p.next_hop),
        path.map(|p| p.via_peer),
    );
    assert!(
        path.is_none(),
        "the hold keeps one copy per destination, so Q's longer copy was \
         never stored; the end state without the hold is no path either \
         (hops-4 refused as no better, then the P path culled). Got {:?}",
        path.map(|p| (p.hops, p.next_hop, p.via_peer)),
    );
}

/// After the purge D is unknown again, so the next copy through a live
/// peer is learned as usual (here: held again and released via Q).
#[test]
fn a_copy_after_the_loss_installs_through_the_live_peer() {
    let mut node = make_node();
    let ble = add_iface(&mut node, "ble_0", 0);
    trip_the_burst(&mut node, ble);
    let (dest, raw) = announce("d");
    let _ = node.handle_packet_from_peer(InterfaceId(ble), P, &relayed(&raw, P, 1));
    let _ = node.handle_interface_peer_lost(InterfaceId(ble), P);
    let _ = node.handle_packet_from_peer(InterfaceId(ble), Q, &relayed(&raw, Q, 3));
    release_all(&mut node);

    let path = node.transport.path(dest.as_bytes());
    assert_eq!(
        path.map(|p| (p.hops, p.next_hop, p.via_peer)),
        Some((4, Some(Q), Some(Q))),
        "the copy through the live peer must be the one installed"
    );
}

/// Negative control: losing a peer leaves a copy held through ANOTHER
/// peer alone; it is released and installed through that peer.
#[test]
fn a_held_announce_from_a_live_peer_survives_another_peers_loss() {
    let mut node = make_node();
    let ble = add_iface(&mut node, "ble_0", 0);
    trip_the_burst(&mut node, ble);
    let dest = hold_copies(&mut node, ble, &[(Q, 1), (P, 3)]);

    let _ = node.handle_interface_peer_lost(InterfaceId(ble), P);
    release_all(&mut node);

    let path = node.transport.path(dest.as_bytes());
    assert_eq!(
        path.map(|p| (p.hops, p.next_hop, p.via_peer)),
        Some((2, Some(Q), Some(Q))),
        "the purge must take only the lost peer's holds"
    );
}
