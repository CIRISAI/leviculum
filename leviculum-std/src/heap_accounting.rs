//! Measured live heap: a counting shim in FRONT of the system allocator.
//!
//! # Why a shim and not the dump
//!
//! [`crate::reticulum::Reticulum::diagnostic_dump`] prices the collections
//! with flat multipliers — 3x for a `BTreeMap`, 1.5x for a `HashMap` — which
//! is a model, and a model cannot be put on the other side of an equation
//! from a measured resident set: any gap it shows could be the model being
//! wrong rather than the memory being lost. Measured against this shim at
//! the bench's table shape on 2026-09-21, the dump over-reported live data
//! by 1.92x, so a resident-to-data ratio computed from it is a ratio of a
//! model and not of the node.
//!
//! [`crate::heap_accounting::live_bytes`] is not a model. It is the sum of
//! what the allocator handed out minus what it took back, so `rss / live`
//! is a fragmentation number and nothing else.
//!
//! # It does not change the allocator
//!
//! "Do not change the allocator" is a standing rule for the heap
//! investigation and this obeys it: every call is forwarded to
//! [`std::alloc::System`] with the layout it arrived with, so a binary
//! carrying the shim runs the same musl mallocng, takes the same code paths
//! and produces the same group layout as one without it. What it adds is two relaxed atomics per call —
//! `heap-gap-bench --alloc-overhead` measures what that costs.
//!
//! # Users
//!
//! `heap-gap-bench` installs it always; `lnsd` installs it under its
//! default-off `heap-accounting` feature, for a field node that reports
//! measured live bytes instead of an estimate.

use core::sync::atomic::{AtomicUsize, Ordering};
use std::alloc::{GlobalAlloc, Layout, System};

/// Bytes currently handed out by the allocator and not yet returned, and
/// the running totals behind them.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static TOTAL: AtomicUsize = AtomicUsize::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

/// The counting shim. Install with
/// `#[global_allocator] static ALLOC: CountingAllocator = CountingAllocator;`
/// — the counters stay at zero in a process that does not.
pub struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() {
            record(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // Forwarded rather than left to the default (alloc + memset): a
        // large zeroed block is a fresh mmap in mallocng and already zero,
        // and rewriting it by hand would touch pages production does not.
        let p = System.alloc_zeroed(layout);
        if !p.is_null() {
            record(layout.size());
        }
        p
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = System.realloc(ptr, layout, new_size);
        if !p.is_null() {
            LIVE.fetch_add(new_size.wrapping_sub(layout.size()), Ordering::Relaxed);
            TOTAL.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

/// `LIVE` is only ever read when the true balance is non-negative, so the
/// wrapping subtraction in `dealloc` and `realloc` costs nothing and saves
/// a signed type.
fn record(size: usize) {
    LIVE.fetch_add(size, Ordering::Relaxed);
    TOTAL.fetch_add(size, Ordering::Relaxed);
    ALLOCS.fetch_add(1, Ordering::Relaxed);
}

/// Bytes handed out and not yet returned.
pub fn live_bytes() -> usize {
    LIVE.load(Ordering::Relaxed)
}

/// Every byte ever handed out, growth on `realloc` included: the churn,
/// against which `live_bytes` is the standing balance.
pub fn total_bytes() -> usize {
    TOTAL.load(Ordering::Relaxed)
}

/// Calls into the allocator that returned memory.
pub fn allocation_count() -> usize {
    ALLOCS.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The arithmetic, in one test rather than three: the counters are
    /// process-global, so two test threads asserting on deltas of the same
    /// statics would race each other. Under `cargo test` this shim is NOT the
    /// global allocator — the harness's is — so nothing else moves them and
    /// the deltas here are exactly what these calls did.
    #[test]
    fn counts_what_it_hands_out_and_takes_back() {
        let layout = Layout::from_size_align(4096, 8).expect("layout");
        let live0 = live_bytes();
        let total0 = total_bytes();
        let allocs0 = allocation_count();

        let p = unsafe { CountingAllocator.alloc(layout) };
        assert!(!p.is_null(), "allocation failed");
        assert_eq!(live_bytes() - live0, 4096, "alloc must add its size");
        assert_eq!(total_bytes() - total0, 4096);
        assert_eq!(allocation_count() - allocs0, 1);

        // Growth is counted as the DIFFERENCE, not as a second full block:
        // the old bytes were already in `LIVE` and were not returned.
        let p = unsafe { CountingAllocator.realloc(p, layout, 8192) };
        assert!(!p.is_null(), "realloc failed");
        assert_eq!(live_bytes() - live0, 8192, "realloc must add the growth");
        assert_eq!(total_bytes() - total0, 8192);

        // A shrink gives bytes back, and the churn total does not shrink with
        // it — `TOTAL` is what was ever handed out.
        let grown = Layout::from_size_align(8192, 8).expect("layout");
        let p = unsafe { CountingAllocator.realloc(p, grown, 2048) };
        assert!(!p.is_null(), "shrinking realloc failed");
        assert_eq!(live_bytes() - live0, 2048, "shrink must return the delta");
        assert_eq!(total_bytes() - total0, 8192, "churn never decreases");

        let shrunk = Layout::from_size_align(2048, 8).expect("layout");
        unsafe { CountingAllocator.dealloc(p, shrunk) };
        assert_eq!(live_bytes(), live0, "a freed block leaves no live bytes");
        assert_eq!(allocation_count() - allocs0, 3, "alloc + two reallocs");

        // And the other half of the contract, which is what makes the `lnsd`
        // feature safe to ship off by default: an allocation that does NOT go
        // through the shim is not counted. It belongs in this test body and
        // not in a second test — a sibling running in parallel would be
        // moving the same statics, and the assertion would fail on a
        // scheduler decision rather than on a defect.
        let counted = allocation_count();
        let uncounted = std::hint::black_box(vec![0u8; 1 << 20]);
        assert_eq!(
            allocation_count(),
            counted,
            "an allocation not routed through the shim must not be counted"
        );
        drop(uncounted);
    }
}
