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
//!   pn_role=… pn_flush=… held=… hold_refused=… pn_dial_held=… other=…
//! ```
//!
//! (one line on the wire; wrapped here for the page).
//!
//! With the propagation role running, a second line follows it:
//!
//! ```text
//! [HEAP_PEAK] offer_response=… inbound_resource=… upload=… store_tail=…
//!   role_headroom=…
//! ```
//!
//! The census is a steady state; the allocations that broke the field T114
//! are transients inside single role operations. This line carries the
//! largest each one has reached since boot, by the model the heap budget
//! crate pins on the host (`leviculum_heap_budget::peak`), and what the
//! role budget leaves for a transient beside its structural term.
//!
//! `links=`/`n_links=` walk the RETICULUM link table; a BLE peer that
//! holds only a GATT connection appears in `ble_sessions=` (total, with
//! `ble_s<slot>=` per drain slot) — the per-connection owner the field
//! boards' #388 panics grew in while `n_links=` stayed 0.
//!
//! `held=` is the count of announces the ingress burst limiter holds
//! across interfaces (their bytes are in `transport=`), capped per
//! interface at [`leviculum_heap_budget::BOARD_MAX_HELD_ANNOUNCES`], on
//! LoRa and BLE only: the USB serial interface runs no limiter (#606);
//! `hold_refused=` counts the
//! announces refused at that cap since boot (#598).
//!
//! `pn_dial_held=` counts the propagation role's peering dials held since
//! boot so a client's link request keeps a slot (order 1666,
//! [`leviculum_heap_budget::CLIENT_RESERVED_LINKS`]); each one is also a
//! `PN_SYNC … action=defer reason=client_pending|client_reserve` line.
//!
//! `other` is the
//! honesty term: `used` minus everything the census attributes — the
//! log ring's boxes, packets in the static channels, anything not yet
//! walked. A growing `other` means the census is missing an owner, not
//! that the heap is fine.
//!
//! Cadence: every [`PERIOD`](crate::heap_census::PERIOD) (4× the `[HEAP]`
//! line's 30 s), and out of turn, at most once a
//! [`LOW_PERIOD`](crate::heap_census::LOW_PERIOD), when free heap is below
//! [`LOW_FREE_BYTES`](crate::heap_census::LOW_FREE_BYTES), preceded by a
//! critical `[HEAP_LOW] free=… threshold=… phase=…` line; riding the
//! main loop the way `[TRANSPORT]` does — the census walks state only
//! the loop owns. On demand: the byte `c` on the debug CDC port
//! ([`request`](crate::heap_census::request)) makes the next loop wake-up
//! emit one, which the
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

/// Free heap below which the census is emitted out of turn (order 1641).
/// The periodic line comes every two minutes, and on 2026-11-10 the T114's
/// last census before each of four `memory allocation ... failed` panics
/// showed 16 to 22 KB free and was up to two minutes old: the census that
/// names the holder of the heap at the moment it ran out was never taken.
/// 24 KiB is above every one of those readings, so a board on its way to
/// that failure crosses it first.
pub const LOW_FREE_BYTES: usize = 24 * 1024;

/// At most one low-heap census per this period: a board hovering at the
/// threshold must not spend its loop walking its own tables.
pub const LOW_PERIOD: Duration = Duration::from_secs(60);

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
    /// The earliest instant a low-heap census may run again.
    low_next: Instant,
}

impl Ticker {
    /// Arm the first emission one [`PERIOD`] from now.
    pub fn new() -> Self {
        Self {
            next: Instant::now() + PERIOD,
            low_next: Instant::from_ticks(0),
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
        let (_, free) = leviculum_firmware::heap::heap_stats();
        let low = free < LOW_FREE_BYTES && now >= self.low_next;
        if now < self.next && !REQUESTED.swap(false, Ordering::Relaxed) && !low {
            return;
        }
        if low {
            self.low_next = now + LOW_PERIOD;
            // Critical, so it reaches the persistent tail the next boot
            // replays; the phase is the work thread mode did last, which is
            // the work that left the heap this low.
            crate::log::log_fmt_critical(
                "[HEAP_LOW] ",
                format_args!(
                    "free={} threshold={} phase={}",
                    free,
                    LOW_FREE_BYTES,
                    leviculum_firmware::thread_watch::current_phase().name()
                ),
            );
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

// The budget arithmetic lives in `leviculum-heap-budget`, where a host test
// reaches it (#388); this module keeps the census, which reads the allocator.
pub use leviculum_heap_budget::{
    budget_per_link, budget_reserve, budget_serve_cap, budget_slack, budget_total, live_serve_cap,
    max_endpoint_links, BOARD_MAX_HELD_ANNOUNCES, SERVE_MARGIN_BYTES,
};

// The crate pins the target's `Link` size because its tests run on a host,
// where `Link` is larger. Asserted here, on the target, so a change to
// `Link` fails this build until the pinned number moves with it.
const _: () = assert!(
    core::mem::size_of::<leviculum_core::link::Link>() == leviculum_heap_budget::LINK_BYTES
);
// The same for a `Vec` header, the spine term of every per-operation peak
// (`leviculum_heap_budget::peak`).
const _: () =
    assert!(core::mem::size_of::<alloc::vec::Vec<u8>>() == leviculum_heap_budget::peak::VEC_BYTES);
// The held-announce hold's two target terms (#598): the entry the core
// keeps per held announce, and the interfaces it may be held on. A change
// to either fails this build until the budget's hold arithmetic, and the
// cap the binaries take from it, move with it.
const _: () = assert!(
    leviculum_core::transport::HELD_ANNOUNCE_ENTRY_BYTES
        == leviculum_heap_budget::HELD_ANNOUNCE_ENTRY_BYTES
);
const _: () = assert!(
    leviculum_firmware::iface_bytes::IFACE_COUNT == leviculum_heap_budget::BOARD_INTERFACES
);
// The serial interface the budget counts out of the hold (#606) is the one
// `iface_bytes` registers as the USB link.
const _: () =
    assert!(!leviculum_heap_budget::BOARD_INGRESS_CONTROL[leviculum_firmware::iface_bytes::SERIAL]);

/// Emit the boot `HEAP_BUDGET` line and refuse to run a configuration
/// whose worst case does not fit the heap (#388 step 4).
///
/// ```text
/// HEAP_BUDGET links=<max> ble_links=<m> per_link=<b> ble_session=<b>
///   role=<b> reserve=<b> total=<b> slack=<b> serve=<b> serve_cap=<b>
/// ```
///
/// (one line on the wire; wrapped here for the page).
///
/// * `links` — [`max_endpoint_links`], the ENFORCED cap on the
///   Reticulum link table (`NodeCoreBuilder::max_links`, #388 pass 3);
/// * `ble_links` — [`leviculum_heap_budget::MAX_LINKS`], every claimable BLE
///   session, budgeted at [`leviculum_heap_budget::SESSION_BUDGET_BYTES`]
///   (`ble_session=`) each on top of the carrier-independent
///   `per_link=` (one boxed `Link` + the link table's node share);
/// * `role` — [`crate::pn::ROLE_BUDGET_BYTES`], the `pn_*` worst case
///   (arithmetic at its definition and in [`crate::pn`]'s module doc);
///   budgeted whether or not the role is enabled this boot, because a
///   budget that only fits with the role off is a config refusal
///   deferred to `--set-pn on`;
/// * `reserve` — `FRAG_RESERVE_BYTES` + the shared node-facing BLE
///   channels + one incoming resource at the binaries' cap;
/// * `slack` — [`budget_slack`], what the sum leaves unclaimed;
/// * `serve` — [`crate::pn::SERVE_PEAK_BYTES`], the peak transient one
///   outbound `/get` serve costs at the announced cap. It is NOT in
///   `total`, and when it exceeds `slack` a second critical line follows:
///
/// ```text
/// HEAP_BUDGET_UNFUNDED serve=<b> slack=<b> deficit=<b>
/// ```
///
///   This is the #384 finding, on the wire at every boot rather than in
///   a post-mortem: the board announces a serve limit its heap plan does
///   not fund, and a peer that collects its mail is what makes it fail.
/// * `serve_cap` — [`budget_serve_cap`], the largest fetch response the
///   plan DOES fund, and the bound the role serves to. The deficit line
///   says the announced cap is unaffordable; this one says what is
///   affordable instead, and a `serve_cap` below one stored message
///   (0 B at the T114's 64 B of slack, #661, #662) means the BOOT plan alone
///   would have the board list mail it will not serve. It is a floor:
///   [`live_serve_cap`] raises it to what the heap actually has at
///   each `/get`, and only a board whose live heap is also that tight
///   is held to this number.
///   It is reported and not asserted on purpose — the panic is a real
///   failure of a real feature, and a boot refusal would trade it for a
///   board that does not come up at all, which is a choice this line
///   exists to put in front of a person rather than make for them.
///
/// `node_box` is passed by the binary (`size_of_val` of its concrete
/// boxed `NodeCore`) and is inside `total=`. Asserted: `total ≤`
/// [`leviculum_heap_budget::HEAP_SIZE`] and `links ≥ ble_links` — a violation panics at
/// boot, lands in the post-mortem, and names the arithmetic instead of
/// failing as a 340-byte allocation three days into a field run.
pub fn log_budget_and_assert(node_box: usize) {
    let links = max_endpoint_links(node_box);
    let ble_links = leviculum_heap_budget::MAX_LINKS;
    let per_link = budget_per_link();
    let ble_session = leviculum_heap_budget::SESSION_BUDGET_BYTES;
    let role = crate::pn::ROLE_BUDGET_BYTES;
    let reserve = budget_reserve();
    let total = budget_total(node_box);
    let slack = budget_slack(node_box);
    let serve = crate::pn::SERVE_PEAK_BYTES;
    let serve_cap = budget_serve_cap(node_box);
    crate::log::log_fmt_critical(
        "[HEAP] ",
        format_args!(
            "HEAP_BUDGET links={} ble_links={} per_link={} ble_session={} role={} reserve={} total={} slack={} serve={} serve_cap={}",
            links, ble_links, per_link, ble_session, role, reserve, total, slack, serve, serve_cap
        ),
    );
    if serve > slack {
        crate::log::log_fmt_critical(
            "[HEAP] ",
            format_args!(
                "HEAP_BUDGET_UNFUNDED serve={} slack={} deficit={}",
                serve,
                slack,
                serve - slack
            ),
        );
    }
    assert!(
        total <= leviculum_heap_budget::HEAP_SIZE,
        "HEAP_BUDGET total {total} exceeds the {} B heap: shrink a term \
         (node box, per-link, role, reserve) before shipping this configuration",
        leviculum_heap_budget::HEAP_SIZE
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
pub fn largest_free_block() -> usize {
    use alloc::alloc::{alloc, dealloc, Layout};
    let (_, free) = leviculum_firmware::heap::heap_stats();
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
    let _phase = leviculum_firmware::thread_watch::phase_scope(
        leviculum_firmware::thread_watch::Phase::Census,
    );
    let (used, _free, _watermark) = leviculum_firmware::heap::heap_watermark_sample();
    let largest = largest_free_block();
    // Re-read free after the probe: the probe cannot change it (every
    // claim is released), but the paired used/free of ONE instant is
    // what the line should carry.
    let (used_now, free_now) = leviculum_firmware::heap::heap_stats();
    let census = node.heap_census();
    let ble = leviculum_firmware::ble_link::defrag_held_bytes();
    // The BLE sessions' own holdings (#388 pass 2): the packets queued
    // on the per-link queues plus what sits in the two node-facing
    // channels. `links=`/`n_links=` above walk the RETICULUM link
    // table, which is empty while a BLE peer merely holds a GATT
    // connection — this owner is the one that grows with a second
    // phone, and before it that growth hid in `other=`.
    let (ble_slots, ble_chan) = leviculum_firmware::ble_link::session_census();
    let ble_sessions = ble_slots.iter().sum::<usize>() + ble_chan;
    // The per-slot keys below spell out exactly MAX_LINKS values.
    const _: () = assert!(leviculum_heap_budget::MAX_LINKS == 4);
    let pn = engine
        .map(crate::pn::Engine::heap_census)
        .unwrap_or_default();
    let attributed = census.total() + ble + ble_sessions + pn.total();
    let other = used_now.max(used).saturating_sub(attributed);
    // Kept for the panic record (order 1665): an allocation failure before
    // the next census replays these words as `[PANIC_CENSUS]` on the next
    // boot, in `leviculum_firmware::heap::CENSUS_KEYS` order.
    leviculum_firmware::heap::note_census(
        [
            used_now,
            free_now,
            largest,
            census.node_struct,
            census.links,
            census.events,
            census.requests,
            census.destinations,
            census.transport,
            census.storage,
            ble_sessions,
            pn.peers,
            pn.work,
            pn.link_maps,
            pn.flush,
            other,
        ],
        Instant::now().as_millis(),
    );
    crate::log::log_fmt(
        "[HEAP_CENSUS] ",
        format_args!(
            "used={} free={} largest={} node_box={} links={} n_links={} res={} events={} req={} dest={} transport={} storage={} ble_defrag={} ble_sessions={} ble_s0={} ble_s1={} ble_s2={} ble_s3={} pn_peers={} n_peers={} pn_work={} pn_batch={} pn_out={} pn_links={} pn_role={} pn_flush={} held={} hold_refused={} pn_dial_held={} other={}",
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
            census.held_announces,
            census.announces_hold_refused,
            pn.dials_held,
            other,
        ),
    );
    // The transients beside the steady state (#388, order 510): what the
    // model says each operation peaked at, the largest since boot, and what
    // the role budget leaves for a transient once its structural term is
    // spent. A `largest=` below one of these names the operation that can
    // no longer run.
    if let Some(engine) = engine {
        let peaks = engine.heap_peaks();
        crate::log::log_fmt(
            "[HEAP_PEAK] ",
            format_args!(
                "offer_response={} inbound_resource={} upload={} store_tail={} role_headroom={}",
                peaks.offer_response,
                peaks.inbound_resource,
                peaks.upload,
                peaks.store_tail,
                leviculum_heap_budget::ROLE_BUDGET_BYTES
                    - leviculum_heap_budget::ROLE_STRUCTURAL_BYTES,
            ),
        );
    }
}

// The node box every budget term is computed against, pinned in the budget
// crate for its host tests (#661) and asserted here, on the target, like
// `LINK_BYTES` above. Placed last so the book's line citations into this
// file stay where they are.
const _: () = assert!(
    core::mem::size_of::<
        NodeCore<
            leviculum_firmware::entropy::Entropy,
            leviculum_firmware::clock::EmbassyClock,
            leviculum_core::embedded_storage::EmbeddedStorage,
        >,
    >() == leviculum_heap_budget::NODE_BOX_BYTES
);
