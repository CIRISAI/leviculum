//! What a board PROMISES an inbound sender, against what it actually does
//! with the message — the two must agree.
//!
//! # Why this file exists
//!
//! A board with a telemetry target registers an `lxmf.delivery` destination
//! (`leviculum_nrf::telemetry::register_delivery_destination`) and announces
//! it before every report, because a receiver can only verify the report's
//! signature against a key it heard in an announce. That announce is a claim
//! every peer on the mesh reads the same way Python-RNS reads it: *messages
//! sent to this hash will be received*.
//!
//! The board cannot keep that claim. It has no inbox, no message store and
//! `max_links(Some(0))`; the one consumer of what arrives there is the
//! telemetry reporter, which keeps a Sideband telemetry request and discards
//! everything else. Until this file existed the destination still carried
//! `ProofStrategy::All` (inherited from [`LxmfNode::delivery_destination`]),
//! so `NodeCore` answered every arrival with a signed proof
//! (`leviculum-core/src/node/mod.rs`, the `ProofRequested` arm) with no
//! application involvement — and a sender's LXMF marks a message DELIVERED on
//! exactly that proof. The message was then dropped without a log line.
//!
//! Silent data loss, confirmed to the other side, is the worst shape we have,
//! and it is precisely the neighbour behaviour Priority 1's second clause
//! forbids.
//!
//! The claim pinned here: a peer that sends an ordinary LXMF message to a
//! board's announced delivery destination is never told it was delivered. The
//! positive control next to it sends the identical message to a node that DOES
//! have an inbox, which must still confirm — otherwise a zero above would only
//! mean the harness cannot observe a confirmation at all.
//!
//! Run: `cargo test -p leviculum-lxmf --test delivery_promise`

use core::cell::Cell;
use std::rc::Rc;

use leviculum_core::transport::{Action, TickOutput};
use leviculum_core::{
    Clock, Destination, DestinationHash, Identity, InterfaceId, MemoryStorage, NodeCore,
    NodeCoreBuilder, NodeEvent,
};
use leviculum_lxmf::{DeliveryMethod, LxmfNode, LxmfNodeConfig, Message};
use rand_core::OsRng;

/// The board's only carrier.
const LORA_IFACE: InterfaceId = InterfaceId(0);
const LORA_IFACE_NAME: &str = "lora_sx1262";
const LORA_HW_MTU: u32 = 508;

/// A plausible wall clock: a node near the epoch is one every peer orders
/// last (Codeberg #155).
const START_MS: u64 = 1_780_000_000_000;

/// The sender's own `lxmf.delivery` hash, as it travels in the message. Its
/// value decides nothing here — the receiver's proof strategy is read before
/// anything looks at the payload — so a fixed one keeps the test readable.
const PEER_LXMF_HASH: [u8; 16] = [0x5a; 16];

#[derive(Clone)]
struct StepClock(Rc<Cell<u64>>);

impl Clock for StepClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
}

type TestNode = NodeCore<OsRng, StepClock, MemoryStorage>;

fn identity_from(seed: u8) -> Identity {
    let mut private = [0u8; 64];
    for (index, byte) in private.iter_mut().enumerate() {
        *byte = seed.wrapping_add(index as u8);
    }
    Identity::from_private_key_bytes(&private).expect("deterministic identity")
}

/// A node configured the way a board configures itself: one LoRa interface,
/// no links.
fn node(clock: &StepClock) -> TestNode {
    let mut node = NodeCoreBuilder::new()
        .enable_transport(true)
        .max_links(Some(0))
        .build(OsRng, clock.clone(), MemoryStorage::with_defaults());
    node.set_interface_name(LORA_IFACE.0, LORA_IFACE_NAME.to_string());
    node.set_interface_hw_mtu(LORA_IFACE.0, LORA_HW_MTU);
    node.set_interface_mode(LORA_IFACE.0, leviculum_core::InterfaceMode::Gateway);
    node
}

/// What actually leaves a node with exactly one interface.
fn on_air(out: &TickOutput) -> Vec<Vec<u8>> {
    out.actions
        .iter()
        .filter_map(|a| match a {
            Action::SendPacket { iface, data, .. } => (*iface == LORA_IFACE).then(|| data.clone()),
            Action::Broadcast {
                data,
                exclude_iface,
                exclude_ifaces,
            } => (*exclude_iface != Some(LORA_IFACE) && !exclude_ifaces.contains(&LORA_IFACE))
                .then(|| data.clone()),
        })
        .collect()
}

/// A receiver that has registered and announced a delivery destination —
/// the only state in which any of this can happen.
struct Receiver {
    node: TestNode,
    delivery: DestinationHash,
}

impl Receiver {
    fn with_destination(clock: &StepClock, destination: Destination) -> Self {
        let mut node = node(clock);
        let delivery = *destination.hash();
        node.register_destination(destination);
        Self { node, delivery }
    }

    /// A board: the destination `register_delivery_destination` builds, with
    /// nothing behind it that could keep a message.
    fn board(clock: &StepClock) -> Self {
        let destination = LxmfNode::delivery_destination_without_inbox(identity_from(1))
            .expect("delivery destination");
        Self::with_destination(clock, destination)
    }

    /// The positive control: a node with an inbox, registered the way `lntd`
    /// registers, which keeps `ProofStrategy::All` and must confirm.
    fn with_inbox(clock: &StepClock) -> Self {
        let mut node = node(clock);
        let destination =
            LxmfNode::delivery_destination(identity_from(2)).expect("delivery destination");
        let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
            .expect("register delivery destination");
        let delivery = lxmf.delivery_destination_hash();
        Self { node, delivery }
    }

    /// The announce a reporter sends before every report — the claim under
    /// test, in the bytes a peer actually hears.
    fn announce(&mut self) -> Vec<Vec<u8>> {
        let out = self
            .node
            .announce_destination(&self.delivery, Some(b"board"))
            .expect("announce the delivery destination");
        on_air(&out)
    }
}

/// One peer that heard the announce and takes it at its word: it holds the
/// receiver's key and a path, which is everything needed to send an ordinary
/// LXMF message.
fn peer_that_heard(clock: &StepClock, receiver: &mut Receiver) -> (TestNode, Identity) {
    let mut peer = node(clock);
    for frame in receiver.announce() {
        let _ = peer.handle_packet(LORA_IFACE, &frame);
    }
    assert!(
        peer.has_path(&receiver.delivery),
        "the announce taught the peer a path — without it nothing below is sent"
    );
    let identity = identity_from(7);
    (peer, identity)
}

/// An ordinary LXMF message — a human writing to the board, no telemetry
/// field anywhere in it. This is what the announce invites and what a board
/// has nowhere to put.
fn a_plain_message(delivery: DestinationHash, signer: &Identity) -> Vec<u8> {
    Message::create(
        delivery.into_bytes(),
        PEER_LXMF_HASH,
        signer,
        1_780_000_000.0,
        b"hello".to_vec(),
        b"are you receiving this?".to_vec(),
        Vec::new(),
        DeliveryMethod::Opportunistic,
    )
    .expect("an opportunistic message to a key we hold")
    .on_air()
    .expect("an opportunistic message has an on-air form")
}

/// Send one plain message from a peer to `receiver` and report how many
/// [`NodeEvent::PacketDeliveryConfirmed`] the SENDER raised, plus how many
/// frames came back.
fn confirmations_for_a_plain_message(receiver: &mut Receiver) -> (usize, usize) {
    let clock = StepClock(Rc::new(Cell::new(START_MS)));
    let (mut peer, peer_identity) = peer_that_heard(&clock, receiver);

    let message = a_plain_message(receiver.delivery, &peer_identity);
    let (_packet_hash, out) = peer
        .send_single_packet(&receiver.delivery, &message)
        .expect("a peer with a path and a key can send");

    let mut answered = Vec::new();
    for frame in on_air(&out) {
        let back = receiver.node.handle_packet(LORA_IFACE, &frame);
        answered.extend(on_air(&back));
    }

    let mut confirmed = 0usize;
    for frame in &answered {
        let out = peer.handle_packet(LORA_IFACE, frame);
        confirmed += out
            .events
            .iter()
            .filter(|e| matches!(e, NodeEvent::PacketDeliveryConfirmed { .. }))
            .count();
    }
    (confirmed, answered.len())
}

/// The claim at the wire, asserted where it is felt: at the SENDER. A peer
/// sends an ordinary LXMF message to a board's announced delivery
/// destination, and its node must not raise
/// [`NodeEvent::PacketDeliveryConfirmed`] — the event LXMF turns into
/// DELIVERED, and the one thing a board that throws the message away may not
/// cause.
///
/// Read at the sender and not as "the board emitted no proof packet" because
/// the sender's verdict is the harm: a proof the sender cannot verify would
/// be a different bug with the same frame count.
#[test]
fn a_board_does_not_confirm_delivery_of_a_message_it_throws_away() {
    let clock = StepClock(Rc::new(Cell::new(START_MS)));
    let mut board = Receiver::board(&clock);
    let (confirmed, answered) = confirmations_for_a_plain_message(&mut board);

    assert_eq!(
        confirmed, 0,
        "the board answered with a delivery proof for a message it has no \
         inbox for: the sender's LXMF now says DELIVERED and the bytes are \
         gone ({answered} frame(s) came back)"
    );
}

/// Positive control for the assertion above: the identical message to a node
/// that DOES have an inbox is still confirmed. Without this, a board's zero
/// could just as well mean the harness never produces a confirmation.
#[test]
fn a_node_with_an_inbox_still_confirms_the_same_message() {
    let clock = StepClock(Rc::new(Cell::new(START_MS)));
    let mut receiver = Receiver::with_inbox(&clock);
    let (confirmed, answered) = confirmations_for_a_plain_message(&mut receiver);

    assert_eq!(
        confirmed, 1,
        "a receiver that keeps the message confirms it exactly once \
         ({answered} frame(s) came back)"
    );
}
