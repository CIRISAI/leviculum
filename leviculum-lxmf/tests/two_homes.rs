//! Two homes under one identity, on today's code: the two losses the study
//! `docs/drafts/mailbox-for-friends-study-2026-11.md` proved on paper
//! (sections 1.1 and 1.2), as minimal reproducers.
//!
//! Both tests assert what the N-home design promises and are red on today's
//! code. Each carries `#[ignore]` with the slice that must remove it, the
//! way the tree keeps a red on record without a red suite
//! (`leviculum-lxmf/tests/direct_delivery_attempts.rs`, the comment above
//! its batch A; `leviculum-std/tests/mvr/link_failure_recovery_silent_resume.rs`,
//! the baseline test). Running them is `cargo test -p leviculum-lxmf --test
//! two_homes -- --ignored`.
//!
//! - [`a_home_opens_what_a_sender_encrypted_to_the_other_homes_ratchet`]
//!   (loss a): two homes rotate their ratchets on their own, a sender
//!   keeps only the last one it heard, and the home the packet reaches
//!   cannot open it. The fix is H1 (study 2.2): every home derives the
//!   ratchet of a 30 minute epoch from one shared seed through a hash
//!   chain, wire neutral.
//! - [`a_phone_home_does_not_take_the_path_from_the_always_on_home`]
//!   (loss b): two homes announcing on one segment, and every node's path
//!   follows the newest emission. The fix is H3 (study 2.3): the phone
//!   home announces the identity on its mesh interfaces only when an
//!   always-on home exists.

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

use leviculum_core::{
    Action, Clock, DestinationHash, Identity, InterfaceId, MemoryStorage, NodeCore,
    NodeCoreBuilder, NodeEvent, TickOutput,
};
use leviculum_lxmf::{
    announce,
    node::{LxmfNode, LxmfNodeConfig, LxmfNodeEvent},
    router::{LxmfRouter, RouterConfig},
    DeliveryMethod, Message,
};
use rand_core::OsRng;

/// A clock whose wall time each test sets: the emission an announce carries
/// is the announcing host's wall clock in whole seconds
/// (`RNS/Destination.py:282`), and the two homes are two hosts.
#[derive(Clone)]
struct HostClock {
    ms: Rc<Cell<u64>>,
    wall: Rc<Cell<u64>>,
}

impl HostClock {
    fn new(wall: u64) -> Self {
        Self {
            ms: Rc::new(Cell::new(1_000)),
            wall: Rc::new(Cell::new(wall)),
        }
    }

    fn advance_secs(&self, secs: u64) {
        self.ms.set(self.ms.get() + secs * 1_000);
        self.wall.set(self.wall.get() + secs);
    }
}

impl Clock for HostClock {
    fn now_ms(&self) -> u64 {
        self.ms.get()
    }
    fn wall_unix_secs(&self) -> Option<u64> {
        Some(self.wall.get())
    }
}

const WALL: u64 = 1_700_000_000;

type Node = NodeCore<OsRng, HostClock, MemoryStorage>;

fn identity_from(seed: u8) -> Identity {
    let mut private = [0u8; 64];
    for (index, byte) in private.iter_mut().enumerate() {
        *byte = seed.wrapping_add(index as u8);
    }
    Identity::from_private_key_bytes(&private).expect("deterministic identity")
}

fn packets(actions: Vec<Action>) -> Vec<Vec<u8>> {
    actions
        .into_iter()
        .map(|action| match action {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data,
        })
        .collect()
}

/// One home of the identity `seed`: its own node, its own clock, and its
/// own ratchets, made at random when it starts, as every home makes them
/// today (`RNS/Destination.py:227-234`; ours `Destination::enable_ratchets`).
struct Home {
    node: Node,
    clock: HostClock,
    lxmf: LxmfNode,
    destination: DestinationHash,
    received: Vec<Message>,
}

fn home(seed: u8, wall: u64) -> Home {
    let clock = HostClock::new(wall);
    let mut node =
        NodeCoreBuilder::new().build(OsRng, clock.clone(), MemoryStorage::with_defaults());
    let mut destination =
        LxmfNode::delivery_destination(identity_from(seed)).expect("delivery destination");
    destination
        .enable_ratchets(&mut OsRng, clock.now_ms())
        .expect("ratchets on an inbound destination");
    let hash = *destination.hash();
    let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
        .expect("register delivery destination");
    Home {
        node,
        clock,
        lxmf,
        destination: hash,
        received: Vec::new(),
    }
}

impl Home {
    fn absorb(&mut self, output: TickOutput) -> Vec<Vec<u8>> {
        let mut actions = output.actions;
        let mut events: VecDeque<NodeEvent> = output.events.into();
        while let Some(event) = events.pop_front() {
            let follow = self
                .lxmf
                .handle_event(&mut self.node, &event)
                .expect("the home handles its node's event");
            for event in &follow.events {
                if let LxmfNodeEvent::MessageReceived(message) = event {
                    self.received.push(message.clone());
                }
            }
            actions.extend(follow.core.actions);
            events.extend(follow.core.events);
        }
        packets(actions)
    }

    /// The home's announce as the mailbox makes it today: on every
    /// interface (`loquax/src/home/daemon.rs`, `Command::Announce` on the
    /// engine, which calls `NodeCore::announce_destination`).
    fn announce(&mut self) -> Vec<Vec<u8>> {
        let app_data = announce::delivery(Some(b"home"), None);
        let output = self
            .node
            .announce_destination(&self.destination, Some(&app_data))
            .expect("announce the home");
        self.absorb(output)
    }

    fn receive(&mut self, packets: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for packet in packets {
            let output = self.node.handle_packet(InterfaceId(0), packet);
            out.extend(self.absorb(output));
        }
        out
    }

    fn ratchet(&self) -> [u8; 32] {
        self.node
            .destination_ratchet_public(&self.destination)
            .expect("ratchets are on")
    }
}

/// A sender, a stock LXMF router on a node of its own.
struct Sender {
    node: Node,
    router: LxmfRouter,
    identity: Identity,
    destination: DestinationHash,
}

fn sender(seed: u8) -> Sender {
    let clock = HostClock::new(WALL);
    let mut node = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());
    let identity = identity_from(seed);
    let private = identity.private_key_bytes().expect("private key");
    let identity_hash = *identity.hash();
    let destination = LxmfNode::delivery_destination(identity).expect("delivery destination");
    let hash = *destination.hash();
    let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
        .expect("register delivery destination");
    Sender {
        node,
        router: LxmfRouter::new(lxmf, identity_hash, RouterConfig::default()),
        identity: Identity::from_private_key_bytes(&private).expect("identity copy"),
        destination: hash,
    }
}

impl Sender {
    fn absorb(&mut self, output: TickOutput) -> Vec<Vec<u8>> {
        let mut actions = output.actions;
        let mut events: VecDeque<NodeEvent> = output.events.into();
        while let Some(event) = events.pop_front() {
            let follow = self
                .router
                .handle_event(&mut self.node, &event)
                .expect("the router handles its node's event");
            actions.extend(follow.core.actions);
            events.extend(follow.core.events);
        }
        packets(actions)
    }

    fn receive(&mut self, packets: &[Vec<u8>]) {
        for packet in packets {
            let output = self.node.handle_packet(InterfaceId(0), packet);
            self.absorb(output);
        }
    }

    fn announce(&mut self) -> Vec<Vec<u8>> {
        let app_data = announce::delivery(Some(b"sender"), None);
        let output = self
            .node
            .announce_destination(&self.destination, Some(&app_data))
            .expect("announce the sender");
        self.absorb(output)
    }

    /// One opportunistic message to `to`, as a stock sender makes it: a
    /// single packet encrypted to the ratchet it heard last
    /// (`RNS/Destination.py:596-599`; ours `NodeCore::known_remote_ratchet`).
    fn opportunistic(&mut self, to: DestinationHash) -> Vec<Vec<u8>> {
        let message = Message::create(
            to.into_bytes(),
            self.destination.into_bytes(),
            &self.identity,
            WALL as f64 + 5.0,
            b"two homes".to_vec(),
            b"for whichever home the network picks".to_vec(),
            Vec::new(),
            DeliveryMethod::Opportunistic,
        )
        .expect("opportunistic message");
        let output = self
            .router
            .enqueue(&self.node, message)
            .expect("enqueue the message");
        let mut out = self.absorb(output.core);
        for _ in 0..4 {
            let output = self.router.tick(&mut self.node).expect("router tick");
            out.extend(self.absorb(output.core));
            if !out.is_empty() {
                break;
            }
        }
        out
    }
}

/// Loss (a), study 1.2. Homes A and B hold one identity and made their
/// ratchets on their own. The sender hears A, then B: the ratchet it keeps
/// is B's. The network routes its packet to A (A announced later at some
/// relay, or the sender's ratchet is older than the relays' path), and A
/// tries its own ring and then the identity key
/// (`leviculum-core/src/identity.rs:563-581`, `decrypt_with_ratchets`, the
/// equivalent of `RNS/Identity.py:890` and `:910-912`): neither opens a
/// packet encrypted to B's ratchet, and the message is lost with no proof.
///
/// The control: the very same packet delivered to B opens, so the packet is
/// sound and the loss is A's ring alone.
#[test]
#[ignore = "red until H1 (study 2.2): ratchets derived per 30 min epoch from a shared seed"]
fn a_home_opens_what_a_sender_encrypted_to_the_other_homes_ratchet() {
    const IDENTITY: u8 = 0x40;
    let mut a = home(IDENTITY, WALL);
    let mut b = home(IDENTITY, WALL + 10);
    assert_eq!(a.destination, b.destination, "one identity, one address");
    assert_ne!(
        a.ratchet(),
        b.ratchet(),
        "two homes made their ratchets on their own"
    );
    let mut s = sender(0x70);
    // Both homes know the sender's key, so a message that opens verifies.
    let from_sender = s.announce();
    a.receive(&from_sender);
    b.receive(&from_sender);
    s.receive(&a.announce());
    s.receive(&b.announce());
    assert_eq!(
        s.node.known_remote_ratchet(&b.destination),
        Some(b.ratchet()),
        "the sender keeps the ratchet it heard last, B's"
    );

    let packet = s.opportunistic(a.destination);
    assert!(!packet.is_empty(), "the sender put the message on the air");

    // Control: B, whose ratchet the sender used, opens it.
    b.receive(&packet);
    assert_eq!(b.received.len(), 1, "home B opens the packet: it is sound");

    // The loss: the same packet routed to home A.
    a.clock.advance_secs(1);
    a.receive(&packet);
    assert_eq!(
        a.received.len(),
        1,
        "home A could not open a message encrypted to home B's ratchet: lost \
         (identity.rs decrypt_with_ratchets found no key in A's ring, and the \
         identity key cannot open a ratchet packet). H1 derives one ring for \
         every home"
    );
}

/// Loss (b), study 1.1. An observer on the segment both homes reach (the
/// home LAN or the internet side, where the always-on home lives) hears the
/// always-on home A on one interface and the phone home B on another. The
/// path table follows the newest emission at any hop count
/// (`leviculum-core/src/transport.rs:7690-7692` for equal or fewer hops,
/// `:7379-7382` for more; `RNS/Transport.py:1765`, `:1809-1810`), so every
/// announce of the other home moves it: A, B, A gives three different
/// interfaces in a row, a flap at the announce cadence.
///
/// The design's rule is that a phone home with an always-on home announces
/// the identity on its mesh interfaces only, through
/// `NodeCore::announce_destination_on_interface`
/// (`leviculum-core/src/node/mod.rs:1024`); then this segment never hears
/// B and the path stays on A. Today the home announces on every interface.
#[test]
#[ignore = "red until H3 (study 2.3): mesh-only announcing for a phone home beside an always-on home"]
fn a_phone_home_does_not_take_the_path_from_the_always_on_home() {
    const IDENTITY: u8 = 0x41;
    const FROM_A: usize = 0;
    const FROM_B: usize = 1;
    let mut a = home(IDENTITY, WALL);
    let mut b = home(IDENTITY, WALL + 20);
    let clock = HostClock::new(WALL);
    let mut observer: Node =
        NodeCoreBuilder::new().build(OsRng, clock.clone(), MemoryStorage::with_defaults());
    let address = a.destination.into_bytes();
    let path_interface = |observer: &Node| {
        observer
            .path_table_entries()
            .into_iter()
            .find(|entry| entry.hash == address)
            .map(|entry| entry.interface_index)
    };
    let hear = |observer: &mut Node, interface: usize, packets: &[Vec<u8>]| {
        for packet in packets {
            let _ = observer.handle_packet(InterfaceId(interface), packet);
        }
    };

    let mut seen = Vec::new();
    hear(&mut observer, FROM_A, &a.announce());
    seen.push(path_interface(&observer));
    clock.advance_secs(20);
    hear(&mut observer, FROM_B, &b.announce());
    seen.push(path_interface(&observer));
    clock.advance_secs(20);
    a.clock.advance_secs(40);
    hear(&mut observer, FROM_A, &a.announce());
    seen.push(path_interface(&observer));

    assert_eq!(
        seen[0],
        Some(FROM_A),
        "the observer took the always-on home's path"
    );
    assert_eq!(
        seen,
        vec![Some(FROM_A); 3],
        "the path flapped between the homes ({seen:?}, interface {FROM_A} is the \
         always-on home, {FROM_B} the phone): the newest emission wins at any hop \
         count (transport.rs newer_emission). H3 keeps the phone's announce of \
         the identity off this segment"
    );
}
