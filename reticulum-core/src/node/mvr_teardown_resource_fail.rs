//! mvr: deterministic reproduction of #78 — a link torn down while a resource
//! is in flight drops the transfer with NO `NodeEvent::ResourceFailed`, so the
//! app that started the transfer only sees `LinkClosed` and hangs forever.
//!
//! Reference-first (reviewer, 2026-06-21): RNS `Link.link_closed()`
//! (vendor/Reticulum/RNS/Link.py) cancels BOTH directions on close —
//! `for resource in self.incoming_resources: resource.cancel()` and the same
//! for `outgoing_resources` — and `resource.cancel()` fires the resource
//! callback, so the RNS app IS notified on teardown. Our equivalent is
//! `ResourceFailed`; before this fix we never emitted it on teardown.
//!
//! Mechanism (our code): every genuine teardown removes its link through
//! `remove_link` (link_management.rs), which simply dropped the link and its
//! in-flight `outgoing_resource` / `incoming_resource`. The fix fails any
//! in-flight resource in `remove_link` BEFORE `self.links.remove`, mirroring
//! RNS. The establishment re-key/alias path (Codeberg #66) does NOT go through
//! `remove_link` (it uses `self.links.remove` directly and is pre-activation),
//! so it can never emit a spurious `ResourceFailed` — the guard test pins that.
//!
//! Sans-I/O: direct initiator <-> responder over one mesh hop, `MockClock`
//! advanced to drive `check_stale_links` without sleeping the real timeout.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::LINK_PENDING_TIMEOUT_MS;
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::{LinkCloseReason, LinkId};
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::resource::ResourceError;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, NoStorage};
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

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

fn count_resource_failed(events: &[NodeEvent], resource_hash: &[u8; 32], is_sender: bool) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                NodeEvent::ResourceFailed {
                    resource_hash: rh,
                    error: ResourceError::LinkClosed,
                    is_sender: s,
                    ..
                } if rh == resource_hash && *s == is_sender
            )
        })
        .count()
}

fn has_any_resource_failed(events: &[NodeEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, NodeEvent::ResourceFailed { .. }))
}

fn has_link_closed(events: &[NodeEvent], reason: LinkCloseReason) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            NodeEvent::LinkClosed { reason: r, .. } if *r == reason
        )
    })
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
        &["teardown"],
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

/// Drive a clean initiator <-> responder link to Active on BOTH sides (no
/// re-key). Returns `(initiator, responder, i_iface, r_iface, caller_link_id)`.
fn establish() -> (EndpointNode, EndpointNode, usize, usize, LinkId) {
    let (mut responder, dest_hash, signing_key) = make_responder();
    let mut initiator = make_initiator();
    let r_iface = add_iface(&mut responder, "R_mesh");
    let i_iface = add_iface(&mut initiator, "I_mesh");

    let (caller_link_id, _routed, out) = initiator.connect(dest_hash, &signing_key);

    // Ping-pong the handshake (request -> proof -> rtt -> ack) until quiescent.
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

/// A payload large enough to be carried as a resource transfer.
fn payload() -> Vec<u8> {
    std::vec![0xABu8; 4096]
}

/// Put an outgoing resource in flight on the initiator and return its hash.
fn start_outgoing(initiator: &mut EndpointNode, caller_link_id: &LinkId) -> [u8; 32] {
    let data = payload();
    let (resource_hash, _out) = initiator
        .send_resource(caller_link_id, &data, None, false)
        .expect("send_resource must start an outgoing transfer");
    assert!(
        initiator
            .link(caller_link_id)
            .expect("link must exist")
            .has_outgoing_resource(),
        "outgoing resource must be in flight after send_resource"
    );
    resource_hash
}

/// Put an incoming resource in flight on the responder by advertising from the
/// initiator and accepting on the responder. Returns
/// `(resource_hash, responder_link_id)`.
fn start_incoming(
    initiator: &mut EndpointNode,
    responder: &mut EndpointNode,
    _i_iface: usize,
    r_iface: usize,
    caller_link_id: &LinkId,
) -> ([u8; 32], LinkId) {
    // Clean (non-rekey) establishment shares the same link id on both ends.
    let responder_link_id = *caller_link_id;
    // App-accept strategy: the responder stores the ADV and surfaces it for the
    // app to accept (default is AcceptNone, which would silently reject it).
    responder
        .set_resource_strategy(
            &responder_link_id,
            crate::resource::ResourceStrategy::AcceptApp,
        )
        .expect("responder link must exist to set resource strategy");

    let data = payload();
    let (resource_hash, out) = initiator
        .send_resource(caller_link_id, &data, None, false)
        .expect("send_resource must advertise");

    // Deliver the advertisement to the responder; it surfaces ResourceAdvertised.
    let mut advertised = false;
    for pkt in action_data(&out) {
        let resp_out = responder.handle_packet(InterfaceId(r_iface), &pkt);
        advertised |= resp_out
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::ResourceAdvertised { .. }));
    }
    assert!(
        advertised,
        "responder must surface a ResourceAdvertised event"
    );

    let _ = responder
        .accept_resource(&responder_link_id)
        .expect("accept_resource must start the incoming transfer");
    assert!(
        responder
            .link(&responder_link_id)
            .expect("responder link must exist")
            .has_incoming_resource(),
        "incoming resource must be in flight after accept_resource"
    );
    (resource_hash, responder_link_id)
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

/// Explicit `close_link` with an outgoing resource in flight must fail the
/// resource (sender side) AND still emit `LinkClosed`.
///
/// RED before the fix: only `LinkClosed`, no `ResourceFailed`.
#[test]
fn close_link_fails_in_flight_outgoing_resource() {
    let (mut initiator, _responder, _i, _r, caller_link_id) = establish();
    let resource_hash = start_outgoing(&mut initiator, &caller_link_id);

    let out = initiator.close_link(&caller_link_id);

    assert_eq!(
        count_resource_failed(&out.events, &resource_hash, true),
        1,
        "close_link must emit ResourceFailed(LinkClosed, sender) for the \
         in-flight outgoing resource.\nevents: {:?}",
        out.events
    );
    assert!(
        has_link_closed(&out.events, LinkCloseReason::Normal),
        "close_link must still emit LinkClosed(Normal).\nevents: {:?}",
        out.events
    );
}

/// A stale teardown (`check_stale_links` via `handle_timeout`) with an incoming
/// resource in flight must fail the resource (receiver side) AND emit
/// `LinkClosed(Stale)`.
///
/// `check_stale_links` runs before `check_resource_timeouts` in
/// `handle_timeout`, so the stale close is the path that drops the resource.
/// RED before the fix: only `LinkClosed(Stale)`, no `ResourceFailed`.
#[test]
fn stale_teardown_fails_in_flight_incoming_resource() {
    let (mut initiator, mut responder, i_iface, r_iface, caller_link_id) = establish();
    let (resource_hash, _responder_link_id) = start_incoming(
        &mut initiator,
        &mut responder,
        i_iface,
        r_iface,
        &caller_link_id,
    );

    // Advance the responder's clock far past any stale/close threshold and tick.
    let now = responder.transport().clock().now_ms();
    responder.transport().clock().set(now + 1_000_000_000);
    let out = responder.handle_timeout();

    assert_eq!(
        count_resource_failed(&out.events, &resource_hash, false),
        1,
        "stale teardown must emit ResourceFailed(LinkClosed, receiver) for the \
         in-flight incoming resource.\nevents: {:?}",
        out.events
    );
    assert!(
        has_link_closed(&out.events, LinkCloseReason::Stale),
        "stale teardown must still emit LinkClosed(Stale).\nevents: {:?}",
        out.events
    );
}

/// Second direction for the explicit close: `close_link` on the responder with
/// an incoming resource in flight fails the receiver side.
#[test]
fn close_link_fails_in_flight_incoming_resource() {
    let (mut initiator, mut responder, i_iface, r_iface, caller_link_id) = establish();
    let (resource_hash, responder_link_id) = start_incoming(
        &mut initiator,
        &mut responder,
        i_iface,
        r_iface,
        &caller_link_id,
    );

    let out = responder.close_link(&responder_link_id);

    assert_eq!(
        count_resource_failed(&out.events, &resource_hash, false),
        1,
        "close_link must emit ResourceFailed(LinkClosed, receiver) for the \
         in-flight incoming resource.\nevents: {:?}",
        out.events
    );
    assert!(
        has_link_closed(&out.events, LinkCloseReason::Normal),
        "close_link must still emit LinkClosed(Normal).\nevents: {:?}",
        out.events
    );
}

/// Guard A: tearing down a link with NO resource in flight must NOT emit any
/// spurious `ResourceFailed`.
#[test]
fn teardown_without_resource_emits_no_resource_failed() {
    let (mut initiator, _responder, _i, _r, caller_link_id) = establish();

    let out = initiator.close_link(&caller_link_id);

    assert!(
        !has_any_resource_failed(&out.events),
        "closing a resource-free link must not emit ResourceFailed.\nevents: {:?}",
        out.events
    );
    assert!(
        has_link_closed(&out.events, LinkCloseReason::Normal),
        "close must still emit LinkClosed.\nevents: {:?}",
        out.events
    );
}

/// Guard B: the establishment re-key/alias path (Codeberg #66) tears the old
/// link id down via `self.links.remove` directly (pre-activation, no resource)
/// and must NEVER emit a spurious `ResourceFailed`.
#[test]
fn rekey_retry_emits_no_resource_failed() {
    let (mut responder, dest_hash, signing_key) = make_responder();
    let mut initiator = make_initiator();
    let r_iface = add_iface(&mut responder, "R_mesh");
    let i_iface = add_iface(&mut initiator, "I_mesh");

    // Connect and DROP the first request so the establishment times out and the
    // #66 retry re-keys the link under a fresh id.
    let (_caller_link_id, _routed, _out) = initiator.connect(dest_hash, &signing_key);

    let now = initiator.transport().clock().now_ms();
    initiator
        .transport()
        .clock()
        .set(now + LINK_PENDING_TIMEOUT_MS + 1);
    let retry_out = initiator.handle_timeout();
    assert!(
        !has_any_resource_failed(&retry_out.events),
        "the re-key retry must not emit ResourceFailed.\nevents: {:?}",
        retry_out.events
    );

    // Complete establishment via the re-keyed request to prove the path was real.
    let retry_request = action_data(&retry_out);
    assert!(
        !retry_request.is_empty(),
        "re-key retry must produce a fresh link request"
    );
    let mut for_responder = retry_request;
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
        "link must establish via the re-keyed retry"
    );
}

/// leviculum#27 / CIRISEdge#353 ask #2 — DETERMINISTIC REPRODUCTION of the
/// reverse-path resource-transfer contention that stalled the first mobile
/// trace (and that the previous edge-side `force_busy` test seam could not
/// reproduce, because it faked the busy instead of holding a real transfer).
///
/// A NAT'd initiator's only reachability is its live inbound link. When the
/// responder must ship a small reply (an anti-entropy Summary/Diff carrying the
/// Key + IdentityOccurrence planes that promote the peer to a KEX'd delivery
/// target) AND a resource transfer is already in flight on that link, the
/// reply-as-a-resource is REFUSED `TransferInProgress`. In the field the retry
/// window (8s) was provably shorter than the transfer (16 attempts, link busy
/// throughout) -> fallback outbound dial -> NAT-blocked -> the planes never land.
///
/// Proves BOTH halves with NO timing race — the resource is in flight
/// synchronously from `send_resource` until an event-loop tick completes it:
///   1. a RESOURCE reply during an in-flight transfer -> `TransferInProgress`
///      (the bug), and
///   2. a link PACKET (`send_on_link`) during the SAME in-flight transfer is
///      accepted (the fix — it checks the link Channel, never
///      `has_outgoing_resource()`, so it categorically bypasses the gate).
#[test]
fn reverse_path_packet_interleaves_a_busy_resource_transfer() {
    let (mut initiator, _responder, _i_iface, _r_iface, link) = establish();

    // A large resource is in flight on the link (the peer's own payload).
    let _big = start_outgoing(&mut initiator, &link);

    // (1) THE BUG — a small reply sent as a RESOURCE is refused: one resource
    //     transfer per link at a time.
    let small = std::vec![0x11u8; 64];
    let resource_reply = initiator.send_resource(&link, &small, None, false);
    assert!(
        matches!(resource_reply, Err(ResourceError::TransferInProgress)),
        "a resource reply during an in-flight transfer MUST be refused \
         TransferInProgress (the field bug); got {resource_reply:?}"
    );

    // (2) THE FIX — the SAME small reply sent as a LINK PACKET is accepted
    //     WHILE the resource is still in flight.
    assert!(
        initiator.link(&link).expect("link").has_outgoing_resource(),
        "precondition: the resource is still in flight"
    );
    let packet_reply = initiator.send_on_link(&link, &small);
    assert!(
        packet_reply.is_ok(),
        "a link-packet reply MUST interleave a busy resource transfer \
         (CIRISEdge#353 ask #2); got {packet_reply:?}"
    );

    // The in-flight resource is undisturbed by the interleaved packet.
    assert!(
        initiator.link(&link).expect("link").has_outgoing_resource(),
        "the in-flight resource must be undisturbed by the interleaved packet"
    );
}
