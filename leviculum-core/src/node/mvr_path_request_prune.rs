//! Item 2 of the 2026-09-21 hygiene batch: `path_requests` is never
//! pruned.
//!
//! ## The leak
//!
//! `set_path_request_time` records one `(dest_hash, ms)` pair per
//! destination this node has ever asked for a path to — written by
//! `request_path` and by the peer-link re-origination. Nothing in the
//! tree ever removes one: `clean_stale_path_metadata` walks past it,
//! and the only `clear()` is a test helper. Small per entry, unbounded
//! in count, and a node that keeps failing to reach a destination keeps
//! asking, so the busy case is the growing case.
//!
//! ## The eviction key is not a choice
//!
//! Every read of the map is the same comparison — `now - last <
//! PATH_REQUEST_MIN_INTERVAL_MS` — in `request_path`,
//! `reoriginate_toward_peer_links` and `clean_link_table`. An entry
//! older than that interval answers "not throttled" exactly as a
//! missing entry does, so dropping it is not a policy decision, it is
//! removing a value that is already unreachable.
//!
//! Sans-I/O: 1 node, no interfaces, deterministic, sub-second.

extern crate std;

use rand_core::OsRng;

use crate::constants::{PATH_REQUEST_MIN_INTERVAL_MS, TRUNCATED_HASHBYTES};
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::test_utils::{MockClock, TEST_TIME_MS};
use crate::traits::{Clock, Storage};

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

fn make_node() -> Node {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults())
}

fn hash_for(n: u8) -> [u8; TRUNCATED_HASHBYTES] {
    let mut h = [0u8; TRUNCATED_HASHBYTES];
    h[0] = n;
    h[15] = 0xA5;
    h
}

fn path_request_count(node: &Node) -> usize {
    node.transport()
        .storage()
        .collection_counts()
        .into_iter()
        .find(|c| c.name == "path_requests")
        .expect("the census names path_requests")
        .entries
}

/// THE pin: the throttle record outlives the throttle by nothing.
#[test]
fn a_path_request_throttle_record_dies_with_its_throttle() {
    let mut node = make_node();
    let dest = hash_for(1);
    let t0 = node.transport().clock().now_ms();

    node.transport
        .request_path(&dest, None, &hash_for(0xF0))
        .expect("asking for a path is best-effort, not an error");
    assert_eq!(
        path_request_count(&node),
        1,
        "the request is recorded so a second one inside the interval is refused"
    );

    // One millisecond before the interval closes the record still
    // decides something: a second request here must be swallowed.
    node.transport()
        .clock()
        .set(t0 + PATH_REQUEST_MIN_INTERVAL_MS - 1);
    let _ = node.handle_timeout();
    assert_eq!(
        path_request_count(&node),
        1,
        "inside the interval the record is the throttle and must stay"
    );

    node.transport()
        .clock()
        .set(t0 + PATH_REQUEST_MIN_INTERVAL_MS);
    let _ = node.handle_timeout();
    assert_eq!(
        path_request_count(&node),
        0,
        "past the interval the record answers exactly as its absence would"
    );
}

/// The shape that makes it a leak: a node that keeps asking about
/// destinations it cannot reach.
#[test]
fn asking_about_many_unreachable_destinations_does_not_grow_the_map() {
    let mut node = make_node();
    let t0 = node.transport().clock().now_ms();

    for n in 0..32u8 {
        node.transport
            .request_path(&hash_for(n), None, &hash_for(0x80 | n))
            .expect("asking for a path is best-effort, not an error");
    }
    assert_eq!(path_request_count(&node), 32, "one record per destination");

    node.transport()
        .clock()
        .set(t0 + PATH_REQUEST_MIN_INTERVAL_MS);
    let _ = node.handle_timeout();

    assert_eq!(
        path_request_count(&node),
        0,
        "the map is a throttle, not a list of everything ever asked about"
    );
}
