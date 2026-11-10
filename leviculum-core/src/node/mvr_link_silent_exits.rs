//! mvr (860, from 858 §Q6): the link layer's refusal exits are named drops.
//!
//! On 2026-10-19 at 17:48 CEST a board terminating a phone's link received
//! an 83 B RTT packet, a 483 B and a 99 B packet on it and printed nothing:
//! no PN line, no `[DROP]`. The exits that could do that logged at trace or
//! debug level, which a board compiles out, and raised no `PacketDropped`.
//! The most economical chain fitting the capture: the RTT did not validate,
//! the responder link stayed in its handshake, and the two data packets fell
//! out at the non-active-link exit.
//!
//! Host model (sans-I/O, deterministic): one initiator, one responder, one
//! interface each. Every test drives one exit with a genuine packet of the
//! link and asserts the `PacketDropped` event a board renders as `[DROP]`,
//! with its reason, its packet name and the header fields 858 §Q5 asked
//! for, and the per-reason counter beside it. Where a packet is refused, a
//! positive control then shows the same kind of packet taken once the
//! refusal's cause is gone, so the line is about the cause and nothing else.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::{HeaderType, PacketContext};
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

fn of_context(packets: &[Vec<u8>], context: PacketContext) -> Vec<u8> {
    let found: Vec<&Vec<u8>> = packets
        .iter()
        .filter(|p| {
            crate::packet::peek_wire_class(p).is_some_and(|w| w.context == context.to_byte())
        })
        .collect();
    assert_eq!(found.len(), 1, "exactly one {context:?} packet expected");
    found[0].clone()
}

/// The drop events one call raised, as `(reason, header type, transport id,
/// has a packet name)`.
fn drops(out: &TickOutput) -> Vec<(DropReason, HeaderType, Option<[u8; 16]>, bool)> {
    out.events
        .iter()
        .filter_map(|e| match e {
            NodeEvent::PacketDropped {
                reason,
                header_type,
                transport_id,
                packet_hash_prefix,
                ..
            } => Some((
                *reason,
                *header_type,
                *transport_id,
                packet_hash_prefix.is_some(),
            )),
            _ => None,
        })
        .collect()
}

/// Exactly one drop, of `reason`, naming its packet, on a HEADER_1 packet
/// (the link packets here are all addressed to the link, never via a
/// transport node).
fn assert_one_drop(out: &TickOutput, reason: DropReason) {
    assert_eq!(
        drops(out),
        [(reason, HeaderType::Type1, None, true)],
        "one [DROP] reason={} naming its packet; events: {:?}",
        reason.kebab(),
        out.events
    );
}

fn delivered(out: &TickOutput) -> bool {
    out.events
        .iter()
        .any(|e| matches!(e, NodeEvent::LinkDataReceived { .. }))
}

fn established(out: &TickOutput) -> bool {
    out.events
        .iter()
        .any(|e| matches!(e, NodeEvent::LinkEstablished { .. }))
}

struct Pair {
    initiator: EndpointNode,
    responder: EndpointNode,
    i_mesh: usize,
    r_mesh: usize,
    link: LinkId,
}

impl Pair {
    fn advance(&mut self, ms: u64) {
        self.initiator.transport.clock().advance(ms);
        self.responder.transport.clock().advance(ms);
    }

    /// Deliver one packet to the responder, and collect what the call and
    /// the next pass of the event loop raised. A drop the node layer decides
    /// is pushed onto the transport's event queue after this call drained
    /// it, so it surfaces on the next pass, as every node-layer drop has
    /// since #421; a board's next tick is that pass.
    fn deliver(&mut self, pkt: &[u8]) -> TickOutput {
        let mut out = self.responder.handle_packet(InterfaceId(self.r_mesh), pkt);
        let next = self.responder.process_events_and_actions();
        out.actions.extend(next.actions);
        out.events.extend(next.events);
        out
    }

    /// A plain data packet the initiator sends on the link.
    fn data(&mut self, payload: &[u8]) -> Vec<u8> {
        let (_, out) = self
            .initiator
            .send_packet_on_link(&self.link, payload)
            .expect("initiator link is active");
        of_context(&action_data(&out), PacketContext::None)
    }

    /// A request the initiator sends on the link.
    fn request(&mut self, path: &str) -> Vec<u8> {
        let (_, out) = self
            .initiator
            .send_request(&self.link, path, None, None)
            .expect("initiator link is active");
        of_context(&action_data(&out), PacketContext::Request)
    }

    /// A channel message the initiator sends on the link.
    fn channel(&mut self, payload: &[u8]) -> Vec<u8> {
        let out = self
            .initiator
            .send_on_link(&self.link, payload)
            .expect("initiator link is active");
        of_context(&action_data(&out), PacketContext::Channel)
    }

    fn responder_link_active(&self) -> bool {
        self.responder
            .links
            .get(&self.link)
            .is_some_and(|l| l.state() == crate::link::LinkState::Active)
    }
}

/// Runs the handshake up to and including the initiator's RTT packet, and
/// hands the RTT back instead of delivering it: the initiator's link is
/// Active, the responder's is still in its handshake.
fn handshake_holding_rtt() -> (Pair, Vec<u8>) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut responder =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrsilent",
        &["exits"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::None);
    let dest_hash = *dest.hash();
    responder.register_destination(dest);
    let r_mesh = add_iface(&mut responder, "R_mesh");

    let mut initiator =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let i_mesh = add_iface(&mut initiator, "I_mesh");

    let (link, _routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");
    let mut pair = Pair {
        initiator,
        responder,
        i_mesh,
        r_mesh,
        link,
    };
    let request = of_context(&action_data(&out), PacketContext::None);
    pair.advance(LEG_MS);
    let out = pair.deliver(&request);
    let proof = of_context(&action_data(&out), PacketContext::Lrproof);
    pair.advance(LEG_MS);
    let out = pair
        .initiator
        .handle_packet(InterfaceId(pair.i_mesh), &proof);
    let rtt = of_context(&action_data(&out), PacketContext::Lrrtt);
    pair.advance(LEG_MS);
    assert!(!pair.responder_link_active(), "the RTT is still held");
    (pair, rtt)
}

fn established_pair() -> Pair {
    let (mut pair, rtt) = handshake_holding_rtt();
    let out = pair.deliver(&rtt);
    assert!(established(&out), "the RTT establishes the responder");
    assert!(drops(&out).is_empty());
    pair
}

fn corrupt_last_byte(pkt: &[u8]) -> Vec<u8> {
    let mut bad = pkt.to_vec();
    if let Some(last) = bad.last_mut() {
        *last ^= 0x5a;
    }
    bad
}

/// The 15:48 shape, the RTT withheld: link data, a request and a channel
/// packet on a responder link still in its handshake that do not decrypt
/// under the link keys are each a `[DROP] reason=link-not-active`, and the
/// link stays in its handshake. Before 860 each was silent.
///
/// A genuine packet is not refused any more (1624): it proves the initiator
/// holds the link keys, as the RTT would have, and establishes the link. The
/// positive control below is that packet.
#[test]
fn traffic_on_a_handshake_link_is_a_named_drop() {
    let (mut pair, _rtt) = handshake_holding_rtt();

    let data = pair.data(b"the 483 B packet");
    let out = pair.deliver(&corrupt_last_byte(&data));
    assert!(!delivered(&out));
    assert!(!established(&out));
    assert_one_drop(&out, DropReason::LinkNotActive);

    let request = pair.request("/page");
    assert_one_drop(
        &pair.deliver(&corrupt_last_byte(&request)),
        DropReason::LinkNotActive,
    );

    let channel = pair.channel(b"the 99 B packet");
    assert_one_drop(
        &pair.deliver(&corrupt_last_byte(&channel)),
        DropReason::LinkNotActive,
    );

    assert_eq!(
        pair.responder.transport().stats().drops_link_not_active(),
        3
    );
    assert!(!pair.responder_link_active());

    // Positive control: the same data packet, intact, establishes the link
    // and is delivered on it.
    let out = pair.deliver(&data);
    assert!(established(&out), "events: {:?}", out.events);
    assert!(delivered(&out), "events: {:?}", out.events);
    assert!(drops(&out).is_empty(), "events: {:?}", out.events);
    assert!(pair.responder_link_active());
}

/// 1624: with the RTT lost, the initiator's first genuine packet of any of
/// the three kinds establishes the responder link, and the RTT that turns up
/// after it is the named refusal an RTT on an established link always is.
#[test]
fn a_genuine_packet_establishes_a_handshake_link() {
    for kind in ["data", "request", "channel"] {
        let (mut pair, rtt) = handshake_holding_rtt();
        let pkt = match kind {
            "data" => pair.data(b"payload"),
            "request" => pair.request("/page"),
            _ => pair.channel(b"payload"),
        };
        let out = pair.deliver(&pkt);
        assert!(established(&out), "{kind}: events: {:?}", out.events);
        assert!(pair.responder_link_active(), "{kind}");
        assert_eq!(
            pair.responder.transport().stats().drops_link_not_active(),
            0,
            "{kind}"
        );

        let out = pair.deliver(&rtt);
        assert!(!established(&out), "{kind}: established once only");
        assert_one_drop(&out, DropReason::RttInvalid);
    }
}

/// The 15:48 shape, the RTT corrupted: the RTT that does not validate is a
/// `[DROP] reason=rtt-invalid`, the link stays in its handshake, and data
/// after it that does not decrypt either is `link-not-active` — the whole
/// chain on the capture. (Genuine data after it establishes the link, 1624.)
#[test]
fn a_corrupted_rtt_is_a_named_drop_and_so_is_what_follows() {
    let (mut pair, rtt) = handshake_holding_rtt();

    let out = pair.deliver(&corrupt_last_byte(&rtt));
    assert!(!established(&out));
    assert_one_drop(&out, DropReason::RttInvalid);
    assert!(!pair.responder_link_active());

    let data = pair.data(b"after the bad rtt");
    assert_one_drop(
        &pair.deliver(&corrupt_last_byte(&data)),
        DropReason::LinkNotActive,
    );
    assert_eq!(pair.responder.transport().stats().drops_rtt_invalid(), 1);

    // Positive control: the genuine RTT still establishes the link.
    assert!(established(&pair.deliver(&rtt)));
}

/// An RTT for a link already established (a retry, a second copy built
/// fresh, so dedup does not take it first) is refused by the state check,
/// and that refusal is named too.
#[test]
fn an_rtt_after_establishment_is_a_named_drop() {
    let mut pair = established_pair();
    let late = pair
        .initiator
        .links
        .get(&pair.link)
        .expect("initiator link")
        .build_rtt_packet(0.05, &mut OsRng)
        .expect("rtt");
    let out = pair.deliver(&late);
    assert!(!established(&out));
    assert_one_drop(&out, DropReason::RttInvalid);
    assert!(pair.responder_link_active(), "nothing else changed");
}

/// Link data and a request that do not decrypt on an Active link are
/// `link-decrypt-fail`; a channel packet that does not is
/// `channel-decrypt-fail`.
#[test]
fn undecryptable_link_packets_are_named_drops() {
    let mut pair = established_pair();

    let data = pair.data(b"plain");
    let out = pair.deliver(&corrupt_last_byte(&data));
    assert!(!delivered(&out));
    assert_one_drop(&out, DropReason::LinkDecryptFail);

    let request = pair.request("/page");
    assert_one_drop(
        &pair.deliver(&corrupt_last_byte(&request)),
        DropReason::LinkDecryptFail,
    );

    let channel = pair.channel(b"reliable");
    let out = pair.deliver(&corrupt_last_byte(&channel));
    assert!(action_data(&out).is_empty(), "nothing kept, nothing proven");
    assert_one_drop(&out, DropReason::ChannelDecryptFail);

    let stats = pair.responder.transport().stats();
    assert_eq!(stats.drops_link_decrypt_fail(), 2);
    assert_eq!(stats.drops_channel_decrypt_fail(), 1);

    // Positive control: the genuine data packet is taken.
    assert!(delivered(&pair.deliver(&data)));
}

/// A request for a path the destination registered no handler for is a
/// `[DROP] reason=request-no-handler`.
#[test]
fn a_request_without_a_handler_is_a_named_drop() {
    let mut pair = established_pair();
    let request = pair.request("/nobody/serves/this");
    let out = pair.deliver(&request);
    assert!(!out
        .events
        .iter()
        .any(|e| matches!(e, NodeEvent::RequestReceived { .. })));
    assert_one_drop(&out, DropReason::RequestNoHandler);
    assert_eq!(
        pair.responder
            .transport()
            .stats()
            .drops_request_no_handler(),
        1
    );
}

/// The reasons render as the kebab spellings the order names.
#[test]
fn the_new_reasons_are_spelled_as_ordered() {
    assert_eq!(DropReason::LinkNotActive.kebab(), "link-not-active");
    assert_eq!(DropReason::LinkDecryptFail.kebab(), "link-decrypt-fail");
    assert_eq!(DropReason::RttInvalid.kebab(), "rtt-invalid");
    assert_eq!(DropReason::RequestNoHandler.kebab(), "request-no-handler");
    assert_eq!(
        DropReason::ChannelDecryptFail.kebab(),
        "channel-decrypt-fail"
    );
}
