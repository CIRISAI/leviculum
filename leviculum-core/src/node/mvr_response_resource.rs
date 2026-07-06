//! mvr: an incoming `is_response` Resource must surface as `ResponseReceived`.
//!
//! NomadNet pages are fetched over RAW RNS request/response (not LXMF): the
//! client issues `send_request`, and a page larger than the link MDU comes back
//! as a Resource carrying `is_response=true` + the `request_id` (Python
//! `RNS.Resource(umsgpack.packb([request_id, response]), is_response=True,
//! request_id=...)`, `Link.py:handle_request`). On the receive side Python
//! (`Link.py`, the `ResourceAdvertisement.is_response(packet)` branch) accepts
//! such a resource REGARDLESS of resource strategy when its `request_id` matches
//! an outstanding request, and on completion (`response_resource_concluded` ->
//! `handle_response`) delivers it to the request callback.
//!
//! Named failure mode this guards: an initiator that sends a request and gets a
//! `> MDU` response back sees only a generic `ResourceCompleted` (or, with the
//! default `AcceptNone` strategy, nothing at all because the ADV is rejected),
//! never a `ResponseReceived`. That leaves every large-page fetch unwired.
//!
//! Topology: deterministic single-hop initiator <-> responder, sans-I/O. The
//! initiator uses the DEFAULT resource strategy (`AcceptNone`) on purpose: the
//! response resource must be accepted through the `is_response`/`request_id`
//! bypass, not because the app opted into accepting arbitrary resources.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::TRUNCATED_HASHBYTES;
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::request::RequestPolicy;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::resource::msgpack::write_bin;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
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
    let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["page"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    node.register_destination(dest);
    node.register_request_handler(dest_hash, "/page/large.mu", RequestPolicy::AllowAll);
    (node, dest_hash, signing_key)
}

fn make_initiator() -> EndpointNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().build(OsRng, clock, NoStorage)
}

/// Drive a clean initiator <-> responder link to Active on both sides.
fn establish() -> (EndpointNode, EndpointNode, usize, usize, LinkId) {
    let (mut responder, dest_hash, signing_key) = make_responder();
    let mut initiator = make_initiator();
    let r_iface = add_iface(&mut responder, "R_mesh");
    let i_iface = add_iface(&mut initiator, "I_mesh");

    let (caller_link_id, _routed, out) = initiator.connect(dest_hash, &signing_key);

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

/// Pull the responder-side (link_id, request_id) out of a RequestReceived event.
fn request_received(events: &[NodeEvent]) -> Option<(LinkId, [u8; TRUNCATED_HASHBYTES])> {
    events.iter().find_map(|e| match e {
        NodeEvent::RequestReceived {
            link_id,
            request_id,
            ..
        } => Some((*link_id, *request_id)),
        _ => None,
    })
}

/// A response value that exceeds the link MDU (~431 B), forcing the resource
/// path. Encoded as a single msgpack bin value (send_response_resource requires
/// `response_data` to be exactly one valid msgpack value).
fn large_response_value() -> Vec<u8> {
    let page: Vec<u8> = (0..3000usize).map(|i| (i % 251) as u8).collect();
    let mut v = Vec::new();
    write_bin(&mut v, &page);
    v
}

/// Full request -> response-resource -> ResponseReceived round trip. Returns
/// every event the initiator observed and the request_id / response value used.
fn run_request_response_resource() -> (Vec<NodeEvent>, [u8; TRUNCATED_HASHBYTES], Vec<u8>) {
    let (mut initiator, mut responder, i_iface, r_iface, caller_link_id) = establish();

    // Initiator issues the page request. The initiator link keeps the DEFAULT
    // AcceptNone strategy: the response resource must ride the is_response
    // bypass, not a broad accept-all opt-in.
    let (request_id, out) = initiator
        .send_request(&caller_link_id, "/page/large.mu", None, None)
        .expect("send_request on active link");
    let req_pkts = action_data(&out);
    let mut init_events: Vec<NodeEvent> = out.events;

    // Responder dispatches the request and answers with a > MDU response resource.
    let mut resp_out_events = Vec::new();
    let mut to_initiator = Vec::new();
    for pkt in req_pkts {
        let o = responder.handle_packet(InterfaceId(r_iface), &pkt);
        to_initiator.extend(action_data(&o));
        resp_out_events.extend(o.events);
    }
    let (resp_link, resp_request_id) =
        request_received(&resp_out_events).expect("responder must dispatch the request");
    assert_eq!(
        resp_request_id, request_id,
        "responder request_id must match the initiator's"
    );

    let response_value = large_response_value();
    let (_res_hash, adv_out) = responder
        .send_response_resource(&resp_link, &resp_request_id, &response_value)
        .expect("send_response_resource must advertise");
    to_initiator.extend(action_data(&adv_out));

    // Bounce every packet between the two nodes until the transfer quiesces.
    let mut to_responder: Vec<Vec<u8>> = Vec::new();
    for _ in 0..2000 {
        if to_initiator.is_empty() && to_responder.is_empty() {
            break;
        }
        // Initiator side.
        let mut next_to_responder = Vec::new();
        for pkt in to_initiator.drain(..) {
            let o = initiator.handle_packet(InterfaceId(i_iface), &pkt);
            next_to_responder.extend(action_data(&o));
            init_events.extend(o.events);
        }
        to_responder.extend(next_to_responder);
        // Responder side.
        let mut next_to_initiator = Vec::new();
        for pkt in to_responder.drain(..) {
            let o = responder.handle_packet(InterfaceId(r_iface), &pkt);
            next_to_initiator.extend(action_data(&o));
        }
        to_initiator.extend(next_to_initiator);
    }

    (init_events, request_id, response_value)
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

/// The initiator must observe a `ResponseReceived` whose `request_id` matches
/// the request and whose `response_data` is the exact value the responder sent.
///
/// RED before the fix: the default-AcceptNone initiator rejects the response
/// ADV, so it sees neither `ResponseReceived` nor `ResourceCompleted`.
#[test]
fn is_response_resource_surfaces_as_response_received() {
    let (events, request_id, response_value) = run_request_response_resource();

    let delivered = events.iter().find_map(|e| match e {
        NodeEvent::ResponseReceived {
            request_id: rid,
            response_data,
            ..
        } if *rid == request_id => Some(response_data.clone()),
        _ => None,
    });

    assert_eq!(
        delivered.as_deref(),
        Some(response_value.as_slice()),
        "initiator must get ResponseReceived with the exact response value.\nevents: {events:?}"
    );
}

/// A response resource must NOT leak a generic receiver-side `ResourceCompleted`
/// to the app: Python delivers it only through the request callback, so the
/// generic resource path must stay silent for `is_response` transfers.
#[test]
fn is_response_resource_does_not_emit_generic_resource_completed() {
    let (events, _request_id, _response_value) = run_request_response_resource();

    let leaked = events.iter().any(|e| {
        matches!(
            e,
            NodeEvent::ResourceCompleted {
                is_sender: false,
                ..
            }
        )
    });

    assert!(
        !leaked,
        "an is_response resource must not surface a receiver-side ResourceCompleted.\nevents: {events:?}"
    );
}
