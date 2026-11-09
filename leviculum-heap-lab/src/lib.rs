//! The boards' heap choices on the host (order 1569).
//!
//! The M9 starves its UI core during a full pull: both cores' stage
//! times grow with the number of live messages and fall back to the
//! baseline after a reset, while the fold itself is flat on the host
//! (`loquax-m9/tests/turn.rs`, 1463). What differs between the two runs
//! is the allocator: the firmware links `embedded_alloc`
//! (`leviculum-esp/src/lib.rs`), and its `llff` choice is
//! `linked_list_allocator` — a first-fit walk of one free list on every
//! `alloc`, and an address-sorted insert walk on every `dealloc`, under
//! the one cross-core critical section. The walk grows with the heap's
//! churn; the host's system allocator shows none of it.
//!
//! This lab runs a closure under either of the two allocators
//! `embedded_alloc` offers — [`Which::Llff`] (the choice before 1569)
//! and [`Which::Tlsf`] (rlsf, constant-time segregated fit) — so a test
//! can measure the same workload under both (`tests/pull.rs`). The
//! global allocator here is a router: whatever mode is set serves the
//! allocations, and every free goes to the heap that made the block,
//! found by its address, so values may cross the mode boundary freely.
//!
//! A lab allocation the mode's heap has no room for is served by the
//! system allocator and counted ([`spilled`]): a measurement that
//! overflowed its region says so instead of measuring the fallback.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::alloc::System;
use std::sync::{Mutex, MutexGuard, Once};

/// Each lab heap's region. Large enough for the pull workload on a
/// 64-bit host (the board's is 8 MiB on a 32-bit one); virtual, so the
/// pages cost nothing until touched.
const REGION: usize = 256 << 20;

/// One lab heap the firmware could link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    /// `embedded_alloc::LlffHeap`'s inner `linked_list_allocator`.
    Llff,
    /// `rlsf`, as the firmware places it ([`pool_tlsf`], 1574).
    Tlsf,
}

const MODE_SYSTEM: u8 = 0;
const MODE_LLFF: u8 = 1;
const MODE_TLSF: u8 = 2;

/// Who serves the next `alloc`; frees route by address regardless.
static MODE: AtomicU8 = AtomicU8::new(MODE_SYSTEM);
/// Lab allocations the mode's heap refused, served by the system.
static SPILLED: AtomicUsize = AtomicUsize::new(0);
/// The largest single block a lab heap served since [`reset_counts`]
/// (1603): the size a board's pool must find in one piece.
static LARGEST: AtomicUsize = AtomicUsize::new(0);
/// Bytes the lab heaps hold now, as the layouts asked them, and the most
/// they held since [`reset_counts`].
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// The regions, `static` so their addresses bound the free-routing.
// `u64` words for alignment the allocators need not then make up.
static LLFF_MEM: Region = Region::new();
static TLSF_MEM: Region = Region::new();

struct Region(core::cell::UnsafeCell<[u64; REGION / 8]>);

// SAFETY: the region's bytes are handed to exactly one allocator, once
// (`Once` below); after that only that allocator touches them, under its
// mutex.
unsafe impl Sync for Region {}

impl Region {
    const fn new() -> Self {
        Self(core::cell::UnsafeCell::new([0; REGION / 8]))
    }

    fn start(&self) -> usize {
        self.0.get() as usize
    }

    fn holds(&self, ptr: *mut u8) -> bool {
        let at = ptr as usize;
        at >= self.start() && at < self.start() + REGION
    }
}

// The firmware's own TLSF wrapper, the one source (1574): the control
// block placed inside the region, as the M9 links it.
#[path = "../../leviculum-esp/src/pool_tlsf.rs"]
pub mod pool_tlsf;

use pool_tlsf::PoolTlsf;

// The firmware's per-core pools (1594), the one source: the routing of
// allocations to the asking core's pool and of frees to the owner's,
// tested with threads in `tests/pools.rs`.
#[path = "../../leviculum-esp/src/pools.rs"]
pub mod pools;

/// The heaps hold raw pointers into their (static, 'static) regions;
/// moving the guard between threads moves only the handle.
struct Held<T>(T);

// SAFETY: see `Held`; all access is under the router's mutexes.
unsafe impl<T> Send for Held<T> {}

struct Router {
    llff: Mutex<Held<linked_list_allocator::Heap>>,
    tlsf: Mutex<Option<PoolTlsf>>,
}

#[global_allocator]
static ROUTER: Router = Router {
    llff: Mutex::new(Held(linked_list_allocator::Heap::empty())),
    tlsf: Mutex::new(None),
};

/// The guard, poison shrugged off: the allocator cannot panic while it
/// holds the lock, so a poisoned one only says some other panic
/// unwound past a `with`.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn ensure_init(which: Which) {
    static LLFF_INIT: Once = Once::new();
    static TLSF_INIT: Once = Once::new();
    match which {
        Which::Llff => LLFF_INIT.call_once(|| {
            // SAFETY: the region is static, unaliased (`Region`), and
            // handed over exactly once.
            unsafe {
                locked(&ROUTER.llff)
                    .0
                    .init(LLFF_MEM.start() as *mut u8, REGION)
            }
        }),
        Which::Tlsf => TLSF_INIT.call_once(|| {
            // SAFETY: as above. A refused placement leaves `None`, and
            // every lab allocation then spills and is counted.
            *locked(&ROUTER.tlsf) = unsafe { PoolTlsf::place(TLSF_MEM.start(), REGION) };
        }),
    }
}

// SAFETY: every path serves a layout-correct block: the lab heaps via
// their own invariants, the system via `System`. Frees go to the heap
// whose region holds the pointer, so a block is always returned to the
// allocator that made it, whatever the mode is by then.
unsafe impl GlobalAlloc for Router {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let served = match MODE.load(Ordering::Relaxed) {
            MODE_LLFF => locked(&self.llff)
                .0
                .allocate_first_fit(layout)
                .map(NonNull::as_ptr)
                .ok(),
            MODE_TLSF => locked(&self.tlsf)
                .as_mut()
                .and_then(|tlsf| tlsf.allocate(layout))
                .map(NonNull::as_ptr),
            _ => None,
        };
        match (served, MODE.load(Ordering::Relaxed)) {
            (Some(ptr), _) => {
                LARGEST.fetch_max(layout.size(), Ordering::Relaxed);
                let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
                PEAK.fetch_max(live, Ordering::Relaxed);
                ptr
            }
            (None, MODE_SYSTEM) => {
                // SAFETY: the caller's contract, passed through.
                unsafe { System.alloc(layout) }
            }
            (None, _) => {
                SPILLED.fetch_add(1, Ordering::Relaxed);
                // SAFETY: as above.
                unsafe { System.alloc(layout) }
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if LLFF_MEM.holds(ptr) || TLSF_MEM.holds(ptr) {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        if LLFF_MEM.holds(ptr) {
            if let Some(ptr) = NonNull::new(ptr) {
                // SAFETY: the block came from this heap (its region
                // holds it), with this layout.
                unsafe { locked(&self.llff).0.deallocate(ptr, layout) };
            }
        } else if TLSF_MEM.holds(ptr) {
            if let Some(ptr) = NonNull::new(ptr) {
                if let Some(tlsf) = locked(&self.tlsf).as_mut() {
                    // SAFETY: as above.
                    unsafe { tlsf.deallocate(ptr, layout.align()) };
                }
            }
        } else {
            // SAFETY: the system made it.
            unsafe { System.dealloc(ptr, layout) };
        }
    }
}

/// Run `f` with its allocations served by `which`, whatever it
/// allocates routed back to the right heap whenever it is freed. Not
/// reentrant: a `with` inside a `with` serves the outer mode no more.
pub fn with<R>(which: Which, f: impl FnOnce() -> R) -> R {
    struct Back;
    impl Drop for Back {
        fn drop(&mut self) {
            MODE.store(MODE_SYSTEM, Ordering::SeqCst);
        }
    }
    ensure_init(which);
    let _back = Back;
    MODE.store(
        match which {
            Which::Llff => MODE_LLFF,
            Which::Tlsf => MODE_TLSF,
        },
        Ordering::SeqCst,
    );
    f()
}

/// Lab allocations the mode's heap refused so far: a measurement is
/// only one while this stayed where it began.
pub fn spilled() -> usize {
    SPILLED.load(Ordering::Relaxed)
}

/// The largest single block a lab heap served since [`reset_counts`]
/// (1603).
pub fn largest() -> usize {
    LARGEST.load(Ordering::Relaxed)
}

/// The bytes the lab heaps hold now, as their layouts asked them.
pub fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

/// The most the lab heaps held at once since [`reset_counts`].
pub fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

/// Start [`largest`] afresh, and [`peak`] from what is held now.
pub fn reset_counts() {
    LARGEST.store(0, Ordering::Relaxed);
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}
