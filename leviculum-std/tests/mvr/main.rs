//! Minimum-viable-reproduction (mvr) test tier.
//!
//! See CLAUDE.md §Protocol debugging discipline for the policy these tests
//! implement. Each file here isolates one named protocol-layer failure from
//! the full-scenario tests so that it runs deterministically in seconds
//! rather than minutes, with full structured event logs on failure.
//!
//! mvrs must not depend on LoRa hardware, Docker, or Python. When a full-
//! scenario bug reproduces over a non-LoRa transport, the mvr builds that
//! transport from process primitives and holds the rest of the protocol
//! stack (daemon, client tools, resource machinery) unchanged.

// Shared harness — loaded once here so all mvr test files can reach
// it via `use crate::harness::...`.  Loading it once avoids the
// `clippy::duplicate_mod` warning that fires when each mvr file
// pulls in `#[path] mod harness;` independently.
#[path = "../rnsd_interop/harness.rs"]
#[allow(dead_code)]
pub mod harness;

/// A counting allocator, for the one mvr that measures heap rather than
/// protocol (`pn_serve_peak_outgrows_the_board_heap`).
///
/// It wraps the system allocator and, while armed, tracks the number of
/// bytes live and the high-water mark of that number. `realloc` is
/// deliberately NOT overridden: the `GlobalAlloc` default implements it
/// as allocate-copy-deallocate, which is exactly the shape a growing
/// `Vec` costs a bump allocator — the old block and the new one live at
/// once. A pass-through to `System::realloc` would hide precisely the
/// transient the board died in.
///
/// Blocks larger than [`alloc_probe::BOARD_HEAP_BYTES`] are not counted.
/// The subject of the one test that arms this is what a 96 KiB board heap
/// is asked for, and a single block bigger than that whole heap is not
/// something a board can allocate at all — it is host-only machinery. In
/// practice it is exactly one thing: the 3.6 MB bzip2 workspace
/// `leviculum-std` links and the firmware does not (`compression` is off
/// in `leviculum-nrf`'s dependency on `leviculum-lxmf`, whose comment
/// says why: encrypted payloads do not compress).
///
/// Arming is per THREAD, not per process, for the reason the crate's other
/// allocator seam already gives (`leviculum-std/src/rpc/connection.rs`,
/// `alloc_probe`): libtest runs this binary's tests in parallel in one
/// process, so process-global counters fold every other test's allocations
/// into the measurement. This module was global and leaned on
/// `--test-threads=1`, which the `mvr` Justfile recipe passes and
/// `cargo test --workspace` — the gate, and the nightly's `just complete` —
/// does not: `cargo test -p leviculum-std --test mvr pn_serve` measured
/// `board_peak` at 29 842 B alone and at 31 922 B and 39 059 B beside its
/// two siblings, against a 33 238 B model. Unarmed the cost is one
/// thread-local read per allocation, which every other mvr pays and none of
/// them can see.
///
/// Per-thread arming was necessary and not sufficient. libtest gives each
/// test its own thread, so no other test's *own* allocations land in this
/// window — but `tracing` dispatch runs on the thread that emits the event,
/// and `EventLogLayer::on_event` (`leviculum-std/src/event_log.rs:1645`)
/// pushes one `String` into the buffer of EVERY capture handle any test has
/// registered, "regardless of which test emitted it" (that module's own doc).
/// So the code under test emitting `PKT_TX`
/// (`leviculum-core/src/transport.rs:3450`) inside the window made THIS
/// thread pay another test's `Vec<String>` growth step: on 2026-09-25,
/// `link_failure_recovery_silent_resume.rs:530` held the only handle in this
/// binary, its buffer crossed 256 -> 512 entries during the calibration
/// serve, and `512 * size_of::<String>()` = 12 288 B landed in a window whose
/// whole budget is 4 096 B. Deterministic, and invisible under
/// `--test-threads=1`, where that test's handle is dropped before this one
/// starts. Hence [`Probe::armed`] holds this thread's dispatch at
/// `NoSubscriber` for the life of the window: the journey the dispatch feeds
/// is host-only anyway — the firmware takes `leviculum-core` with
/// `default-features = false` (`leviculum-nrf/Cargo.toml:17`), so
/// `crate::tracing::enabled!` is the `false` shim at
/// `leviculum-core/src/lib.rs:83` and the whole `PKT_TX` block is dead code
/// on a board. Muting it measures the board's path, not this host's
/// observability.
pub mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The board heap the measurement is for
    /// (`HEAP_SIZE`, `leviculum-nrf/src/lib.rs:275`). A single block
    /// bigger than this is host-only by construction; see the module
    /// doc.
    pub const BLOCK_CEILING_BYTES: usize = 32 * 1024;

    thread_local! {
        /// Whether this thread is inside a measurement window.
        ///
        /// All four cells are `const`-initialised and `Drop`-free on purpose:
        /// a thread-local that needed lazy initialisation or a destructor
        /// would allocate from inside the allocator.
        static ARMED: Cell<bool> = const { Cell::new(false) };
        /// Counted bytes this thread was handed and has not returned.
        static LIVE: Cell<isize> = const { Cell::new(0) };
        /// High-water mark of [`LIVE`] since arming.
        static PEAK: Cell<isize> = const { Cell::new(0) };
        /// Largest single counted block since arming.
        static MAXBLOCK: Cell<isize> = const { Cell::new(0) };
        /// Where [`trap_block_size`] leaves the backtrace of the first
        /// matching block this thread allocated inside a window.
        ///
        /// A leaked `&'static str` rather than a `String`, to keep this
        /// module's no-destructor rule: a thread-local that needed a
        /// destructor would register it from inside the allocator. One
        /// leak per trapped thread, and only when the trap is armed.
        static TRAPPED: Cell<Option<&'static str>> = const { Cell::new(None) };
        /// Re-entry guard: capturing a backtrace allocates.
        static IN_TRAP: Cell<bool> = const { Cell::new(false) };
    }

    /// The block size [`trap_block_size`] is watching for; 0 = off.
    ///
    /// Process-global rather than thread-local so a test can arm it for a
    /// window that has not started yet. Read once per *counted* allocation
    /// inside an armed window, so an unarmed thread pays nothing for it.
    static TRAP_BLOCK: AtomicUsize = AtomicUsize::new(0);

    /// Name the code that allocates a block of exactly `bytes`.
    ///
    /// The debugging entry point for "this window caught a block it should
    /// not have": take the size from [`largest_block`], arm this, re-run,
    /// and read [`trap_report`]. A size filter is used rather than a
    /// threshold because a threshold backtraces every large block and the
    /// capture itself allocates — enough to move the numbers of every
    /// other probe test in the binary (measured 2026-09-25). Pass 0 to
    /// disarm.
    pub fn trap_block_size(bytes: usize) {
        TRAP_BLOCK.store(bytes, Ordering::Relaxed);
    }

    /// The backtrace of the first trapped block on this thread, if one
    /// was caught since the trap was armed.
    pub fn trap_report() -> Option<&'static str> {
        TRAPPED.try_with(Cell::get).ok().flatten()
    }

    /// Capture one backtrace for a block the trap is watching for.
    ///
    /// Counting is suspended across the capture so the diagnostic cannot
    /// move the number it exists to explain.
    fn trap(size: usize) {
        if size != TRAP_BLOCK.load(Ordering::Relaxed) {
            return;
        }
        if IN_TRAP.try_with(Cell::get).unwrap_or(true) {
            return;
        }
        let _ = IN_TRAP.try_with(|guard| guard.set(true));
        let _ = ARMED.try_with(|armed| armed.set(false));
        if TRAPPED.try_with(Cell::get).ok().flatten().is_none() {
            let captured: &'static str = Box::leak(
                std::backtrace::Backtrace::force_capture()
                    .to_string()
                    .into_boxed_str(),
            );
            let _ = TRAPPED.try_with(|slot| slot.set(Some(captured)));
        }
        let _ = ARMED.try_with(|armed| armed.set(true));
        let _ = IN_TRAP.try_with(|guard| guard.set(false));
    }

    /// Whether a block of this layout is one the board could hold.
    fn counted(layout: Layout) -> bool {
        layout.size() <= BLOCK_CEILING_BYTES
    }

    /// Book one counted allocation against this thread's window.
    ///
    /// `try_with` rather than `with` throughout: a thread tearing down can
    /// still allocate after its thread-locals are gone, and a panic from
    /// inside the global allocator would abort the process.
    fn record(layout: Layout) {
        if !counted(layout) || !armed() {
            return;
        }
        let size = layout.size() as isize;
        let _ = LIVE.try_with(|live| {
            let now = live.get() + size;
            live.set(now);
            let _ = PEAK.try_with(|peak| {
                if now > peak.get() {
                    peak.set(now);
                }
            });
        });
        let _ = MAXBLOCK.try_with(|block| {
            if size > block.get() {
                block.set(size);
            }
        });
        // Last, not first: the block is booked before it is backtraced, so
        // arming the trap during a parallel run cannot make some other
        // thread's window miss a block it should have counted.
        trap(layout.size());
    }

    fn release(layout: Layout) {
        if !counted(layout) || !armed() {
            return;
        }
        let _ = LIVE.try_with(|live| live.set(live.get() - layout.size() as isize));
    }

    fn armed() -> bool {
        ARMED.try_with(Cell::get).unwrap_or(false)
    }

    pub struct Counting;

    // SAFETY: every method forwards to `System` with the layout it arrived
    // with; `record` and `release` only touch `Cell`s that never allocate.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                record(layout);
            }
            ptr
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            release(layout);
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc_zeroed(layout) };
            if !ptr.is_null() {
                record(layout);
            }
            ptr
        }
    }

    /// An armed measurement window on the calling thread. Disarms when
    /// dropped, so a panicking test cannot leave the counter running into
    /// the next test this thread picks up.
    pub struct Probe {
        /// This thread's `tracing` dispatch, held at `NoSubscriber` for the
        /// life of the window. See the module doc: the dispatch runs on the
        /// emitting thread and hands the line to every *other* test's
        /// capture buffer, so leaving it live measures the schedule.
        _no_tracing: tracing::dispatcher::DefaultGuard,
    }

    impl Probe {
        pub fn armed() -> Self {
            // Installed before arming on purpose: `set_default` touches a
            // thread-local of its own, and those bytes are the probe's, not
            // the code under test's.
            let no_tracing = tracing::dispatcher::set_default(&tracing::Dispatch::none());
            LIVE.with(|live| live.set(0));
            PEAK.with(|peak| peak.set(0));
            MAXBLOCK.with(|block| block.set(0));
            ARMED.with(|armed| armed.set(true));
            Self {
                _no_tracing: no_tracing,
            }
        }

        /// The high-water mark of live bytes since arming, in bytes the
        /// program asked for — no allocator block headers, no rounding.
        pub fn peak(&self) -> usize {
            PEAK.with(Cell::get).max(0) as usize
        }
    }

    /// The largest single block counted since arming — the check that the
    /// ceiling above is excluding only what it claims to.
    pub fn largest_block() -> usize {
        MAXBLOCK.with(Cell::get).max(0) as usize
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            // The body runs before the fields, so counting stops before
            // the dispatch guard is restored and never bills the window
            // for tracing's own teardown.
            ARMED.with(|armed| armed.set(false));
        }
    }
}

#[global_allocator]
static COUNTING: alloc_probe::Counting = alloc_probe::Counting;

/// The probe's own positive control: what a measurement window counts and
/// what it must not.
///
/// The window in `pn_serve_peak_outgrows_the_board_heap` is 4 096 B wide and
/// caught a 12 288 B block that belonged to another test (see the
/// [`alloc_probe`] module doc). These pin both halves of the fix — that a
/// block of exactly that size is invisible when it is allocated outside the
/// window and fully visible when it is allocated inside — so a later change
/// to the probe cannot quietly restore either failure.
mod alloc_probe_self_test {
    use crate::alloc_probe;

    /// The size the real contamination had: another test's `Vec<String>`
    /// capacity step, `512 * size_of::<String>()`.
    const CONTAMINANT_BYTES: usize = 512 * std::mem::size_of::<String>();

    #[test]
    fn a_block_allocated_before_the_window_is_not_in_the_window() {
        assert_eq!(CONTAMINANT_BYTES, 12_288, "the block the probe caught");
        let outside: Vec<u8> = Vec::with_capacity(CONTAMINANT_BYTES);
        let (peak, largest) = {
            let probe = alloc_probe::Probe::armed();
            (probe.peak(), alloc_probe::largest_block())
        };
        assert_eq!(
            largest, 0,
            "a block from before arming is not this window's"
        );
        assert!(peak < 4096, "peak {peak} B");
        drop(outside);
    }

    #[test]
    fn a_block_allocated_inside_the_window_is_counted_and_nameable() {
        alloc_probe::trap_block_size(CONTAMINANT_BYTES);
        let (peak, largest, named) = {
            let probe = alloc_probe::Probe::armed();
            let inside: Vec<u8> = Vec::with_capacity(CONTAMINANT_BYTES);
            let seen = (probe.peak(), alloc_probe::largest_block());
            drop(inside);
            (seen.0, seen.1, alloc_probe::trap_report())
        };
        alloc_probe::trap_block_size(0);
        assert_eq!(
            largest, CONTAMINANT_BYTES,
            "the window must see its own block"
        );
        assert!(peak >= CONTAMINANT_BYTES, "peak {peak} B");
        let named = named.expect("an armed trap must name the block it matched");
        assert!(
            named.contains("a_block_allocated_inside_the_window_is_counted_and_nameable"),
            "the backtrace must reach the allocating code: {named}"
        );
    }

    /// The contamination itself, reproduced: a `tracing` event emitted by
    /// the code under test used to hand this thread the growth of a
    /// `Vec<String>` owned by whatever other test had a capture handle
    /// registered. A window holds its own dispatch at `NoSubscriber`, so
    /// the event does not reach any layer and costs the window nothing.
    #[test]
    fn a_tracing_event_inside_the_window_reaches_no_layer() {
        let _handle = leviculum_std::test_support::event_log::init_event_log();
        let (peak, largest) = {
            let probe = alloc_probe::Probe::armed();
            for _ in 0..64 {
                tracing::debug!(target: "pkt", event = "PROBE_SELF_TEST", t = 0);
            }
            (probe.peak(), alloc_probe::largest_block())
        };
        assert_eq!(
            (peak, largest),
            (0, 0),
            "a muted window must allocate nothing for 64 events"
        );
        // The subscriber is genuinely there: the same events outside the
        // window do reach the layer, so the assertion above is not vacuous.
        let before = _handle.dump().len();
        tracing::debug!(target: "pkt", event = "PROBE_SELF_TEST", t = 0);
        assert!(
            _handle.dump().len() > before,
            "control: outside a window the event must reach the capture buffer"
        );
    }
}

mod a_frame_the_modem_consumed_without_transmitting;
mod announce_emission_unix_time;
mod burst_continuation_contends_inside_its_own_answer;
mod cad_tears_down_the_frame_it_then_detects;
mod client_wait_for_path_request_fallback;
mod core_lock_reentrancy;
mod core_processor_seam;
mod iface_online_after_task_death;
mod link_failure_recovery_silent_resume;
mod lncp_fetch_rust_responder;
mod local_client_announce_burst_half_duplex;
mod lxmf_opportunistic_fallback_threshold;
mod media_silence_restore_signal;
mod one_frame_in_the_modem_at_a_time;
mod pn_offer_outgrows_the_link_mdu;
mod pn_serve_cap_bounds_one_fetch;
mod pn_serve_cap_survives_a_shrinking_heap;
mod pn_serve_peak_outgrows_the_board_heap;
mod radio_config_sleeps_through_the_peer_yield_window;
mod radio_config_wedges_without_lora_task;
mod ratchet_rotation_single_packet;
mod recall_survives_a_restart;
mod reliable_channel_delivery_backpressure;
mod resource_consecutive_push_window_policy;
mod responder_close_delivery;
mod retained_mark_survives_a_daemon_swap;
mod rust_client_path_install_from_python;
mod rust_client_path_install_loop_race;
mod rust_client_path_install_via_relay;
mod rust_client_path_install_with_own_echo;
mod sender_modem_counts_a_frame_the_far_modem_never_hears;
mod shared_instance_client_survives_daemon_restart;
mod two_responders_overlap_inside_one_airtime;
mod udp_hostname_forward;
mod unparsable_config_must_not_start_a_daemon;
