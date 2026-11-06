//! A held proof whose link is gone must not take the others with it
//! (leviculum 1412).
//!
//! A home node holds the proof of every accepted message until the daemon
//! has archived it ([`LxmfRouter::hold_proofs_until_kept`], 1239). The
//! archive lands a pump later, and under load (miauhaus, 2026-11-04, a
//! Columba sending its position every second, one link per message) a
//! sender's link can close in between. Releasing the proof of that message
//! then fails at `send_data_proof` (`LinkError::NotFound`), and
//! `release_proofs` used to drain the whole held queue before it returned
//! that error: every other held proof went with it, the loquax engine
//! logged `HOME engine broken: router release_proofs: Node(ProofFailed)`,
//! and the senders of those messages never heard DELIVERED for messages the
//! archive held.
//!
//! The claims, read at the SENDERS: a proof whose link went away is dropped
//! on its own, the release reports no error, and the proof held for another
//! sender on a link still up reaches it.
//!
//! Run: `cargo test -p leviculum-lxmf --test held_proof_lost_link`

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

use leviculum_core::{
    Action, Clock, DestinationHash, Identity, InterfaceId, LinkId, MemoryStorage, NodeCore,
    NodeCoreBuilder, NodeEvent, TickOutput,
};
use leviculum_lxmf::{
    announce,
    node::LxmfNode,
    router::{LxmfRouter, MessageState, RouterConfig, RouterEvent, RouterOutput},
    DeliveryMethod, LxmfNodeConfig, Message,
};
use rand_core::OsRng;

const NOW_UNIX: f64 = 1_780_000_000.0;

#[derive(Clone)]
struct TestClock(Rc<Cell<u64>>);

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
    fn wall_unix_secs(&self) -> Option<u64> {
        Some(NOW_UNIX as u64 + self.0.get() / 1000)
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

/// One node with a full router: the home, or a sender.
struct Peer {
    node: TestNode,
    router: LxmfRouter,
    destination: DestinationHash,
    identity: Identity,
    events: Vec<RouterEvent>,
}

fn peer(seed: u8, clock: &Rc<Cell<u64>>) -> Peer {
    let mut node = NodeCoreBuilder::new().build(
        OsRng,
        TestClock(Rc::clone(clock)),
        MemoryStorage::with_defaults(),
    );
    let identity = identity_from(seed);
    let private = identity.private_key_bytes().expect("private delivery key");
    let identity_hash = *identity.hash();
    let destination = LxmfNode::delivery_destination(identity).expect("delivery destination");
    let destination_hash = *destination.hash();
    let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
        .expect("register delivery destination");
    Peer {
        node,
        router: LxmfRouter::new(lxmf, identity_hash, RouterConfig::default()),
        destination: destination_hash,
        identity: Identity::from_private_key_bytes(&private).expect("signing identity copy"),
        events: Vec::new(),
    }
}

impl Peer {
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

    fn receive(&mut self, packets: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for packet in packets {
            let core = self.node.handle_packet(InterfaceId(0), packet);
            out.extend(self.absorb_core(core));
        }
        out
    }

    fn announce(&mut self) -> Vec<Vec<u8>> {
        let app_data = announce::delivery(Some(b"peer"), None);
        let out = self
            .node
            .announce_destination(&self.destination, Some(&app_data))
            .expect("announce");
        self.absorb_core(out)
    }

    fn tick(&mut self) -> Vec<Vec<u8>> {
        let output = self.router.tick(&mut self.node).expect("tick");
        self.absorb_router(output)
    }

    fn received(&self) -> Vec<[u8; 32]> {
        self.events
            .iter()
            .filter_map(|event| match event {
                RouterEvent::MessageReceived(message) => Some(message.message_id),
                _ => None,
            })
            .collect()
    }

    fn delivered(&self, id: &[u8; 32]) -> bool {
        self.events.iter().any(|event| {
            matches!(
                event,
                RouterEvent::MessageState { message_id, state: MessageState::Delivered }
                    if message_id == id
            )
        })
    }

    fn inbound_links(&self) -> Vec<LinkId> {
        self.events
            .iter()
            .filter_map(|event| match event {
                RouterEvent::DirectLinkEstablished {
                    link_id,
                    is_initiator: false,
                    ..
                } => Some(*link_id),
                _ => None,
            })
            .collect()
    }
}

/// A shared medium: what one peer sends, every other one hears, until
/// nobody has anything left to say.
fn pump(peers: &mut [&mut Peer], from: usize, packets: Vec<Vec<u8>>) {
    let mut queue = VecDeque::from([(from, packets)]);
    let mut rounds = 0;
    while let Some((from, packets)) = queue.pop_front() {
        rounds += 1;
        assert!(rounds < 4096, "the exchange did not quiesce");
        if packets.is_empty() {
            continue;
        }
        for (index, peer) in peers.iter_mut().enumerate() {
            if index != from {
                let back = peer.receive(&packets);
                queue.push_back((index, back));
            }
        }
    }
}

fn direct(to: DestinationHash, from: &Identity, seed: u8) -> Message {
    Message::create(
        to.into_bytes(),
        [seed; 16],
        from,
        NOW_UNIX,
        b"position".to_vec(),
        vec![seed; 48],
        Vec::new(),
        DeliveryMethod::Direct,
    )
    .expect("direct message")
}

/// Queue `message` at peer `index` and tick it until the home took it.
fn deliver(
    peers: &mut [&mut Peer],
    clock: &Rc<Cell<u64>>,
    index: usize,
    message: Message,
) -> [u8; 32] {
    let id = message.message_id;
    let output = {
        let sender = &mut peers[index];
        sender
            .router
            .enqueue(&sender.node, message)
            .expect("enqueue")
    };
    let packets = peers[index].absorb_router(output);
    pump(peers, index, packets);
    for _ in 0..8 {
        if peers[0].received().contains(&id) {
            return id;
        }
        let packets = peers[index].tick();
        pump(peers, index, packets);
        clock.set(clock.get() + 11_000);
    }
    assert!(
        peers[0].received().contains(&id),
        "the home never took the message"
    );
    id
}

#[test]
fn a_held_proof_whose_link_closed_does_not_drop_the_others() {
    let clock = Rc::new(Cell::new(1_000));
    let mut home = peer(2, &clock);
    home.router
        .hold_proofs_until_kept(&mut home.node)
        .expect("hold proofs");
    let mut first = peer(30, &clock);
    let mut second = peer(40, &clock);
    let mut peers: [&mut Peer; 3] = [&mut home, &mut first, &mut second];
    for index in 0..3 {
        let packets = peers[index].announce();
        pump(&mut peers, index, packets);
    }

    let home_destination = peers[0].destination;
    let a = direct(home_destination, &peers[1].identity.clone(), 0xa1);
    let a = deliver(&mut peers, &clock, 1, a);
    let b = direct(home_destination, &peers[2].identity.clone(), 0xb2);
    let b = deliver(&mut peers, &clock, 2, b);
    assert!(
        !peers[1].delivered(&a) && !peers[2].delivered(&b),
        "both proofs are held until the archive keeps the message"
    );

    // The first sender's link goes away before its message is kept.
    let links = peers[0].inbound_links();
    assert_eq!(links.len(), 2, "one inbound link per sender");
    let closed = peers[0].node.close_link(&links[0]);
    let packets = peers[0].absorb_core(closed);
    pump(&mut peers, 0, packets);

    // The archive keeps A: its proof has no link left to travel.
    let released = peers[0].router.release_proofs(&mut peers[0].node, &a);
    assert!(
        released.is_ok(),
        "a proof whose link closed is not an engine fault: {released:?}"
    );
    if let Ok(output) = released {
        let packets = peers[0].absorb_router(output);
        pump(&mut peers, 0, packets);
    }

    // The archive keeps B: its sender's link is up, so it hears DELIVERED.
    let released = peers[0]
        .router
        .release_proofs(&mut peers[0].node, &b)
        .expect("release B");
    let packets = peers[0].absorb_router(released);
    pump(&mut peers, 0, packets);
    assert!(
        peers[2].delivered(&b),
        "the proof held for the second sender was lost with the first's link"
    );
}
