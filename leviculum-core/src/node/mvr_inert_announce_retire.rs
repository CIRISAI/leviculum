//! Item 1 of the 2026-09-21 hygiene batch: an announce entry that will
//! never retransmit is never retired.
//!
//! ## The leak
//!
//! `handle_announce` inserts an `AnnounceEntry` for every accepted
//! announce, and on a node with `enable_transport = false` the entry is
//! inserted with `retransmit_at_ms: None` — there is nothing to
//! rebroadcast. The two removal conditions in
//! `check_announce_rebroadcasts` are `retries > PATHFINDER_RETRIES` and
//! `local_rebroadcasts >= LOCAL_REBROADCASTS_MAX`, and both counters can
//! only advance when the entry fires. An entry that never fires
//! therefore lives until the same destination announces again and
//! overwrites it. Each one carries `raw_packet: Vec<u8>`, a second full
//! copy of the announce beside `announce_cache`.
//!
//! ## Why the entry exists at all
//!
//! It is not dead on arrival: `handle_announce` reads `timestamp_ms`
//! off it to apply the per-destination announce rate window
//! (`announce_rate_limit_ms`, 2 s), and counts neighbour echoes into
//! `local_rebroadcasts` inside the same window. Dropping the insertion
//! outright — which is what the reference does, inserting only when
//! `transport_enabled() or is_from_local_client` (Transport.py:1886) —
//! would take the anti-flood window with it on a non-transport node.
//! So the entry is kept for exactly as long as it is read, and retired
//! when the window closes.
//!
//! Sans-I/O: 1 node, 1 mock interface, deterministic, sub-second.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, Storage};
use crate::transport::InterfaceId;

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

fn add_iface(node: &mut Node, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    idx
}

/// A plain leaf node: it learns paths, it does not forward announces.
fn make_leaf_node() -> Node {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().enable_transport(false).build(
        OsRng,
        clock,
        MemoryStorage::with_defaults(),
    )
}

/// One foreign announce, for a destination this node has never seen.
fn make_announce(app_suffix: &str) -> ([u8; TRUNCATED_HASHBYTES], Vec<u8>) {
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &[app_suffix],
    )
    .unwrap();
    let dest_hash = *dest.hash().as_bytes();
    let ann = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .unwrap();
    let mut buf = [0u8; MTU];
    let len = ann.pack(&mut buf).unwrap();
    (dest_hash, buf[..len].to_vec())
}

/// THE pin: the inert entry survives its rate window and not one poll
/// longer.
#[test]
fn an_announce_that_never_retransmits_is_retired_when_its_rate_window_closes() {
    let mut node = make_leaf_node();
    let iface = add_iface(&mut node, "A_in");
    let (dest, raw) = make_announce("inert");

    let t0 = node.transport().clock().now_ms();
    let _ = node.handle_packet(InterfaceId(iface), &raw);

    let entry = node
        .transport()
        .storage()
        .get_announce(&dest)
        .expect("the announce is recorded");
    assert!(
        entry.retransmit_at_ms.is_none(),
        "a leaf node schedules no rebroadcast: this is the inert entry"
    );
    assert_eq!(
        entry.retries, 0,
        "nothing fired, so the retry counter that would retire it stands still"
    );

    // Inside the rate window the entry is still load-bearing: it is what
    // a second announce from this destination is measured against.
    let window = node.transport().config().announce_rate_limit_ms;
    node.transport().clock().set(t0 + window - 1);
    let _ = node.handle_timeout();
    assert!(
        node.transport().storage().get_announce(&dest).is_some(),
        "the rate window has not closed, the entry still answers for it"
    );

    // Once it has closed, nothing reads the entry again.
    node.transport().clock().set(t0 + window);
    let _ = node.handle_timeout();
    assert!(
        node.transport().storage().get_announce(&dest).is_none(),
        "an entry nothing can read and nothing can retire must not outlive its window"
    );
}

/// The shape that makes it a leak rather than a stale entry: many
/// destinations, one table that only grows.
#[test]
fn a_leaf_node_hearing_many_destinations_does_not_accumulate_announce_entries() {
    let mut node = make_leaf_node();
    let iface = add_iface(&mut node, "A_in");

    let suffixes = ["d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7"];
    for suffix in suffixes {
        let (_dest, raw) = make_announce(suffix);
        let _ = node.handle_packet(InterfaceId(iface), &raw);
    }
    assert_eq!(
        node.transport().storage().announce_keys().len(),
        suffixes.len(),
        "every announce is recorded on arrival"
    );

    let t = node.transport().clock().now_ms();
    node.transport()
        .clock()
        .set(t + node.transport().config().announce_rate_limit_ms);
    let _ = node.handle_timeout();

    assert_eq!(
        node.transport().storage().announce_keys().len(),
        0,
        "the table shrinks back to empty; a leaf node's announce table is not a log"
    );
}
