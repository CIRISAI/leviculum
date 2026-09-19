//! Codeberg #186: an LXMF router on a clockless node keeps its caches across
//! the timebase jump that node is guaranteed to make.
//!
//! A board without an RTC emits uptime seconds from `Transport::emission_secs`
//! until the first validated announce seats a real timebase, and then steps by
//! roughly 1.7e9 seconds at once (`docs/src/concepts/time-and-clocks.md`, the
//! source-priority chain). The router ages its stamp-cost cache and its
//! delivered/processed ID windows on that value, so before #186 a single step
//! aged every entry out.
//!
//! The unit tests in `router.rs` pin the mechanism against supplied readings.
//! This file pins the deployment: a real `NodeCore`, a real announce decoded
//! off a real packet, a real jump, and a real `tick` — the path an LNode
//! actually walks. The consequence being prevented is not cosmetic: a queued
//! message whose recipient cost has just been forgotten is sent unstamped, and
//! a peer that demands a stamp drops it.
//!
//! No radio is involved and no medium is under test; the packet is handed to
//! `NodeCore::handle_packet` directly.

use core::cell::Cell;
use std::rc::Rc;

use leviculum_core::transport::TimeSource;
use leviculum_core::{
    Clock, Identity, InterfaceId, MemoryStorage, NodeCore, NodeCoreBuilder, NodeEvent,
};
use leviculum_lxmf::announce;
use leviculum_lxmf::router::{LxmfRouter, RouterConfig};
use leviculum_lxmf::{LxmfNode, LxmfNodeConfig};
use rand_core::OsRng;

/// Plausible, and reachable by no uptime this test runs long enough to produce.
const INJECTED_UNIX: u64 = leviculum_core::constants::BUILD_UNIX_SECS + 1_234_567;

const BOOT_MS: u64 = 1_000;

/// The cost the peer announces. Inside the window the reference is willing to
/// announce (Codeberg #181), so the read side does not filter it for a reason
/// other than the one under test.
const ANNOUNCED_STAMP_COST: u8 = 12;

/// The LNode shape: a monotonic timer and no calendar. The cell is shared so
/// the test can advance uptime without reaching into the node.
struct ClocklessClock(Rc<Cell<u64>>);

impl Clock for ClocklessClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
}

type TestNode = NodeCore<OsRng, ClocklessClock, MemoryStorage>;

fn identity_from(seed: u8) -> Identity {
    let mut private = [0u8; 64];
    for (index, byte) in private.iter_mut().enumerate() {
        *byte = seed.wrapping_add(index as u8);
    }
    Identity::from_private_key_bytes(&private).expect("deterministic identity")
}

fn clockless_router() -> (LxmfRouter, TestNode, Rc<Cell<u64>>) {
    let uptime_ms = Rc::new(Cell::new(BOOT_MS));
    let mut core = NodeCoreBuilder::new().build(
        OsRng,
        ClocklessClock(Rc::clone(&uptime_ms)),
        MemoryStorage::with_defaults(),
    );
    let identity = identity_from(1);
    let identity_hash = *identity.hash();
    let destination = LxmfNode::delivery_destination(identity).expect("delivery destination");
    let node = LxmfNode::register(&mut core, destination, LxmfNodeConfig::default())
        .expect("register delivery destination");
    (
        LxmfRouter::new(node, identity_hash, RouterConfig::default()),
        core,
        uptime_ms,
    )
}

/// Announce a delivery destination carrying a stamp cost, from a peer that is
/// itself clockless: its emission timestamp is uptime seconds, so hearing it
/// does NOT seat a real timebase here. That is what lets the cache be
/// populated before the jump — a mesh of boards with one PC in it.
fn announce_clockless_peer(
    router: &mut LxmfRouter,
    node: &mut TestNode,
    seed: u8,
) -> leviculum_core::DestinationHash {
    let identity = identity_from(seed);
    let mut destination =
        LxmfNode::delivery_destination(identity).expect("remote delivery destination");
    let destination_hash = *destination.hash();
    let app_data = announce::delivery(Some(b"remote"), Some(ANNOUNCED_STAMP_COST));
    let packet = destination
        .announce(
            Some(&app_data),
            &mut OsRng,
            node.now_ms(),
            node.now_ms() / 1000,
        )
        .expect("remote delivery announce");
    let mut packed = vec![0; packet.packed_size()];
    let length = packet.pack(&mut packed).expect("pack remote announce");
    let event = node
        .handle_packet(InterfaceId(0), &packed[..length])
        .events
        .into_iter()
        .find(|event| matches!(event, NodeEvent::AnnounceReceived { .. }))
        .expect("remote announce event");
    let _ = router
        .handle_event(node, &event)
        .expect("remember remote announce");
    destination_hash
}

#[test]
fn a_learned_timebase_does_not_forget_the_stamp_cost_a_peer_announced() {
    let (mut router, mut node, uptime_ms) = clockless_router();
    let peer = announce_clockless_peer(&mut router, &mut node, 9);

    // Still clockless: the peer's announce carried uptime seconds too, so the
    // cost was cached against a timebase that has not become real yet.
    assert!(!node.has_plausible_wall_clock());
    assert_eq!(
        router.outbound_stamp_cost(&node, peer.as_bytes()),
        Some(ANNOUNCED_STAMP_COST),
    );

    // One tick before the jump, so the router has an anchor to measure against
    // — the shape of a node that has been up and running for a while.
    uptime_ms.set(BOOT_MS + 4_000);
    let _ = router.tick(&mut node).expect("tick before the jump");

    // The timebase becomes real. A second of monotonic time passes, which is
    // all the time that has actually passed.
    uptime_ms.set(BOOT_MS + 5_000);
    assert!(node.set_wall_time_unix_secs(INJECTED_UNIX, TimeSource::Host));
    assert!(node.has_plausible_wall_clock());
    assert!(node.emission_secs() >= INJECTED_UNIX);

    let _ = router.tick(&mut node).expect("tick across the jump");

    assert_eq!(
        router.outbound_stamp_cost(&node, peer.as_bytes()),
        Some(ANNOUNCED_STAMP_COST),
        "a timebase jump is not 45 days of elapsed time",
    );
}
