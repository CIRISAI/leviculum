//! Order 1648 (LV-1000): `PN_CENSUS store_index=` against the bytes the
//! store index really holds.
//!
//! The miauhaus census of 2026-11-10 printed `store_index=105716960` for a
//! process whose whole resident set was 86 MB: the estimate was not what
//! was allocated. This binary installs a counting allocator, builds a
//! store of a known number of messages both ways the field builds one
//! (appended one by one, and reopened from disk), and holds the census
//! to the bytes the store gives back when it is dropped.
//!
//! The counter is leviculum-std's shim, the one `heap-accounting` builds
//! install. One test in the binary on purpose: the counter is process-wide, and a
//! second test running beside it would move it.

use leviculum_lxmf::PropagationStore as _;
use leviculum_std::heap_accounting::{live_bytes, CountingAllocator};
use leviculum_std::FilePropagationStore;

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator;

/// Messages in the built store: enough B-tree nodes that the per-node
/// arithmetic dominates every fixed cost.
const MESSAGES: u32 = 20_000;

fn transient_id(n: u32) -> [u8; 32] {
    // Spread like a SHA-256: the index is keyed by it, and its insertion
    // order decides how full the B-tree's nodes are.
    let mut id = [0u8; 32];
    let mut state = u64::from(n).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for chunk in id.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        chunk.copy_from_slice(&state.to_le_bytes());
    }
    id
}

fn body(n: u32) -> Vec<u8> {
    let mut body = vec![0xCD; 64];
    body[..4].copy_from_slice(&n.to_le_bytes());
    body
}

/// Bytes the store gives back on drop, and its census taken just before.
fn measure(store: FilePropagationStore) -> (usize, leviculum_std::StoreHeapCensus) {
    let census = store.heap_census();
    let before = live_bytes();
    drop(store);
    (before.saturating_sub(live_bytes()), census)
}

fn open(dir: &std::path::Path) -> FilePropagationStore {
    FilePropagationStore::open(dir, u64::MAX / 2).expect("open")
}

fn append(store: &mut FilePropagationStore, range: std::ops::Range<u32>) {
    for n in range {
        store
            .append(&transient_id(n), 1_789_000_000 + u64::from(n), 13, &body(n))
            .expect("append");
    }
}

/// The census within 5 % of the measured bytes, every way a store is built:
/// appended one by one, reopened (its sequence and receive-time sets
/// collected full from a sorted run), and reopened, thinned and appended
/// to. Before 1648 the first two read 0.95x and 1.21x.
#[test]
fn the_store_index_census_is_what_the_index_allocates() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut store = open(dir.path());
    append(&mut store, 0..MESSAGES);
    store.save_index();
    let appended = store;
    let reopened = open(dir.path());
    let mixed_dir = tempfile::tempdir().expect("temp dir");
    let mut mixed = open(mixed_dir.path());
    append(&mut mixed, 0..MESSAGES);
    drop(mixed);
    let mut mixed = open(mixed_dir.path());
    // The oldest third leaves as expiry takes it, every fifth of the rest
    // as clients collect, and a half again arrives.
    for n in (0..MESSAGES / 3).chain((MESSAGES / 3..MESSAGES).step_by(5)) {
        assert!(mixed.purge(&transient_id(n)).expect("purge"));
    }
    append(&mut mixed, MESSAGES..MESSAGES + MESSAGES / 2);
    let mut failures = Vec::new();
    for (how, store) in [
        ("appended", appended),
        ("reopened", reopened),
        ("mixed", mixed),
    ] {
        let messages = store.len();
        let (freed, census) = measure(store);
        assert_eq!(census.messages, messages, "{how}");
        assert_eq!(
            census.index_entry_bytes + census.index_spare_bytes,
            census.index_bytes,
            "{how}: the two named parts make the whole"
        );
        let ratio = census.index_bytes as f64 / freed as f64;
        let line = format!(
            "{how}: messages={messages} census index_bytes={} (entries {} + spare {}) \
             measured={freed} ratio={ratio:.3} per_message census={} measured={}",
            census.index_bytes,
            census.index_entry_bytes,
            census.index_spare_bytes,
            census.index_bytes / messages,
            freed / messages,
        );
        println!("{line}");
        if !(0.95..=1.05).contains(&ratio) {
            failures.push(line);
        }
    }
    assert!(
        failures.is_empty(),
        "store_index is not what the index allocates: {failures:#?}"
    );
}
