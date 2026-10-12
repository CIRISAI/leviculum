//! mvr (Codeberg #658, audit D7): a link packet is accepted only on the
//! interface the link was formed on.
//!
//! The reference hands a link-addressed DATA packet to its link only when it
//! arrived on the link's attached interface
//! (`reference/Reticulum/RNS/Transport.py:2129`), and `Link.receive` refuses
//! every other packet loudly
//! (`reference/Reticulum/RNS/Link.py:975-976`). Resource proofs reach
//! `Link.receive` too (`reference/Reticulum/RNS/Transport.py:2237-2242`), so
//! they are pinned the same way. Keepalives, resource parts and resource
//! proofs carry no per-packet authentication: ours accepted all three from
//! any interface, which lets a node bridging two of our interfaces replay
//! them across.
//!
//! Host model (sans-I/O, deterministic): two endpoints, each with a mesh
//! interface the link is formed on and a second interface it is not. Every
//! test feeds a genuine packet of the link first on the second interface
//! (refused and counted) and then on the link's own (taken), so the refusal
//! is about the interface and nothing else.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::PacketContext;
use crate::resource::ResourceStrategy;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::{Action, DropReason, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// One handshake leg; a LAN-like link.
const LEG_MS: u64 = 20;

fn add_iface(node: &mut EndpointNode, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
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

fn is_context(pkt: &[u8], context: PacketContext) -> bool {
    crate::packet::peek_wire_class(pkt).is_some_and(|w| w.context == context.to_byte())
}

fn only_context(packets: &[Vec<u8>], context: PacketContext) -> Vec<u8> {
    let found: Vec<&Vec<u8>> = packets.iter().filter(|p| is_context(p, context)).collect();
    assert_eq!(found.len(), 1, "exactly one {context:?} packet expected");
    found[0].clone()
}

struct Pair {
    initiator: EndpointNode,
    responder: EndpointNode,
    /// The interfaces the link is formed on.
    i_mesh: usize,
    r_mesh: usize,
    /// Each node's other interface, which the link never used.
    i_other: usize,
    r_other: usize,
    link: LinkId,
}

impl Pair {
    fn advance(&mut self, ms: u64) {
        self.initiator.transport.clock().advance(ms);
        self.responder.transport.clock().advance(ms);
    }

    fn mismatch_drops(node: &EndpointNode) -> u64 {
        node.transport().stats().drops_link_iface_mismatch()
    }
}

fn establish() -> Pair {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut responder =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrpin",
        &["iface"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    dest.set_resource_strategy(ResourceStrategy::AcceptAll);
    let dest_hash = *dest.hash();
    responder.register_destination(dest);
    let r_mesh = add_iface(&mut responder, "R_mesh");
    let r_other = add_iface(&mut responder, "R_other");

    let mut initiator =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let i_mesh = add_iface(&mut initiator, "I_mesh");
    let i_other = add_iface(&mut initiator, "I_other");

    let (link, _routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");
    let mut pair = Pair {
        initiator,
        responder,
        i_mesh,
        r_mesh,
        i_other,
        r_other,
        link,
    };
    let mut established = false;
    let mut for_responder = action_data(&out);
    for _ in 0..8 {
        if for_responder.is_empty() {
            break;
        }
        pair.advance(LEG_MS);
        let mut back = Vec::new();
        for pkt in for_responder {
            let o = pair.responder.handle_packet(InterfaceId(pair.r_mesh), &pkt);
            established |= o
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkEstablished { .. }));
            back.extend(action_data(&o));
        }
        pair.advance(LEG_MS);
        for_responder = Vec::new();
        for pkt in back {
            let o = pair.initiator.handle_packet(InterfaceId(pair.i_mesh), &pkt);
            for_responder.extend(action_data(&o));
        }
    }
    assert!(established, "responder side must reach Active");
    assert_eq!(
        pair.responder
            .links
            .get(&pair.link)
            .and_then(|l| l.attached_interface()),
        Some(pair.r_mesh)
    );
    pair
}

/// An initiator keepalive on the responder's other interface draws no echo
/// and is counted; the same bytes on the link's own interface are echoed.
#[test]
fn keepalive_on_another_interface_is_refused() {
    let mut pair = establish();
    let keepalive = pair
        .initiator
        .links
        .get(&pair.link)
        .expect("initiator link")
        .build_keepalive_packet()
        .expect("keepalive");

    let before = Pair::mismatch_drops(&pair.responder);
    let out = pair
        .responder
        .handle_packet(InterfaceId(pair.r_other), &keepalive);
    assert!(
        action_data(&out).is_empty(),
        "a keepalive from another interface must not be echoed"
    );
    assert_eq!(Pair::mismatch_drops(&pair.responder), before + 1);

    let out = pair
        .responder
        .handle_packet(InterfaceId(pair.r_mesh), &keepalive);
    only_context(&action_data(&out), PacketContext::Keepalive);
    assert_eq!(Pair::mismatch_drops(&pair.responder), before + 1);
}

/// A resource part and a resource proof, each fed first on the other
/// interface and then on the link's own.
#[test]
fn resource_part_and_proof_on_another_interface_are_refused() {
    let mut pair = establish();
    let data: Vec<u8> = (0..200u32).map(|i| (i % 251) as u8).collect();
    let (hash, out) = pair
        .initiator
        .send_resource(&pair.link, &data, None, false)
        .expect("advertise");
    let adv = only_context(&action_data(&out), PacketContext::ResourceAdv);

    pair.advance(LEG_MS);
    let out = pair.responder.handle_packet(InterfaceId(pair.r_mesh), &adv);
    let req = only_context(&action_data(&out), PacketContext::ResourceReq);

    pair.advance(LEG_MS);
    let out = pair.initiator.handle_packet(InterfaceId(pair.i_mesh), &req);
    let part = only_context(&action_data(&out), PacketContext::Resource);

    // The part on the responder's other interface: nothing assembled.
    pair.advance(LEG_MS);
    let before = Pair::mismatch_drops(&pair.responder);
    let out = pair
        .responder
        .handle_packet(InterfaceId(pair.r_other), &part);
    assert!(
        !out.events
            .iter()
            .any(|e| matches!(e, NodeEvent::ResourceCompleted { .. })),
        "a resource part from another interface must not be assembled"
    );
    assert!(action_data(&out).is_empty(), "and must draw no proof");
    assert_eq!(Pair::mismatch_drops(&pair.responder), before + 1);

    // The same part on the link's own interface completes the resource.
    let out = pair
        .responder
        .handle_packet(InterfaceId(pair.r_mesh), &part);
    assert!(out.events.iter().any(|e| matches!(
        e,
        NodeEvent::ResourceCompleted { resource_hash, is_sender: false, .. }
            if *resource_hash == hash
    )));
    let proof = only_context(&action_data(&out), PacketContext::ResourcePrf);

    // The proof on the initiator's other interface: the sender stays open.
    pair.advance(LEG_MS);
    let before = Pair::mismatch_drops(&pair.initiator);
    let out = pair
        .initiator
        .handle_packet(InterfaceId(pair.i_other), &proof);
    assert!(
        !out.events
            .iter()
            .any(|e| matches!(e, NodeEvent::ResourceCompleted { .. })),
        "a resource proof from another interface must not conclude the transfer"
    );
    assert_eq!(Pair::mismatch_drops(&pair.initiator), before + 1);

    let out = pair
        .initiator
        .handle_packet(InterfaceId(pair.i_mesh), &proof);
    assert!(out.events.iter().any(|e| matches!(
        e,
        NodeEvent::ResourceCompleted { resource_hash, is_sender: true, .. }
            if *resource_hash == hash
    )));
    assert_eq!(Pair::mismatch_drops(&pair.initiator), before + 1);
}

/// The refusal is a named drop: its own kebab reason on the journey line.
#[test]
fn mismatch_reason_is_named() {
    assert_eq!(DropReason::LinkIfaceMismatch.kebab(), "link-iface-mismatch");
}
