//! mvr: deterministic reproduction of #272 — a link that has gone Stale spends
//! its whole channel retry budget without a single packet reaching the air.
//!
//! Mechanism (our code), three steps that line up:
//!   1. `Channel::poll` charges the attempt before anything is built
//!      (`link/channel/mod.rs`, second pass over the timed-out envelopes).
//!   2. `build_data_packet_with_context` opened with
//!      `require_state(LinkState::Active)`, so on a Stale link it returned
//!      `Err(InvalidState)` before producing any bytes.
//!   3. `check_channel_timeouts` discarded that error with `.ok()` inside an
//!      `if let Some(..)` with no else arm, so the action became a silent
//!      no-op: no packet, no `ChannelRetransmit` event, no log line.
//!
//! The retry counter therefore ran to exhaustion while nothing was attempted,
//! and the link was then declared dead. The recovery mechanism is fully wired
//! but unreachable: reaching it requires transmitting, and transmitting
//! required the state that recovery would restore.
//!
//! Reference-first: in RNS the FIRST transmission of a channel message is
//! Active-only (`Channel.py:669-673`, `LinkChannelOutlet.send` checks
//! `link.status == ACTIVE`), but the RETRANSMISSION is not
//! (`Channel.py:675-679`, `LinkChannelOutlet.resend` calls `packet.resend()`
//! with no status check). A Python peer therefore keeps retransmitting across
//! the stale grace window, and the peer's reply flips the link back to ACTIVE
//! (`Link.py:983-984`). Our `build_keepalive_packet` already carries the same
//! Active-or-Stale rule for the same reason.
//!
//! Sans-I/O: direct initiator <-> responder over one mesh hop, `MockClock`
//! advanced to drive `check_stale_links` without sleeping the real timeout.
//! Only the initiator's clock moves, so the responder stays Active and can
//! answer the retransmit.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::{LinkId, LinkState};
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::PacketContext;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, NoStorage};
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// Byte offset of the context field in a link packet:
/// `[flags(1)][hops(1)][link_id(16)][context(1)][payload]`.
const CONTEXT_OFFSET: usize = 18;

// ----------------------------------------------------------------------------
// Sans-I/O helpers (same pattern as the other mvr modules).
// ----------------------------------------------------------------------------

fn add_iface(node: &mut EndpointNode, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    idx
}

/// All bytes a node wants to put on the wire this step (SendPacket + Broadcast).
fn action_data(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .collect()
}

/// Deliver every packet to `target` and collect everything it emits in return.
fn deliver_all(target: &mut EndpointNode, iface: usize, packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for pkt in packets {
        out.extend(action_data(&target.handle_packet(InterfaceId(iface), &pkt)));
    }
    out
}

/// Keep only the packets carrying `context`. The stale watchdog also emits a
/// keepalive in the same tick; a keepalive echo would recover the link on its
/// own and mask whether the channel retransmit ever happened.
fn with_context(packets: &[Vec<u8>], context: PacketContext) -> Vec<Vec<u8>> {
    packets
        .iter()
        .filter(|p| {
            p.len() > CONTEXT_OFFSET && PacketContext::from_byte(p[CONTEXT_OFFSET]) == context
        })
        .cloned()
        .collect()
}

fn make_responder() -> (EndpointNode, crate::DestinationHash, [u8; 32]) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["stalertx"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    node.register_destination(dest);
    (node, dest_hash, signing_key)
}

fn make_initiator() -> EndpointNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().build(OsRng, clock, NoStorage)
}

/// Drive a clean initiator <-> responder link to Active on BOTH sides.
/// Returns `(initiator, responder, i_iface, r_iface, caller_link_id)`.
fn establish() -> (EndpointNode, EndpointNode, usize, usize, LinkId) {
    let (mut responder, dest_hash, signing_key) = make_responder();
    let mut initiator = make_initiator();
    let r_iface = add_iface(&mut responder, "R_mesh");
    let i_iface = add_iface(&mut initiator, "I_mesh");

    let (caller_link_id, _routed, out) =
        initiator.connect(dest_hash, &signing_key).expect("connect");

    let mut for_responder = action_data(&out);
    for _ in 0..8 {
        if for_responder.is_empty() {
            break;
        }
        let back = deliver_all(&mut responder, r_iface, for_responder);
        for_responder = deliver_all(&mut initiator, i_iface, back);
    }

    assert_eq!(
        initiator.active_link_count(),
        1,
        "precondition: initiator link must be active"
    );
    assert_eq!(
        responder.active_link_count(),
        1,
        "precondition: responder link must be active"
    );
    (initiator, responder, i_iface, r_iface, caller_link_id)
}

/// Queue one channel message and throw its packet away, so the envelope sits
/// unacknowledged in the tx ring, then jump the initiator's clock past
/// `stale_time` (keepalive 5s * LINK_STALE_FACTOR = 10s with the mock
/// handshake's near-zero RTT) and run the watchdog once. `check_stale_links`
/// runs before `check_channel_timeouts`, so the retransmit is attempted on a
/// link that is already Stale — in one deterministic tick.
fn send_lose_and_go_stale(
    initiator: &mut EndpointNode,
    link_id: &LinkId,
) -> (Vec<Vec<u8>>, Vec<NodeEvent>) {
    let out = initiator
        .send_on_link(link_id, b"stale-retransmit")
        .expect("send_on_link must succeed on an active link");
    assert!(
        !with_context(&action_data(&out), PacketContext::Channel).is_empty(),
        "positive control: the first transmission must reach the wire"
    );
    // The packet is dropped here: the peer never sees it, never ACKs it.

    let now = initiator.transport().clock().now_ms();
    initiator.transport().clock().set(now + 11_000);
    let tick = initiator.handle_timeout();

    assert_eq!(
        initiator.link(link_id).map(|l| l.state()),
        Some(LinkState::Stale),
        "precondition: 11s without inbound must mark the link Stale"
    );
    (action_data(&tick), tick.events)
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

/// #272, minimal: the retransmit of a timed-out channel envelope on a Stale
/// link must produce bytes on the wire and a `ChannelRetransmit` event. Before
/// the fix the attempt was charged against the retry budget and then silently
/// dropped, so both were absent.
#[test]
fn stale_link_channel_retransmit_reaches_the_wire() {
    let (mut initiator, _responder, _i_iface, _r_iface, link_id) = establish();
    let (wire, events) = send_lose_and_go_stale(&mut initiator, &link_id);

    let retransmitted = with_context(&wire, PacketContext::Channel);
    assert!(
        !retransmitted.is_empty(),
        "#272: a Stale link's channel retransmit must reach the wire, \
         but nothing with context=Channel was emitted.\nwire: {} packets, events: {events:?}",
        wire.len()
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, NodeEvent::ChannelRetransmit { .. })),
        "#272: a charged retry must be observable as ChannelRetransmit.\nevents: {events:?}"
    );
}

/// #272, end to end: the retransmit is what makes the stale link recoverable.
/// The peer answers the retransmitted envelope, and that inbound flips the
/// initiator's link back to Active — the recovery path that was unreachable
/// while the retransmit built nothing. Only channel-context packets are
/// delivered, so the keepalive echo cannot stand in for the mechanism.
#[test]
fn stale_link_recovers_through_its_own_channel_retransmit() {
    let (mut initiator, mut responder, i_iface, r_iface, link_id) = establish();
    let (wire, _events) = send_lose_and_go_stale(&mut initiator, &link_id);

    let retransmitted = with_context(&wire, PacketContext::Channel);
    assert!(
        !retransmitted.is_empty(),
        "precondition: there must be a retransmit to deliver"
    );

    let replies = deliver_all(&mut responder, r_iface, retransmitted);
    assert!(
        !replies.is_empty(),
        "the peer must answer the retransmitted channel envelope"
    );

    let mut events = Vec::new();
    for pkt in replies {
        events.extend(initiator.handle_packet(InterfaceId(i_iface), &pkt).events);
    }

    assert_eq!(
        initiator.link(&link_id).map(|l| l.state()),
        Some(LinkState::Active),
        "#272: the peer's answer to the retransmit must recover the stale link.\nevents: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkRecovered { .. })),
        "recovery must be reported as LinkRecovered.\nevents: {events:?}"
    );
}
