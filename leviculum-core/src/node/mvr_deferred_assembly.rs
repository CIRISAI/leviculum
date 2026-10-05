//! mvr: an incoming resource's assembly can run outside the node
//! (leviculum#71) and conclude exactly as the inline path does.
//!
//! The std driver decrypts and decompresses a finished transfer on a blocking
//! thread so concurrent transfers on different links stop serializing on the
//! node lock. That is only sound if the deferred path is indistinguishable
//! from the inline one to everyone outside the node: same bytes, same
//! metadata, a proof the sender accepts, and a clean failure when the link
//! closes while the assembly is running.
//!
//! Sans-I/O: direct initiator <-> responder over one mesh hop.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::resource::{ResourceError, ResourceStrategy};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, crate::traits::NoStorage>;

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

/// Deliver every packet, returning what the target sends back and the events
/// it raised.
fn deliver_all(
    target: &mut EndpointNode,
    iface: usize,
    packets: Vec<Vec<u8>>,
) -> (Vec<Vec<u8>>, Vec<NodeEvent>) {
    let mut out = Vec::new();
    let mut events = Vec::new();
    for pkt in packets {
        let tick = target.handle_packet(InterfaceId(iface), &pkt);
        out.extend(action_data(&tick));
        events.extend(tick.events);
    }
    (out, events)
}

fn establish() -> (EndpointNode, EndpointNode, usize, usize, LinkId) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut responder = NodeCoreBuilder::new().build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        crate::traits::NoStorage,
    );
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["deferasm"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    responder.register_destination(dest);
    let mut initiator = NodeCoreBuilder::new().build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        crate::traits::NoStorage,
    );
    let r_iface = add_iface(&mut responder, "R_mesh");
    let i_iface = add_iface(&mut initiator, "I_mesh");

    let (link_id, _routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");
    let mut for_responder = action_data(&out);
    for _ in 0..8 {
        if for_responder.is_empty() {
            break;
        }
        let (back, _) = deliver_all(&mut responder, r_iface, for_responder);
        (for_responder, _) = deliver_all(&mut initiator, i_iface, back);
    }
    assert_eq!(initiator.active_link_count(), 1, "initiator link active");
    assert_eq!(responder.active_link_count(), 1, "responder link active");
    responder
        .set_resource_strategy(&link_id, ResourceStrategy::AcceptAll)
        .expect("AcceptAll on the responder link");
    (initiator, responder, i_iface, r_iface, link_id)
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

const METADATA: &[u8] = b"deferred-assembly-meta";

fn received(events: &[NodeEvent]) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    events.iter().find_map(|e| match e {
        NodeEvent::ResourceCompleted {
            is_sender: false,
            data,
            metadata,
            ..
        } => Some((data.clone(), metadata.clone())),
        _ => None,
    })
}

fn sender_completed(events: &[NodeEvent]) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            NodeEvent::ResourceCompleted {
                is_sender: true,
                ..
            }
        )
    })
}

/// Pump a transfer until the responder goes quiet. Returns the responder's
/// events and the initiator's events seen on the way.
fn pump(
    initiator: &mut EndpointNode,
    responder: &mut EndpointNode,
    i_iface: usize,
    r_iface: usize,
    mut to_responder: Vec<Vec<u8>>,
) -> (Vec<NodeEvent>, Vec<NodeEvent>) {
    let mut r_events = Vec::new();
    let mut i_events = Vec::new();
    for _ in 0..20_000 {
        if to_responder.is_empty() {
            break;
        }
        let (back, ev) = deliver_all(responder, r_iface, to_responder);
        r_events.extend(ev);
        let (next, ev) = deliver_all(initiator, i_iface, back);
        i_events.extend(ev);
        to_responder = next;
    }
    (r_events, i_events)
}

/// Run one transfer of `data` + `METADATA`, deferring assembly on the
/// responder when asked. Returns what the responder delivered and whether the
/// sender saw its own completion.
fn transfer(data: &[u8], defer: bool) -> ((Vec<u8>, Option<Vec<u8>>), bool) {
    let (mut initiator, mut responder, i_iface, r_iface, link_id) = establish();
    responder.set_defer_resource_assembly(defer);

    let (_hash, tick) = initiator
        .send_resource(&link_id, data, Some(METADATA), true)
        .expect("send_resource");
    let (r_events, mut i_events) = pump(
        &mut initiator,
        &mut responder,
        i_iface,
        r_iface,
        action_data(&tick),
    );

    let delivered = if defer {
        assert!(
            received(&r_events).is_none(),
            "a deferred resource must not complete before its job has run"
        );
        let jobs = responder.take_resource_assembly_jobs();
        assert_eq!(jobs.len(), 1, "one transfer, one assembly job");
        assert_eq!(jobs[0].link_id(), link_id);
        // The job runs with no node borrow at all: that is the whole point.
        let result = jobs.into_iter().next().unwrap().run();
        let tick = responder.complete_resource_assembly(result);
        let delivered = received(&tick.events).expect("completes once the job is applied");
        // The proof the completion sent must satisfy the sender.
        let (_, ev) = deliver_all(&mut initiator, i_iface, action_data(&tick));
        i_events.extend(ev);
        delivered
    } else {
        assert!(responder.take_resource_assembly_jobs().is_empty());
        received(&r_events).expect("inline assembly completes in place")
    };
    (delivered, sender_completed(&i_events))
}

#[test]
fn deferred_assembly_concludes_exactly_as_inline_assembly_does() {
    let data = pattern(40_000);
    let (inline, inline_sender_done) = transfer(&data, false);
    let (deferred, deferred_sender_done) = transfer(&data, true);

    assert_eq!(inline.0, data, "inline delivers the bytes sent");
    assert_eq!(inline.1.as_deref(), Some(METADATA));
    assert_eq!(
        deferred, inline,
        "deferred delivers the same bytes and metadata"
    );
    assert!(inline_sender_done, "the inline proof completes the sender");
    assert!(
        deferred_sender_done,
        "the deferred proof completes the sender"
    );
}

#[test]
fn a_link_closed_during_assembly_fails_the_resource_and_drops_the_late_result() {
    let (mut initiator, mut responder, i_iface, r_iface, link_id) = establish();
    responder.set_defer_resource_assembly(true);
    let (resource_hash, tick) = initiator
        .send_resource(&link_id, &pattern(20_000), None, true)
        .expect("send_resource");
    let _ = pump(
        &mut initiator,
        &mut responder,
        i_iface,
        r_iface,
        action_data(&tick),
    );

    let job = responder
        .take_resource_assembly_jobs()
        .pop()
        .expect("an assembly job");

    let closed = responder.close_link(&link_id);
    let failed = closed.events.iter().any(|e| {
        matches!(
            e,
            NodeEvent::ResourceFailed {
                is_sender: false,
                error: ResourceError::LinkClosed,
                resource_hash: h,
                ..
            } if *h == resource_hash
        )
    });
    assert!(
        failed,
        "the waiting resource fails with the link: {:?}",
        closed.events
    );

    let late = responder.complete_resource_assembly(job.run());
    assert!(
        received(&late.events).is_none() && late.actions.is_empty(),
        "a result whose link is gone concludes nothing and sends nothing"
    );
}
