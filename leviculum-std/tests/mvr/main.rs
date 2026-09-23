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
/// Arming is global and this crate's tests run with `--test-threads=1`
/// (the `mvr` recipe), so exactly one test is inside the window at a
/// time. Unarmed the cost is one relaxed atomic load per allocation,
/// which every other mvr pays and none of them can see.
pub mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    /// The board heap the measurement is for
    /// (`HEAP_SIZE`, `leviculum-nrf/src/lib.rs:252`). A single block
    /// bigger than this is host-only by construction; see the module
    /// doc.
    pub const BLOCK_CEILING_BYTES: usize = 32 * 1024;

    pub static LIVE: AtomicIsize = AtomicIsize::new(0);
    pub static PEAK: AtomicIsize = AtomicIsize::new(0);
    pub static MAXBLOCK: AtomicIsize = AtomicIsize::new(0);
    static ARMED: AtomicBool = AtomicBool::new(false);

    /// Whether a block of this layout is one the board could hold.
    fn counted(layout: Layout) -> bool {
        layout.size() <= BLOCK_CEILING_BYTES
    }

    pub struct Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc(layout) };
            if counted(layout) && ARMED.load(Ordering::Relaxed) && !ptr.is_null() {
                let live = LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed)
                    + layout.size() as isize;
                PEAK.fetch_max(live, Ordering::Relaxed);
                MAXBLOCK.fetch_max(layout.size() as isize, Ordering::Relaxed);
            }
            ptr
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            if counted(layout) && ARMED.load(Ordering::Relaxed) {
                LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
            }
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc_zeroed(layout) };
            if counted(layout) && ARMED.load(Ordering::Relaxed) && !ptr.is_null() {
                let live = LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed)
                    + layout.size() as isize;
                PEAK.fetch_max(live, Ordering::Relaxed);
                MAXBLOCK.fetch_max(layout.size() as isize, Ordering::Relaxed);
            }
            ptr
        }
    }

    /// An armed measurement window. Disarms when dropped, so a panicking
    /// test cannot leave the counter running into the next one.
    pub struct Probe {
        _private: (),
    }

    impl Probe {
        pub fn armed() -> Self {
            LIVE.store(0, Ordering::SeqCst);
            PEAK.store(0, Ordering::SeqCst);
            MAXBLOCK.store(0, Ordering::SeqCst);
            ARMED.store(true, Ordering::SeqCst);
            Self { _private: () }
        }

        /// The high-water mark of live bytes since arming, in bytes the
        /// program asked for — no allocator block headers, no rounding.
        pub fn peak(&self) -> usize {
            PEAK.load(Ordering::SeqCst).max(0) as usize
        }
    }

    /// The largest single block counted since arming — the check that the
    /// ceiling above is excluding only what it claims to.
    pub fn largest_block() -> usize {
        MAXBLOCK.load(Ordering::SeqCst).max(0) as usize
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            ARMED.store(false, Ordering::SeqCst);
        }
    }
}

#[global_allocator]
static COUNTING: alloc_probe::Counting = alloc_probe::Counting;

mod announce_emission_unix_time;
mod burst_continuation_contends_inside_its_own_answer;
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
mod rust_client_path_install_from_python;
mod rust_client_path_install_loop_race;
mod rust_client_path_install_via_relay;
mod rust_client_path_install_with_own_echo;
mod sender_modem_counts_a_frame_the_far_modem_never_hears;
mod shared_instance_client_survives_daemon_restart;
mod two_responders_overlap_inside_one_airtime;
mod udp_hostname_forward;
mod unparsable_config_must_not_start_a_daemon;
