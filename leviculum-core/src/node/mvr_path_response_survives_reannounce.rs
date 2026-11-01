//! 1262 mvr: a path response waiting out its grace must survive a newer
//! announce of the same destination.
//!
//! ## The defect
//!
//! A transport node answers a path request for a known destination by
//! putting a targeted response entry in the announce table, fired once after
//! `PATH_REQUEST_GRACE_MS` on the requesting interface. When a NEWER
//! emission of the same destination arrives inside that grace,
//! `handle_announce` wrote its rebroadcast entry over the response entry,
//! keyed by the same destination hash. The rebroadcast that fires instead
//! goes on every interface but the access point ones (an access point port
//! hears no announces, Transport.py:1054-1061), so a phone on an access
//! point port that asked for the path never got it. The reference has the
//! same overwrite (Transport.py:1898 inserts unconditionally); its hold of
//! `held_announces` (:2991-2999) covers only the opposite order, a request
//! landing on a pending rebroadcast (#170, [`super::mvr_announce_hold`]).
//!
//! Measured in loquax-android's `home_reach` cells on two CPUs (1262): the
//! mailbox announces its home destination at start and again the moment
//! its rendezvous connection is up, the phone asks for the path the moment
//! its own connection is up, and when the second announce lands within the
//! 400 ms grace of the answer the phone waits for a path that never comes.
//!
//! ## The fix, and why it is a permitted deviation
//!
//! The response entry stays, carrying the newer emission (the path table
//! already moved to it), and the new rebroadcast entry goes to the held slot
//! the #170 hold uses, reinserted when the response fires. Wire format
//! untouched; the requester gets the answer the reference would have
//! dropped, and the rest of the mesh still gets the rebroadcast.
//!
//! Sans-I/O: one node, two mock interfaces, deterministic, sub-second.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, PATH_REQUEST_GRACE_MS, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Clock;
use crate::transport::{Action, InterfaceId, TickOutput};

type TransportNode = NodeCore<OsRng, MockClock, MemoryStorage>;

fn add_iface(node: &mut TransportNode, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    idx
}

fn make_transport_node() -> TransportNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        clock,
        MemoryStorage::with_defaults(),
    )
}

fn pack(packet: &Packet) -> Vec<u8> {
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

/// A path request for `dest`, addressed to `path_req_hash`.
fn build_path_request(
    path_req_hash: &[u8; TRUNCATED_HASHBYTES],
    dest: &[u8; TRUNCATED_HASHBYTES],
) -> Vec<u8> {
    let mut data = Vec::with_capacity(48);
    data.extend_from_slice(dest);
    data.extend_from_slice(&[0xBB; 16]);
    data.extend_from_slice(&[0xA1; 16]);
    pack(&Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Plain,
            packet_type: PacketType::Data,
        },
        hops: 0,
        transport_id: None,
        destination_hash: *path_req_hash,
        context: PacketContext::None,
        data: PacketData::Owned(data),
    })
}

/// Targeted path responses for `dest` on `iface`, with their packets.
fn targeted_responses(
    out: &TickOutput,
    iface: usize,
    dest: &[u8; TRUNCATED_HASHBYTES],
) -> Vec<Vec<u8>> {
    out.actions
        .iter()
        .filter_map(|a| match a {
            Action::SendPacket { iface: i, data, .. } if i.0 == iface => Packet::unpack(data)
                .ok()
                .filter(|p| {
                    p.flags.packet_type == PacketType::Announce
                        && p.context == PacketContext::PathResponse
                        && &p.destination_hash == dest
                })
                .map(|_| data.clone()),
            _ => None,
        })
        .collect()
}

/// Network-wide announce rebroadcasts for `dest`.
fn broadcasts(out: &TickOutput, dest: &[u8; TRUNCATED_HASHBYTES]) -> usize {
    out.actions
        .iter()
        .filter(|a| match a {
            Action::Broadcast { data, .. } => Packet::unpack(data).is_ok_and(|p| {
                p.flags.packet_type == PacketType::Announce
                    && p.context == PacketContext::None
                    && &p.destination_hash == dest
            }),
            _ => false,
        })
        .count()
}

/// The order the red cell had: the first announce, the request 100 ms
/// later, the second emission 1 ms after that. The requester still gets its
/// one targeted response, carrying the second emission, and the mesh still
/// gets the second emission's rebroadcast afterwards.
#[test]
fn a_newer_announce_inside_the_grace_keeps_the_response() {
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["reannounce"],
    )
    .unwrap();
    let dest_hash = *dest.hash().as_bytes();
    let first = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .unwrap();
    let second = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000 + 1)
        .unwrap();

    let mut relay = make_transport_node();
    let iface_mailbox = add_iface(&mut relay, "mailbox");
    let iface_phone = add_iface(&mut relay, "phone");
    let t0 = relay.transport().clock().now_ms();

    let _ = relay.handle_packet(InterfaceId(iface_mailbox), &pack(&first));
    relay.transport().clock().set(t0 + 100);
    let path_req_hash = *relay.transport().path_request_hash();
    let _ = relay.handle_packet(
        InterfaceId(iface_phone),
        &build_path_request(&path_req_hash, &dest_hash),
    );
    relay.transport().clock().set(t0 + 101);
    let _ = relay.handle_packet(InterfaceId(iface_mailbox), &pack(&second));

    let mut responses = Vec::new();
    let mut rebroadcasts = 0;
    for step in 1..=200u64 {
        relay.transport().clock().set(t0 + 101 + step * 50);
        let out = relay.handle_timeout();
        responses.extend(targeted_responses(&out, iface_phone, &dest_hash));
        rebroadcasts += broadcasts(&out, &dest_hash);
    }
    assert_eq!(
        responses.len(),
        1,
        "the requester gets its one targeted response although a newer \
         announce arrived inside the {PATH_REQUEST_GRACE_MS} ms grace"
    );
    let answered = Packet::unpack(&responses[0]).unwrap();
    let second_raw = pack(&second);
    let sent = Packet::unpack(&second_raw).unwrap();
    assert_eq!(
        answered.data.as_slice(),
        sent.data.as_slice(),
        "the response carries the newer emission, the one the path table holds"
    );
    assert!(
        rebroadcasts >= 1,
        "the newer emission's rebroadcast still goes out to the mesh"
    );
}
