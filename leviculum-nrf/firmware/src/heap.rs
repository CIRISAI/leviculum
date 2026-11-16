//! The board's heap: the global allocator and the two facts the
//! orchestration reads from it, the instantaneous (used, free) pair and the
//! high-watermark since boot (Codeberg #65), with the periodic `[HEAP]` line
//! that carries both into the log (#469 961: moved from `leviculum-nrf`'s
//! `lib.rs`; `alloc` is this stratum's knowledge by the page's table).
//!
//! What stays below is where the pool lies: the bind crate owns the memory
//! the allocator manages and hands it over once, through [`init`], before
//! the first allocation. The pool's size is the boot heap budget's
//! ([`leviculum_heap_budget::HEAP_SIZE`]), the same constant the census
//! assertion sums against.
//!
//! # The allocator only on the target
//!
//! `#[global_allocator]` is attached on `target_os = "none"` alone, which is
//! every board image and never a host build. A host test binary of this
//! crate, or of any host crate that ever links it, keeps the system
//! allocator: the static here would otherwise serve the test harness's first
//! allocation before anything could call [`init`]. `target_os` names the
//! absence of an operating system, not a chip, so the orchestration stays
//! chip-blind. The static itself exists on both, which is what lets a host
//! test drive the watermark through a real `LlffHeap` over a buffer of its
//! own.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use embedded_alloc::LlffHeap as Heap;
use leviculum_heap_budget::HEAP_SIZE;

#[cfg_attr(target_os = "none", global_allocator)]
static HEAP: WatchedHeap = WatchedHeap::empty();

/// The board's allocator: `LlffHeap`, plus the two things a post-mortem of
/// an allocation failure needs and the panic message alone cannot say
/// (order 1665).
///
/// `memory allocation of 3456 bytes failed phase=ble-peer` names the victim
/// and the arm, not the holder: the last `[HEAP_CENSUS]` before each of the
/// T114's four such panics (2026-11-10 and 11) showed 16 to 22 KB free, and
/// nothing on the wire said what took the rest. So the wrapper keeps, per
/// phase ([`crate::thread_watch::set_phase`] resets them), how many
/// allocations were served since the phase was entered, their bytes, the
/// bytes returned and the largest single block, beside the free heap at
/// entry; and on a refused allocation it freezes those with the request's
/// size, the free heap and the largest block still servable. The panic
/// handler appends the frozen record and the last census to the panic
/// message (`crate::postmortem::record_panic`), and the next boot prints it
/// as `[PANIC_CENSUS]`.
///
/// Allocations by the interrupt executor (the LoRa task) land in the phase
/// thread mode is in. Every counter is an atomic; nothing here allocates.
/// `realloc` is the trait's default (allocate, copy, free), which is what
/// `LlffHeap` does as well, so a growing `Vec` is counted as the new block it
/// asks for.
pub struct WatchedHeap {
    inner: Heap,
}

impl WatchedHeap {
    /// An allocator with no pool yet.
    pub const fn empty() -> Self {
        Self {
            inner: Heap::empty(),
        }
    }
}

/// Allocations served since the phase was entered.
static PHASE_ALLOCS: AtomicU32 = AtomicU32::new(0);
/// Bytes those allocations asked for.
static PHASE_ALLOC_BYTES: AtomicU32 = AtomicU32::new(0);
/// Bytes returned since the phase was entered (whoever allocated them).
static PHASE_FREED_BYTES: AtomicU32 = AtomicU32::new(0);
/// The largest single block served since the phase was entered.
static PHASE_MAX_BLOCK: AtomicU32 = AtomicU32::new(0);
/// Free heap when the phase was entered.
static PHASE_ENTRY_FREE: AtomicU32 = AtomicU32::new(0);

/// Set by the first refused allocation; the fields below are frozen with it.
static FAILED: AtomicBool = AtomicBool::new(false);
static FAIL_SIZE: AtomicU32 = AtomicU32::new(0);
static FAIL_FREE: AtomicU32 = AtomicU32::new(0);
static FAIL_LARGEST: AtomicU32 = AtomicU32::new(0);
static FAIL_ENTRY_FREE: AtomicU32 = AtomicU32::new(0);
static FAIL_ALLOCS: AtomicU32 = AtomicU32::new(0);
static FAIL_ALLOC_BYTES: AtomicU32 = AtomicU32::new(0);
static FAIL_FREED_BYTES: AtomicU32 = AtomicU32::new(0);
static FAIL_MAX_BLOCK: AtomicU32 = AtomicU32::new(0);

fn saturate(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Largest block `heap` can serve now, by binary search with real probe
/// allocations at 64-byte resolution (the census's
/// `largest_free_block`, done on the inner heap so the probes are not
/// counted and work from inside the allocator).
fn largest_servable(heap: &Heap) -> usize {
    let mut lo = 0usize;
    let mut hi = heap.free();
    while hi.saturating_sub(lo) > 64 {
        let mid = lo + (hi - lo) / 2;
        let Ok(layout) = Layout::from_size_align(mid, 4) else {
            break;
        };
        // SAFETY: a non-zero layout; a null return is checked before use.
        let ptr = unsafe { GlobalAlloc::alloc(heap, layout) };
        if ptr.is_null() {
            hi = mid;
        } else {
            // SAFETY: `ptr` came from this heap with this layout.
            unsafe { GlobalAlloc::dealloc(heap, ptr, layout) };
            lo = mid;
        }
    }
    lo
}

impl WatchedHeap {
    /// Freeze the phase counters with a refused request of `size` bytes.
    /// Only the first refusal is kept: it is the one the panic is about.
    fn record_failure(&self, size: usize) {
        if FAILED.swap(true, Ordering::Relaxed) {
            return;
        }
        FAIL_SIZE.store(saturate(size), Ordering::Relaxed);
        FAIL_FREE.store(saturate(self.inner.free()), Ordering::Relaxed);
        FAIL_LARGEST.store(saturate(largest_servable(&self.inner)), Ordering::Relaxed);
        for (from, to) in [
            (&PHASE_ENTRY_FREE, &FAIL_ENTRY_FREE),
            (&PHASE_ALLOCS, &FAIL_ALLOCS),
            (&PHASE_ALLOC_BYTES, &FAIL_ALLOC_BYTES),
            (&PHASE_FREED_BYTES, &FAIL_FREED_BYTES),
            (&PHASE_MAX_BLOCK, &FAIL_MAX_BLOCK),
        ] {
            to.store(from.load(Ordering::Relaxed), Ordering::Relaxed);
        }
    }
}

// SAFETY: every call forwards to `LlffHeap` with the layout it arrived with;
// the bookkeeping only touches atomics and never allocates (the failure
// path's probes go to the inner heap and are released before it returns).
unsafe impl GlobalAlloc for WatchedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded from this function's contract.
        let ptr = unsafe { GlobalAlloc::alloc(&self.inner, layout) };
        if ptr.is_null() {
            self.record_failure(layout.size());
        } else {
            let size = saturate(layout.size());
            PHASE_ALLOCS.fetch_add(1, Ordering::Relaxed);
            PHASE_ALLOC_BYTES.fetch_add(size, Ordering::Relaxed);
            PHASE_MAX_BLOCK.fetch_max(size, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        PHASE_FREED_BYTES.fetch_add(saturate(layout.size()), Ordering::Relaxed);
        // SAFETY: forwarded from this function's contract.
        unsafe { GlobalAlloc::dealloc(&self.inner, ptr, layout) }
    }
}

/// Thread mode entered a new phase: restart the per-phase counters from the
/// heap as it is now. Called by [`crate::thread_watch::set_phase`].
pub fn phase_entered() {
    PHASE_ENTRY_FREE.store(saturate(HEAP.inner.free()), Ordering::Relaxed);
    PHASE_ALLOCS.store(0, Ordering::Relaxed);
    PHASE_ALLOC_BYTES.store(0, Ordering::Relaxed);
    PHASE_FREED_BYTES.store(0, Ordering::Relaxed);
    PHASE_MAX_BLOCK.store(0, Ordering::Relaxed);
}

/// The first refused allocation of this boot, as the allocator froze it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocFailure {
    /// Bytes the refused request asked for.
    pub size: u32,
    /// Free heap at the refusal.
    pub free: u32,
    /// Largest block the heap could still serve at the refusal.
    pub largest: u32,
    /// Free heap when the phase the refusal happened in was entered.
    pub phase_entry_free: u32,
    /// Allocations served in that phase before the refusal.
    pub phase_allocs: u32,
    /// Their bytes.
    pub phase_alloc_bytes: u32,
    /// Bytes returned in that phase before the refusal.
    pub phase_freed_bytes: u32,
    /// The largest block served in that phase.
    pub phase_max_block: u32,
}

/// The refusal this boot recorded, if any. For the panic handler.
pub fn alloc_failure() -> Option<AllocFailure> {
    if !FAILED.load(Ordering::Relaxed) {
        return None;
    }
    Some(AllocFailure {
        size: FAIL_SIZE.load(Ordering::Relaxed),
        free: FAIL_FREE.load(Ordering::Relaxed),
        largest: FAIL_LARGEST.load(Ordering::Relaxed),
        phase_entry_free: FAIL_ENTRY_FREE.load(Ordering::Relaxed),
        phase_allocs: FAIL_ALLOCS.load(Ordering::Relaxed),
        phase_alloc_bytes: FAIL_ALLOC_BYTES.load(Ordering::Relaxed),
        phase_freed_bytes: FAIL_FREED_BYTES.load(Ordering::Relaxed),
        phase_max_block: FAIL_MAX_BLOCK.load(Ordering::Relaxed),
    })
}

/// The heap census's owners, as the last `[HEAP_CENSUS]` line counted them,
/// in the order of [`CENSUS_KEYS`]. The census in `leviculum-nrf` writes
/// them ([`note_census`]); the panic handler appends them to the record.
static CENSUS: [AtomicU32; CENSUS_KEYS.len()] = [const { AtomicU32::new(0) }; CENSUS_KEYS.len()];
/// When the last census was taken (ms since boot, truncated); 0 = never.
static CENSUS_AT_MS: AtomicU32 = AtomicU32::new(0);

/// The `[HEAP_CENSUS]` words a post-mortem carries, a subset of the line's
/// owners chosen to sum to most of `used=`: the walked owners that ever
/// reached a kilobyte on the T114, and `other=`.
pub const CENSUS_KEYS: [&str; 16] = [
    "used",
    "free",
    "largest",
    "node_box",
    "links",
    "events",
    "req",
    "dest",
    "transport",
    "storage",
    "ble_sessions",
    "pn_peers",
    "pn_work",
    "pn_links",
    "pn_flush",
    "other",
];

/// Keep the words of the census just taken, at `now_ms`, for the panic
/// record. `words` follows [`CENSUS_KEYS`].
pub fn note_census(words: [usize; CENSUS_KEYS.len()], now_ms: u64) {
    for (slot, word) in CENSUS.iter().zip(words) {
        slot.store(saturate(word), Ordering::Relaxed);
    }
    CENSUS_AT_MS.store((now_ms as u32).max(1), Ordering::Relaxed);
}

/// Write the post-mortem's allocation words: the refusal, then the last
/// census with its age at `now_ms`. Nothing when no allocation was refused,
/// so a panic of any other kind keeps its message as it was. No allocation.
pub fn write_alloc_postmortem(w: &mut dyn core::fmt::Write, now_ms: u64) -> core::fmt::Result {
    let Some(fail) = alloc_failure() else {
        return Ok(());
    };
    write!(
        w,
        "alloc size={} free={} largest={} phase_entry_free={} phase_allocs={} phase_alloc_bytes={} phase_freed_bytes={} phase_max_block={}",
        fail.size,
        fail.free,
        fail.largest,
        fail.phase_entry_free,
        fail.phase_allocs,
        fail.phase_alloc_bytes,
        fail.phase_freed_bytes,
        fail.phase_max_block,
    )?;
    let at = CENSUS_AT_MS.load(Ordering::Relaxed);
    if at == 0 {
        return write!(w, " census=none");
    }
    write!(w, " census_age_ms={}", (now_ms as u32).wrapping_sub(at))?;
    for (key, slot) in CENSUS_KEYS.iter().zip(CENSUS.iter()) {
        write!(w, " {}={}", key, slot.load(Ordering::Relaxed))?;
    }
    Ok(())
}

/// Hand the allocator its pool.
///
/// # Safety
///
/// Call exactly once, before the first allocation, with `start..start +
/// size` a region of RAM that nothing else reads or writes for the rest of
/// the program; the same contract as `embedded_alloc::LlffHeap::init`,
/// which this forwards to.
pub unsafe fn init(start: usize, size: usize) {
    // SAFETY: forwarded from this function's contract.
    unsafe { HEAP.inner.init(start, size) }
}

/// Return (used, free) heap bytes at this instant.
pub fn heap_stats() -> (usize, usize) {
    (HEAP.inner.used(), HEAP.inner.free())
}

/// High-watermark of heap usage since boot (bytes). Updated by
/// [`heap_watermark_sample`]; resets with every boot — the periodic
/// `[HEAP]` log lines (which land in the persistent tail) carry the
/// history across resets. Codeberg #65 instrumentation.
static HEAP_WATERMARK: AtomicUsize = AtomicUsize::new(0);

/// Sample the heap, update the high-watermark, and return
/// `(used, free, watermark)`.
pub fn heap_watermark_sample() -> (usize, usize, usize) {
    let (used, free) = heap_stats();
    let watermark = HEAP_WATERMARK.fetch_max(used, Ordering::Relaxed).max(used);
    (used, free, watermark)
}

/// Periodic heap telemetry (Codeberg #65): one `[HEAP]` line every 30 s
/// into the normal log path, which feeds both the USB debug capture and
/// the persistent tail in `.uninit` — so an OOM panic's post-mortem boot
/// replays the heap trajectory leading up to it. Strictly additive: the
/// cadence is coarse and the sampling is two atomic loads; the alloc
/// path itself is untouched. The bind crate's executor task runs it.
pub async fn watermark_loop() -> ! {
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(30)).await;
        let (used, free, watermark) = heap_watermark_sample();
        crate::log::log_fmt(
            "[HEAP] ",
            format_args!("used={used} free={free} watermark={watermark} size={HEAP_SIZE}"),
        );
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::String;

    /// One test, because the heap and the watermark are one static per test
    /// binary and the allocator takes its pool once.
    #[test]
    fn the_watermark_holds_the_peak_after_the_heap_shrinks() {
        assert_eq!(heap_stats(), (0, 0), "an uninitialised heap has no pool");

        static mut POOL: [u8; 4096] = [0; 4096];
        // SAFETY: the one init of this binary's HEAP, over a pool nothing
        // else names.
        unsafe { init(core::ptr::addr_of_mut!(POOL) as usize, 4096) };
        let (used0, free0) = heap_stats();
        // The allocator keeps a few bytes of the pool for its own hole list,
        // so the pair sums to a fixed total just under the pool.
        let total = used0 + free0;
        assert!(
            total > 4000 && total <= 4096,
            "the pool is the heap: {total}"
        );

        let layout = Layout::from_size_align(1024, 8).unwrap();
        // SAFETY: a non-zero layout, freed below with the same layout.
        let block = unsafe { HEAP.alloc(layout) };
        assert!(!block.is_null());
        let (used1, free1, mark1) = heap_watermark_sample();
        assert!(used1 >= used0 + 1024, "the block is counted as used");
        assert_eq!(used1 + free1, total);
        assert_eq!(mark1, used1, "the watermark rises with the heap");

        // SAFETY: `block` came from this allocator with this layout.
        unsafe { HEAP.dealloc(block, layout) };
        let (used2, _, mark2) = heap_watermark_sample();
        assert_eq!(used2, used0, "the block is returned");
        assert_eq!(mark2, mark1, "the watermark keeps the peak");

        // Order 1665: the per-phase counters and the frozen refusal. A
        // phase is entered, two blocks are served and one returned, and a
        // request larger than the pool is refused.
        phase_entered();
        let entry_free = heap_stats().1;
        let big = Layout::from_size_align(512, 8).unwrap();
        let small = Layout::from_size_align(256, 8).unwrap();
        // SAFETY: non-zero layouts, freed below with the same layouts.
        let (a, b) = unsafe { (HEAP.alloc(big), HEAP.alloc(small)) };
        assert!(!a.is_null() && !b.is_null());
        // SAFETY: `b` came from this allocator with `small`.
        unsafe { HEAP.dealloc(b, small) };
        assert_eq!(alloc_failure(), None, "nothing refused yet");
        let mut text = String::new();
        write_alloc_postmortem(&mut text, 1).unwrap();
        assert!(text.is_empty(), "no refusal, no words: {text}");

        let refused = Layout::from_size_align(8192, 8).unwrap();
        // SAFETY: a non-zero layout; the refusal is the point.
        assert!(unsafe { HEAP.alloc(refused) }.is_null());
        let fail = alloc_failure().expect("the refusal is recorded");
        let free_now = heap_stats().1;
        assert_eq!(fail.size, 8192);
        assert_eq!(fail.free as usize, free_now);
        assert_eq!(fail.phase_entry_free as usize, entry_free);
        assert_eq!(fail.phase_allocs, 2, "two blocks served in the phase");
        assert_eq!(fail.phase_alloc_bytes, 768);
        assert_eq!(fail.phase_freed_bytes, 256);
        assert_eq!(fail.phase_max_block, 512);
        assert!(
            fail.largest as usize <= free_now && fail.largest as usize + 64 >= free_now - 512,
            "the largest servable block is measured: {} of {free_now}",
            fail.largest
        );
        assert_eq!(heap_stats().1, free_now, "the probes are all returned");

        // Only the first refusal is kept.
        let other = Layout::from_size_align(16384, 8).unwrap();
        // SAFETY: as above.
        assert!(unsafe { HEAP.alloc(other) }.is_null());
        assert_eq!(alloc_failure().unwrap().size, 8192);

        let mut text = String::new();
        write_alloc_postmortem(&mut text, 5_000).unwrap();
        assert!(text.starts_with("alloc size=8192 free="), "{text}");
        assert!(text.ends_with(" census=none"), "{text}");

        let mut words = [0usize; CENSUS_KEYS.len()];
        for (i, word) in words.iter_mut().enumerate() {
            *word = 100 + i;
        }
        note_census(words, 3_000);
        let mut text = String::new();
        write_alloc_postmortem(&mut text, 5_000).unwrap();
        assert!(
            text.contains(" census_age_ms=2000 used=100 free=101 largest=102 "),
            "{text}"
        );
        assert!(text.ends_with(" other=115"), "{text}");

        // SAFETY: `a` came from this allocator with `big`.
        unsafe { HEAP.dealloc(a, big) };
    }
}
