//! mvr: a relayed link packet names the peer it is for (Codeberg #573,
//! from #444).
//!
//! ## The field failure this reproduces
//!
//! periculum 570, `ble_room_transfer_relayed` on btvirt, relayed run 1,
//! node 1: the relay holds ONE BLE interface with two links, the sender's
//! and the receiver's. The LinkRequest left as
//! `LINK_ENTRY_SET recv=ble_0 next_hop=ble_0`, and every one of the 342
//! link-table repeats that followed (340 resource parts and requests on
//! the link id, two proofs) went out as `BLE_TX_FLOOD links=2`, never
//! once as `BLE_TX_ROUTE`. The sender got its own parts back and the
//! relay spent half its paced BLE airtime on copies.
//!
//! #376 taught the core to name the peer for path-table traffic
//! (`PathEntry::via_peer`). The link table named an interface and no
//! peer, so a link-table repeat carried `peer: None`, which a multi-peer
//! interface reads as "every live link". Both peers are known when the
//! entry is written: the LinkRequest arrives through the initiator's
//! link (`ingress_peer`) and leaves on the path's `via_peer`.
//!
//! ## Reference semantics
//!
//! Python's `ble-reticulum` gives every peer its own sub-interface
//! (`BLEInterface._spawn_peer_interface`,
//! ble-reticulum/src/ble_reticulum/BLEInterface.py:1892), so the link
//! table's `IDX_LT_NH_IF` and `IDX_LT_RCVD_IF` already name one peer
//! each and a repeat reaches that peer alone. The two peer fields here
//! carry the same statement on our one shared interface. On one shared
//! interface the hop counts cannot tell the direction apart (both are 1
//! on a two-hop BLE chain); the peer the packet arrived through can.
//!
//! ## Shape
//!
//! 1 node, deterministic, sub-second. The observable is the `peer` field
//! of the forwarded `SendPacket` actions.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::embedded_storage::EmbeddedStorage;
use crate::identity::Identity;
use crate::link::Link;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
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

/// The receiver's destination and one direct announce for it.
fn receiver_announce() -> (crate::DestinationHash, Vec<u8>) {
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["linkhint"],
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

fn pack(packet: &Packet) -> Vec<u8> {
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

/// The sender's LinkRequest for `dest`, addressed to the relay
/// `transport_id`, as it leaves the sender (wire hops 0).
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

/// One packet on the link, as either endpoint puts it on the wire.
fn on_link(link_id: &[u8; TRUNCATED_HASHBYTES], packet_type: PacketType, seed: u8) -> Vec<u8> {
    pack(&Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Link,
            packet_type,
        },
        hops: 0,
        transport_id: None,
        destination_hash: *link_id,
        context: match packet_type {
            PacketType::Proof => PacketContext::ResourcePrf,
            _ => PacketContext::Resource,
        },
        data: PacketData::Owned(std::vec![seed; 96]),
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

/// The relay in 570's shape: the receiver's path learned on the
/// receiver's link, the sender's LinkRequest relayed through it.
/// Returns the relay, its interfaces and the link id.
struct Relayed {
    node: Box<EmbeddedNode>,
    sender_iface: usize,
    receiver_iface: usize,
    link_id: [u8; TRUNCATED_HASHBYTES],
}

fn relay_a_link(two_interfaces: bool) -> Relayed {
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
    let request = link_request_via(own_id, &dest);
    let link_id = *Link::calculate_link_id(&request).as_bytes();
    let out = node.handle_packet_from_peer(InterfaceId(sender_iface), SENDER, &request);
    assert_eq!(
        sends_to(&out, dest.as_bytes()),
        std::vec![(InterfaceId(receiver_iface), Some(RECEIVER))],
        "the LinkRequest itself is path-table traffic and already names the \
         receiver (#376); without it this test measures nothing"
    );
    Relayed {
        node,
        sender_iface,
        receiver_iface,
        link_id,
    }
}

/// THE 570 capture, one relay wide: a resource part from the sender,
/// repeated on the one BLE interface both links share, names the
/// receiver's link and nothing else.
#[test]
fn a_link_packet_from_the_sender_names_the_receivers_link() {
    let mut r = relay_a_link(false);
    let out = r.node.handle_packet_from_peer(
        InterfaceId(r.sender_iface),
        SENDER,
        &on_link(&r.link_id, PacketType::Data, 1),
    );
    assert_eq!(
        sends_to(&out, &r.link_id),
        std::vec![(InterfaceId(r.receiver_iface), Some(RECEIVER))],
        "a link-table repeat must name the receiver's link; with no hint the \
         BLE interface floods it onto the sender's link too (570, node 1: \
         342 repeats, all BLE_TX_FLOOD links=2)"
    );
}

/// The other direction on the same interface: the receiver's resource
/// request and its proofs go back to the sender alone.
#[test]
fn a_link_packet_from_the_receiver_names_the_senders_link() {
    let mut r = relay_a_link(false);
    for packet_type in [PacketType::Data, PacketType::Proof] {
        let out = r.node.handle_packet_from_peer(
            InterfaceId(r.receiver_iface),
            RECEIVER,
            &on_link(&r.link_id, packet_type, 2),
        );
        assert_eq!(
            sends_to(&out, &r.link_id),
            std::vec![(InterfaceId(r.sender_iface), Some(SENDER))],
            "{packet_type:?} from the receiver must name the sender's link"
        );
    }
}

/// Across two interfaces the direction comes from the interface, and the
/// hint still names the peer behind the outbound one.
#[test]
fn across_two_interfaces_the_repeat_names_the_peer_behind_the_outbound_one() {
    let mut r = relay_a_link(true);
    let out = r.node.handle_packet_from_peer(
        InterfaceId(r.sender_iface),
        SENDER,
        &on_link(&r.link_id, PacketType::Data, 3),
    );
    assert_eq!(
        sends_to(&out, &r.link_id),
        std::vec![(InterfaceId(r.receiver_iface), Some(RECEIVER))]
    );
    let out = r.node.handle_packet_from_peer(
        InterfaceId(r.receiver_iface),
        RECEIVER,
        &on_link(&r.link_id, PacketType::Data, 4),
    );
    assert_eq!(
        sends_to(&out, &r.link_id),
        std::vec![(InterfaceId(r.sender_iface), Some(SENDER))]
    );
}

/// The hint is read, never invented: on one shared interface a link
/// packet that arrived without a named peer has no knowable direction,
/// so it goes out unaddressed, exactly as before.
#[test]
fn a_link_packet_with_no_named_peer_on_a_shared_interface_carries_no_hint() {
    let mut r = relay_a_link(false);
    let out = r.node.handle_packet(
        InterfaceId(r.sender_iface),
        &on_link(&r.link_id, PacketType::Data, 5),
    );
    assert_eq!(
        sends_to(&out, &r.link_id),
        std::vec![(InterfaceId(r.receiver_iface), None)]
    );
}

/// A peer the entry does not know (neither endpoint's link) on a shared
/// interface: no hint, never the wrong one.
#[test]
fn a_link_packet_from_a_third_peer_carries_no_hint() {
    let mut r = relay_a_link(false);
    let out = r.node.handle_packet_from_peer(
        InterfaceId(r.sender_iface),
        [0xC3; TRUNCATED_HASHBYTES],
        &on_link(&r.link_id, PacketType::Data, 6),
    );
    assert_eq!(
        sends_to(&out, &r.link_id),
        std::vec![(InterfaceId(r.receiver_iface), None)]
    );
}
