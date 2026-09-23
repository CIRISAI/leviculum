//! What a board promises a peer that asks for a LINK to its announced
//! delivery destination, against what it can actually serve.
//!
//! # Why this file exists
//!
//! The sibling file `delivery_promise.rs` pins the single-packet half of the
//! same claim: a board with no inbox must not CONFIRM an ordinary LXMF
//! message it throws away. This file pins the worse half.
//!
//! A peer that heard the board's `lxmf.delivery` announce may open a link to
//! it — that is the normal way Python-RNS LXMF delivers anything that does
//! not fit one packet, and the announce invites it. Until this file existed
//! the board ACCEPTED: `delivery_destination_without_inbox` inherited the
//! `accepts_links = true` that `Destination::new` sets as Python parity, so
//! the request passed the gate in
//! `leviculum-core/src/node/link_management.rs`, a link proof went back, and
//! a link-table entry appeared. Nothing then served it —
//! `NodeEvent::LinkDataReceived` has exactly one reader in the whole
//! firmware tree (`leviculum-nrf/src/pn.rs`, gated on the propagation
//! role's own destination) and a link's resource strategy defaults to
//! `AcceptNone`. The peer's messages reached no reader and left no line;
//! the `[TELEMETRY] discarded` line is written from a `PacketReceived` and
//! the link path never touches it.
//!
//! Two things make this worse than the single-packet case. All three nRF
//! binaries register this destination unconditionally, telemetry target
//! configured or not, so it is every nRF board on the mesh. And a link is a
//! session: the peer holds it open and keeps writing into it, so one
//! accepted request swallows a conversation rather than a packet.
//!
//! The claim pinned here: an inbound link request to a board's no-inbox
//! delivery destination is REFUSED — no frame back, no link-table entry.
//! What the peer sees is the same thing it sees from a node with no room
//! left: a request that draws no proof, which every initiator's
//! establishment timeout already handles.
//!
//! The positive control next to it sends the identical request to a node
//! that DOES have an inbox and must still be served — otherwise a zero
//! above would only mean the harness cannot establish a link at all.
//!
//! Run: `cargo test -p leviculum-lxmf --test inbound_link_promise`

use core::cell::Cell;
use std::rc::Rc;

use leviculum_core::transport::{Action, TickOutput};
use leviculum_core::{
    Clock, Destination, DestinationHash, Identity, InterfaceId, MemoryStorage, NodeCore,
    NodeCoreBuilder,
};
use leviculum_lxmf::{LxmfNode, LxmfNodeConfig};
use rand_core::OsRng;

/// The board's only carrier.
const LORA_IFACE: InterfaceId = InterfaceId(0);
const LORA_IFACE_NAME: &str = "lora_sx1262";
const LORA_HW_MTU: u32 = 508;

/// A plausible wall clock: a node near the epoch is one every peer orders
/// last (Codeberg #155).
const START_MS: u64 = 1_780_000_000_000;

/// A board's link cap is a heap term derived from `HEAP_BUDGET`
/// (`MAX_ENDPOINT_LINKS` in the three nRF binaries) and it is never zero —
/// the board's own propagation role and its BLE sessions need entries. Any
/// positive value reproduces the defect, so the harness takes a small one:
/// a cap of zero would refuse the request for the wrong reason and turn
/// this test green against the very code it has to fail.
const BOARD_LINK_CAP: usize = 4;

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
/// room for a handful of links.
fn node(clock: &StepClock) -> TestNode {
    let mut node = NodeCoreBuilder::new()
        .enable_transport(true)
        .max_links(Some(BOARD_LINK_CAP))
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
                ..
            } => (*exclude_iface != Some(LORA_IFACE) && !exclude_ifaces.contains(&LORA_IFACE))
                .then(|| data.clone()),
        })
        .collect()
}

/// A receiver that has registered a delivery destination, with the signing
/// key a peer needs to address a link request at it.
struct Receiver {
    node: TestNode,
    delivery: DestinationHash,
    signing_key: [u8; 32],
}

impl Receiver {
    fn with_destination(clock: &StepClock, destination: Destination, signer: &Identity) -> Self {
        let mut node = node(clock);
        let delivery = *destination.hash();
        node.register_destination(destination);
        Self {
            node,
            delivery,
            signing_key: signer.ed25519_verifying().to_bytes(),
        }
    }

    /// A board: the destination `register_delivery_destination` builds, with
    /// nothing behind it that could serve a link.
    fn board(clock: &StepClock) -> Self {
        let identity = identity_from(1);
        let destination = LxmfNode::delivery_destination_without_inbox(identity_from(1))
            .expect("delivery destination");
        Self::with_destination(clock, destination, &identity)
    }

    /// The positive control: a node with an inbox, registered the way `lntd`
    /// registers, which serves links and must answer.
    fn with_inbox(clock: &StepClock) -> Self {
        let identity = identity_from(2);
        let mut node = node(clock);
        let destination =
            LxmfNode::delivery_destination(identity_from(2)).expect("delivery destination");
        let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
            .expect("register delivery destination");
        let delivery = lxmf.delivery_destination_hash();
        Self {
            node,
            delivery,
            signing_key: identity.ed25519_verifying().to_bytes(),
        }
    }
}

/// One peer that heard the announce and takes it at its word: it opens a
/// link to the announced delivery destination, which is what LXMF does for
/// anything that does not fit a single packet.
fn a_link_request_for(receiver: &Receiver, clock: &StepClock) -> Vec<u8> {
    let mut peer = node(clock);
    let (_link_id, _routed, out) = peer
        .connect(receiver.delivery, &receiver.signing_key)
        .expect("a peer with the announced key can request a link");
    let frames = on_air(&out);
    assert_eq!(
        frames.len(),
        1,
        "connect emits exactly the link request — without it nothing is tested"
    );
    frames.into_iter().next().expect("the link request")
}

/// Deliver one inbound link request to `receiver` and report how many frames
/// it answered with, plus the size of its link table afterwards.
fn answer_to_a_link_request(receiver: &mut Receiver) -> (usize, usize) {
    let clock = StepClock(Rc::new(Cell::new(START_MS)));
    let request = a_link_request_for(receiver, &clock);
    let out = receiver.node.handle_packet(LORA_IFACE, &request);
    (on_air(&out).len(), receiver.node.link_count())
}

/// The claim: a board REFUSES an inbound link request to the delivery
/// destination it announces but cannot serve. No proof goes back and no
/// link-table entry appears, so the peer's establishment times out and it
/// knows where it stands instead of holding open a session nobody reads.
#[test]
fn a_board_refuses_a_link_to_the_destination_it_cannot_serve() {
    let clock = StepClock(Rc::new(Cell::new(START_MS)));
    let mut board = Receiver::board(&clock);
    let (answered, links) = answer_to_a_link_request(&mut board);

    assert_eq!(
        answered, 0,
        "the board answered a link request for a destination it has no \
         reader for: the peer now holds a session whose data reaches nobody \
         and leaves no line ({links} link-table entrie(s) exist)"
    );
    assert_eq!(
        links, 0,
        "a refused request must leave no link-table entry — an entry is a \
         heap slot spent on a session that can never be served"
    );
}

/// Positive control for the assertion above: the identical request to a node
/// that DOES have an inbox is still served. Without this, a board's zero
/// could just as well mean the harness never establishes a link.
#[test]
fn a_node_with_an_inbox_still_serves_the_same_link_request() {
    let clock = StepClock(Rc::new(Cell::new(START_MS)));
    let mut receiver = Receiver::with_inbox(&clock);
    let (answered, links) = answer_to_a_link_request(&mut receiver);

    assert_eq!(
        answered, 1,
        "a receiver that can serve the link answers with exactly one proof"
    );
    assert_eq!(links, 1, "the served request leaves exactly one link entry");
}
