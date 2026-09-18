//! #158 mvr: a destination's CONFIGURED default announce app data must reach
//! the wire on internally generated announces — above all the path response.
//!
//! The reference lets an application park app_data on the destination
//! (`Destination.set_default_app_data`, Destination.py:667-675) and applies it
//! whenever `announce()` is called without explicit app_data
//! (Destination.py:289-293). `Transport.path_request` answers for a local
//! destination with exactly such a bare `destination.announce(path_response=
//! True)` (Transport.py:2938-2941), so the default is what a peer that asked
//! for a path reads out of the response.
//!
//! Before this pin the only way to seed the default here was to announce once
//! WITH explicit app_data (the #151 remember-the-last-announce deviation,
//! pinned by `own_destination_path_response_is_a_fresh_regeneration` in
//! mvr_generated_field_pins.rs). A destination that had never announced — the
//! common case for a service that comes up and waits to be asked for — could
//! not carry application metadata in its path responses at all.
//!
//! Every test reads the app_data out of the actual response wire bytes and
//! also re-checks the two properties the response must not lose while gaining
//! app data: the PATH_RESPONSE context (peers suppress rebroadcast on it,
//! Transport.py:1886) and the targeting at the requesting interface.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::TRUNCATED_HASHBYTES;
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Clock;
use crate::transport::{Action, InterfaceId};

/// KAT from the vendored reference:
/// RNS.Destination.hash(None, "rnstransport", "path", "request").
const PATH_REQUEST_DEST_KAT: [u8; TRUNCATED_HASHBYTES] = [
    0x6b, 0x9f, 0x66, 0x01, 0x4d, 0x98, 0x53, 0xfa, 0xab, 0x22, 0x0f, 0xba, 0x47, 0xd0, 0x27, 0x61,
];

type PinNode = NodeCore<OsRng, MockClock, MemoryStorage>;

/// A node with one interface and one registered SINGLE IN destination.
fn node_with_destination(aspect: &'static str) -> (PinNode, usize, crate::DestinationHash) {
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node: PinNode =
        NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());
    let iface = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new("if0", 0)));
    node.set_interface_name(iface, String::from("if0"));

    let dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "defappdata",
        &[aspect],
    )
    .unwrap();
    let dest_hash = *dest.hash();
    node.register_destination(dest);
    (node, iface, dest_hash)
}

/// Inject a path request for `dest_hash` on `iface`, then advance past the
/// path-request grace and return the packets the node emitted on `iface`.
///
/// `tag` must differ between two requests in one test: a repeated
/// destination+tag pair is dropped as a duplicate, here and in the reference
/// (`discovery_pr_tags`, Transport.py:2893-2906).
fn ask_for_path(
    node: &mut PinNode,
    iface: usize,
    dest_hash: &crate::DestinationHash,
    tag: u8,
) -> Vec<Vec<u8>> {
    let mut pr_data = Vec::new();
    pr_data.extend_from_slice(dest_hash.as_bytes());
    pr_data.extend_from_slice(&[tag; TRUNCATED_HASHBYTES]);
    let request = Packet {
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
        destination_hash: PATH_REQUEST_DEST_KAT,
        context: PacketContext::None,
        data: PacketData::Owned(pr_data),
    };
    let mut buf = [0u8; crate::constants::MTU];
    let len = request.pack(&mut buf).unwrap();
    let _ = node.handle_packet(InterfaceId(iface), &buf[..len]);

    let now = node.transport().clock().now_ms();
    node.transport()
        .clock()
        .set(now + crate::constants::PATH_REQUEST_GRACE_MS + 1);
    let out = node.handle_timeout();
    out.actions
        .iter()
        .filter_map(|a| match a {
            Action::SendPacket { iface: i, data, .. } if i.0 == iface => Some(data.clone()),
            _ => None,
        })
        .collect()
}

/// Announce app_data of a Type1 announce payload: pk(64) + name(10) +
/// random(10) + signature(64), app_data is the remainder (Destination.py:296).
fn announce_app_data(wire: &[u8]) -> Vec<u8> {
    wire[19 + 148..].to_vec()
}

/// The configured default reaches the path response of a destination that has
/// never announced, and the response keeps its context and its targeting.
#[test]
fn path_response_carries_configured_default_app_data() {
    let (mut node, iface, dest_hash) = node_with_destination("configured");
    let default = b"service=chat;v=3";
    node.destination_mut(&dest_hash)
        .unwrap()
        .set_default_app_data(default);
    // Deliberately NO announce: the default must not need an announce to seed
    // it (that was the whole gap — Destination.py:667 is a plain setter).

    let responses = ask_for_path(&mut node, iface, &dest_hash, 0x5A);
    assert_eq!(
        responses.len(),
        1,
        "exactly one path response, targeted at the requesting interface \
         (Transport.py:2938-2941 answers the asking interface, not a broadcast)"
    );
    let parsed = Packet::unpack(&responses[0]).unwrap();
    assert_eq!(parsed.flags.packet_type, PacketType::Announce);
    assert_eq!(
        parsed.context,
        PacketContext::PathResponse,
        "peers suppress rebroadcast on this context byte (Transport.py:1886)"
    );
    assert_eq!(
        &parsed.destination_hash,
        dest_hash.as_bytes(),
        "the response must announce the requested destination"
    );
    assert_eq!(
        announce_app_data(&responses[0]),
        default.to_vec(),
        "a bare announce must fall back to the configured default \
         (Destination.py:289-293)"
    );
}

/// Explicit app_data wins over the configured default, in the same announce
/// and on the wire (Destination.py:289 — the fallback is guarded on
/// `app_data == None`).
#[test]
fn explicit_app_data_overrides_the_configured_default() {
    let (mut node, iface, dest_hash) = node_with_destination("override");
    node.destination_mut(&dest_hash)
        .unwrap()
        .set_default_app_data(b"the-default");

    let explicit = b"the-explicit-one";
    let out = node
        .announce_destination(&dest_hash, Some(explicit))
        .unwrap();
    let announce = out
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .next()
        .expect("announce must be emitted");
    assert_eq!(
        announce_app_data(&announce),
        explicit.to_vec(),
        "explicit app_data must take precedence over the default"
    );

    // And the explicit payload is what the subsequent path response carries:
    // our deviation from the reference is that an explicit announce also
    // becomes the new default (#151, destination.rs), so the peer that asks
    // for a path afterwards sees the CURRENT announce, not the stale default.
    let responses = ask_for_path(&mut node, iface, &dest_hash, 0x5A);
    assert_eq!(
        announce_app_data(&responses[0]),
        explicit.to_vec(),
        "the path response reproduces the last explicit announce, not the \
         default it replaced"
    );
}

/// Clearing the default returns the destination to an empty-app_data announce.
#[test]
fn clearing_the_default_yields_empty_announce_app_data() {
    let (mut node, iface, dest_hash) = node_with_destination("cleared");
    {
        let dest = node.destination_mut(&dest_hash).unwrap();
        dest.set_default_app_data(b"transient-metadata");
        assert_eq!(dest.default_app_data(), Some(&b"transient-metadata"[..]));
        dest.clear_default_app_data();
        assert_eq!(
            dest.default_app_data(),
            None,
            "clear must drop the default (Destination.py:677-681)"
        );
    }

    let responses = ask_for_path(&mut node, iface, &dest_hash, 0x5A);
    assert_eq!(
        responses.len(),
        1,
        "a cleared default still leaves a path response to send"
    );
    let parsed = Packet::unpack(&responses[0]).unwrap();
    assert_eq!(
        parsed.context,
        PacketContext::PathResponse,
        "clearing app data must not change the response context"
    );
    assert!(
        announce_app_data(&responses[0]).is_empty(),
        "with no default and no explicit data the announce carries empty \
         application data"
    );
}

/// An oversized default must fail at compose time: no packet on the wire, no
/// panic, and the node keeps serving. The budget check lives in
/// `Destination::announce` because it depends on whether a ratchet rides
/// along, which is only known at announce time.
#[test]
fn oversized_default_app_data_emits_nothing() {
    let (mut node, iface, dest_hash) = node_with_destination("oversized");
    let budget = crate::announce::announce_app_data_budget(false);
    let oversized = std::vec![0xA5u8; budget + 1];
    node.destination_mut(&dest_hash)
        .unwrap()
        .set_default_app_data(&oversized);

    // The destination itself refuses, with the byte counts the caller needs.
    let err = node
        .announce_destination(&dest_hash, None)
        .expect_err("an over-budget announce must be refused");
    std::println!("oversized announce refused with {err:?}");

    let responses = ask_for_path(&mut node, iface, &dest_hash, 0x5A);
    assert!(
        responses.is_empty(),
        "an over-budget default must produce silence, never a truncated or \
         otherwise invalid announce"
    );

    // The node is still alive and answers normally once the default fits.
    node.destination_mut(&dest_hash)
        .unwrap()
        .set_default_app_data(b"fits");
    let responses = ask_for_path(&mut node, iface, &dest_hash, 0x6B);
    assert_eq!(
        announce_app_data(&responses[0]),
        b"fits".to_vec(),
        "the destination recovers as soon as the default fits the budget"
    );
}
