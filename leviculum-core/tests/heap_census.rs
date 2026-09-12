//! Host-side checks for the #388 heap census: `NodeCore::heap_census`
//! attributes what the firmware's `[HEAP_CENSUS]` line will print.
//!
//! The estimators themselves are unit-tested in `heap_census.rs`; this
//! file exercises the walk over a real node: an empty node reports its
//! own box and nothing imaginary, an established link shows up in
//! `links`/`link_count`, and a cached announce shows up in the
//! `EmbeddedStorage` figure — the three owners the field T114's heap
//! shape turns on (links and the announce/path caches).
//!
//! Run: `cargo test -p leviculum-core --test heap_census`

use std::cell::Cell;
use std::rc::Rc;

use rand_core::OsRng;

use leviculum_core::constants::MTU;
use leviculum_core::embedded_storage::EmbeddedStorage;
use leviculum_core::traits::Clock;
use leviculum_core::{
    Action, Destination, DestinationHash, DestinationType, Direction, Identity, InterfaceId,
    NoStorage, NodeCore, NodeCoreBuilder, ProofStrategy, TickOutput,
};

const START_MS: u64 = 1_000_000;
const IFACE: InterfaceId = InterfaceId(0);

#[derive(Clone)]
struct StepClock(Rc<Cell<u64>>);

impl StepClock {
    fn new(ms: u64) -> Self {
        Self(Rc::new(Cell::new(ms)))
    }
}

impl Clock for StepClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
}

type EndpointNode = NodeCore<OsRng, StepClock, NoStorage>;

fn outbound(out: &TickOutput) -> Vec<Vec<u8>> {
    out.actions
        .iter()
        .map(|a| match a {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data.clone(),
        })
        .collect()
}

/// Lossless point-to-point wire between two nodes, as in `heap_leak.rs`.
fn settle(a: &mut EndpointNode, b: &mut EndpointNode, seed_from_a: TickOutput) {
    let mut to_b = outbound(&seed_from_a);
    let mut to_a: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        if to_a.is_empty() && to_b.is_empty() {
            break;
        }
        for d in std::mem::take(&mut to_b) {
            let out = b.handle_packet(IFACE, &d);
            to_a.extend(outbound(&out));
        }
        for d in std::mem::take(&mut to_a) {
            let out = a.handle_packet(IFACE, &d);
            to_b.extend(outbound(&out));
        }
    }
}

/// A link-accepting responder plus the announce bytes that teach a peer
/// its path (the `heap_leak.rs` fixture, trimmed).
fn make_responder(clock_ms: u64) -> (EndpointNode, DestinationHash, [u8; 32], Vec<u8>) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "heapcensus",
        &["responder"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();

    let announce = dest
        .announce(None, &mut OsRng, clock_ms, clock_ms / 1000)
        .unwrap();
    let mut buf = [0u8; MTU];
    let len = announce.pack(&mut buf).unwrap();
    let announce_bytes = buf[..len].to_vec();

    let mut node = NodeCoreBuilder::new().build(OsRng, StepClock::new(clock_ms), NoStorage);
    node.register_destination(dest);
    (node, dest_hash, signing_key, announce_bytes)
}

#[test]
fn empty_node_census_reports_its_box_and_no_links() {
    let node = NodeCoreBuilder::new().build(OsRng, StepClock::new(START_MS), NoStorage);
    let census = node.heap_census();
    assert!(census.node_struct > 0);
    assert_eq!(census.link_count, 0);
    assert_eq!(census.links, 0, "no links, nothing to pin");
    assert_eq!(census.resources, 0);
    assert!(census.total() >= census.node_struct);
}

#[test]
fn established_link_shows_up_in_the_census() {
    let (mut responder, dest_hash, signing_key, announce_bytes) = make_responder(START_MS);
    let mut initiator = NodeCoreBuilder::new().build(OsRng, StepClock::new(START_MS), NoStorage);

    let before = initiator.heap_census();
    assert_eq!(before.link_count, 0);

    // Teach the initiator the path, then establish.
    let out = initiator.handle_packet(IFACE, &announce_bytes);
    settle(&mut initiator, &mut responder, out);
    let (link_id, _routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");
    settle(&mut initiator, &mut responder, out);
    assert!(
        initiator
            .link(&link_id)
            .map(|l| l.is_active())
            .unwrap_or(false),
        "link did not establish; census fixture is broken"
    );

    let after = initiator.heap_census();
    assert_eq!(after.link_count, 1);
    assert!(
        after.links > 0,
        "an active link pins at least its table node"
    );
    let responder_census = responder.heap_census();
    assert_eq!(responder_census.link_count, 1);
}

#[test]
fn cached_announce_shows_up_in_embedded_storage() {
    let (_responder, _dest_hash, _signing_key, announce_bytes) = make_responder(START_MS);
    let mut node =
        NodeCoreBuilder::new().build(OsRng, StepClock::new(START_MS), EmbeddedStorage::new());
    let before = node.heap_census().storage;
    let _ = node.handle_packet(IFACE, &announce_bytes);
    let after = node.heap_census().storage;
    assert!(
        after > before,
        "accepted announce not visible in storage census (before={before} after={after})"
    );
    assert!(
        after >= announce_bytes.len() - 32,
        "cached announce smaller than the raw packet minus framing: {after}"
    );
}
