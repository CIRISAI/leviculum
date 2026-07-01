//! Allocation regression guard for EmbeddedStorage insert-with-eviction
//! (Codeberg #65).
//!
//! Root cause of the LNode self-reset under load: the previous `map_set`
//! evicted the oldest entry on every insert into a FULL capacity-bounded map
//! by COPYING THE WHOLE MAP through two transient heap `Vec`s
//! (`Vec<K>` + `Vec<(K, V)>`). Under churn the maps sit at capacity, so this
//! fired per insert; on a near-full 64 KiB heap the multi-KB transient OOMs
//! the `embedded_alloc` allocator and resets the node.
//!
//! The fix makes eviction allocation-free (a companion inline insertion-order
//! index). This test drives a bounded map PAST capacity in steady state and
//! asserts the GROSS heap bytes allocated DURING the eviction loop is ZERO.
//!
//! Note on the metric: the #65 failure is a TRANSIENT allocation spike, not a
//! leak. The pre-fix `map_set` allocated its scratch `Vec`s and freed them
//! within the same insert, so a NET live-bytes metric (allocated minus freed)
//! reads zero for both versions and would NOT catch the regression. The OOM
//! happens at the moment the multi-KB request hits a near-full heap with no
//! large-enough free block, regardless of the later free. So we count GROSS
//! bytes allocated and the number of allocation CALLS in the eviction window.
//! Maps with `Copy` values (path_states) and the dedup tag set allocate
//! nothing at all on the value side, so a clean eviction path makes ZERO
//! allocation calls; the pre-fix code made ~2 calls (~1 KB) per eviction.
//!
//! This file is its own test binary, so its `#[global_allocator]` does not
//! affect the rest of the suite (same pattern as `tests/heap_leak.rs`).
//!
//! Run: `cargo test -p leviculum-core --test embedded_eviction_alloc -- --nocapture`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use leviculum_core::storage_types::{PathEntry, PathState};
use leviculum_core::traits::Storage;
use leviculum_core::EmbeddedStorage;

// ----------------------------------------------------------------------------
// Counting global allocator: net live bytes (allocated minus freed).
// ----------------------------------------------------------------------------

struct CountingAlloc;

/// Gross bytes ever requested (never decremented). The transient-spike metric.
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
/// Number of allocation calls (alloc + alloc_zeroed + realloc).
static ALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every branch forwards to the System allocator with the same layout
// it was handed; the atomic counters never touch the returned memory.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() {
            ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = System.realloc(ptr, layout, new_size);
        if !new_ptr.is_null() {
            // A realloc that grows is a fresh large request against the heap.
            ALLOCATED.fetch_add(new_size, Ordering::Relaxed);
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        new_ptr
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn allocated_bytes() -> usize {
    ALLOCATED.load(Ordering::Relaxed)
}

fn alloc_calls() -> usize {
    ALLOC_CALLS.load(Ordering::Relaxed)
}

// The counters are process-global and the allocator runs on whatever thread
// allocates. libtest's own parallel orchestration (thread spawn, stdout
// capture) allocates on OTHER threads and would leak into the counter during a
// measured window. A per-test `MutexGuard` cannot stop that. So this binary
// exposes a SINGLE `#[test]` that runs every scenario sequentially: with one
// test there is no intra-binary parallelism, and other test binaries are
// separate processes with their own counter. The measured windows are then
// truly clean (verified: 0 allocations).

fn key_th(i: usize) -> [u8; TRUNCATED_HASHBYTES] {
    let mut h = [0u8; TRUNCATED_HASHBYTES];
    h[..8].copy_from_slice(&(i as u64).to_le_bytes());
    h
}

fn key32(i: usize) -> [u8; 32] {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(&(i as u64).to_le_bytes());
    h
}

/// Warm `body` over `[0, WARMUP)` to settle any one-time cold-path init, then
/// measure `iters` more calls (indices continue past warmup so they never
/// collide) and assert ZERO heap allocations. The value side of the maps under
/// test owns no heap, so a clean eviction path allocates nothing at all; the
/// pre-fix rebuild made ~2 calls / ~1 KB per eviction. A strict zero is the
/// right bar: nothing else runs in the measured window.
const WARMUP: usize = 1_000;

fn assert_alloc_free(label: &str, iters: u64, mut body: impl FnMut(usize)) {
    for i in 0..WARMUP {
        body(i);
    }
    let bytes0 = allocated_bytes();
    let calls0 = alloc_calls();
    for i in 0..iters as usize {
        body(WARMUP + i);
    }
    let bytes = allocated_bytes() - bytes0;
    let calls = alloc_calls() - calls0;
    println!(
        "[evict_alloc] {label:<24} iters={iters:<6} gross={bytes:>12} B  calls={calls:>10}  ({:.4} B/iter, {:.4} calls/iter)",
        bytes as f64 / iters as f64,
        calls as f64 / iters as f64,
    );
    assert_eq!(
        calls, 0,
        "{label}: {calls} heap allocations ({bytes} B gross) during steady-state eviction; the eviction path is not allocation-free"
    );
}

/// Run every allocation-free scenario sequentially in one test (see the note
/// above on why this is a single test rather than four).
#[test]
fn eviction_paths_are_allocation_free() {
    check_path_states_eviction();
    check_path_table_eviction();
    check_path_table_refresh();
    check_path_request_tag_set_eviction();
}

/// path_states: `PathState` is a `Copy` enum, so the value side allocates
/// nothing. Once the table is full (cap 32), every distinct-key insert evicts.
/// Gross heap growth across the measured window must be zero.
fn check_path_states_eviction() {
    let cap = 32usize;
    let mut s = EmbeddedStorage::new();

    // Fill to capacity, then warm up well past it so the table is in steady
    // eviction and any one-time setup has settled.
    for i in 0..(cap * 4) {
        s.set_path_state(key_th(i), PathState::Unresponsive);
    }

    let iters = 50_000u64;
    let base = cap * 4;
    // Distinct fresh key every time -> guaranteed eviction every insert.
    assert_alloc_free("path_states_evict", iters, |i| {
        s.set_path_state(key_th(base + i), PathState::Responsive);
    });

    // Sanity: the table is still exactly at capacity (not vacuous). The last
    // measured key index is base + WARMUP + iters - 1.
    let last = base + WARMUP + iters as usize;
    let live = ((last - cap)..last)
        .filter(|i| s.get_path_state(&key_th(*i)).is_some())
        .count();
    assert_eq!(
        live, cap,
        "table must remain full; measurement otherwise vacuous"
    );
}

/// path_table with empty `random_blobs` and `next_hop: None`: the value owns
/// no heap, so this isolates the eviction path the same way path_states does,
/// while also exercising the refresh-on-re-insert branch.
fn check_path_table_eviction() {
    let cap = 32usize;
    let entry = PathEntry {
        hops: 1,
        expires_ms: 10_000,
        interface_index: 0,
        random_blobs: Vec::new(),
        next_hop: None,
    };

    let mut s = EmbeddedStorage::new();
    for i in 0..(cap * 4) {
        s.set_path(key_th(i), entry.clone());
    }

    let iters = 50_000u64;
    let base = cap * 4;
    // Empty `random_blobs` clones without allocating, so any heap call here is
    // the eviction path itself.
    assert_alloc_free("path_table_evict", iters, |i| {
        s.set_path(key_th(base + i), entry.clone());
    });

    assert_eq!(
        s.path_count(),
        cap,
        "table must remain full; otherwise vacuous"
    );
}

/// Refresh-on-re-insert must also be allocation-free: repeatedly re-insert
/// keys already present (the `set_path` re-announce path) while the table is
/// full. No eviction occurs here, but the order-index move-to-back must not
/// allocate.
fn check_path_table_refresh() {
    let cap = 32usize;
    let entry = PathEntry {
        hops: 1,
        expires_ms: 10_000,
        interface_index: 0,
        random_blobs: Vec::new(),
        next_hop: None,
    };

    let mut s = EmbeddedStorage::new();
    for i in 0..cap {
        s.set_path(key_th(i), entry.clone());
    }

    let iters = 50_000u64;
    // Re-insert an existing key (cycles through the full key set).
    assert_alloc_free("path_table_refresh", iters, |i| {
        s.set_path(key_th(i % cap), entry.clone());
    });

    assert_eq!(
        s.path_count(),
        cap,
        "refresh must not change membership count"
    );
}

/// The path-request dedup tag set (OrderedSet) had the same KB-per-eviction
/// rebuild; its eviction must now be allocation-free too.
fn check_path_request_tag_set_eviction() {
    let cap = 32usize;
    let mut s = EmbeddedStorage::new();

    for i in 0..(cap * 4) {
        s.check_path_request_tag(&key32(i));
    }

    let iters = 50_000u64;
    let base = cap * 4;
    assert_alloc_free("tag_set_evict", iters, |i| {
        let seen = s.check_path_request_tag(&key32(base + i));
        assert!(!seen, "fresh tag wrongly reported as seen");
    });
}
