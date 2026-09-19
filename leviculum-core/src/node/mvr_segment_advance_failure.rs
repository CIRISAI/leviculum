//! mvr: a mid-transfer segment build failure must be reported as
//! `ResourceFailed`, never as a completed transfer (Codeberg #270).
//!
//! `advance_outgoing_segments` used to return `SegmentAdvance::Done` when the
//! next segment of a split transfer could not be built, so
//! `handle_resource_proof` emitted `NodeEvent::ResourceCompleted {
//! is_sender: true }` and LXMF retired the outbound message as `Delivered`
//! (`leviculum-lxmf/src/node.rs`, the `ResourceCompleted { is_sender: true }`
//! arm). A message whose transfer stopped between segment 1 and segment 2 was
//! reported to the application as delivered, and the only trace was a `debug!`
//! line, so the caller had nothing to retry on.
//!
//! Trigger, minimal: a two-segment resource transfer where the sender's link is
//! no longer `Active` at the moment segment 1's proof arrives.
//! `build_segment` -> `OutgoingResource::new_with_flags` rejects a non-active
//! link with `ResourceError::LinkNotActive` (`resource/outgoing.rs`, the
//! `!crypt.active` guard), which is exactly the error arm in question.
//!
//! The state is forced directly rather than produced by a real teardown on
//! purpose: a teardown emits its OWN `ResourceFailed` from the teardown path
//! (`link_management.rs`, the teardown arm pinned by `mvr_teardown_resource_fail`),
//! so a torn-down link could not tell the two sources apart and would pass even
//! with the bug present. Forcing the one precondition the arm depends on — the
//! link is not `Active` when the next segment must be built — keeps the
//! attribution unambiguous. Note that `Stale` specifically no longer reaches
//! here: `handle_resource_proof` runs `try_recover_stale` before advancing
//! (#124), which is why this test uses `Closed`.
//!
//! The `no_break` case is the positive control: the same transfer with the link
//! left alone completes with `total_segments == 2`, so a green `break_link`
//! assertion cannot come from a transfer that never reached segment 2.
//!
//! Sans-I/O: direct initiator <-> responder, `MockClock`, no timers involved.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::{LinkId, LinkState};
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::resource::{ResourceError, ResourceStrategy, RESOURCE_MAX_EFFICIENT_SIZE};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, crate::traits::NoStorage>;

const MAX: usize = RESOURCE_MAX_EFFICIENT_SIZE;

// ----------------------------------------------------------------------------
// Sans-I/O helpers (same pattern as `mvr_send_segmentation`).
// ----------------------------------------------------------------------------

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

fn deliver_all(target: &mut EndpointNode, iface: usize, packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for pkt in packets {
        out.extend(action_data(&target.handle_packet(InterfaceId(iface), &pkt)));
    }
    out
}

fn make_responder() -> (EndpointNode, crate::DestinationHash, [u8; 32]) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node = NodeCoreBuilder::new().build(OsRng, clock, crate::traits::NoStorage);

    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["segfail"],
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
    NodeCoreBuilder::new().build(OsRng, clock, crate::traits::NoStorage)
}

/// Drive a clean initiator <-> responder link to Active on both sides.
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

    assert_eq!(initiator.active_link_count(), 1, "initiator link active");
    assert_eq!(responder.active_link_count(), 1, "responder link active");
    (initiator, responder, i_iface, r_iface, caller_link_id)
}

/// Position-dependent, compressible payload (as in `mvr_send_segmentation`).
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// What the SENDER was told about its own transfer.
#[derive(Default)]
struct SenderOutcome {
    /// `total_segments` of every `ResourceCompleted { is_sender: true }`.
    completed: Vec<u32>,
    /// Error of every `ResourceFailed { is_sender: true }`.
    failed: Vec<ResourceError>,
    /// `(segment_index, total_segments)` the receiver actually reassembled.
    receiver_segments: Vec<(u32, u32)>,
}

fn absorb(out: &mut SenderOutcome, events: &[NodeEvent]) {
    for e in events {
        match e {
            NodeEvent::ResourceCompleted {
                is_sender: true,
                total_segments,
                ..
            } => out.completed.push(*total_segments),
            NodeEvent::ResourceCompleted {
                is_sender: false,
                segment_index,
                total_segments,
                ..
            } => out
                .receiver_segments
                .push((*segment_index, *total_segments)),
            NodeEvent::ResourceFailed {
                is_sender: true,
                error,
                ..
            } => out.failed.push(*error),
            _ => {}
        }
    }
}

/// Send a two-segment resource. When `break_link` is set, the sender's link is
/// pushed out of `Active` in the same step in which the receiver completes
/// segment 1 — i.e. before the segment-1 proof it just emitted is handed to the
/// sender, and therefore before the sender tries to build segment 2.
fn run_two_segment_transfer(break_link: bool) -> SenderOutcome {
    let (mut initiator, mut responder, i_iface, r_iface, link_id) = establish();
    responder
        .set_resource_strategy(&link_id, ResourceStrategy::AcceptAll)
        .expect("set AcceptAll on responder link");

    let mut out = SenderOutcome::default();

    // One byte over the efficient size splits into exactly two segments.
    let data = pattern(MAX + 1);
    let (_hash, tick) = initiator
        .send_resource(&link_id, &data, None, true)
        .expect("send_resource");
    let mut to_responder = action_data(&tick);
    absorb(&mut out, &tick.events);

    let mut broken = false;
    // Generous cap; the loop breaks as soon as no packets remain in flight.
    for _ in 0..20_000 {
        if to_responder.is_empty() {
            break;
        }
        let mut from_responder = Vec::new();
        let mut segment_one_done = false;
        for pkt in to_responder.drain(..) {
            let o = responder.handle_packet(InterfaceId(r_iface), &pkt);
            from_responder.extend(action_data(&o));
            segment_one_done |= o.events.iter().any(|e| {
                matches!(
                    e,
                    NodeEvent::ResourceCompleted {
                        is_sender: false,
                        segment_index: 1,
                        ..
                    }
                )
            });
            absorb(&mut out, &o.events);
        }

        if break_link && segment_one_done && !broken {
            // The receiver has just proved segment 1; that proof is in
            // `from_responder` and has not reached the sender yet.
            initiator
                .link_mut(&link_id)
                .expect("sender link still tracked")
                .set_state(LinkState::Closed);
            broken = true;
        }

        let mut next = Vec::new();
        for pkt in from_responder {
            let o = initiator.handle_packet(InterfaceId(i_iface), &pkt);
            next.extend(action_data(&o));
            absorb(&mut out, &o.events);
        }
        to_responder = next;
    }

    if break_link {
        assert!(broken, "the receiver never completed segment 1");
    }
    out
}

/// Positive control: the same transfer, untouched, really does have a second
/// segment to build, and the sender is told it completed with `l = 2`.
#[test]
fn an_untouched_two_segment_transfer_completes() {
    let out = run_two_segment_transfer(false);
    assert_eq!(
        out.receiver_segments,
        std::vec![(1, 2), (2, 2)],
        "the control transfer must really span two segments"
    );
    assert_eq!(
        out.completed,
        std::vec![2],
        "sender completes exactly once, tagged l=2"
    );
    assert!(
        out.failed.is_empty(),
        "nothing fails in the control: {:?}",
        out.failed
    );
}

/// #270: segment 2 cannot be built, so the sender must be told the transfer
/// FAILED. Emitting `ResourceCompleted` here is what made LXMF mark an unsent
/// message `Delivered`.
#[test]
fn a_failed_segment_build_is_not_reported_as_completion() {
    let out = run_two_segment_transfer(true);

    assert_eq!(
        out.receiver_segments,
        std::vec![(1, 2)],
        "only segment 1 ever reached the receiver"
    );
    assert!(
        out.completed.is_empty(),
        "a transfer that stopped after segment 1 must not report completion, \
         got total_segments {:?}",
        out.completed
    );
    assert_eq!(
        out.failed,
        std::vec![ResourceError::LinkNotActive],
        "the sender must see exactly the build failure"
    );
}
