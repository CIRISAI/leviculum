//! mvr (Codeberg #404): a lost first request must not strand a transfer
//! while its sender keeps advertising.
//!
//! The field case, `lora_4node_contention_lnsd` on e9e1dca1 (periculum 402),
//! gamma to delta: delta answered the advertisement with a REQ that no node
//! on the medium logged, and its own first-part timeout (about 30 s at the
//! link's 1851 ms RTT) re-sent a REQ that was lost as well. In between,
//! gamma re-advertised three times, every 12.1 s (`6 * rtt + 1 s`), and
//! delta answered each with "Resource ADV on link with active resource,
//! ignoring". Gamma's fourth advertisement window expired at 48.4 s and the
//! transfer failed with no part ever sent.
//!
//! A re-advertisement of the resource the receiver already holds can only
//! mean the sender never heard a request: a sender that has one leaves
//! `Advertised` for good. So the receiver answers it with its request again,
//! as long as no part of that resource has arrived yet. A sender handles a
//! second REQ for the same window like any other, so the answer is
//! idempotent for it.
//!
//! The harness reproduces the field's losses exactly: it drops the REQ that
//! answers the first advertisement and every REQ the receiver's own timeout
//! produces, and passes everything else. The link is built with the field's
//! RTT so both timers keep the field's ratio.

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
use crate::traits::{Clock, NoStorage};
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// Half the field's link RTT (1851 ms): each handshake leg costs this much.
const HANDSHAKE_LEG_MS: u64 = 925;

/// Poll step of the simulated clock.
const STEP_MS: u64 = 250;

/// Simulated horizon: past the sender's whole advertisement budget
/// (`sender_advertisement_budget_ms`, 6 windows of `6 * rtt + 1 s` or
/// 72.6 s at the field RTT).
const HORIZON_MS: u64 = 90_000;

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

fn count_context(packets: &[Vec<u8>], context: PacketContext) -> usize {
    packets.iter().filter(|p| is_context(p, context)).count()
}

struct Pair {
    sender: EndpointNode,
    receiver: EndpointNode,
    s_iface: usize,
    r_iface: usize,
    sender_link: LinkId,
}

impl Pair {
    fn advance(&mut self, ms: u64) {
        self.sender.transport.clock().advance(ms);
        self.receiver.transport.clock().advance(ms);
    }

    fn now_ms(&self) -> u64 {
        self.sender.transport.clock().now_ms()
    }
}

/// Establish the link with the field's RTT; the receiver (the link's
/// responder, as delta was) accepts every Resource.
fn establish() -> Pair {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut receiver = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrreadv",
        &["adv"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    dest.set_resource_strategy(ResourceStrategy::AcceptAll);
    let dest_hash = *dest.hash();
    receiver.register_destination(dest);
    let r_iface = add_iface(&mut receiver, "R_mesh");

    let mut sender = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let s_iface = add_iface(&mut sender, "S_mesh");

    let (sender_link, _routed, out) = sender.connect(dest_hash, &signing_key).expect("connect");
    let mut pair = Pair {
        sender,
        receiver,
        s_iface,
        r_iface,
        sender_link,
    };
    let mut established = false;
    let mut for_receiver = action_data(&out);
    for _ in 0..8 {
        if for_receiver.is_empty() {
            break;
        }
        pair.advance(HANDSHAKE_LEG_MS);
        let mut back = Vec::new();
        for pkt in for_receiver {
            let o = pair.receiver.handle_packet(InterfaceId(pair.r_iface), &pkt);
            established |= o
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkEstablished { .. }));
            back.extend(action_data(&o));
        }
        pair.advance(HANDSHAKE_LEG_MS);
        for_receiver = Vec::new();
        for pkt in back {
            let o = pair.sender.handle_packet(InterfaceId(pair.s_iface), &pkt);
            for_receiver.extend(action_data(&o));
        }
    }
    assert!(established, "receiver side must reach Active");
    pair
}

fn payload() -> Vec<u8> {
    (0..3000usize).map(|i| ((i * 31 + 7) % 251) as u8).collect()
}

/// Advertise and drop the receiver's first REQ; returns the advertisement.
fn advertise_and_lose_the_request(pair: &mut Pair) -> Vec<u8> {
    let (_hash, out) = pair
        .sender
        .send_resource(&pair.sender_link, &payload(), None, false)
        .expect("sender advertises the resource");
    let adv = action_data(&out);
    assert_eq!(count_context(&adv, PacketContext::ResourceAdv), 1);
    let answer = action_data(
        &pair
            .receiver
            .handle_packet(InterfaceId(pair.r_iface), &adv[0]),
    );
    assert_eq!(
        count_context(&answer, PacketContext::ResourceReq),
        1,
        "the first advertisement is answered with one REQ, which the harness drops"
    );
    adv[0].clone()
}

/// The single failure mode: a re-advertisement of the resource the receiver
/// already holds, with no part arrived, gets no answer.
#[test]
fn readvertisement_before_any_part_is_answered_with_a_request() {
    let mut pair = establish();
    advertise_and_lose_the_request(&mut pair);

    let mut readv = Vec::new();
    while readv.is_empty() && pair.now_ms() < TEST_TIME_MS + HORIZON_MS {
        pair.advance(STEP_MS);
        let out = action_data(&pair.sender.handle_timeout());
        readv.extend(
            out.into_iter()
                .filter(|p| is_context(p, PacketContext::ResourceAdv)),
        );
    }
    assert_eq!(
        readv.len(),
        1,
        "the sender re-advertises once its window expires"
    );

    let answer = action_data(
        &pair
            .receiver
            .handle_packet(InterfaceId(pair.r_iface), &readv[0]),
    );
    assert_eq!(
        count_context(&answer, PacketContext::ResourceReq),
        1,
        "a re-advertisement means the sender never heard the request; the \
         receiver must send it again"
    );
}

/// The field chain end to end: first REQ lost, the receiver's own retry lost,
/// the sender re-advertises. The transfer must complete before the sender's
/// advertisement budget runs out.
#[test]
fn lost_first_request_does_not_strand_the_transfer() {
    complete_despite_lost_requests(&[]);
}

/// The same chain with the sender's first re-advertisement lost as well
/// (Codeberg #406): a later one still draws the request.
#[test]
fn lost_first_request_and_first_readvertisement_do_not_strand_the_transfer() {
    complete_despite_lost_requests(&[1]);
}

/// The first three re-advertisements lost too: the whole count the reference
/// spends. The sender's budget reaches past the receiver's second request
/// retry (`sender_advertisement_budget_ms`), which at the field RTT is a
/// fourth re-advertisement, and that one draws the request. Red while the
/// sender stopped after three.
#[test]
fn transfer_survives_three_lost_readvertisements() {
    complete_despite_lost_requests(&[1, 2, 3]);
}

/// Drive the transfer with every request the receiver's timeout produces
/// lost, and the sender's re-advertisements at the 1-based ordinals in
/// `lost_readvertisements` lost as well; assert it completes.
fn complete_despite_lost_requests(lost_readvertisements: &[usize]) {
    let mut pair = establish();
    advertise_and_lose_the_request(&mut pair);
    let mut readvertisements = 0usize;

    let mut sender_done = None;
    let mut receiver_done = None;
    let mut sender_failed = None;
    let mut dropped_timeout_reqs = 0usize;
    while pair.now_ms() < TEST_TIME_MS + HORIZON_MS && sender_done.is_none() {
        pair.advance(STEP_MS);

        // The receiver's own timeout retry is lost, as 38aefedf was.
        let r_tick = pair.receiver.handle_timeout();
        let mut to_sender: Vec<Vec<u8>> = Vec::new();
        for pkt in action_data(&r_tick) {
            if is_context(&pkt, PacketContext::ResourceReq) {
                dropped_timeout_reqs += 1;
            } else {
                to_sender.push(pkt);
            }
        }
        let s_tick = pair.sender.handle_timeout();
        let mut events = s_tick.events.clone();
        let mut to_receiver = Vec::new();
        for pkt in action_data(&s_tick) {
            if is_context(&pkt, PacketContext::ResourceAdv) {
                readvertisements += 1;
                if lost_readvertisements.contains(&readvertisements) {
                    continue;
                }
            }
            to_receiver.push(pkt);
        }

        // Everything else crosses the link instantly and in full.
        for _ in 0..64 {
            if to_sender.is_empty() && to_receiver.is_empty() {
                break;
            }
            let mut next_to_sender = Vec::new();
            for pkt in core::mem::take(&mut to_receiver) {
                let o = pair.receiver.handle_packet(InterfaceId(pair.r_iface), &pkt);
                events.extend(o.events.iter().cloned());
                next_to_sender.extend(action_data(&o));
            }
            for pkt in core::mem::take(&mut to_sender) {
                let o = pair.sender.handle_packet(InterfaceId(pair.s_iface), &pkt);
                events.extend(o.events.iter().cloned());
                to_receiver.extend(action_data(&o));
            }
            to_sender = next_to_sender;
        }
        events.extend(pair.receiver.handle_timeout().events);

        for ev in events {
            match ev {
                NodeEvent::ResourceCompleted {
                    is_sender: true, ..
                } => sender_done = Some(pair.now_ms() - TEST_TIME_MS),
                NodeEvent::ResourceCompleted {
                    is_sender: false,
                    data,
                    ..
                } => receiver_done = Some(data),
                NodeEvent::ResourceFailed {
                    is_sender: true,
                    error,
                    ..
                } => sender_failed = Some((pair.now_ms() - TEST_TIME_MS, error)),
                _ => {}
            }
        }
        if sender_failed.is_some() {
            break;
        }
    }

    assert_eq!(
        sender_failed, None,
        "the sender gave up (at ms, error) with the receiver holding an \
         unanswered advertisement; receiver timeout REQs dropped: \
         {dropped_timeout_reqs}, re-advertisements sent: {readvertisements}, \
         lost: {lost_readvertisements:?}"
    );
    assert!(sender_done.is_some(), "the sender saw its transfer proven");
    assert_eq!(
        receiver_done.as_deref(),
        Some(payload().as_slice()),
        "the receiver assembled the advertised bytes"
    );
}

/// The answer is bounded to a receiver that has no part yet: once a part
/// arrived the sender has heard a request, and an advertisement replayed
/// after that (a late duplicate on the air) stays ignored.
#[test]
fn advertisement_after_a_part_arrived_stays_ignored() {
    let mut pair = establish();
    let (_hash, out) = pair
        .sender
        .send_resource(&pair.sender_link, &payload(), None, false)
        .expect("sender advertises the resource");
    let adv = action_data(&out).remove(0);
    let reqs = action_data(&pair.receiver.handle_packet(InterfaceId(pair.r_iface), &adv));
    assert_eq!(count_context(&reqs, PacketContext::ResourceReq), 1);

    let parts = action_data(
        &pair
            .sender
            .handle_packet(InterfaceId(pair.s_iface), &reqs[0]),
    );
    let first_part = parts
        .iter()
        .find(|p| is_context(p, PacketContext::Resource))
        .expect("the request is answered with parts");
    let _ = pair
        .receiver
        .handle_packet(InterfaceId(pair.r_iface), first_part);
    let progress = pair
        .receiver
        .links
        .values()
        .find_map(|link| link.incoming_resource_progress());
    assert!(
        progress.is_some_and(|p| p > 0.0),
        "the receiver stored the first part: progress {progress:?}"
    );

    let answer = action_data(&pair.receiver.handle_packet(InterfaceId(pair.r_iface), &adv));
    assert!(
        answer.is_empty(),
        "a replayed advertisement after the first part must not draw a REQ: {} packets",
        answer.len()
    );
}
