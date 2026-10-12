//! mvr: a request or response past one segment travels as a split resource
//! and is served (Codeberg #666, audit 654 D11).
//!
//! The reference sends a request larger than the link MDU as a resource
//! with the request flag (`reference/Reticulum/RNS/Link.py:516-517`) and a
//! response the same way with the response flag (`Link.py:895-901`). Past
//! `RESOURCE_MAX_EFFICIENT_SIZE` that resource is split
//! (`reference/Reticulum/RNS/Resource.py:299-313`), every segment carrying
//! the request id and the flag (`Resource.py:770-778`), and the receiver
//! hands the joined body to its request or response handler after the last
//! segment (`Resource.py:708-738`). Ours refused that shape on both sides:
//! the senders returned `ResourceTooLarge`, the receiver ignored the
//! advertisement, and a peer's large request or response died as a timeout.
//!
//! Each test drives a real initiator <-> responder link end to end and
//! checks the delivered body byte for byte. The payloads compress, so each
//! ~1 MiB segment is a few parts and the ping-pong stays fast; the
//! incompressible, cross-stack direction is the rnsd_interop drop-in.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::TRUNCATED_HASHBYTES;
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::msgpack::{write_bin, write_fixstr};
use crate::node::request::RequestPolicy;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::resource::{SliceSource, RESOURCE_MAX_EFFICIENT_SIZE};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

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

struct Pair {
    initiator: EndpointNode,
    responder: EndpointNode,
    i_iface: usize,
    r_iface: usize,
    caller_link: LinkId,
    responder_link: LinkId,
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
        "mvrsplit",
        &["request"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    responder.register_destination(dest);
    responder.register_request_handler(dest_hash, "/big", RequestPolicy::AllowAll);
    let mut initiator =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let r_iface = add_iface(&mut responder, "R_mesh");
    let i_iface = add_iface(&mut initiator, "I_mesh");

    let (caller_link, _routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");
    let mut responder_link = None;
    let mut to_responder = action_data(&out);
    for _ in 0..8 {
        if to_responder.is_empty() {
            break;
        }
        let mut back = Vec::new();
        for pkt in to_responder.drain(..) {
            let o = responder.handle_packet(InterfaceId(r_iface), &pkt);
            back.extend(action_data(&o));
            for e in &o.events {
                if let NodeEvent::LinkEstablished { link_id, .. } = e {
                    responder_link = Some(*link_id);
                }
            }
        }
        for pkt in back {
            let o = initiator.handle_packet(InterfaceId(i_iface), &pkt);
            to_responder.extend(action_data(&o));
        }
    }
    Pair {
        initiator,
        responder,
        i_iface,
        r_iface,
        caller_link,
        responder_link: responder_link.expect("responder link active"),
    }
}

/// Bounce packets between the two nodes until none are in flight. Returns
/// the events each side emitted, initiator first.
fn pump(
    pair: &mut Pair,
    mut to_responder: Vec<Vec<u8>>,
    mut to_initiator: Vec<Vec<u8>>,
) -> (Vec<NodeEvent>, Vec<NodeEvent>) {
    let mut i_events = Vec::new();
    let mut r_events = Vec::new();
    for _ in 0..20_000 {
        if to_responder.is_empty() && to_initiator.is_empty() {
            break;
        }
        for pkt in to_responder.drain(..) {
            let o = pair
                .responder
                .handle_packet(InterfaceId(pair.r_iface), &pkt);
            to_initiator.extend(action_data(&o));
            r_events.extend(o.events);
        }
        for pkt in core::mem::take(&mut to_initiator) {
            let o = pair
                .initiator
                .handle_packet(InterfaceId(pair.i_iface), &pkt);
            to_responder.extend(action_data(&o));
            i_events.extend(o.events);
        }
    }
    (i_events, r_events)
}

/// Position-dependent, compressible bytes: a misplaced segment boundary
/// changes the joined body and fails the exact comparison.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// A msgpack bin past one segment on its own.
fn oversized_value() -> Vec<u8> {
    let mut v = Vec::new();
    write_bin(&mut v, &pattern(RESOURCE_MAX_EFFICIENT_SIZE + 200_000));
    v
}

fn no_failures(events: &[NodeEvent]) {
    let failures: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                NodeEvent::ResourceFailed { .. } | NodeEvent::RequestTimedOut { .. }
            )
        })
        .collect();
    assert!(failures.is_empty(), "no failure in the lab: {failures:?}");
}

/// Send a small request and return its id with the responder's events.
fn small_request(pair: &mut Pair) -> [u8; TRUNCATED_HASHBYTES] {
    let (request_id, out) = pair
        .initiator
        .send_request(&pair.caller_link, "/big", None, None)
        .expect("send_request");
    let (_, r_events) = pump(pair, action_data(&out), Vec::new());
    assert!(
        r_events.iter().any(
            |e| matches!(e, NodeEvent::RequestReceived { request_id: r, .. } if *r == request_id)
        ),
        "responder must see the small request: {r_events:?}"
    );
    request_id
}

fn received_response(
    events: &[NodeEvent],
    request_id: [u8; TRUNCATED_HASHBYTES],
) -> (Vec<u8>, Option<Vec<u8>>) {
    let found: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            NodeEvent::ResponseReceived {
                request_id: r,
                response_data,
                metadata,
                ..
            } if *r == request_id => Some((response_data.clone(), metadata.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(found.len(), 1, "exactly one ResponseReceived");
    found.into_iter().next().unwrap()
}

/// A request body past one segment is sent split and served once, whole,
/// with the request id the reference computes over the full packed request
/// (`Link.py:931`).
#[test]
fn split_request_is_served_whole() {
    let mut pair = establish();
    let value = oversized_value();
    let (request_id, upload_hash, out) = pair
        .initiator
        .send_request_resource(&pair.caller_link, "/big", Some(&value), None)
        .expect("a request past one segment must be sent, split");
    let adv_shape = pair
        .initiator
        .link(&pair.caller_link)
        .and_then(|l| l.outgoing_resource())
        .map(|r| (*r.resource_hash(), r.flags().split, r.flags().is_request));
    assert_eq!(
        adv_shape,
        Some((upload_hash, true, true)),
        "segment 1 is a split request"
    );

    let (i_events, r_events) = pump(&mut pair, action_data(&out), Vec::new());
    no_failures(&i_events);
    no_failures(&r_events);
    let served: Vec<_> = r_events
        .iter()
        .filter_map(|e| match e {
            NodeEvent::RequestReceived {
                request_id: r,
                data,
                path,
                ..
            } => Some((*r, data.clone(), path.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(served.len(), 1, "the request is served exactly once");
    assert_eq!(served[0].0, request_id, "request id over the whole body");
    assert_eq!(served[0].2, "/big");
    assert_eq!(served[0].1, value, "the joined body is the request data");
    assert!(
        !r_events
            .iter()
            .any(|e| matches!(e, NodeEvent::ResourceCompleted { .. })),
        "request segments are link internals, never an application resource"
    );
    assert!(
        pair.initiator.request_resource_uploads.is_empty(),
        "the upload correlation follows the segments and ends with the last"
    );
    assert_eq!(pair.responder.split_reassembly_count(), 0);
    assert!(
        pair.responder.split_internal.is_none(),
        "the box is dropped"
    );

    // The large request is answered, and the answer reaches the caller.
    let (_, out) = pair
        .responder
        .send_response_resource(&pair.responder_link, &request_id, &value)
        .expect("respond");
    let (i_events, _) = pump(&mut pair, Vec::new(), action_data(&out));
    no_failures(&i_events);
    let (response, metadata) = received_response(&i_events, request_id);
    assert_eq!(response, value);
    assert_eq!(metadata, None);
}

/// A response past one segment is sent split and delivered once, unwrapped.
#[test]
fn split_response_is_received_whole() {
    let mut pair = establish();
    let request_id = small_request(&mut pair);
    let value = oversized_value();
    let (_, out) = pair
        .responder
        .send_response_resource(&pair.responder_link, &request_id, &value)
        .expect("a response past one segment must be sent, split");
    let (i_events, r_events) = pump(&mut pair, Vec::new(), action_data(&out));
    no_failures(&i_events);
    no_failures(&r_events);
    let (response, metadata) = received_response(&i_events, request_id);
    assert_eq!(
        response, value,
        "the joined body unwraps to the response value"
    );
    assert_eq!(metadata, None);
    assert_eq!(pair.initiator.split_reassembly_count(), 0);
    assert!(
        pair.initiator.split_internal.is_none(),
        "the box is dropped"
    );
    assert!(pair.initiator.response_resource_downloads.is_empty());
}

/// The streamed response path splits the same way.
#[test]
fn split_streamed_response_is_received_whole() {
    let mut pair = establish();
    let request_id = small_request(&mut pair);
    let value = oversized_value();
    let mut source = SliceSource::new(&value);
    let (_, out) = pair
        .responder
        .send_response_resource_from_source(&pair.responder_link, &request_id, &mut source)
        .expect("a streamed response past one segment must be sent, split");
    let (i_events, _) = pump(&mut pair, Vec::new(), action_data(&out));
    no_failures(&i_events);
    let (response, _) = received_response(&i_events, request_id);
    assert_eq!(response, value);
}

/// A file response past one segment keeps its metadata, which only segment 1
/// carries (`Resource.py:696`), and delivers the raw joined bytes.
#[test]
fn split_file_response_keeps_metadata() {
    let mut pair = establish();
    let request_id = small_request(&mut pair);
    let data = pattern(2 * RESOURCE_MAX_EFFICIENT_SIZE + 4096);
    let mut metadata = std::vec![0x81];
    write_fixstr(&mut metadata, "name");
    write_fixstr(&mut metadata, "big.bin");
    let (_, out) = pair
        .responder
        .send_file_response(&pair.responder_link, &request_id, &data, &metadata)
        .expect("a file response past one segment must be sent, split");
    let (i_events, _) = pump(&mut pair, Vec::new(), action_data(&out));
    no_failures(&i_events);
    let (response, got_metadata) = received_response(&i_events, request_id);
    assert_eq!(response.len(), data.len());
    assert_eq!(response, data, "three segments joined in order");
    assert_eq!(got_metadata.as_deref(), Some(metadata.as_slice()));
}

/// A later segment with no reassembly open on the link is ignored, not
/// accepted as a fresh request: nothing could put it together.
#[test]
fn stray_later_segment_is_ignored() {
    let mut pair = establish();
    let value = oversized_value();
    let (_, _, out) = pair
        .initiator
        .send_request_resource(&pair.caller_link, "/big", Some(&value), None)
        .expect("send");
    // Deliver nothing of segment 1 to the responder; build segment 2 by
    // hand from a fresh plan and advertise it alone.
    drop(out);
    let link = pair.initiator.links.get_mut(&pair.caller_link).unwrap();
    let mut plan = link.take_outgoing_segments().expect("split plan");
    let crypt = link.resource_crypt_params();
    plan.set_original_hash([0x5a; 32]);
    let tag = crate::resource::outgoing::InternalRequestTag {
        request_id: [0x11; TRUNCATED_HASHBYTES],
        is_response: false,
        advertisement_timeout_ms: None,
    };
    let segment2 = plan
        .build_segment(2, &crypt, &mut OsRng, TEST_TIME_MS, Some(&tag))
        .expect("segment 2");
    assert!(segment2.flags().is_request && segment2.flags().split);
    let adv = segment2.adv_packet().to_vec();
    let raw = link
        .build_data_packet_with_context(&adv, crate::packet::PacketContext::ResourceAdv, &mut OsRng)
        .expect("adv packet");
    let o = pair
        .responder
        .handle_packet(InterfaceId(pair.r_iface), &raw);
    assert!(action_data(&o).is_empty(), "no REQ for a stray segment");
    assert_eq!(
        pair.responder
            .link(&pair.responder_link)
            .map(|l| l.incoming_resource_count()),
        Some(0)
    );
}
