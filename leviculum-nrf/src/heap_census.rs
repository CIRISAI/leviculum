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
//!   ble_sessions=… ble_s0=… ble_s1=… ble_s2=… ble_s3=…
//!   pn_peers=… n_peers=… pn_work=… pn_batch=… pn_out=… pn_links=…
//!   pn_role=… pn_flush=… other=…
//! ```
//!
//! (one line on the wire; wrapped here for the page).
//!
//! `links=`/`n_links=` walk the RETICULUM link table; a BLE peer that
//! holds only a GATT connection appears in `ble_sessions=` (total, with
//! `ble_s<slot>=` per drain slot) — the per-connection owner the field
//! boards' #388 panics grew in while `n_links=` stayed 0.
//!
//! `other` is the
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

/// Fragmentation reserve in the boot heap budget (#388): heap the
/// budget leaves unclaimed so the allocator can still serve mid-sized
/// blocks when the free list is cut up. The number to reason from is
/// the census's `largest=`: at the rig baseline (56 K used) the loss
/// was 41 B (`free=41632 largest=41591`), while the field failure
/// refused 340 B with 4 760 free — fragmentation loss grows with fill,
/// so the reserve is sized at roughly the largest single event-path
/// allocation (an MTU packet plus its copies) times a churn factor,
/// not at the idle loss. Re-derive from `largest=` under two-phone
/// load when the field census arrives.
const FRAG_RESERVE_BYTES: usize = 6 * 1024;

/// One 11-slot B-tree node of the boxed link table, amortised per link
/// in the budget's `per_link=` term (the value is a `Box` pointer since
/// #388; the `Link` blocks are counted separately).
const LINK_MAP_BYTES_PER_LINK: usize = 64;

/// The budget's `per_link=` term: what EVERY endpoint link costs
/// regardless of carrier — one boxed `Link` plus its link-table node
/// share. A link's BLE session (queue, pump, defragmenter) exists only
/// for links riding a GATT connection and is budgeted separately, once
/// per claimable session ([`crate::ble::MAX_LINKS`] ×
/// [`crate::ble::SESSION_BUDGET_BYTES`]) — a LoRa-backed link has no BLE
/// session. `const fn`: the binaries also assert the whole sum at
/// compile time against their concrete `NodeCore`.
pub const fn budget_per_link() -> usize {
    core::mem::size_of::<leviculum_core::link::Link>() + LINK_MAP_BYTES_PER_LINK
}

/// The budget's `reserve=` term (fragmentation + shared BLE channels +
/// one incoming resource at the binaries' cap).
pub const fn budget_reserve() -> usize {
    FRAG_RESERVE_BYTES + crate::ble::CHANNEL_BUDGET_BYTES + crate::MAX_INCOMING_RESOURCE_BYTES
}

/// How many endpoint links a node box of `node_box` bytes affords — the
/// `links=` term of the boot line, the value the binaries hand to
/// `NodeCoreBuilder::max_links`, and the count [`budget_total`] sums, so
/// the enforced cap and the budget cannot drift (#388 pass 3).
///
/// Derivation: everything that is not a per-link cost is fixed —
/// `node_box`, the role, the reserve, and the
/// [`crate::ble::MAX_LINKS`] BLE sessions (budgeted whether or not a
/// link currently rides them, because a claimable GATT connection can
/// fill its queues). What remains of [`crate::HEAP_SIZE`] is divided by
/// [`budget_per_link`], the carrier-independent cost of one more link.
/// With today's numbers (T114: node box 30 984 B, role 21 120 B,
/// reserve 18 848 B, sessions 4 × 3 948 B, per-link 2 664 B) the
/// division yields 4 — the heap affords exactly the BLE-session count,
/// and no extra LoRa-backed links until a fixed term shrinks. The
/// binaries assert `>=` [`crate::ble::MAX_LINKS`]: a node box grown past
/// that line is a budget violation at compile time, not a dead session
/// in the field.
pub const fn max_endpoint_links(node_box: usize) -> usize {
    let fixed = node_box
        + crate::pn::ROLE_BUDGET_BYTES
        + budget_reserve()
        + crate::ble::MAX_LINKS * crate::ble::SESSION_BUDGET_BYTES;
    if fixed >= crate::HEAP_SIZE {
        0
    } else {
        (crate::HEAP_SIZE - fixed) / budget_per_link()
    }
}

/// The budget's `total=` for a node box of `node_box` bytes — the one
/// sum both the boot line and the binaries' compile-time assertions
/// use, so they cannot drift. `<=` [`crate::HEAP_SIZE`] holds by
/// construction of [`max_endpoint_links`]; the assertions keep it as a
/// belt against a future edit decoupling the two.
pub const fn budget_total(node_box: usize) -> usize {
    node_box
        + max_endpoint_links(node_box) * budget_per_link()
        + crate::ble::MAX_LINKS * crate::ble::SESSION_BUDGET_BYTES
        + crate::pn::ROLE_BUDGET_BYTES
        + budget_reserve()
}

/// Emit the boot `HEAP_BUDGET` line and refuse to run a configuration
/// whose worst case does not fit the heap (#388 step 4).
///
/// ```text
/// HEAP_BUDGET links=<max> ble_links=<m> per_link=<b> ble_session=<b>
///   role=<b> reserve=<b> total=<b>
/// ```
///
/// (one line on the wire; wrapped here for the page).
///
/// * `links` — [`max_endpoint_links`], the ENFORCED cap on the
///   Reticulum link table (`NodeCoreBuilder::max_links`, #388 pass 3);
/// * `ble_links` — [`crate::ble::MAX_LINKS`], every claimable BLE
///   session, budgeted at [`crate::ble::SESSION_BUDGET_BYTES`]
///   (`ble_session=`) each on top of the carrier-independent
///   `per_link=` (one boxed `Link` + the link table's node share);
/// * `role` — [`crate::pn::ROLE_BUDGET_BYTES`], the `pn_*` worst case
///   (arithmetic at its definition and in [`crate::pn`]'s module doc);
///   budgeted whether or not the role is enabled this boot, because a
///   budget that only fits with the role off is a config refusal
///   deferred to `--set-pn on`;
/// * `reserve` — [`FRAG_RESERVE_BYTES`] + the shared node-facing BLE
///   channels + one incoming resource at the binaries' cap.
///
/// `node_box` is passed by the binary (`size_of_val` of its concrete
/// boxed `NodeCore`) and is inside `total=`. Asserted: `total ≤`
/// [`crate::HEAP_SIZE`] and `links ≥ ble_links` — a violation panics at
/// boot, lands in the post-mortem, and names the arithmetic instead of
/// failing as a 340-byte allocation three days into a field run.
pub fn log_budget_and_assert(node_box: usize) {
    let links = max_endpoint_links(node_box);
    let ble_links = crate::ble::MAX_LINKS;
    let per_link = budget_per_link();
    let ble_session = crate::ble::SESSION_BUDGET_BYTES;
    let role = crate::pn::ROLE_BUDGET_BYTES;
    let reserve = budget_reserve();
    let total = budget_total(node_box);
    crate::log::log_fmt_critical(
        "[HEAP] ",
        format_args!(
            "HEAP_BUDGET links={} ble_links={} per_link={} ble_session={} role={} reserve={} total={}",
            links, ble_links, per_link, ble_session, role, reserve, total
        ),
    );
    assert!(
        total <= crate::HEAP_SIZE,
        "HEAP_BUDGET total {total} exceeds the {} B heap: shrink a term \
         (node box, per-link, role, reserve) before shipping this configuration",
        crate::HEAP_SIZE
    );
    assert!(
        links >= ble_links,
        "HEAP_BUDGET affords only {links} endpoint links, below the {ble_links} \
         claimable BLE sessions: shrink a fixed term before shipping this configuration",
    );
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
    // The BLE sessions' own holdings (#388 pass 2): the packets queued
    // on the per-link queues plus what sits in the two node-facing
    // channels. `links=`/`n_links=` above walk the RETICULUM link
    // table, which is empty while a BLE peer merely holds a GATT
    // connection — this owner is the one that grows with a second
    // phone, and before it that growth hid in `other=`.
    let (ble_slots, ble_chan) = crate::ble::session_census();
    let ble_sessions = ble_slots.iter().sum::<usize>() + ble_chan;
    // The per-slot keys below spell out exactly MAX_LINKS values.
    const _: () = assert!(crate::ble::MAX_LINKS == 4);
    let pn = engine
        .map(crate::pn::Engine::heap_census)
        .unwrap_or_default();
    let attributed = census.total() + ble + ble_sessions + pn.total();
    let other = used_now.max(used).saturating_sub(attributed);
    crate::log::log_fmt(
        "[HEAP_CENSUS] ",
        format_args!(
            "used={} free={} largest={} node_box={} links={} n_links={} res={} events={} req={} dest={} transport={} storage={} ble_defrag={} ble_sessions={} ble_s0={} ble_s1={} ble_s2={} ble_s3={} pn_peers={} n_peers={} pn_work={} pn_batch={} pn_out={} pn_links={} pn_role={} pn_flush={} other={}",
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
            ble_sessions,
            ble_slots[0],
            ble_slots[1],
            ble_slots[2],
            ble_slots[3],
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
