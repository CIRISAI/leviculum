//! #180 mvr: an inbound PATH_RESPONSE must not cancel a pending announce
//! rebroadcast for the same destination.
//!
//! ## The defect
//!
//! `handle_announce` used to insert EVERY path-table-updating announce into
//! the announce table, PATH_RESPONSE context included. Such an entry is
//! inert (`retransmit_at_ms = None`, `block_rebroadcasts = true`), and
//! `set_announce` overwrites by destination hash — so a targeted response
//! arriving from upstream silently replaced a LIVE entry whose network-wide
//! rebroadcast was still scheduled, and the mesh never learned the path from
//! us. The reference inserts nothing for PATH_RESPONSE context outside its
//! pending-local-forward branch (Transport.py:1886 gates on
//! `packet.context != RNS.Packet.PATH_RESPONSE`, :1910-1930 is the only
//! exception).
//!
//! ## Reachability
//!
//! The per-destination announce-rate window (`ANNOUNCE_RATE_LIMIT_MS`, 2 s)
//! shields an entry younger than 2 s: inside it `rate_limited` is true and
//! the insert was skipped anyway. The reachable case is therefore an entry
//! OLDER than 2 s that still has a retry scheduled, which the retry backoff
//! produces routinely: the first fire reschedules at
//! `now + PATHFINDER_G_MS` (5 s) without touching `timestamp_ms`, leaving a
//! ~3 s window in which the entry is both unshielded and live. That window
//! is what this test lands the path response in.
//!
//! ## What is pinned
//!
//! The second, network-wide rebroadcast still goes out after a PATH_RESPONSE
//! for the same destination arrived in that window — and the response
//! itself, being unsolicited here, adds no announce-table entry of its own.
//!
//! Fixed by 6a57627a (landed for #255); this file pins the clobber case,
//! which the pre-existing `test_announce_rate_path_response_exempt` does not
//! reach (it has no live entry to clobber).
//!
//! Sans-I/O: 1 node, 2 mock interfaces, deterministic, sub-second.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{ANNOUNCE_RATE_LIMIT_MS, MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{Packet, PacketContext, PacketType};
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

/// Pack `packet` with `context` stamped on it. The context byte is outside
/// the announce signature, so a plain announce can be turned into a path
/// response without re-signing it — which is exactly how an upstream node's
/// targeted response differs from the broadcast copy.
fn pack_with_context(mut packet: Packet, context: PacketContext) -> Vec<u8> {
    packet.context = context;
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

/// Network-wide announce rebroadcasts for `dest` in this tick's output.
fn broadcast_rebroadcasts(out: &TickOutput, dest: &[u8; TRUNCATED_HASHBYTES]) -> usize {
    out.actions
        .iter()
        .filter(|a| match a {
            Action::Broadcast { data, .. } => Packet::unpack(data)
                .map(|p| {
                    p.flags.packet_type == PacketType::Announce
                        && p.context == PacketContext::None
                        && &p.destination_hash == dest
                })
                .unwrap_or(false),
            _ => false,
        })
        .count()
}

/// THE bug (#180): the inert PATH_RESPONSE entry overwrites a live one.
#[test]
fn unsolicited_path_response_keeps_pending_rebroadcast() {
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["prclobber"],
    )
    .unwrap();
    let dest_hash = *dest.hash().as_bytes();
    let emission = TEST_TIME_MS / 1000;

    let mut relay = make_transport_node();
    let iface_a = add_iface(&mut relay, "A_announce_in");
    let iface_b = add_iface(&mut relay, "B_upstream");

    let t0 = relay.transport().clock().now_ms();

    // The announce arrives on A: entry scheduled for two network-wide
    // rebroadcasts (retries 0 then 1).
    let announce = dest.announce(None, &mut OsRng, t0, emission).unwrap();
    let raw = pack_with_context(announce, PacketContext::None);
    let _ = relay.handle_packet(InterfaceId(iface_a), &raw);

    // First rebroadcast fires inside the jitter window; the entry is
    // rescheduled PATHFINDER_G_MS out, its timestamp_ms still t0.
    relay
        .transport()
        .clock()
        .set(t0 + relay.transport().announce_jitter_max_ms() + 1);
    let out = relay.handle_timeout();
    assert_eq!(
        broadcast_rebroadcasts(&out, &dest_hash),
        1,
        "first rebroadcast must fire within the jitter window"
    );

    // Now the reachable window: past the 2 s rate shield, before the second
    // rebroadcast is due. An upstream node's targeted PATH_RESPONSE for the
    // same destination arrives on B, carrying a newer emission so it updates
    // the path table (the condition under which the insert used to run).
    let t_pr = t0 + ANNOUNCE_RATE_LIMIT_MS + 1_000;
    assert!(
        t_pr < t0 + crate::constants::PATHFINDER_G_MS,
        "the path response must land while the retry is still scheduled"
    );
    relay.transport().clock().set(t_pr);
    let response = dest.announce(None, &mut OsRng, t_pr, emission + 1).unwrap();
    let response_raw = pack_with_context(response, PacketContext::PathResponse);
    let out = relay.handle_packet(InterfaceId(iface_b), &response_raw);
    assert_eq!(
        broadcast_rebroadcasts(&out, &dest_hash),
        0,
        "a path response is not itself rebroadcast"
    );

    // The pending rebroadcast must have survived it.
    relay.transport().clock().set(t0 + 100_000);
    let out = relay.handle_timeout();
    assert_eq!(
        broadcast_rebroadcasts(&out, &dest_hash),
        1,
        "the scheduled network-wide rebroadcast must survive an inbound \
         PATH_RESPONSE for the same destination: the reference inserts no \
         announce-table entry for PATH_RESPONSE context (Transport.py:1886), \
         so there is nothing to clobber it with — Codeberg #180"
    );
}
