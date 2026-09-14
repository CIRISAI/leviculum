//! mvr: LXMF accepts an opportunistic delivery request only while the packed
//! message fits one encrypted packet. One byte over, and delivery switches to
//! a link on its own — the message still arrives, but from here on the sender
//! pays a link handshake per message that it explicitly did not want.
//!
//! Python performs the same switch and reports it on `LOG_DEBUG` and nowhere
//! else, overwriting `desired_method` as it goes, so the caller's wish is not
//! recoverable afterwards (`LXMessage.pack`,
//! `reference/LXMF/LXMF/LXMessage.py:399-401`). Ours is in
//! [`LxmfNode::representation`] (`leviculum-lxmf/src/node.rs:512`), and until
//! this file nothing in the suite pinned any of it.
//!
//! **The named failure mode:** a message at the single-packet threshold is
//! delivered over a link instead of in one packet, or a message one byte over
//! it is not. Both deliver either way, so *arrival is not the evidence* — the
//! carrier is. Each test asserts the chosen representation first and the new
//! [`LxmfNodeEvent::DeliveryMethodFallback`] second, so it does not stand or
//! fall with the event's shape.
//!
//! **Why the threshold is not written down here.** The byte counts live in
//! `leviculum-lxmf/src/constants.rs`, where a sibling test proves they equal
//! what Python computes from the MTU. This file takes the threshold from
//! those constants, so bending either the constants' derivation or the
//! comparison in `representation_of` turns one of the two cases red.
//!
//! **Ruling out every other source of a link** for the no-link case, which is
//! a silence and therefore needs its causes enumerated:
//!
//! * *Path discovery* — both peers learn the path from the announce exchange
//!   in [`connected_pair`], which is complete before the send. Nothing in this
//!   test requests a path, and a path request is a packet, never a link.
//! * *Announce handling* — [`LxmfNode::handle_event`] raises a link on an
//!   announce only for a destination an earlier [`LxmfNode::ensure_direct_link`]
//!   put on its wanted list. This test never calls it in the threshold case.
//! * *A link left from an earlier step* — each test builds its own pair, so
//!   there is no earlier step to leave one. The pre-send assertion records
//!   `link_count() == 0` on both sides anyway.
//! * *Anything else, named or not* — the ledger is not a list of causes this
//!   test thought of. Every `NodeEvent` from both peers is recorded by name,
//!   and every name beginning with `Link` counts as a link having happened,
//!   whatever raised it. `NodeCore::link_count` is checked on top of that,
//!   because it also counts a link request that went out and was never
//!   answered, which emits nothing at all.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use leviculum_core::{
    Action, Clock, DestinationHash, Identity, InterfaceId, MemoryStorage, NodeCore,
    NodeCoreBuilder, NodeEvent, TickOutput,
};
use leviculum_lxmf::constants::{ENCRYPTED_PACKET_MAX_CONTENT, LXMF_OVERHEAD};
use leviculum_lxmf::node::{
    DeliveryRepresentation, DirectLinkState, LxmfNode, LxmfNodeConfig, LxmfNodeError, LxmfNodeEvent,
};
use leviculum_lxmf::{announce, DeliveryMethod, Message};
use rand_core::OsRng;

const NOW_UNIX: f64 = 1_700_000_000.0;

/// The largest packed LXMF message that still travels as one opportunistic
/// packet, stated the way the protocol states it rather than as a number:
/// content of exactly [`ENCRYPTED_PACKET_MAX_CONTENT`] plus the LXMF envelope
/// around it. `representation_of` compares the same quantity after
/// subtracting the destination hash, which an opportunistic packet infers
/// from its own header instead of carrying.
fn threshold_packed_len() -> usize {
    ENCRYPTED_PACKET_MAX_CONTENT + LXMF_OVERHEAD
}

#[derive(Clone)]
struct TestClock(Rc<Cell<u64>>);

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
    fn wall_unix_secs(&self) -> Option<u64> {
        Some(NOW_UNIX as u64)
    }
}

type TestNode = NodeCore<OsRng, TestClock, MemoryStorage>;

fn identity_from(seed: u8) -> Identity {
    let mut private = [0u8; 64];
    for (index, byte) in private.iter_mut().enumerate() {
        *byte = seed.wrapping_add(index as u8);
    }
    Identity::from_private_key_bytes(&private).expect("deterministic identity")
}

fn take_packets(actions: Vec<Action>) -> Vec<Vec<u8>> {
    actions
        .into_iter()
        .map(|action| match action {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data,
        })
        .collect()
}

/// The leading identifier of a `NodeEvent`'s `Debug` form, which is its
/// variant name. Taken structurally rather than matched variant by variant so
/// a link raised by a path this test never considered is still counted.
fn event_name(event: &NodeEvent) -> String {
    format!("{event:?}")
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// One unified timeline for both peers, in wall order.
struct Timeline {
    start: Instant,
    lines: Vec<String>,
}

impl Timeline {
    fn push(&mut self, event: &str, fields: &str) {
        let t = self.start.elapsed().as_millis();
        self.lines.push(format!("{event} {fields} t={t}"));
    }

    fn dump(&self) -> String {
        self.lines.join("\n")
    }
}

/// One LXMF peer: a `NodeCore` with a registered delivery destination, driven
/// directly through [`LxmfNode`]. No `LxmfRouter`, so no retry timer, no
/// propagation sync and no stamp scheduling can put a link on the wire behind
/// the test's back.
struct Peer {
    name: &'static str,
    node: TestNode,
    lxmf: LxmfNode,
    destination: DestinationHash,
    identity: Identity,
    lxmf_events: Vec<LxmfNodeEvent>,
    received: Vec<Message>,
    /// Every `NodeEvent` this peer produced, by variant name.
    seen: Vec<String>,
}

fn peer(name: &'static str, seed: u8, clock: &Rc<Cell<u64>>) -> Peer {
    let mut node = NodeCoreBuilder::new().build(
        OsRng,
        TestClock(Rc::clone(clock)),
        MemoryStorage::with_defaults(),
    );
    let identity = identity_from(seed);
    let private = identity.private_key_bytes().expect("private delivery key");
    let destination = LxmfNode::delivery_destination(identity).expect("delivery destination");
    let destination_hash = *destination.hash();
    let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
        .expect("register delivery destination");
    Peer {
        name,
        node,
        lxmf,
        destination: destination_hash,
        identity: Identity::from_private_key_bytes(&private).expect("signing identity copy"),
        lxmf_events: Vec::new(),
        received: Vec::new(),
        seen: Vec::new(),
    }
}

impl Peer {
    fn absorb(&mut self, core: TickOutput, timeline: &mut Timeline) -> Vec<Vec<u8>> {
        let mut actions = core.actions;
        let mut events: std::collections::VecDeque<NodeEvent> = core.events.into();
        while let Some(event) = events.pop_front() {
            let name = event_name(&event);
            timeline.push("NODE_EVENT", &format!("peer={} event={name}", self.name));
            self.seen.push(name);
            let follow_up = self
                .lxmf
                .handle_event(&mut self.node, &event)
                .expect("LXMF adapter handles NodeCore event");
            for lxmf_event in &follow_up.events {
                timeline.push(
                    "LXMF_EVENT",
                    &format!("peer={} event={}", self.name, lxmf_event_name(lxmf_event)),
                );
                if let LxmfNodeEvent::MessageReceived(message) = lxmf_event {
                    self.received.push(message.clone());
                }
            }
            self.lxmf_events.extend(follow_up.events);
            actions.extend(follow_up.core.actions);
            events.extend(follow_up.core.events);
        }
        take_packets(actions)
    }

    fn receive(&mut self, packets: Vec<Vec<u8>>, timeline: &mut Timeline) -> Vec<Vec<u8>> {
        let mut outbound = Vec::new();
        for packet in packets {
            let core = self.node.handle_packet(InterfaceId(0), &packet);
            outbound.extend(self.absorb(core, timeline));
        }
        outbound
    }

    /// Names of every link-touching `NodeEvent` this peer produced.
    fn link_events(&self) -> Vec<&str> {
        self.seen
            .iter()
            .filter(|name| name.starts_with("Link"))
            .map(String::as_str)
            .collect()
    }
}

fn lxmf_event_name(event: &LxmfNodeEvent) -> String {
    format!("{event:?}")
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Shuttle packets between the two peers until the exchange quiesces.
fn pump(a: &mut Peer, b: &mut Peer, mut to_b: Vec<Vec<u8>>, timeline: &mut Timeline) {
    let mut to_a: Vec<Vec<u8>> = Vec::new();
    for _ in 0..512 {
        if !to_b.is_empty() {
            to_a.extend(b.receive(std::mem::take(&mut to_b), timeline));
        }
        if !to_a.is_empty() {
            to_b.extend(a.receive(std::mem::take(&mut to_a), timeline));
        }
        if to_a.is_empty() && to_b.is_empty() {
            return;
        }
    }
    panic!("the exchange did not quiesce:\n{}", timeline.dump());
}

/// Two peers that know each other's path and identity, and nothing more: no
/// link has been requested, and neither is on the other's wanted list.
fn connected_pair(timeline: &mut Timeline) -> (Peer, Peer) {
    let clock = Rc::new(Cell::new(1_000));
    let mut sender = peer("sender", 41, &clock);
    let mut receiver = peer("receiver", 141, &clock);

    let app_data = announce::delivery(Some(b"peer"), None);
    let from_sender = sender
        .node
        .announce_destination(&sender.destination, Some(&app_data))
        .expect("announce sender");
    let to_receiver = sender.absorb(from_sender, timeline);
    let from_receiver = receiver
        .node
        .announce_destination(&receiver.destination, Some(&app_data))
        .expect("announce receiver");
    let to_sender = receiver.absorb(from_receiver, timeline);

    pump(&mut sender, &mut receiver, to_receiver, timeline);
    pump(&mut receiver, &mut sender, to_sender, timeline);

    assert!(
        sender.node.has_path(&receiver.destination),
        "the sender must know the path before the threshold case starts:\n{}",
        timeline.dump()
    );
    assert_eq!(
        (sender.node.link_count(), receiver.node.link_count()),
        (0, 0),
        "the announce exchange must not have raised a link:\n{}",
        timeline.dump()
    );
    (sender, receiver)
}

/// An opportunistic message from `sender` to `receiver` whose packed form is
/// exactly `packed_len` bytes.
///
/// Everything but the body length is fixed: same destination, same source
/// identity, same title, same timestamp, same requested method. The body
/// length is found by measuring, never by arithmetic on the msgpack framing,
/// so the two cases differ in the one byte they claim to differ in.
fn message_of_packed_len(sender: &Peer, receiver: &Peer, packed_len: usize) -> Message {
    let build = |body_len: usize| {
        Message::create(
            receiver.destination.into_bytes(),
            sender.destination.into_bytes(),
            &sender.identity,
            NOW_UNIX + 0.25,
            b"mvr".to_vec(),
            vec![b'x'; body_len],
            Vec::new(),
            DeliveryMethod::Opportunistic,
        )
        .expect("opportunistic message")
    };
    for body_len in 0..=packed_len {
        let message = build(body_len);
        if message.pack().len() == packed_len {
            return message;
        }
    }
    panic!("no body length packs to exactly {packed_len} bytes");
}

/// The representation the `Submitted` event reports, which is the carrier the
/// message actually went out on — not what a second call to the decision
/// function would say.
fn submitted_representation(events: &[LxmfNodeEvent]) -> DeliveryRepresentation {
    events
        .iter()
        .find_map(|event| match event {
            LxmfNodeEvent::Submitted { representation, .. } => Some(*representation),
            _ => None,
        })
        .expect("the send reported no submission")
}

fn fallbacks(events: &[LxmfNodeEvent]) -> Vec<&LxmfNodeEvent> {
    events
        .iter()
        .filter(|event| matches!(event, LxmfNodeEvent::DeliveryMethodFallback { .. }))
        .collect()
}

#[test]
fn at_the_threshold_one_packet_and_no_link() {
    let mut timeline = Timeline {
        start: Instant::now(),
        lines: Vec::new(),
    };
    let (mut sender, mut receiver) = connected_pair(&mut timeline);

    let message = message_of_packed_len(&sender, &receiver, threshold_packed_len());
    timeline.push(
        "SEND",
        &format!("case=threshold packed_len={}", message.pack().len()),
    );
    let output = sender
        .lxmf
        .send(&mut sender.node, &message)
        .expect("the threshold message is deliverable without a link");

    // The carrier first.
    assert_eq!(
        submitted_representation(&output.events),
        DeliveryRepresentation::OpportunisticPacket,
        "a message at the single-packet threshold must go out as one packet:\n{}",
        timeline.dump()
    );
    // Then the event, which reports nothing because nothing was given up.
    assert!(
        fallbacks(&output.events).is_empty(),
        "nothing fell back, so nothing may be reported: {:?}",
        fallbacks(&output.events)
    );

    let packets = sender.absorb(output.core, &mut timeline);
    pump(&mut sender, &mut receiver, packets, &mut timeline);

    assert_eq!(
        (sender.node.link_count(), receiver.node.link_count()),
        (0, 0),
        "the opportunistic delivery raised a link:\n{}",
        timeline.dump()
    );
    assert!(
        sender.link_events().is_empty() && receiver.link_events().is_empty(),
        "link events on a delivery that must not use one: sender={:?} receiver={:?}\n{}",
        sender.link_events(),
        receiver.link_events(),
        timeline.dump()
    );
    assert!(
        sender.lxmf.direct_link(&receiver.destination).is_none(),
        "the sender holds a direct link it never asked for:\n{}",
        timeline.dump()
    );
    // Arrival is the control, not the evidence: both sizes arrive.
    assert_eq!(
        receiver.received.len(),
        1,
        "the packet did not arrive, so the no-link result says nothing:\n{}",
        timeline.dump()
    );
    assert_eq!(receiver.received[0].content, message.content);
}

#[test]
fn one_byte_over_the_threshold_switches_to_the_link() {
    let mut timeline = Timeline {
        start: Instant::now(),
        lines: Vec::new(),
    };
    let (mut sender, mut receiver) = connected_pair(&mut timeline);

    let message = message_of_packed_len(&sender, &receiver, threshold_packed_len() + 1);
    assert_eq!(
        message.method,
        DeliveryMethod::Opportunistic,
        "the caller's request is unchanged; only the body grew by one byte"
    );
    timeline.push(
        "SEND",
        &format!(
            "case=threshold_plus_one packed_len={}",
            message.pack().len()
        ),
    );

    // The switch, at the moment it costs something: the identical send that
    // needed nothing one byte ago now needs a link, and says so.
    let refused = sender.lxmf.send(&mut sender.node, &message);
    assert_eq!(
        refused.err(),
        Some(LxmfNodeError::DirectLinkUnavailable),
        "one byte over the threshold the message must no longer be deliverable \
         as a packet:\n{}",
        timeline.dump()
    );
    assert_eq!(
        sender.node.link_count(),
        0,
        "the refusal must not have raised a link by itself:\n{}",
        timeline.dump()
    );

    let (state, output) = sender
        .lxmf
        .ensure_direct_link(&mut sender.node, receiver.destination)
        .expect("the peer is known, so a link can be started");
    assert!(
        matches!(state, DirectLinkState::Started(_)),
        "expected a fresh link request, got {state:?}:\n{}",
        timeline.dump()
    );
    let packets = sender.absorb(output.core, &mut timeline);
    pump(&mut sender, &mut receiver, packets, &mut timeline);
    assert!(
        sender.lxmf.direct_link(&receiver.destination).is_some(),
        "the link the fallback forces did not come up:\n{}",
        timeline.dump()
    );

    let output = sender
        .lxmf
        .send(&mut sender.node, &message)
        .expect("with the link up the fallback delivery goes through");

    // The carrier first.
    assert_eq!(
        submitted_representation(&output.events),
        DeliveryRepresentation::DirectPacket,
        "one byte over the threshold the message must travel over the link:\n{}",
        timeline.dump()
    );
    // Then the event: the switch is reported, with both methods and the size.
    assert_eq!(
        fallbacks(&output.events),
        vec![&LxmfNodeEvent::DeliveryMethodFallback {
            message_id: message.message_id,
            requested: DeliveryMethod::Opportunistic,
            chosen: DeliveryMethod::Direct,
            packed_len: message.pack().len(),
        }],
        "the switch away from the requested method was not reported:\n{}",
        timeline.dump()
    );

    let packets = sender.absorb(output.core, &mut timeline);
    pump(&mut sender, &mut receiver, packets, &mut timeline);
    assert_eq!(
        receiver.received.len(),
        1,
        "the fallback delivery did not arrive:\n{}",
        timeline.dump()
    );
    assert_eq!(receiver.received[0].content, message.content);
}
