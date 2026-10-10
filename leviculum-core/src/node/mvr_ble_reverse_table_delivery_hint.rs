//! mvr: a relayed proof of a plain packet names the peer it is for
//! (Codeberg #582, from #444 and #573).
//!
//! ## The field failure this reproduces
//!
//! periculum 573, `ble_room_transfer_relayed` on btvirt: after #573 the
//! relay's link-table repeats went out as `BLE_TX_ROUTE`, and of the ten
//! floods left per transfer two were proofs of the probes' plain packets.
//! Those come back through the reverse table, which recorded the
//! interface the plain packet arrived on and no peer, so the proof's
//! repeat carried `peer: None`, which a multi-peer interface reads as
//! "every live link". The peer is known when the entry is written: the
//! plain packet is being relayed out of `process_incoming_from_peer`,
//! with `ingress_peer` naming the link it came through.
//!
//! ## Reference semantics
//!
//! Python's `ble-reticulum` gives every peer its own sub-interface
//! (`BLEInterface._spawn_peer_interface`,
//! ble-reticulum/src/ble_reticulum/BLEInterface.py:1892), so the reverse
//! table's `IDX_RT_RCVD_IF` already names one peer and the proof reaches
//! that peer alone. The peer field carries the same statement on our one
//! shared interface.
//!
//! ## Shape
//!
//! 1 node, deterministic, sub-second. The observable is the `peer` field
//! of the forwarded proof's `SendPacket` action.

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
    truncated_packet_hash, HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType,
    TransportType,
};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{Action, InterfaceId, TickOutput};

type EmbeddedNode = NodeCore<OsRng, MockClock, EmbeddedStorage>;

/// The sender's link and the receiver's link, as the BLE interface
/// reports them on peer-up.
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

/// The receiver's destination and one direct announce for it.
fn receiver_announce() -> (crate::DestinationHash, Vec<u8>) {
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["reversehint"],
    )
    .unwrap();
    let dest_hash = *dest.hash();
    let ann = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .unwrap();
    (dest_hash, pack(&ann))
}

/// The sender's plain data packet for `dest`, addressed to the relay
/// `transport_id`, as it leaves the sender (wire hops 0).
fn plain_packet_via(
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
            packet_type: PacketType::Data,
        },
        hops: 0,
        transport_id: Some(transport_id),
        destination_hash: *dest.as_bytes(),
        context: PacketContext::None,
        data: PacketData::Owned(std::vec![0x5C; 48]),
    })
}

/// The receiver's proof of the packet whose truncated hash is `proved`.
fn proof_of(proved: &[u8; TRUNCATED_HASHBYTES]) -> Vec<u8> {
    pack(&Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Single,
            packet_type: PacketType::Proof,
        },
        hops: 0,
        transport_id: None,
        destination_hash: *proved,
        context: PacketContext::None,
        data: PacketData::Owned(std::vec![0x77; 96]),
    })
}

/// Every send of a packet addressed to `dest`, as (interface, hint).
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

/// The relay in 573's shape: the receiver's path learned on the
/// receiver's link, the sender's plain packet relayed through it.
/// Returns the relay, its interfaces and the plain packet's reverse key.
struct Relayed {
    node: Box<EmbeddedNode>,
    sender_iface: usize,
    receiver_iface: usize,
    proved: [u8; TRUNCATED_HASHBYTES],
}

fn relay_a_plain_packet(two_interfaces: bool, from_peer: bool) -> Relayed {
    let mut node = make_node();
    let sender_iface = add_iface(&mut node, "ble_0", 0);
    let receiver_iface = if two_interfaces {
        add_iface(&mut node, "ble_1", 1)
    } else {
        sender_iface
    };

    let (dest, announce) = receiver_announce();
    let _ = node.handle_packet_from_peer(InterfaceId(receiver_iface), RECEIVER, &announce);

    let own_id = *node.identity().hash();
    let plain = plain_packet_via(own_id, &dest);
    let proved = truncated_packet_hash(&plain);
    let out = if from_peer {
        node.handle_packet_from_peer(InterfaceId(sender_iface), SENDER, &plain)
    } else {
        node.handle_packet(InterfaceId(sender_iface), &plain)
    };
    assert_eq!(
        sends_to(&out, dest.as_bytes()),
        std::vec![(InterfaceId(receiver_iface), Some(RECEIVER))],
        "the plain packet itself is path-table traffic and already names the \
         receiver (#376); without it this test measures nothing"
    );
    Relayed {
        node,
        sender_iface,
        receiver_iface,
        proved,
    }
}

/// THE 573 remainder, one relay wide: the receiver's proof of the
/// sender's plain packet, repeated on the one BLE interface both links
/// share, names the sender's link and nothing else.
#[test]
fn a_relayed_proof_of_a_plain_packet_names_the_senders_link() {
    let mut r = relay_a_plain_packet(false, true);
    let out = r.node.handle_packet_from_peer(
        InterfaceId(r.receiver_iface),
        RECEIVER,
        &proof_of(&r.proved),
    );
    assert_eq!(
        sends_to(&out, &r.proved),
        std::vec![(InterfaceId(r.sender_iface), Some(SENDER))],
        "a reverse-table repeat must name the sender's link; with no hint the \
         BLE interface floods it onto the receiver's link too (573: two of \
         the relay's ten floods per transfer)"
    );
}

/// Across two interfaces the proof still names the peer behind the
/// interface the plain packet came in on.
#[test]
fn across_two_interfaces_the_proof_names_the_peer_it_returns_to() {
    let mut r = relay_a_plain_packet(true, true);
    let out = r.node.handle_packet_from_peer(
        InterfaceId(r.receiver_iface),
        RECEIVER,
        &proof_of(&r.proved),
    );
    assert_eq!(
        sends_to(&out, &r.proved),
        std::vec![(InterfaceId(r.sender_iface), Some(SENDER))]
    );
}

/// The hint is read, never invented: a plain packet that arrived without
/// a named peer leaves no peer in the reverse entry, so its proof goes
/// out unaddressed, exactly as before.
#[test]
fn a_proof_of_a_plain_packet_with_no_named_peer_carries_no_hint() {
    let mut r = relay_a_plain_packet(false, false);
    let out = r.node.handle_packet_from_peer(
        InterfaceId(r.receiver_iface),
        RECEIVER,
        &proof_of(&r.proved),
    );
    assert_eq!(
        sends_to(&out, &r.proved),
        std::vec![(InterfaceId(r.sender_iface), None)]
    );
}
