//! mvr: the initiator refuses a 96-byte LRPROOF that the reference accepts
//! and that our own relay already forwards (Codeberg #335, review finding
//! L-0013).
//!
//! A link proof carries `signature(64) + x25519_pub(32)`, optionally followed
//! by the three MTU/mode signalling bytes that arrived with Reticulum 1.0.
//! `Link.validate_proof` (`reference/Reticulum/RNS/Link.py:396-420`) accepts
//! BOTH shapes: it strips the signalling when it is there (`:404-408`), leaves
//! `signalling_bytes = b""` when it is not (`:399`), and gates the payload on
//! `len(packet.data) == SIGLENGTH//8 + ECPUBSIZE//2` — 96 — AFTER that strip.
//! A peer that predates the signalling therefore still establishes links
//! against a current Python node.
//!
//! Our initiator gated on `proof_data.len() < LINK_PROOF_SIZE` (99) and
//! returned `InvalidProof` for the 96-byte shape, which
//! `handle_link_proof` turns into a `LinkCloseReason::InvalidProof` teardown.
//! The same node's RELAY path has always accepted both
//! (`transport.rs`, the `LINK_PROOF_SIZE_MIN`/`MAX` gate, which mirrors
//! `Transport.py:2179-2191`) — so a board acting as transport would carry a
//! legacy peer's proof for someone else and refuse the identical bytes when
//! they were addressed to itself.
//!
//! Single node, no I/O, no timers: the initiator's own `connect()` output
//! supplies the link id, and the tests hand-build the responder side so the
//! proof shape is the only variable.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MODE_AES256_CBC, MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::link::{encode_signaling_bytes, LinkId};
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::{HeaderType, PacketContext, PacketFlags, PacketType, TransportType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// The responder half, held by the test so it can sign proofs of any shape.
struct Peer {
    identity: Identity,
    verifying_key: [u8; 32],
    dest_hash: crate::DestinationHash,
    ephemeral_public: x25519_dalek::PublicKey,
}

fn make_peer() -> Peer {
    let identity = Identity::generate(&mut OsRng);
    let verifying_key = identity.ed25519_verifying().to_bytes();
    let dest = Destination::new(
        Some(identity.clone()),
        Direction::In,
        DestinationType::Single,
        "lrproofshape",
        &["peer"],
    )
    .unwrap();
    let dest_hash = *dest.hash();
    let ephemeral_private = x25519_dalek::StaticSecret::random_from_rng(OsRng);
    Peer {
        identity,
        verifying_key,
        dest_hash,
        ephemeral_public: x25519_dalek::PublicKey::from(&ephemeral_private),
    }
}

fn make_initiator() -> (EndpointNode, usize) {
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new("i0", 0)));
    node.set_interface_name(idx, String::from("i0"));
    (node, idx)
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

/// Build an LRPROOF wire packet with `signalling` appended verbatim, the way
/// `Link.prove` composes it (`Link.py:371-378`): the signed data and the
/// payload carry the SAME trailing bytes, so an empty `signalling` yields the
/// 96-byte legacy shape and a 3-byte one the current shape.
fn legacy_proof_wire(peer: &Peer, link_id: &LinkId, signalling: &[u8]) -> Vec<u8> {
    let mut signed = Vec::new();
    signed.extend_from_slice(link_id.as_bytes());
    signed.extend_from_slice(peer.ephemeral_public.as_bytes());
    signed.extend_from_slice(&peer.verifying_key);
    signed.extend_from_slice(signalling);
    let signature = peer.identity.sign(&signed).expect("sign");

    let flags = PacketFlags {
        ifac_flag: false,
        header_type: HeaderType::Type1,
        context_flag: false,
        transport_type: TransportType::Broadcast,
        dest_type: DestinationType::Link,
        packet_type: PacketType::Proof,
    };

    let mut wire = Vec::new();
    wire.push(flags.to_byte());
    wire.push(0); // hops
    wire.extend_from_slice(link_id.as_bytes());
    wire.push(PacketContext::Lrproof.to_byte());
    wire.extend_from_slice(&signature);
    wire.extend_from_slice(peer.ephemeral_public.as_bytes());
    wire.extend_from_slice(signalling);
    wire
}

/// Drive `connect()` and feed the crafted proof back in. Returns the events
/// the proof produced.
fn connect_then_prove(signalling: &[u8]) -> (Vec<NodeEvent>, Vec<Vec<u8>>, usize) {
    let peer = make_peer();
    let (mut initiator, iface) = make_initiator();
    let (link_id, _routed, out) = initiator
        .connect(peer.dest_hash, &peer.verifying_key)
        .expect("connect");
    assert_eq!(
        action_data(&out).len(),
        1,
        "connect must emit exactly the LINKREQUEST"
    );

    let wire = legacy_proof_wire(&peer, &link_id, signalling);
    let payload_len = wire.len() - (1 + 1 + TRUNCATED_HASHBYTES + 1);
    let out = initiator.handle_packet(InterfaceId(iface), &wire);
    (out.events.clone(), action_data(&out), payload_len)
}

fn established(events: &[NodeEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, NodeEvent::LinkEstablished { .. }))
}

fn closed_invalid(events: &[NodeEvent]) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            NodeEvent::LinkClosed {
                reason: crate::LinkCloseReason::InvalidProof,
                ..
            }
        )
    })
}

/// THE REPRODUCER. Before the fix this is red: the 96-byte proof is refused
/// with `InvalidProof` and the pending link is torn down.
#[test]
fn legacy_96_byte_lrproof_establishes_the_link() {
    let (events, out, payload_len) = connect_then_prove(&[]);
    assert_eq!(payload_len, 96, "the legacy shape is sig(64) + x25519(32)");
    assert!(
        !closed_invalid(&events),
        "a 96-byte proof is valid for the reference (Link.py:399/:410) and must not close the link"
    );
    assert!(
        established(&events),
        "the initiator must activate the link on the legacy proof shape"
    );
    assert!(
        out.iter()
            .any(|p| p.len() > 18 && p[18] == PacketContext::Lrrtt.to_byte()),
        "an established link answers the proof with the LRRTT packet"
    );
}

/// Control: the signalled shape keeps working, and keeps carrying its MTU.
#[test]
fn signalled_99_byte_lrproof_still_establishes() {
    let signalling = encode_signaling_bytes(MTU as u32, MODE_AES256_CBC);
    let (events, out, payload_len) = connect_then_prove(&signalling);
    assert_eq!(payload_len, 99);
    assert!(established(&events));
    assert!(out
        .iter()
        .any(|p| p.len() > 18 && p[18] == PacketContext::Lrrtt.to_byte()));
}

/// Pin the ACCEPTED shapes: the reference gates on equality, not on a minimum
/// (`Link.py:404` strips exactly 3, `:410` then demands exactly 96), and so
/// does our relay. Anything between or beyond the two shapes is refused.
#[test]
fn off_shape_lrproof_is_refused() {
    for extra in [1usize, 2, 4, 8] {
        let signalling = std::vec![0u8; extra];
        let (events, _out, payload_len) = connect_then_prove(&signalling);
        assert_eq!(payload_len, 96 + extra);
        assert!(
            !established(&events),
            "a {payload_len}-byte proof is neither shape and must not establish a link"
        );
        assert!(
            closed_invalid(&events),
            "an off-shape proof is refused as invalid, not silently ignored"
        );
    }
}
