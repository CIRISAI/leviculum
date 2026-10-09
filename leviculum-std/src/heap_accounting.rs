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
//! Changing the allocator is a separate, deliberate experiment (508): the
//! `alloc-mimalloc` lnsd puts [`crate::heap_accounting::CountingOver`] in
//! front of mimalloc, the same counting over a different allocator, so its
//! census compares with this one's.
//!
//! # Users
//!
//! `heap-gap-bench` installs it always; `lnsd` and `lnpnd` install it under
//! their default-off `heap-accounting` features, for a field node that
//! reports measured live bytes instead of an estimate. The same feature
//! carries the opt-in call-site sampler (`LEVICULUM_HEAP_SAMPLE`, see
//! `sample`), and the two pieces both daemons print from it,
//! `configure_sampler_from_env` and `measured_live_line`, live here so
//! the two dumps cannot drift apart in shape (550).

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

/// The same shim in front of another allocator, for a build that replaces
/// the system allocator and still wants the measured balance (508: the
/// `alloc-mimalloc` lnsd). A separate type rather than a type parameter on
/// [`CountingAllocator`], so the symbol names the massif recipe in
/// `heap-gap-bench` passes as `--alloc-fn` stay what they are.
pub struct CountingOver<A>(pub A);

/// One body for both shims: forward with the layout the call arrived with,
/// count what came back.
macro_rules! counting_global_alloc {
    () => {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let p = self.inner().alloc(layout);
            if !p.is_null() {
                record(layout.size());
            }
            p
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            // Forwarded rather than left to the default (alloc + memset): a
            // large zeroed block is a fresh mmap in mallocng and already zero,
            // and rewriting it by hand would touch pages production does not.
            let p = self.inner().alloc_zeroed(layout);
            if !p.is_null() {
                record(layout.size());
            }
            p
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let p = self.inner().realloc(ptr, layout, new_size);
            if !p.is_null() {
                LIVE.fetch_add(new_size.wrapping_sub(layout.size()), Ordering::Relaxed);
                TOTAL.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
                let n = ALLOCS.fetch_add(1, Ordering::Relaxed);
                maybe_sample(n, new_size);
            }
            p
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            self.inner().dealloc(ptr, layout);
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
    };
}

impl CountingAllocator {
    #[inline(always)]
    fn inner(&self) -> &'static System {
        &System
    }
}

impl<A> CountingOver<A> {
    #[inline(always)]
    fn inner(&self) -> &A {
        &self.0
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    counting_global_alloc!();
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingOver<A> {
    counting_global_alloc!();
}

/// `LIVE` is only ever read when the true balance is non-negative, so the
/// wrapping subtraction in `dealloc` and `realloc` costs nothing and saves
/// a signed type.
fn record(size: usize) {
    LIVE.fetch_add(size, Ordering::Relaxed);
    TOTAL.fetch_add(size, Ordering::Relaxed);
    let n = ALLOCS.fetch_add(1, Ordering::Relaxed);
    maybe_sample(n, size);
}

/// The sampler's hook: one relaxed load and a branch when sampling is off,
/// which is the default and the only state a build without
/// `heap-accounting` can be in.
#[inline(always)]
fn maybe_sample(n: usize, size: usize) {
    #[cfg(feature = "heap-accounting")]
    {
        let every = sample::EVERY.load(Ordering::Relaxed);
        if every != 0 && n.is_multiple_of(every) {
            sample::take(size);
        }
    }
    #[cfg(not(feature = "heap-accounting"))]
    let _ = (n, size);
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

/// Opt-in call-site sampling (508): which code asks the allocator so often.
///
/// The balance above says how much is live; on the soak node it also said
/// 331 million allocations in 3.5 hours, about 26 000 a second, and the
/// question that leaves is who makes them. `LEVICULUM_HEAP_SAMPLE=<n>` makes
/// every n-th counted allocation walk its own stack and file the return
/// addresses in a fixed table; the SIGUSR1 census then names the frames and
/// prints the busiest sites by count and by bytes.
///
/// Off by default, and off costs one relaxed load and a branch per
/// allocation ([`maybe_sample`]). On, the walk happens under the table lock
/// and allocates nothing of its own: the stack goes into a fixed array, the
/// table is a static, and an allocation the unwinder itself makes while the
/// walk runs is counted but not sampled (the per-thread `BUSY` flag). The
/// table never grows, so turning the sampler on does not move the resident
/// set it is used to explain; a stack that finds no free slot is counted in
/// `dropped`. Naming the frames allocates, and happens only at census time,
/// with the census thread's own allocations kept out of the table.
#[cfg(feature = "heap-accounting")]
mod sample {
    use core::cell::Cell;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::collections::HashMap;
    use std::fmt::Write as _;
    use std::sync::Mutex;

    /// Sample every n-th allocation; 0 is off.
    pub(super) static EVERY: AtomicUsize = AtomicUsize::new(0);
    static SAMPLES: AtomicUsize = AtomicUsize::new(0);
    static DROPPED: AtomicUsize = AtomicUsize::new(0);

    /// Return addresses kept per sample: the allocator plumbing takes the
    /// first eight to twelve, and the census wants two of ours after it.
    const DEPTH: usize = 32;
    const SLOTS: usize = 4096;
    const PROBE: usize = 64;

    #[derive(Clone, Copy)]
    struct Slot {
        frames: [usize; DEPTH],
        len: usize,
        count: u64,
        bytes: u64,
    }

    const EMPTY: Slot = Slot {
        frames: [0; DEPTH],
        len: 0,
        count: 0,
        bytes: 0,
    };

    static TABLE: Mutex<[Slot; SLOTS]> = Mutex::new([EMPTY; SLOTS]);

    thread_local! {
        static BUSY: Cell<bool> = const { Cell::new(false) };
    }

    /// Enter the per-thread guard; false when this thread is already inside
    /// the sampler or the census, or has no thread-locals left.
    fn enter() -> bool {
        BUSY.try_with(|b| !b.replace(true)).unwrap_or(false)
    }

    fn leave() {
        let _ = BUSY.try_with(|b| b.set(false));
    }

    pub(super) fn take(size: usize) {
        if !enter() {
            return;
        }
        let mut table = TABLE.lock().unwrap_or_else(|e| e.into_inner());
        let mut frames = [0usize; DEPTH];
        let mut len = 0;
        // SAFETY: the table lock serialises every walk this module makes,
        // which is the synchronisation `trace_unsynchronized` leaves to its
        // caller; the closure only writes into the local array.
        unsafe {
            backtrace::trace_unsynchronized(|frame| {
                frames[len] = frame.ip() as usize;
                len += 1;
                len < DEPTH
            });
        }
        SAMPLES.fetch_add(1, Ordering::Relaxed);
        let start = hash(&frames[..len]) % SLOTS;
        let mut filed = false;
        for i in 0..PROBE {
            let slot = &mut table[(start + i) % SLOTS];
            if slot.count == 0 {
                slot.frames = frames;
                slot.len = len;
            } else if slot.len != len || slot.frames[..len] != frames[..len] {
                continue;
            }
            slot.count += 1;
            slot.bytes += size as u64;
            filed = true;
            break;
        }
        if !filed {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
        drop(table);
        leave();
    }

    /// FNV-1a over the addresses.
    fn hash(frames: &[usize]) -> usize {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for f in frames {
            h ^= *f as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h as usize
    }

    /// Frames that are the allocator's machinery or a generic container's
    /// growth, never the code that decided to allocate. The census names a
    /// site by the first frame that is not one of these.
    pub(super) fn is_plumbing(name: &str) -> bool {
        let n = name.trim_start_matches('<');
        const PREFIXES: [&str; 16] = [
            "alloc::",
            "core::",
            "std::",
            "hashbrown::",
            "bytes::",
            "backtrace::",
            "__rust",
            "__rdl",
            "__rg_",
            "_Unwind",
            "unw_",
            "libunwind::",
            "leviculum_std::heap_accounting::sample",
            "leviculum_std::heap_accounting::record",
            "leviculum_std::heap_accounting::maybe_sample",
            "leviculum_std::heap_accounting::Counting",
        ];
        n.is_empty() || PREFIXES.iter().any(|p| n.starts_with(p))
    }

    /// The site of one stack, innermost first: the first two frames of
    /// ours, joined by ` < ` (callee < caller), and in brackets the last
    /// plumbing frame before them, which says what kind of allocation it
    /// was (a `Vec` growing, an `Arc`, a `Box`).
    pub(super) fn site_label(names: &[&str]) -> String {
        let mut via = "";
        let mut own: Vec<&str> = Vec::new();
        for name in names {
            if own.is_empty() && is_plumbing(name) {
                if !name.is_empty() {
                    via = name;
                }
                continue;
            }
            if !is_plumbing(name) {
                own.push(name);
                if own.len() == 2 {
                    break;
                }
            }
        }
        let via = without_generics(via);
        if own.is_empty() {
            return format!("(no own frame) [{via}]");
        }
        let own: Vec<String> = own.iter().map(|n| without_generics(n)).collect();
        format!("{} [{via}]", own.join(" < "))
    }

    /// The label of a stack none of whose frames has a name: its return
    /// addresses, innermost first.
    pub(super) fn unresolved_label(frames: &[usize]) -> String {
        let addrs: Vec<String> = frames.iter().map(|ip| format!("{ip:#x}")).collect();
        format!("(unresolved) {}", addrs.join(" "))
    }

    /// Drop the generic arguments from a demangled name, which in this
    /// codebase are the bulk of it (`Transport<SystemClock, Storage>` on
    /// every core frame) and say nothing about the site. A `<` that opens
    /// a qualified path (`<X as Trait>::f`) follows no identifier and stays.
    pub(super) fn without_generics(name: &str) -> String {
        let mut out = String::with_capacity(name.len());
        let mut depth = 0usize;
        let mut prev = ' ';
        for c in name.chars() {
            if depth > 0 {
                match c {
                    '<' => depth += 1,
                    '>' => depth -= 1,
                    _ => {}
                }
                continue;
            }
            if c == '<' && (prev.is_alphanumeric() || prev == '_') {
                depth = 1;
                continue;
            }
            out.push(c);
            prev = c;
        }
        out
    }

    fn name_of(ip: usize, first: bool) -> String {
        // A return address points past its call; one byte back lands in
        // the calling function even when the call was its last instruction.
        let addr = if first { ip } else { ip.saturating_sub(1) };
        let mut out = String::new();
        backtrace::resolve(addr as *mut core::ffi::c_void, |symbol| {
            if out.is_empty() {
                if let Some(name) = symbol.name() {
                    out = format!("{name:#}");
                }
            }
        });
        out
    }

    pub(super) fn report(top: usize) -> Option<String> {
        let every = EVERY.load(Ordering::Relaxed);
        if every == 0 {
            return None;
        }
        // The census allocates while it copies and names; with the guard
        // held none of that is filed, and the copy cannot try to take the
        // table lock it is already holding.
        let entered = enter();
        let stacks: Vec<Slot> = {
            let table = TABLE.lock().unwrap_or_else(|e| e.into_inner());
            table.iter().filter(|s| s.count > 0).copied().collect()
        };
        let mut names: HashMap<usize, String> = HashMap::new();
        let mut sites: HashMap<String, (u64, u64)> = HashMap::new();
        for stack in &stacks {
            for (i, ip) in stack.frames[..stack.len].iter().enumerate() {
                names.entry(*ip).or_insert_with(|| name_of(*ip, i == 0));
            }
            let resolved: Vec<&str> = stack.frames[..stack.len]
                .iter()
                .map(|ip| names.get(ip).map_or("", String::as_str))
                .collect();
            // A binary without a symbol table (the arm64 zigbuild packages:
            // zig turns `strip = "debuginfo"` into a full strip) names no
            // frame at all, and every stack would fold into one empty site.
            // The addresses still identify it, and the non-PIE arm64
            // binary loads where it was linked, so they resolve offline with
            // `addr2line -f -e` against an unstripped build of the commit.
            let label = if resolved.iter().all(|n| n.is_empty()) {
                unresolved_label(&stack.frames[..stack.len])
            } else {
                site_label(&resolved)
            };
            let entry = sites.entry(label).or_insert((0, 0));
            entry.0 += stack.count;
            entry.1 += stack.bytes;
        }
        let mut out = String::new();
        let _ = writeln!(
            out,
            "=== Heap sample: every {every}th allocation, {} samples, {} stacks, {} sites, {} dropped ===",
            SAMPLES.load(Ordering::Relaxed),
            stacks.len(),
            sites.len(),
            DROPPED.load(Ordering::Relaxed),
        );
        let mut ranked: Vec<(&String, &(u64, u64))> = sites.iter().collect();
        for (key, by) in [("count", 0usize), ("bytes", 1usize)] {
            ranked.sort_by(|a, b| {
                let (ka, kb) = if by == 0 {
                    (a.1 .0, b.1 .0)
                } else {
                    (a.1 .1, b.1 .1)
                };
                kb.cmp(&ka).then_with(|| a.0.cmp(b.0))
            });
            for (i, (label, (count, bytes))) in ranked.iter().take(top).enumerate() {
                let _ = writeln!(
                    out,
                    "sample by {key} #{}: {count} samples (~{} allocations), {bytes} bytes sampled (~{} bytes) {label}",
                    i + 1,
                    count * every as u64,
                    bytes * every as u64,
                );
            }
        }
        if entered {
            leave();
        }
        Some(out)
    }
}

/// Turn the call-site sampler on (every `n`-th allocation) or off (0).
/// [`configure_sampler_from_env`] calls it once at startup.
#[cfg(feature = "heap-accounting")]
pub fn set_sample_every(n: usize) {
    sample::EVERY.store(n, Ordering::Relaxed);
}

/// The sampler's top `top` sites by count and by bytes, for the census;
/// `None` while the sampler is off.
#[cfg(feature = "heap-accounting")]
pub fn sample_report(top: usize) -> Option<String> {
    sample::report(top)
}

/// How many sites the sampler's census lists, by count and by bytes.
#[cfg(feature = "heap-accounting")]
pub const SAMPLE_TOP: usize = 20;

/// Read `LEVICULUM_HEAP_SAMPLE` once: a positive n samples every n-th
/// allocation for the census's call-site table, anything else leaves the
/// sampler off. Said in the log either way it was asked for, so a census
/// without the table is not mistaken for one with nothing in it.
#[cfg(feature = "heap-accounting")]
pub fn configure_sampler_from_env() {
    let Some(raw) = std::env::var_os("LEVICULUM_HEAP_SAMPLE") else {
        return;
    };
    match raw.to_str().and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(n) if n > 0 => {
            set_sample_every(n);
            tracing::info!("Heap sampling: every {n}th allocation records its call site (SIGUSR1 lists the top {SAMPLE_TOP})");
        }
        _ => tracing::warn!(
            "LEVICULUM_HEAP_SAMPLE={raw:?} is not a positive integer; heap sampling stays off"
        ),
    }
}

/// The line that rides beside the dump's `=== Total estimated: ... ===`, so
/// the model and the measurement are read together and the model stays
/// checkable (it is only knowable that it over-reports by 1.92x because both
/// were printed), followed by the sampler's table when the sampler is on.
///
/// `allocator` is named by the caller, because the census's Allocator
/// section reads the libc and cannot tell that the libc's allocator is not
/// the one in use; only the binary that installs the global allocator knows.
#[cfg(feature = "heap-accounting")]
pub fn measured_live_line(allocator: &str) -> String {
    let mut line = format!(
        "=== Measured live: {} bytes ({} allocations, {} bytes handed out, allocator {allocator}) ===\n",
        live_bytes(),
        allocation_count(),
        total_bytes(),
    );
    if let Some(report) = sample_report(SAMPLE_TOP) {
        line.push_str(&report);
    }
    line
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

    /// The site is the first frame that is not allocator machinery, with
    /// its caller, and the machinery frame just below it says what kind of
    /// allocation it was.
    #[cfg(feature = "heap-accounting")]
    #[test]
    fn site_label_skips_the_allocator_plumbing() {
        let stack = [
            "leviculum_std::heap_accounting::sample::take",
            "<leviculum_std::heap_accounting::CountingAllocator as core::alloc::global::GlobalAlloc>::alloc",
            "__rustc::__rust_alloc",
            "alloc::raw_vec::finish_grow",
            "alloc::raw_vec::RawVecInner<A>::reserve::do_reserve_and_handle",
            "leviculum_std::interfaces::hdlc::Deframer::push",
            "",
            "leviculum_std::interfaces::tcp::read_loop::{{closure}}",
            "tokio::runtime::task::harness::poll_future",
        ];
        assert_eq!(
            sample::site_label(&stack),
            "leviculum_std::interfaces::hdlc::Deframer::push < \
             leviculum_std::interfaces::tcp::read_loop::{{closure}} \
             [alloc::raw_vec::RawVecInner::reserve::do_reserve_and_handle]"
        );
        assert_eq!(
            sample::site_label(&["alloc::alloc::alloc", "std::thread::spawn"]),
            "(no own frame) [std::thread::spawn]"
        );
        assert_eq!(
            sample::unresolved_label(&[0x40_1000, 0x40_2abc]),
            "(unresolved) 0x401000 0x402abc"
        );
        // Generic arguments go, qualified-path brackets stay.
        assert_eq!(
            sample::site_label(&[
                "<alloc::raw_vec::RawVec<leviculum_core::transport::Action>>::grow_one",
                "<leviculum_core::transport::Transport<leviculum_std::clock::SystemClock, \
                 leviculum_std::storage::Storage>>::push_packet",
            ]),
            "<leviculum_core::transport::Transport>::push_packet [<alloc::raw_vec::RawVec>::grow_one]"
        );
    }

    #[cfg(feature = "heap-accounting")]
    #[inline(never)]
    fn sampled_allocation_site() {
        sample::take(std::hint::black_box(24));
    }

    /// End to end on this process: the walk and the naming find the
    /// function the sample was taken in. It calls the sampler directly and
    /// not through the shim, because the shim's counters are the statics
    /// the counting test above asserts deltas on, and a second test moving
    /// them in parallel would fail it on a scheduler decision.
    #[cfg(feature = "heap-accounting")]
    #[test]
    fn sampler_names_the_calling_function() {
        assert!(sample_report(20).is_none(), "off by default");
        set_sample_every(1);
        for _ in 0..5 {
            sampled_allocation_site();
        }
        let report = sample_report(20).expect("sampler is on");
        set_sample_every(0);
        let line = report
            .lines()
            .find(|l| l.starts_with("sample by count") && l.contains("sampled_allocation_site"))
            .unwrap_or_else(|| panic!("no line names the site:\n{report}"));
        assert!(line.contains("bytes sampled"), "{line}");
        assert!(
            report.starts_with("=== Heap sample: every 1th allocation"),
            "{report}"
        );
    }
}
