//! mvr: a held announce remembers the peer it came from (Codeberg #589,
//! from #445).
//!
//! ## The field failure this reproduces
//!
//! #445, btvirt 2026-10-10, `ble_room_transfer_relayed`: the receiver's
//! announce reached the relay while its ingress burst limit was active,
//! so it was held (`ANN_HELD`) and installed only when the tick released
//! it. The release runs outside `process_incoming_from_peer`, where
//! `ingress_peer` is `None`, so the path got no `via_peer`. The relayed
//! LinkRequest then left unaddressed, the link entry got no
//! `next_hop_peer` (#573 reads it off the path), and every packet of that
//! link flooded both BLE links for the link's lifetime.
//!
//! The burst limit is ordinary field behaviour on a relay with several
//! neighbours announcing at once, so the hold has to carry the arrival's
//! peer to the release, the way `TransportEvent::ProofRequested` carries
//! it to a deferred proof (#376).
//!
//! ## Shape
//!
//! 1 node, 1 shared BLE interface, deterministic, sub-second wall clock.
//! A noise peer trips the burst, the receiver's announce is held, the
//! tick releases it. Observables: the installed path's `via_peer` and the
//! `peer` field of the forwarded LinkRequest.

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
use crate::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Clock;
use crate::transport::{Action, InterfaceId, TickOutput};

type EmbeddedNode = NodeCore<OsRng, MockClock, EmbeddedStorage>;

/// The link of the neighbour whose burst trips the limit, the sender's
/// link and the receiver's link, as the BLE interface reports them.
const NOISE: [u8; TRUNCATED_HASHBYTES] = [0xD4; TRUNCATED_HASHBYTES];
const SENDER: [u8; TRUNCATED_HASHBYTES] = [0xA1; TRUNCATED_HASHBYTES];
const RECEIVER: [u8; TRUNCATED_HASHBYTES] = [0xB2; TRUNCATED_HASHBYTES];

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

fn pack(packet: &Packet) -> Vec<u8> {
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).unwrap();
    buf[..len].to_vec()
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

/// The sender's LinkRequest for `dest`, addressed to the relay.
fn link_request_via(
    transport_id: [u8; TRUNCATED_HASHBYTES],
    dest: &crate::DestinationHash,
) -> Vec<u8> {
    pack(&Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type2,
            context_flag: false,
            transport_type: TransportType::Transport,
            dest_type: DestinationType::Single,
            packet_type: PacketType::LinkRequest,
        },
        hops: 0,
        transport_id: Some(transport_id),
        destination_hash: *dest.as_bytes(),
        context: PacketContext::None,
        data: PacketData::Owned(std::vec![0x5C; 64]),
    })
}

/// Every forward of a packet addressed to `dest`, as (interface, hint).
fn sends_to(
    out: &TickOutput,
    dest: &[u8; TRUNCATED_HASHBYTES],
) -> Vec<(InterfaceId, Option<[u8; TRUNCATED_HASHBYTES]>)> {
    out.actions
        .iter()
        .filter_map(|action| match action {
            Action::SendPacket { iface, data, peer } => Packet::unpack(data)
                .ok()
                .filter(|p| &p.destination_hash == dest)
                .map(|_| (*iface, *peer)),
            Action::Broadcast { .. } => None,
        })
        .collect()
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

/// Tick until `dest` has a path, or give up after three minutes of node
/// time. Releases go one per 5 s after a 15 s penalty, ties in
/// destination-hash order, so the receiver may be the last of the twelve
/// noise holds out: 75 s at worst.
fn release(node: &mut EmbeddedNode, dest: &crate::DestinationHash) {
    for _ in 0..360 {
        if node.transport.path(dest.as_bytes()).is_some() {
            return;
        }
        node.transport.clock().advance(500);
        let _ = node.handle_timeout();
    }
}

/// The relay with the receiver's path installed by a held announce's
/// release.
struct Released {
    node: Box<EmbeddedNode>,
    ble: usize,
    dest: crate::DestinationHash,
}

fn relay_with_a_released_path(peer: Option<[u8; TRUNCATED_HASHBYTES]>) -> Released {
    let mut node = make_node();
    let ble = add_iface(&mut node, "ble_0", 0);
    trip_the_burst(&mut node, ble);

    let (dest, raw) = announce("receiver");
    let _ = match peer {
        Some(peer) => node.handle_packet_from_peer(InterfaceId(ble), peer, &raw),
        None => node.handle_packet(InterfaceId(ble), &raw),
    };
    assert!(
        node.transport.path(dest.as_bytes()).is_none(),
        "the receiver's announce must be HELD by the burst limit, not \
         installed on arrival; without the hold this test measures nothing"
    );

    let t0 = node.transport.clock().now_ms();
    release(&mut node, &dest);
    assert!(
        node.transport.path(dest.as_bytes()).is_some(),
        "the held announce must be released by the tick (gave up after {} ms)",
        node.transport.clock().now_ms() - t0
    );
    Released { node, ble, dest }
}

/// THE #445 relay: the path a released announce installs names the peer
/// the announce arrived through.
#[test]
fn a_released_announce_installs_the_peer_it_arrived_from() {
    let r = relay_with_a_released_path(Some(RECEIVER));
    let path = r.node.transport.path(r.dest.as_bytes()).unwrap();
    assert_eq!(
        path.via_peer,
        Some(RECEIVER),
        "a held announce is released from the tick, outside \
         process_incoming_from_peer, so the peer it arrived through has to \
         travel with it; without it the path has no via_peer and every \
         packet of a link over it floods (#445)"
    );
}

/// One packet later: the LinkRequest relayed along that path names the
/// receiver's link, so the link entry learns its `next_hop_peer` and the
/// link's traffic is routed, not flooded.
#[test]
fn a_link_request_along_a_released_path_names_the_receiver() {
    let mut r = relay_with_a_released_path(Some(RECEIVER));
    let own_id = *r.node.identity().hash();
    let out = r.node.handle_packet_from_peer(
        InterfaceId(r.ble),
        SENDER,
        &link_request_via(own_id, &r.dest),
    );
    assert_eq!(
        sends_to(&out, r.dest.as_bytes()),
        std::vec![(InterfaceId(r.ble), Some(RECEIVER))],
        "the forwarded LinkRequest must name the receiver's link (#445: it \
         flooded, and so did the link's 340 resource packets after it)"
    );
}

/// Negative control: an announce held without a named peer (a
/// single-peer interface, or a BLE packet before the identity handshake)
/// is released without one. The hold carries the peer, never invents it.
#[test]
fn an_announce_held_without_a_peer_is_released_without_one() {
    let r = relay_with_a_released_path(None);
    assert_eq!(
        r.node.transport.path(r.dest.as_bytes()).unwrap().via_peer,
        None
    );
}
