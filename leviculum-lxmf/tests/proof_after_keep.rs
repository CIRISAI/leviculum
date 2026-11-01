//! A home node proves a message only once it is kept (leviculum 1239).
//!
//! `loquax --daemon` archives every received message, and the archive write
//! happens a pump of the daemon after the router handed the message out. With
//! the delivery destination on `ProofStrategy::All` the core proved the packet
//! on arrival (`leviculum-core/src/node/mod.rs`, the `ProofRequested` arm),
//! before the router had even seen it, so a sender could read DELIVERED while
//! the archive did not hold the message yet; a crash or a full disk in that
//! window lost a message its sender will not send again. The land gate on
//! f4daff08 caught the window (`loquax/tests/home_node.rs`, "proven but not
//! archived").
//!
//! The claims, read at the SENDER: a router that holds proofs until kept
//! confirms nothing on arrival, confirms once on
//! [`LxmfRouter::release_proofs`], and proves a duplicate of a message it
//! already took at once, since nothing new is there to keep.
//!
//! Run: `cargo test -p leviculum-lxmf --test proof_after_keep`

use core::cell::Cell;
use std::rc::Rc;

use leviculum_core::transport::{Action, TickOutput};
use leviculum_core::{
    Clock, DestinationHash, Identity, InterfaceId, MemoryStorage, NodeCore, NodeCoreBuilder,
    NodeEvent,
};
use leviculum_lxmf::router::{LxmfRouter, RouterConfig, RouterEvent};
use leviculum_lxmf::{DeliveryMethod, LxmfNode, LxmfNodeConfig, Message};
use rand_core::OsRng;

const IFACE: InterfaceId = InterfaceId(0);
const START_MS: u64 = 1_780_000_000_000;

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

fn node(clock: &StepClock) -> TestNode {
    let mut node = NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        clock.clone(),
        MemoryStorage::with_defaults(),
    );
    node.set_interface_name(IFACE.0, "test".to_string());
    node.set_interface_hw_mtu(IFACE.0, 508);
    node
}

fn on_air(out: &TickOutput) -> Vec<Vec<u8>> {
    out.actions
        .iter()
        .map(|a| match a {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data.clone(),
        })
        .collect()
}

/// A home node's receiving side: the router over its delivery destination,
/// holding proofs until kept.
struct Home {
    node: TestNode,
    router: LxmfRouter,
    delivery: DestinationHash,
}

impl Home {
    fn new(clock: &StepClock) -> Self {
        let mut node = node(clock);
        let identity = identity_from(2);
        let identity_hash = *identity.hash();
        let destination = LxmfNode::delivery_destination(identity).expect("delivery destination");
        let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
            .expect("register delivery destination");
        let delivery = lxmf.delivery_destination_hash();
        let mut router = LxmfRouter::new(lxmf, identity_hash, RouterConfig::default());
        router
            .hold_proofs_until_kept(&mut node)
            .expect("hold proofs");
        Self {
            node,
            router,
            delivery,
        }
    }

    /// One frame in, every core event through the router: what went back on
    /// the air, and the IDs of the messages the router handed out.
    fn receive(&mut self, frame: &[u8]) -> (Vec<Vec<u8>>, Vec<[u8; 32]>) {
        let out = self.node.handle_packet(IFACE, frame);
        let mut back = on_air(&out);
        let mut received = Vec::new();
        for event in &out.events {
            let routed = self
                .router
                .handle_event(&mut self.node, event)
                .expect("router handles the event");
            back.extend(on_air(&routed.core));
            received.extend(routed.events.iter().filter_map(|e| match e {
                RouterEvent::MessageReceived(message) => Some(message.message_id),
                _ => None,
            }));
        }
        (back, received)
    }
}

/// The sender, holding the home node's key and a path from its announce.
fn sender(clock: &StepClock, home: &mut Home) -> TestNode {
    let mut peer = node(clock);
    let out = home
        .node
        .announce_destination(&home.delivery, Some(b"home"))
        .expect("announce");
    for frame in on_air(&out) {
        let _ = peer.handle_packet(IFACE, &frame);
    }
    assert!(peer.has_path(&home.delivery), "the announce taught a path");
    peer
}

fn confirmations(peer: &mut TestNode, frames: &[Vec<u8>]) -> usize {
    frames
        .iter()
        .map(|frame| {
            peer.handle_packet(IFACE, frame)
                .events
                .iter()
                .filter(|e| matches!(e, NodeEvent::PacketDeliveryConfirmed { .. }))
                .count()
        })
        .sum()
}

#[test]
fn a_home_node_proves_a_message_only_once_it_is_kept() {
    let clock = StepClock(Rc::new(Cell::new(START_MS)));
    let mut home = Home::new(&clock);
    let mut peer = sender(&clock, &mut home);
    let message = Message::create(
        home.delivery.into_bytes(),
        [0x5a; 16],
        &identity_from(7),
        1_780_000_000.0,
        Vec::new(),
        b"Klaro".to_vec(),
        Vec::new(),
        DeliveryMethod::Opportunistic,
    )
    .expect("a message");
    let on_air_form = message.on_air().expect("an opportunistic form");

    let (_, out) = peer
        .send_single_packet(&home.delivery, &on_air_form)
        .expect("send");
    let mut back = Vec::new();
    let mut received = Vec::new();
    for frame in on_air(&out) {
        let (b, r) = home.receive(&frame);
        back.extend(b);
        received.extend(r);
    }
    assert_eq!(
        received,
        [message.message_id],
        "the router took the message"
    );
    assert_eq!(
        confirmations(&mut peer, &back),
        0,
        "proven on arrival, before anything kept the message"
    );

    let released = home
        .router
        .release_proofs(&mut home.node, &message.message_id)
        .expect("release");
    assert_eq!(
        confirmations(&mut peer, &on_air(&released.core)),
        1,
        "kept, so proven, once"
    );
    let again = home
        .router
        .release_proofs(&mut home.node, &message.message_id)
        .expect("release again");
    assert!(on_air(&again.core).is_empty(), "a proof is released once");

    // The same message in a new packet, as a sender whose proof was lost
    // sends it: the router refuses the duplicate and proves it at once.
    let (_, out) = peer
        .send_single_packet(&home.delivery, &on_air_form)
        .expect("send again");
    let mut back = Vec::new();
    let mut received = Vec::new();
    for frame in on_air(&out) {
        let (b, r) = home.receive(&frame);
        back.extend(b);
        received.extend(r);
    }
    assert!(received.is_empty(), "a duplicate is not handed out again");
    assert_eq!(
        confirmations(&mut peer, &back),
        1,
        "a duplicate of a kept message is proven at once"
    );
}
