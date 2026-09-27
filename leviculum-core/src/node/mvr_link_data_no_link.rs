//! mvr: link-addressed DATA for a link no local endpoint holds dies without
//! a counter (#433 follow-up, order 368).
//!
//! Lane-2 measurement 2026-09-27 (order 370, `orphan_link_initiator_*`): a
//! responder's LXMF helper restarts mid-link, the initiator keeps sending on
//! the old link id for ~10 s (3 DATA packets in both arms) before its proof
//! timeout tears the link down. At the restarted responder every one of
//! those packets fell through transport's link-table check (not a relayed
//! link), matched no registered destination at the node layer, and vanished
//! with a trace line only — `lrproof_no_link=0` and `no_path=0` on both
//! daemons, in every run. The field day of 2026-09-27 asked the same
//! question at the base: whatever arrives on a link nobody owns is dropped
//! without a counter, at several layers.
//!
//! Host model (sans-I/O, deterministic, < 5 s): the failure mode is purely
//! flags-level — dest type LINK, packet type DATA, an id no `links` entry
//! and no `destinations` entry claims. The payload is opaque ciphertext to
//! every layer that participates in the drop, so a crafted packet reproduces
//! exactly the failure and nothing more.
//!
//! Two tests:
//!   * `link_data_for_unheld_link_is_counted` — the reproduction. RED before
//!     the `link-data-no-link` counter existed: the packet vanished with
//!     `packets_dropped` unchanged. GREEN with the counter.
//!   * `relayed_link_data_is_not_counted` — the relay control: a transport
//!     node holding a link-table entry forwards the same shape of packet and
//!     the new counter stays at zero (a relayed link is owned by its
//!     endpoints, not by the relay).

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{NoStorage, Storage};
use crate::transport::{Action, DropReason, InterfaceId, TickOutput};

type TransportNode = NodeCore<OsRng, MockClock, MemoryStorage>;
type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

fn add_iface<C, S>(
    node: &mut NodeCore<OsRng, C, S>,
    name: &'static str,
    local_client: bool,
) -> usize
where
    C: crate::traits::Clock,
    S: crate::traits::Storage,
{
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    if local_client {
        node.set_interface_local_client(idx, true);
    }
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

/// A link DATA packet as an initiator emits it on an established link:
/// HEADER_1 broadcast, dest type LINK, context None, opaque payload.
fn link_data_packet(link_id: [u8; 16], payload_len: usize) -> Vec<u8> {
    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Link,
            packet_type: PacketType::Data,
        },
        hops: 0,
        transport_id: None,
        destination_hash: link_id,
        context: PacketContext::None,
        data: PacketData::Owned(std::vec![0x5a; payload_len]),
    };
    let mut buf = [0u8; crate::constants::MTU];
    let len = packet.pack(&mut buf).expect("pack link data packet");
    buf[..len].to_vec()
}

/// The reproduction: an endpoint that holds no such link receives link DATA.
/// Before the fix the packet died at the node layer's destination dispatch
/// with a trace line and no counter.
#[test]
fn link_data_for_unheld_link_is_counted() {
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node: EndpointNode = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
    let iface = add_iface(&mut node, "wire", false);

    // An id that is provably nobody's: this process created no link, and the
    // node has no registered destination at all.
    let orphan_link_id = [0x6bu8; 16];

    let dropped_before = node.transport().stats().packets_dropped();
    let out = node.handle_packet(InterfaceId(iface), &link_data_packet(orphan_link_id, 48));

    assert!(
        action_data(&out).is_empty(),
        "an endpoint must not answer or forward link data it cannot place"
    );
    assert!(
        !out.events
            .iter()
            .any(|e| matches!(e, NodeEvent::PacketReceived { .. })),
        "no application delivery may happen for a link nobody holds"
    );
    assert_eq!(
        node.transport().stats().drops_link_data_no_link(),
        1,
        "link data for an unheld link must be counted under its own named \
         reason (lane-2 order 370: 3 post-restart DATA packets per arm, zero \
         counted anywhere)"
    );
    assert_eq!(
        node.transport().stats().packets_dropped(),
        dropped_before + 1,
        "the named drop must also reach the grand total (record_drop invariant)"
    );
    assert_eq!(
        DropReason::LinkDataNoLink.kebab(),
        "link-data-no-link",
        "the reason must have a stable name for the PKT_DROP event catalogue"
    );
}

/// The relay control: a transport node that holds a link-table entry for the
/// id repeats the packet and counts nothing — the relay does not own the
/// link, but somebody does, and a repeat is not a drop.
#[test]
fn relayed_link_data_is_not_counted() {
    // Responder owning a link-accepting destination, announced to the relay.
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let clock = MockClock::new(TEST_TIME_MS);
    let mut responder: EndpointNode = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["orphanlink"],
    )
    .expect("destination");
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    let announce = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .expect("announce");
    let mut buf = [0u8; crate::constants::MTU];
    let len = announce.pack(&mut buf).expect("pack announce");
    let announce_raw = buf[..len].to_vec();
    responder.register_destination(dest);

    let clock = MockClock::new(TEST_TIME_MS);
    let mut relay: TransportNode = NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        clock,
        MemoryStorage::with_defaults(),
    );
    let clock = MockClock::new(TEST_TIME_MS);
    let mut initiator: EndpointNode = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

    let near = add_iface(&mut relay, "near_initiator", true);
    let far = add_iface(&mut relay, "far_responder", false);
    let _i = add_iface(&mut initiator, "to_relay", false);

    let _ = relay.handle_packet(InterfaceId(far), &announce_raw);
    assert_eq!(relay.hops_to(&dest_hash), Some(1));

    // The link request through the relay freezes the link-table entry the
    // DATA below is routed by.
    let (link_id, _routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");
    let request = action_data(&out)
        .into_iter()
        .next()
        .expect("initiator emits the link request");
    let out = relay.handle_packet(InterfaceId(near), &request);
    assert_eq!(
        action_data(&out).len(),
        1,
        "the relay must forward the link request"
    );
    assert!(
        relay
            .transport()
            .storage()
            .get_link_entry(link_id.as_bytes())
            .is_some(),
        "the relay must hold a link-table entry after forwarding the request"
    );

    // Link DATA from the initiator side: repeated toward the responder,
    // counted as nothing.
    let out = relay.handle_packet(
        InterfaceId(near),
        &link_data_packet(*link_id.as_bytes(), 48),
    );
    assert_eq!(
        action_data(&out).len(),
        1,
        "the relay must repeat link data for an entry it holds"
    );
    assert_eq!(
        relay.transport().stats().drops_link_data_no_link(),
        0,
        "a relayed link is not an unheld link — the counter must not tick"
    );
    assert_eq!(
        relay.transport().stats().packets_forwarded_link(),
        1,
        "the repeat must show up as the link-addressed share of the forwards \
         (the firmware's `link_fwd=` field, order 368)"
    );
}
