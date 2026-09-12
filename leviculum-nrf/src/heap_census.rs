//! Periodic `[HEAP_CENSUS]` line on the debug CDC (Codeberg #388).
//!
//! The `[HEAP]` line says how much of the 96 KiB heap is gone; this one
//! says who holds it. Two phones plus the propagation role put the field
//! T114 at 93 KiB and a 340-byte allocation failed — with the census the
//! next such capture names the holder instead of inviting a guess.
//!
//! Method (instruction step 1): **walkers at the allocation owners, on
//! demand** — `NodeCore::heap_census`, `pn::Engine::heap_census`, the
//! BLE defragmenter mirrors — not a global allocator hook, because
//! `embedded-alloc`'s `LlffHeap` carries no per-allocation tag to hand
//! a block back to its owner, and a tagging wrapper would spend heap to
//! measure heap. The estimation model (BTree nodes, spine capacities)
//! is documented at [`leviculum_core::heap_census`].
//!
//! `LlffHeap` also reports no fragmentation figure, so `largest=` is
//! measured directly: a binary-search probe allocation (alloc, check,
//! free) finds the largest single block the allocator can still serve.
//! `free − largest` is the fragmentation loss; the field failure — 340
//! bytes refused with 4 760 free — is exactly the case this
//! distinguishes. The probe runs inside one executor poll (no await
//! between alloc and free), so no task ever sees the transient claim.
//!
//! ```text
//! [HEAP_CENSUS] used=… free=… largest=… node_box=… links=… n_links=…
//!   res=… events=… req=… dest=… transport=… storage=… ble_defrag=…
//!   pn_peers=… n_peers=… pn_work=… pn_batch=… pn_out=… pn_links=…
//!   pn_role=… pn_flush=… other=…
//! ```
//! (one line on the wire; wrapped here for the page). `other` is the
//! honesty term: `used` minus everything the census attributes — the
//! log ring's boxes, packets in the static channels, anything not yet
//! walked. A growing `other` means the census is missing an owner, not
//! that the heap is fine.
//!
//! Cadence: every [`PERIOD`] (4× the `[HEAP]` line's 30 s), riding the
//! main loop the way `[TRANSPORT]` does — the census walks state only
//! the loop owns. On demand: the byte `c` on the debug CDC port
//! ([`request`]) makes the next loop wake-up emit one, which the
//! `[TRANSPORT]`-clamped select reaches within 30 s.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_time::{Duration, Instant};
use leviculum_core::node::NodeCore;
use leviculum_core::traits::{Clock, Storage};
use rand_core::CryptoRngCore;

/// How often the census line is emitted unasked. Coarser than `[HEAP]`
/// (30 s) on purpose: the census is O(state) walks plus a ~12-probe
/// allocation search, and the trajectory the post-mortem needs is
/// carried by `[HEAP]` — the census names the holders along it.
pub const PERIOD: Duration = Duration::from_secs(120);

/// One-shot request flag, set by the debug-port byte `c`.
static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Ask the main loop for a census at its next wake-up (debug port `c`).
pub fn request() {
    REQUESTED.store(true, Ordering::Relaxed);
}

/// Emission schedule for the `[HEAP_CENSUS]` line. Not a spawned task
/// for the same reason `[TRANSPORT]`'s is not: the state being walked
/// lives in the `NodeCore` and the engine, which the main loop owns
/// exclusively.
pub struct Ticker {
    next: Instant,
}

impl Ticker {
    /// Arm the first emission one [`PERIOD`] from now.
    pub fn new() -> Self {
        Self {
            next: Instant::now() + PERIOD,
        }
    }

    /// The instant the loop must wake by for the next line to be on
    /// time. Fold into the select deadline with `.min()`.
    pub fn deadline(&self) -> Instant {
        self.next
    }

    /// Emit the line if it is due (or asked for via [`request`]), and
    /// re-arm. Cheap on the common path: one `Instant` comparison and
    /// one relaxed load.
    pub fn poll<R, C, S>(&mut self, node: &NodeCore<R, C, S>, engine: Option<&crate::pn::Engine>)
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let now = Instant::now();
        if now < self.next && !REQUESTED.swap(false, Ordering::Relaxed) {
            return;
        }
        // Re-arm from now, as [TRANSPORT] does: no catch-up bursts.
        self.next = now + PERIOD;
        log(node, engine);
    }
}

impl Default for Ticker {
    fn default() -> Self {
        Self::new()
    }
}

/// Largest single allocation the heap can currently serve, by binary
/// search with real probe allocations. 64-byte resolution: ~12 probes
/// against a 96 KiB pool, each an O(free-list) walk. Runs without an
/// await between alloc and free, so the claim is invisible to every
/// other task on the executor.
fn largest_free_block() -> usize {
    use alloc::alloc::{alloc, dealloc, Layout};
    let (_, free) = crate::heap_stats();
    let mut lo = 0usize;
    let mut hi = free;
    while hi.saturating_sub(lo) > 64 {
        let mid = lo + (hi - lo) / 2;
        let Ok(layout) = Layout::from_size_align(mid, 4) else {
            break;
        };
        // SAFETY: layout is non-zero (mid > 64 > 0); a null return is
        // the allocator's refusal, checked before any use.
        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            hi = mid;
        } else {
            // SAFETY: ptr came from alloc with this exact layout.
            unsafe { dealloc(ptr, layout) };
            lo = mid;
        }
    }
    lo
}

/// Format one `[HEAP_CENSUS]` line from a fresh walk of the owners.
pub fn log<R, C, S>(node: &NodeCore<R, C, S>, engine: Option<&crate::pn::Engine>)
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    let (used, _free, _watermark) = crate::heap_watermark_sample();
    let largest = largest_free_block();
    // Re-read free after the probe: the probe cannot change it (every
    // claim is released), but the paired used/free of ONE instant is
    // what the line should carry.
    let (used_now, free_now) = crate::heap_stats();
    let census = node.heap_census();
    let ble = crate::ble::defrag_held_bytes();
    let pn = engine
        .map(crate::pn::Engine::heap_census)
        .unwrap_or_default();
    let attributed = census.total() + ble + pn.total();
    let other = used_now.max(used).saturating_sub(attributed);
    crate::log::log_fmt(
        "[HEAP_CENSUS] ",
        format_args!(
            "used={} free={} largest={} node_box={} links={} n_links={} res={} events={} req={} dest={} transport={} storage={} ble_defrag={} pn_peers={} n_peers={} pn_work={} pn_batch={} pn_out={} pn_links={} pn_role={} pn_flush={} other={}",
            used_now,
            free_now,
            largest,
            census.node_struct,
            census.links,
            census.link_count,
            census.resources,
            census.events,
            census.requests,
            census.destinations,
            census.transport,
            census.storage,
            ble,
            pn.peers,
            pn.peer_count,
            pn.work,
            pn.batch,
            pn.outbound,
            pn.link_maps,
            pn.role,
            pn.flush,
            other,
        ),
    );
}
