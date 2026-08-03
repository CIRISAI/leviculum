//! Direct-delivery attempt accounting and Resource-conclusion handling,
//! pinned against the LXMF/RNS reference.
//!
//! These pins were written for Codeberg PR #179 (nilu96) and driven red on
//! master before the contribution was integrated. They use only the public
//! crate API, so they observe the same surface an application does.
//!
//! Reference citations:
//!
//! * `reference/LXMF/LXMF/LXMRouter.py:2820-2839` — in the `DIRECT` branch,
//!   `delivery_attempts += 1` happens once, in the "no link exists" arm, when
//!   the router either creates the `RNS.Link` (2831) or requests the path
//!   (2837). Submitting over an already-active link (2784-2791) does not
//!   touch the counter.
//! * `reference/LXMF/LXMF/LXMessage.py:597-606` — a Resource that concludes
//!   non-`COMPLETE` tears its link down and returns the message to `OUTBOUND`
//!   (retryable), *except* when the Resource status is `REJECTED`, which sets
//!   the message state to `REJECTED` and leaves the link alone.
//! * `reference/Reticulum/RNS/Link.py:1143-1150` and
//!   `reference/Reticulum/RNS/Resource.py:1106-1110` — a receiver cancel
//!   (`RESOURCE_RCL`) on the sending side is exactly what produces
//!   `Resource.REJECTED`. Our equivalent is `NodeEvent::ResourceFailed` with
//!   `is_sender: true` and `ResourceError::Cancelled`
//!   (`leviculum-core/src/node/link_management.rs:2805-2816`).

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

use leviculum_core::{
    resource::ResourceError, Action, Clock, DestinationHash, Identity, InterfaceId, LinkId,
    MemoryStorage, NodeCore, NodeCoreBuilder, NodeEvent, TickOutput,
};
use leviculum_lxmf::{
    announce,
    node::{LxmfNode, LxmfNodeConfig},
    router::{LxmfRouter, MessageState, RouterConfig, RouterEvent, RouterOutput},
    DeliveryMethod, Message,
};
use rand_core::OsRng;

const NOW_UNIX: f64 = 1_700_000_000.0;

#[derive(Clone)]
struct TestClock(Rc<Cell<u64>>);

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
    /// The router derives every wall-clock field from `Transport::emission_secs`
    /// (Codeberg #182), so wall time is injected through the platform clock.
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

fn test_node(clock: TestClock) -> TestNode {
    NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults())
}

fn take_packets(actions: Vec<Action>) -> Vec<Vec<u8>> {
    actions
        .into_iter()
        .map(|action| match action {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data,
        })
        .collect()
}

/// A sender: `NodeCore` driven through a full `LxmfRouter`.
struct Sender {
    node: TestNode,
    router: LxmfRouter,
    clock: Rc<Cell<u64>>,
    destination: DestinationHash,
    signing_identity: Identity,
    events: Vec<RouterEvent>,
}

fn sender(seed: u8) -> Sender {
    let clock = Rc::new(Cell::new(1_000));
    let mut node = test_node(TestClock(Rc::clone(&clock)));
    let identity = identity_from(seed);
    let private = identity.private_key_bytes().expect("private delivery key");
    let identity_hash = *identity.hash();
    let destination = LxmfNode::delivery_destination(identity).expect("delivery destination");
    let destination_hash = *destination.hash();
    let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
        .expect("register delivery destination");
    Sender {
        node,
        router: LxmfRouter::new(lxmf, identity_hash, RouterConfig::default()),
        clock,
        destination: destination_hash,
        signing_identity: Identity::from_private_key_bytes(&private)
            .expect("signing identity copy"),
        events: Vec::new(),
    }
}

impl Sender {
    fn absorb_core(&mut self, core: TickOutput) -> Vec<Vec<u8>> {
        let mut actions = core.actions;
        let mut events: VecDeque<NodeEvent> = core.events.into();
        while let Some(event) = events.pop_front() {
            let follow_up = self
                .router
                .handle_event(&mut self.node, &event)
                .expect("router handles NodeCore event");
            self.events.extend(follow_up.events);
            actions.extend(follow_up.core.actions);
            events.extend(follow_up.core.events);
        }
        take_packets(actions)
    }

    fn absorb_router(&mut self, output: RouterOutput) -> Vec<Vec<u8>> {
        self.events.extend(output.events);
        self.absorb_core(output.core)
    }

    fn receive(&mut self, packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        let mut outbound = Vec::new();
        for packet in packets {
            let core = self.node.handle_packet(InterfaceId(0), &packet);
            outbound.extend(self.absorb_core(core));
        }
        outbound
    }

    fn advance_ms(&mut self, delta: u64) {
        self.clock.set(self.clock.get() + delta);
    }

    fn tick(&mut self) -> Vec<Vec<u8>> {
        let output = self.router.tick(&mut self.node).expect("tick");
        self.absorb_router(output)
    }

    fn attempts(&self, id: &[u8; 32]) -> u8 {
        self.router
            .outbound()
            .get(id)
            .expect("message still queued")
            .attempts
    }

    fn state(&self, id: &[u8; 32]) -> Option<MessageState> {
        self.router.outbound().get(id).map(|entry| entry.state)
    }
}

/// A receiver: `NodeCore` with a plain registered LXMF delivery destination.
struct Receiver {
    node: TestNode,
    lxmf: LxmfNode,
    destination: DestinationHash,
    /// Hashes of the transfers this receiver has started accepting. NodeCore
    /// only emits `ResourceTransferStarted` on the receiving side, so this is
    /// where the sender's in-flight Resource hash is observable.
    accepted_resources: Vec<[u8; 32]>,
}

fn receiver(seed: u8) -> Receiver {
    let clock = Rc::new(Cell::new(1_000));
    let mut node = test_node(TestClock(clock));
    let identity = identity_from(seed);
    let destination = LxmfNode::delivery_destination(identity).expect("delivery destination");
    let destination_hash = *destination.hash();
    let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
        .expect("register delivery destination");
    Receiver {
        node,
        lxmf,
        destination: destination_hash,
        accepted_resources: Vec::new(),
    }
}

impl Receiver {
    fn absorb_core(&mut self, core: TickOutput) -> Vec<Vec<u8>> {
        let mut actions = core.actions;
        let mut events: VecDeque<NodeEvent> = core.events.into();
        while let Some(event) = events.pop_front() {
            if let NodeEvent::ResourceTransferStarted {
                resource_hash,
                is_sender: false,
                ..
            } = &event
            {
                self.accepted_resources.push(*resource_hash);
            }
            let follow_up = self
                .lxmf
                .handle_event(&mut self.node, &event)
                .expect("receiver handles NodeCore event");
            actions.extend(follow_up.core.actions);
            events.extend(follow_up.core.events);
        }
        take_packets(actions)
    }

    fn receive(&mut self, packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        let mut outbound = Vec::new();
        for packet in packets {
            let core = self.node.handle_packet(InterfaceId(0), &packet);
            outbound.extend(self.absorb_core(core));
        }
        outbound
    }
}

/// Shuttle packets between the two peers until the exchange quiesces.
fn pump(sender: &mut Sender, receiver: &mut Receiver, mut to_receiver: Vec<Vec<u8>>) {
    let mut to_sender: Vec<Vec<u8>> = Vec::new();
    for _ in 0..512 {
        if !to_receiver.is_empty() {
            to_sender.extend(receiver.receive(std::mem::take(&mut to_receiver)));
        }
        if !to_sender.is_empty() {
            to_receiver.extend(sender.receive(std::mem::take(&mut to_sender)));
        }
        if to_sender.is_empty() && to_receiver.is_empty() {
            return;
        }
    }
    panic!("NodeCore exchange did not quiesce");
}

fn exchange_announces(sender: &mut Sender, receiver: &mut Receiver) {
    let app_data = announce::delivery(Some(b"peer"), None);
    let from_sender = sender
        .node
        .announce_destination(&sender.destination, Some(&app_data))
        .expect("announce sender");
    let to_receiver = sender.absorb_core(from_sender);
    let from_receiver = receiver
        .node
        .announce_destination(&receiver.destination, Some(&app_data))
        .expect("announce receiver");
    let to_sender = receiver.absorb_core(from_receiver);

    let replies_to_receiver = sender.receive(to_sender);
    pump(sender, receiver, replies_to_receiver);
    let replies_to_sender = receiver.receive(to_receiver);
    let mut back = replies_to_sender;
    for _ in 0..8 {
        if back.is_empty() {
            break;
        }
        back = sender.receive(std::mem::take(&mut back));
        if back.is_empty() {
            break;
        }
        back = receiver.receive(std::mem::take(&mut back));
    }
}

fn direct_message(
    seed: u8,
    destination: DestinationHash,
    source: &Identity,
    body: Vec<u8>,
) -> Message {
    Message::create(
        destination.into_bytes(),
        [seed; 16],
        source,
        NOW_UNIX + 0.25,
        b"direct".to_vec(),
        body,
        Vec::new(),
        DeliveryMethod::Direct,
    )
    .expect("direct message")
}

/// Shuttle at most `rounds` request/response round trips, so a transfer can
/// be observed mid-flight instead of run to completion.
fn exchange_rounds(
    sender: &mut Sender,
    receiver: &mut Receiver,
    mut to_receiver: Vec<Vec<u8>>,
    rounds: usize,
) {
    for _ in 0..rounds {
        let to_sender = receiver.receive(std::mem::take(&mut to_receiver));
        if to_sender.is_empty() {
            return;
        }
        to_receiver = sender.receive(to_sender);
        if to_receiver.is_empty() {
            return;
        }
    }
}

/// Queue a direct message and drive the router until NodeCore has it,
/// returning the message id, the link that carries it, and the packets the
/// submitting tick produced.
fn queue_and_submit(
    sender: &mut Sender,
    receiver: &mut Receiver,
    body: Vec<u8>,
    seed: u8,
) -> ([u8; 32], LinkId, Vec<Vec<u8>>) {
    let destination = receiver.destination;
    let message = direct_message(seed, destination, &sender.signing_identity, body);
    let id = message.message_id;
    let output = sender
        .router
        .enqueue(&sender.node, message)
        .expect("enqueue direct message");
    let packets = sender.absorb_router(output);
    pump(sender, receiver, packets);

    // Tick 1 finds no link and starts one. Python charges exactly this
    // attempt (LXMRouter.py:2825 with 2831/2837). A later tick submits over
    // the established link, which Python does without touching the counter
    // (LXMRouter.py:2784-2791). Stop at the submitting tick without pumping
    // its packets, so the observation is of a message in flight rather than
    // of one the receiver has already proved.
    let mut submitted = Vec::new();
    for _ in 0..8 {
        let packets = sender.tick();
        if sender.state(&id) == Some(MessageState::Sending) {
            submitted = packets;
            break;
        }
        pump(sender, receiver, packets);
        sender.advance_ms(11_000);
    }
    assert_eq!(
        sender.state(&id),
        Some(MessageState::Sending),
        "the direct message was never handed to NodeCore"
    );

    let link_id = sender
        .router
        .node()
        .direct_link(&destination)
        .expect("an active direct link carries the submission");
    (id, link_id, submitted)
}

#[test]
fn direct_submission_over_an_established_link_consumes_no_extra_attempt() {
    let mut sender = sender(21);
    let mut receiver = receiver(121);
    exchange_announces(&mut sender, &mut receiver);
    assert!(
        sender.node.has_path(&receiver.destination),
        "the sender must know a path before the direct attempt starts"
    );

    let (id, _link_id, _packets) = queue_and_submit(
        &mut sender,
        &mut receiver,
        b"small direct payload".to_vec(),
        7,
    );

    // One direct delivery cycle -- discover/establish, then submit -- is one
    // attempt in the reference (LXMRouter.py:2820-2839).
    assert_eq!(
        sender.attempts(&id),
        1,
        "one direct delivery cycle must consume exactly one delivery attempt"
    );
}

#[test]
fn a_retryable_outgoing_resource_failure_tears_down_its_direct_link() {
    let mut sender = sender(22);
    let mut receiver = receiver(122);
    exchange_announces(&mut sender, &mut receiver);

    let (_id, link_id, packets) =
        queue_and_submit(&mut sender, &mut receiver, vec![0x5au8; 2_048], 8);
    // One round trip: the receiver accepts the advertisement and the sender
    // starts the transfer. Its parts are then dropped, leaving the Resource
    // in flight.
    exchange_rounds(&mut sender, &mut receiver, packets, 1);
    let resource_hash = *receiver
        .accepted_resources
        .last()
        .expect("the submission is an outgoing Resource");

    let output = sender
        .router
        .handle_event(
            &mut sender.node,
            &NodeEvent::ResourceFailed {
                link_id,
                resource_hash,
                error: ResourceError::Timeout,
                is_sender: true,
            },
        )
        .expect("router handles the failed outgoing Resource");
    let _ = sender.absorb_router(output);

    // LXMessage.py:604-606 tears the link down before the message becomes
    // eligible for another attempt.
    assert!(
        sender.node.link(&link_id).is_none(),
        "a retryable Resource failure must tear its direct link down"
    );
}

#[test]
fn a_receiver_cancelled_resource_is_rejected_and_keeps_the_link() {
    let mut sender = sender(23);
    let mut receiver = receiver(123);
    exchange_announces(&mut sender, &mut receiver);

    let (id, link_id, packets) =
        queue_and_submit(&mut sender, &mut receiver, vec![0x3cu8; 2_048], 9);
    exchange_rounds(&mut sender, &mut receiver, packets, 1);
    let resource_hash = *receiver
        .accepted_resources
        .last()
        .expect("the submission is an outgoing Resource");

    sender.events.clear();
    let output = sender
        .router
        .handle_event(
            &mut sender.node,
            &NodeEvent::ResourceFailed {
                link_id,
                resource_hash,
                error: ResourceError::Cancelled,
                is_sender: true,
            },
        )
        .expect("router handles the receiver's cancel");
    let _ = sender.absorb_router(output);

    // Resource.py:1106-1110 -> LXMessage.py:601-602: REJECTED is terminal and
    // does not tear the link down.
    assert!(
        sender.events.iter().any(|event| matches!(
            event,
            RouterEvent::MessageState {
                message_id,
                state: MessageState::Rejected,
            } if *message_id == id
        )),
        "a receiver cancel is a terminal rejection, not a retry: {:?}",
        sender.events
    );
    assert!(
        !sender.router.outbound().contains_key(&id),
        "a rejected message must leave the outbound queue"
    );
    assert!(
        sender.node.link(&link_id).is_some(),
        "a rejection must preserve the reusable direct link"
    );
}
