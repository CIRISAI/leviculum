//! Reticulum firmware for ESP32-class boards.
//!
//! The sibling of `leviculum-nrf`, on the other half of the hardware we
//! own: four boards (Heltec V3, Heltec V4, two T-Beams) that today run
//! somebody else's firmware because we had none of our own for the
//! family. The first target is the Heltec WiFi LoRa 32 V4 (ESP32-S3R2 +
//! SX1262 + KCT8103L front end).
//!
//! # What this crate is, at this step
//!
//! It boots, it says what it is, and it holds the SX1262's SPI port open.
//! Nothing goes on the air. The point of the step is not the banner — it
//! is that the shape a second and third board plug into exists before the
//! first board's details have hardened around it:
//!
//!  * the per-board facts live in [`boards`], one module per wiring, each
//!    constant carrying the document it was read out of;
//!  * the per-class facts live here: the init order, the clock, the USB
//!    serial the banner leaves by, the banner itself;
//!  * the radio is reached through [`leviculum_core::sx126x`]'s two
//!    traits, implemented once in [`sx1262`], so the sequences that decide
//!    what goes on the air stay in the crate that has host tests.
//!
//! # The build stamp
//!
//! [`FW_BUILD_STAMP`] is the same one contiguous literal `leviculum-nrf`
//! carries, produced by the same `build.rs`, printed after the same
//! `[FW_BUILD] ` prefix. The rig's flasher and the reviewer's boot proof
//! grep one shape; a firmware family that printed a second shape would be
//! a firmware family the proof cannot read.

#![no_std]
// `WAITI` for the idle cores (1507, `idle`): inline asm on Xtensa is
// still behind this gate in the esp toolchain, as in `xtensa-lx`.
#![cfg_attr(target_arch = "xtensa", feature(asm_experimental_arch))]

extern crate alloc;

pub mod boards;
pub mod corepark;
pub mod flash;
pub mod gnss_uart;
pub mod handheld;
pub mod idle;
pub mod mspi;
pub mod pool_tlsf;
pub mod pools;
pub mod serial_iface;
pub mod stall;
pub mod sx1262;

use core::alloc::{GlobalAlloc, Layout};
use core::cell::RefCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use critical_section::Mutex;

// Tlsf, not Llff (1569): the llff free list is walked on every alloc
// AND every free, inside the cross-core critical section, and the walk
// grows with the churn — the M9's pull drove core 0's host stage from
// 170 ms to 2.5 s a turn and the UI core's drain past its 5 s watchdog
// that way (window 16a; `leviculum-heap-lab/tests/pull.rs` shows the
// same workload flat under tlsf and growing 14x under llff). Our own
// wrapper rather than `embedded_alloc::TlsfHeap` (1574): that one keeps
// rlsf's 4228 B control block in its `static`, which is `.bss`, which
// is core 0's stack on the ESP32-S3; ours puts it inside the heap region
// ([`pool_tlsf`]).
use esp_hal::{
    clock::CpuClock,
    peripherals::{Peripherals, USB_DEVICE},
    time::{Duration, Instant},
    uart::{Config as UartConfig, RxConfig, RxError, Uart, UartInterrupt, UartRx, UartTx},
    usb::usb_serial_jtag::UsbSerialJtag,
    Blocking,
};
use pool_tlsf::PoolTlsf;

use boards::BoardConfig;
use leviculum_serial_mux::rx::{RxCounts, RxHeld, RxKeep};
use leviculum_serial_mux::tx::{Kind, TxRing};

/// The build identity this image carries, as ONE contiguous string
/// literal.
///
/// It is printed verbatim after `[FW_BUILD] ` by the boot line and by the
/// periodic banner. Being a single literal is the point, and the reason is
/// the one `leviculum-nrf/src/lib.rs` states: the flash runner greps the
/// same text out of the flat image it is about to write, so what it
/// expects to read back is taken from the image rather than from the
/// working tree. Two `env!` arguments formatted at the call site would put
/// the sha in its own rodata entry with nothing next to it to grep for.
///
/// `dirty` is part of the stamp, not decoration: two images built from the
/// same commit with different working trees are different images, and a
/// rig that confirmed only the sha would call them the same one.
///
/// `version` is the crate's, moved at every landing that touched this
/// firmware (scripts/bump-touched.py, 1226); last, as in `leviculum-nrf`.
pub const FW_BUILD_STAMP: &str = concat!(
    "git_sha=",
    env!("LEVICULUM_GIT_SHA"),
    " dirty=",
    env!("LEVICULUM_GIT_DIRTY"),
    " version=",
    env!("CARGO_PKG_VERSION")
);

// ---------------------------------------------------------------------
// Heap
// ---------------------------------------------------------------------

/// The lock of one pool ([`pools::PoolLock`], 1594): interrupts off on
/// this core and a spin on an atomic, so it excludes the other core and
/// this core's interrupts and nothing else. Not the cross-core critical
/// section: a core waits here only for the other core's one TLSF step in
/// the same pool, never for core 0's flash ops, queue pushes or the UART
/// interrupt. The bench image counts the spin ([`alloc_clock::waited`]).
///
/// AutoPark may stall the UI core while it holds its pool's lock: core 0
/// parks it only inside `flash::parked`, whose op allocates nothing, so
/// nothing waits for the lock until the UI core runs again.
pub(crate) struct IrqSpin {
    held: core::sync::atomic::AtomicBool,
}

impl IrqSpin {
    const fn new() -> Self {
        Self {
            held: core::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl pools::PoolLock for IrqSpin {
    fn with<R>(&self, f: impl FnOnce() -> R) -> R {
        use esp_sync::raw::{RawLock, SingleCoreInterruptLock};
        #[cfg(m9_bench)]
        let t0 = alloc_clock::now();
        let token = loop {
            // SAFETY: exited below on this core, the one token in flight.
            let token = unsafe { SingleCoreInterruptLock.enter() };
            if self
                .held
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break token;
            }
            // SAFETY: the token `enter` just gave, on this core.
            unsafe { SingleCoreInterruptLock.exit(token) };
            core::hint::spin_loop();
        };
        #[cfg(m9_bench)]
        alloc_clock::waited(t0);
        let out = f();
        self.held.store(false, Ordering::Release);
        // SAFETY: the token this call's `enter` gave, on this core.
        unsafe { SingleCoreInterruptLock.exit(token) };
        out
    }
}

/// The pools: 0 core 0's and every core's fallback, 1 the UI core's
/// once a board places it ([`init_core_heap_at`]).
type Heap = pools::Pools<IrqSpin, 2>;

/// The core that runs this, as the pool index: the processor id is a
/// special register read, not a lock.
fn this_core() -> usize {
    match esp_hal::system::Cpu::current() {
        esp_hal::system::Cpu::ProCpu => 0,
        _ => 1,
    }
}

// SAFETY: blocks come from the pool of the asking core (or a fallback),
// under that pool's lock, and go back to the pool whose region holds
// them, with the alignment they were asked with; before a pool is placed
// nothing is handed out of it.
unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.allocate(this_core(), layout)
            .map_or(core::ptr::null_mut(), |(ptr, _)| ptr.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(ptr) = core::ptr::NonNull::new(ptr) {
            // SAFETY: the caller's contract: `ptr` came from `alloc` with
            // this layout, so from one of the pools.
            unsafe { self.deallocate(ptr, layout.align()) };
        }
    }
}

/// The allocator, counted: what is live now and the most that was since
/// [`heap_reset_peak`]. The count is what a board's census reads
/// (`heap-budget`-style, order 1361); two atomics per call, beside the
/// allocator's own constant-time step (tlsf since 1569).
struct CountedHeap {
    heap: Heap,
    live: AtomicUsize,
    peak: AtomicUsize,
    /// A second peak, since [`heap_window_start`] (1565): what one draw
    /// took, read without resetting [`heap_peak`], which a census on the
    /// other core may be counting by.
    window: AtomicUsize,
}

// SAFETY: every call goes to the inner allocator unchanged; the counts are
// bookkeeping beside it and never decide what is handed out.
unsafe impl GlobalAlloc for CountedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        #[cfg(m9_bench)]
        let t0 = alloc_clock::now();
        // SAFETY: the caller's contract, passed through.
        let ptr = unsafe { self.heap.alloc(layout) };
        if !ptr.is_null() {
            let live = self.live.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            self.peak.fetch_max(live, Ordering::Relaxed);
            self.window.fetch_max(live, Ordering::Relaxed);
        }
        #[cfg(m9_bench)]
        {
            alloc_clock::counted(layout.size());
            alloc_clock::spent(t0);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        #[cfg(m9_bench)]
        let t0 = alloc_clock::now();
        self.live.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller's contract, passed through.
        unsafe { self.heap.dealloc(ptr, layout) };
        #[cfg(m9_bench)]
        {
            alloc_clock::counted(0);
            alloc_clock::spent(t0);
        }
    }
}

/// The time each core spent inside the allocator, in CPU cycles (1556,
/// the M9 bench's `[OPEN] alloc_ms`): the bench image only, so the
/// release image's allocator pays nothing for it. A cycle count wraps
/// every 17.9 s at 240 MHz; a reader takes the difference of two reads
/// a moment apart, never the count itself.
#[cfg(m9_bench)]
pub mod alloc_clock {
    use core::sync::atomic::{AtomicU32, Ordering};

    static SPENT: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

    /// The cycle counter of the core that runs this.
    #[inline(always)]
    pub fn now() -> u32 {
        esp_hal::xtensa_lx::timer::get_cycle_count()
    }

    fn core() -> usize {
        match esp_hal::system::Cpu::current() {
            esp_hal::system::Cpu::ProCpu => 0,
            _ => 1,
        }
    }

    /// Calls into the allocator and bytes it handed out, per core (1575,
    /// the `[FOLD]` line's `allocs=` and `alloc_bytes=`): a free is a
    /// call, a realloc counts the bytes it asked. Both wrap; a reader
    /// takes differences.
    static CALLS: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];
    static BYTES: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

    /// Cycles each core spun for a pool's lock (1594): the part of
    /// [`cycles`] the other core's step in the same pool cost, apart from
    /// this core's own TLSF work.
    static WAITED: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

    pub(crate) fn waited(t0: u32) {
        WAITED[core()].fetch_add(now().wrapping_sub(t0), Ordering::Relaxed);
    }

    /// The cycles this core spun for a pool's lock so far.
    pub fn wait_cycles() -> u32 {
        WAITED[core()].load(Ordering::Relaxed)
    }

    /// The cycles `core` (0 or 1) spun for a pool's lock so far.
    pub fn wait_cycles_of(core: usize) -> u32 {
        WAITED[core.min(1)].load(Ordering::Relaxed)
    }

    pub(crate) fn spent(t0: u32) {
        SPENT[core()].fetch_add(now().wrapping_sub(t0), Ordering::Relaxed);
    }

    pub(crate) fn counted(bytes: usize) {
        let core = core();
        CALLS[core].fetch_add(1, Ordering::Relaxed);
        BYTES[core].fetch_add(bytes as u32, Ordering::Relaxed);
    }

    /// The calls into the allocator this core made so far.
    pub fn calls() -> u32 {
        CALLS[core()].load(Ordering::Relaxed)
    }

    /// The calls `core` (0 or 1) made into the allocator so far (1594).
    pub fn calls_of(core: usize) -> u32 {
        CALLS[core.min(1)].load(Ordering::Relaxed)
    }

    /// The bytes the allocator handed this core so far.
    pub fn bytes() -> u32 {
        BYTES[core()].load(Ordering::Relaxed)
    }

    /// The cycles this core spent in the allocator so far.
    pub fn cycles() -> u32 {
        SPENT[core()].load(Ordering::Relaxed)
    }

    /// The cycles `core` (0 or 1) spent in the allocator so far: for
    /// the `[IDLE]` window's deltas (1569), read from either core.
    pub fn cycles_of(core: usize) -> u32 {
        SPENT[core.min(1)].load(Ordering::Relaxed)
    }
}

/// The largest block the calling core's pool hands out now, found by
/// halving between nothing and that pool's free bytes (1556, the M9
/// bench's `[OPEN]` line; per pool since 1594): about 23 tries, each a
/// walk of the free list, the block taken and given back at once and
/// never counted. A fragmented pool shows a largest block far below its
/// free bytes.
#[cfg(m9_bench)]
pub fn heap_largest() -> usize {
    let Some(pool) = HEAP.heap.pool(this_core()).filter(|pool| pool.placed()) else {
        return 0;
    };
    let (mut fits, mut fails) = (0usize, pool.free() + 1);
    while fails - fits > 64 {
        let size = fits + (fails - fits) / 2;
        let Ok(layout) = Layout::from_size_align(size, 4) else {
            break;
        };
        match pool.allocate(layout) {
            None => fails = size,
            Some(ptr) => {
                // SAFETY: the block this pool just handed out, with its
                // layout's alignment, given back at once.
                unsafe { pool.deallocate(ptr, layout.align()) };
                fits = size;
            }
        }
    }
    fits
}

#[global_allocator]
static HEAP: CountedHeap = CountedHeap {
    heap: Heap::new([
        pools::Pool::new(IrqSpin::new()),
        pools::Pool::new(IrqSpin::new()),
    ]),
    live: AtomicUsize::new(0),
    peak: AtomicUsize::new(0),
    window: AtomicUsize::new(0),
};

/// Bytes allocated and not yet freed.
pub fn heap_live() -> usize {
    HEAP.live.load(Ordering::Relaxed)
}

/// The most [`heap_live`] has been since the last [`heap_reset_peak`].
pub fn heap_peak() -> usize {
    HEAP.peak.load(Ordering::Relaxed)
}

/// Start a new peak at what is live now.
pub fn heap_reset_peak() {
    HEAP.peak.store(heap_live(), Ordering::Relaxed);
}

/// Start the window's peak at what is live now (1565): the map's draw
/// says what it took on its `[MAP]` line as [`heap_window_peak`] minus
/// what was live here. Both cores allocate from the one heap, so a window
/// counts what the other core took meanwhile too; it never touches
/// [`heap_peak`].
pub fn heap_window_start() -> usize {
    let live = heap_live();
    HEAP.window.store(live, Ordering::Relaxed);
    live
}

/// The most [`heap_live`] has been since [`heap_window_start`].
pub fn heap_window_peak() -> usize {
    HEAP.window.load(Ordering::Relaxed)
}

/// Bytes the allocator still has, all pools.
pub fn heap_free() -> usize {
    HEAP.heap.free()
}

/// Bytes reserved for the heap.
///
/// Nothing in this crate allocates yet. The allocator is here because
/// `leviculum-core` is `no_std + alloc`, and a binary that links `alloc`
/// without a `#[global_allocator]` does not link at all — so the choice is
/// not whether to have a heap but how big to say it is before anything has
/// measured it. 64 KiB is a placeholder against the ESP32-S3's 512 KiB of
/// internal SRAM, deliberately smaller than the nRF's 96 KiB so that the
/// first thing to move it is a measurement rather than a copied number.
///
/// rlsf's control block ([`pool_tlsf::PoolTlsf::CONTROL_BYTES`]) is taken
/// from these bytes since 1574, not from `.bss` beside them.
pub const HEAP_SIZE: usize = 64 * 1024;

// The array always holds the control block and a pool behind it, so
// `init_heap` cannot be refused for its size.
const _: () = assert!(HEAP_SIZE > 2 * PoolTlsf::CONTROL_BYTES);

/// Initialise the heap allocator. Called once by [`init`].
fn init_heap() {
    static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
    // SAFETY: called once at startup, before anything concurrent exists.
    // `addr_of!` avoids forming a reference to the `static mut`.
    // The array fits (the assertion beside HEAP_SIZE) and `init` runs
    // once, so the placement cannot be refused; were it, every allocation
    // would fail loudly in the allocation error handler.
    let _placed = unsafe {
        let heap_start = core::ptr::addr_of!(HEAP_MEM) as usize;
        HEAP.heap.place(0, heap_start, HEAP_SIZE)
    };
}

/// Give the allocator `size` bytes at `start` instead of the [`HEAP_SIZE`]
/// array: a board whose heap lives elsewhere, the M9's in PSRAM (order
/// 1361), calls this after [`init_soc`] and before anything allocates.
/// The pool is core 0's, and every core's while it has none of its own.
///
/// # Safety
///
/// Called once, instead of [`init`]'s heap, before any allocation; the
/// region is mapped, writable, and used by nothing else for as long as the
/// image runs.
///
/// False when the region cannot hold rlsf's control block (placed at its
/// front, 1574) and a pool behind it, or a heap is already placed: the
/// board then has no heap and says so.
#[must_use]
pub unsafe fn init_heap_at(start: usize, size: usize) -> bool {
    // SAFETY: the caller's contract.
    unsafe { HEAP.heap.place(0, start, size) }
}

/// Give core 1 a pool of its own, the `size` bytes at `start` (1594): its
/// allocations are served there, under a lock core 0 takes only to free
/// a block of it, so they never wait for core 0's flash ops or its own
/// allocations. Until this is called, and whenever this pool is full,
/// core 1 allocates from core 0's pool as before.
///
/// # Safety
///
/// Called once, before core 1 starts; the region is mapped, writable,
/// disjoint from every other pool's, and used by nothing else for as long
/// as the image runs.
#[must_use]
pub unsafe fn init_core_heap_at(core: usize, start: usize, size: usize) -> bool {
    // SAFETY: the caller's contract.
    core == 1 && unsafe { HEAP.heap.place(1, start, size) }
}

/// The free bytes of `core`'s own pool (1594); 0 for a pool not placed.
pub fn heap_free_of(core: usize) -> usize {
    HEAP.heap.pool(core).map_or(0, pools::Pool::free)
}

// ---------------------------------------------------------------------
// Class init
// ---------------------------------------------------------------------

/// Bring up the SoC and the heap, and hand back the peripherals.
///
/// The order is the class's, not the board's, and it is the whole reason
/// this function exists rather than three lines at the top of each binary:
///
/// 1. `esp_hal::init` — clocks and the watchdogs, before anything reads a
///    clock or a timer;
/// 2. the heap, before any code that might allocate;
/// 3. return, leaving the peripherals to the board's binary to hand out.
///
/// The CPU clock is taken at maximum. Nothing here is power-tuned yet and
/// pretending otherwise with a lower number would be a decision without a
/// measurement behind it.
pub fn init() -> Peripherals {
    let peripherals = init_soc();
    init_heap();
    peripherals
}

/// [`init`]'s first step alone: the SoC, its clocks and watchdogs, and no
/// heap. For a board that places its heap itself with [`init_heap_at`].
pub fn init_soc() -> Peripherals {
    esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()))
}

/// Milliseconds since boot, the stamp every log line carries.
pub fn uptime_ms() -> u64 {
    Instant::now().duration_since_epoch().as_millis()
}

/// [`uptime_ms`] in microseconds: what the M9's boot times its phases
/// by (1521).
pub fn uptime_us() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

/// The node's clock: [`uptime_ms`], monotonic from boot. No wall time: the
/// board has not learned one, and the core says so itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct EspClock;

impl leviculum_core::traits::Clock for EspClock {
    fn now_ms(&self) -> u64 {
        uptime_ms()
    }
}

// ---------------------------------------------------------------------
// The debug log
// ---------------------------------------------------------------------

/// How long a single byte may wait for room in the USB FIFO.
///
/// esp-hal's own `UsbSerialJtag::write` spins on the endpoint status until
/// the host drains it. With no host attached that spin does not end, and a
/// board whose first act after init is to print would hang on the bench
/// the moment it is powered from a battery instead of a cable. So the
/// writer here is the non-blocking one with a deadline, and a line that
/// cannot be placed is dropped: diagnostics may never be the reason the
/// firmware stops.
const USB_BYTE_TIMEOUT: Duration = Duration::from_millis(2);

/// Upper bound on one formatted log line.
///
/// `leviculum_log_line::format_line` truncates the body rather than the
/// stamp, so this is a limit on what is readable, never on what is
/// well-formed.
const LINE_MAX: usize = 160;

/// The board's debug log: `<prefix><body> t=<uptime_ms>\r\n` out of the
/// SoC's USB Serial/JTAG port.
///
/// There is no USB-to-UART bridge on these boards — the Type-C connector
/// goes straight to the SoC's own USB pins — so this peripheral IS the
/// debug port, and what a capture on the host sees is what is written
/// here.
pub struct UsbLog<'d> {
    port: UsbSerialJtag<'d, Blocking>,
}

impl<'d> UsbLog<'d> {
    /// Open the USB Serial/JTAG port.
    pub fn new(usb_device: USB_DEVICE<'d>) -> Self {
        Self {
            port: UsbSerialJtag::new(usb_device),
        }
    }

    /// Emit one line, shaped by [`leviculum_log_line::format_line`].
    ///
    /// The shape is not this crate's to choose. `lnflash::verify` splits
    /// host-side captures on `[FW_BUILD]`, periculum's
    /// `fw_build_version_in` greps for it, and both were written against
    /// the nRF boards. Going through the same crate that pins the shape
    /// for those boards is what keeps one reader able to read both
    /// families.
    pub fn line(&mut self, prefix: &str, args: core::fmt::Arguments) {
        let mut buf = [0u8; LINE_MAX];
        let line = leviculum_log_line::format_line(&mut buf, prefix, args, uptime_ms());
        for byte in line {
            if !self.put(*byte) {
                // No reader, or a reader that stopped draining. Abandon
                // the rest of the line rather than the boot.
                return;
            }
        }
        let _ = self.port.flush_tx_nb();
    }

    /// State what this image is and what board it thinks it is on.
    ///
    /// Two lines, in this order, because they answer different questions
    /// and a capture may only catch one of them: `[FW_BUILD]` says which
    /// commit produced the bytes, `[BOARD]` says which wiring those bytes
    /// were compiled for. A board flashed with the right commit and the
    /// wrong board file is a real failure mode and the first line alone
    /// cannot show it.
    pub fn identify(&mut self, config: &'static BoardConfig) {
        self.line("[FW_BUILD] ", format_args!("{FW_BUILD_STAMP}"));
        self.line(
            "[BOARD] ",
            format_args!(
                "id={} name={} gnss={} heap={}",
                config.log_prefix,
                config.board_name,
                if config.gnss_onboard {
                    "onboard"
                } else {
                    "none"
                },
                HEAP_SIZE
            ),
        );
    }

    /// Place one byte, or report that the FIFO never freed up.
    fn put(&mut self, byte: u8) -> bool {
        let deadline = Instant::now() + USB_BYTE_TIMEOUT;
        loop {
            if self.port.write_byte_nb(byte).is_ok() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }
}

/// The debug log of a board behind a USB-UART bridge (the ThinkNode M9's
/// CH340K on UART0): the same line shape as [`UsbLog`], plus the raw lines
/// a host reads by their own first word (`KB`, `KBRAW`, the `PBM` dump),
/// which carry no ` t=` stamp because their readers parse them whole, and
/// HDLC frames for the control envelope's answers.
///
/// The bridge always drains the UART at the line rate, host or no host, so
/// a write here blocks for the bytes' wire time and never longer.
///
/// One byte stream carries all three, so the rules of
/// `leviculum-nrf/serial-mux` apply: a line is written whole and never
/// inside a frame, and text never carries a raw 0x7E, 0x7D, 0x11 or 0x13.
/// Every line this crate writes is formatted ASCII from fixed words and
/// numbers; [`UartLog::line`] escapes what a formatted argument could
/// still bring.
///
/// Since 1414 the frames are the node's transport as well as the control
/// envelope's answers, so the line carries the whole contract: every frame
/// leaves with its XON and XOFF bytes escaped (`escape_flow`) and the
/// capability report states `serial_mux=1`, so lnsd escapes its own and
/// strips none of ours. The board sends no flow bytes itself: esp-hal 1.2
/// keeps `SwFlowControl` in its config and never programs it.
///
/// What the host sends is taken off the 128-byte receive FIFO by an
/// interrupt (1440) whenever the FIFO holds more than
/// [`RX_FIFO_THRESHOLD`] bytes, into [`RX_KEEP`] bytes of memory the loop
/// reads from ([`UartLog::read_ready`]). Before 1440 the FIFO was emptied
/// only when the loop wrote or read the line, and a turn of the loop that
/// spent more than the FIFO's 11 ms on one frame (an announce checked)
/// overflowed it under the next one: esp-hal resets an overflowed FIFO and
/// says `FifoOverflowed`, which was thrown away, and the bench of
/// 2026-11-06 lost three `/sync` answers so
/// (`leviculum_serial_mux::rx`, whose host model pins it). Every loss is
/// now counted and said as a `[UART]` line ([`UartLog::say_rx_counts`]).
///
/// Since 1507 the send side is an interrupt's too: a write goes into
/// [`TX_RING`] bytes of memory and the UART's TX-done interrupt feeds the
/// FIFO from there, so the loop never waits for the line
/// (`leviculum_serial_mux::tx`, whose host model pins the ring). A full
/// ring drops its oldest whole record, counted and said as `tx_drop=` on
/// the `[UART]` line, by kind as `tx_drop_kind=` (1553); a writer that must
/// not lose (the boot's lines) waits for room instead
/// ([`UartLog::set_lossy`]). A frame dump's lines are records the ring
/// never drops for another, and their writer waits for room and then for
/// its tail ([`UartLog::dump_line`], [`UartLog::wait_dump_out`]).
pub struct UartLog<'d> {
    /// The UART's send half, for a log that writes and waits (a panic's);
    /// `None` when the interrupt owns the UART.
    tx: Option<UartTx<'d, Blocking>>,
    /// A full ring drops the oldest record rather than wait.
    lossy: bool,
    /// Frames the board's HDLC reader dropped, as the loop reports them.
    deframe_drop: u32,
    /// Frames the loop read whole and still took nothing from (1597,
    /// window 21: two key frames the walker sent moved no counter on the
    /// board): an envelope too short or of the wrong length (refused as
    /// malformed), an envelope of a type this image does not take
    /// (refused as unknown), and a frame that is no envelope and too short
    /// to be a Reticulum packet (handed to the node, which drops it). A
    /// key frame that lost or gained a byte on the line lands in one of
    /// them.
    env_bad: u32,
    env_refused: u32,
    runt: u32,
    /// The losses of the three above the last `[UART]` line said.
    frames_said: [u32; 3],
    /// The longest the loop went between two reads since the last
    /// `[UART]` line, and when it last read.
    gap_max_ms: u64,
    last_read_ms: Option<u64>,
    /// The counts the last `[UART]` line said.
    said: Option<RxCounts>,
    /// The send ring's drops the last `[UART]` line said (1507).
    tx_said: u32,
    /// The last write was a frame, which ends on its flag and no line end
    /// (1521b): the next line starts on a line of its own, or a reader
    /// that splits on line ends finds it glued to the frame's tail.
    after_frame: bool,
    /// The I/O core's stack bytes never touched since the board painted
    /// them, said on every `[UART]` line when the board measures it
    /// (1521b, [`UartLog::set_stack_free`]).
    stack_free: Option<usize>,
    /// The UI core's the same (1582, [`UartLog::set_ui_stack_free`]):
    /// `None` on a board with one core, which says no such key; `Some(None)`
    /// before that core painted, said `unset`.
    ui_stack_free: Option<Option<usize>>,
}

/// The most host bytes kept while the loop is busy: 8 KiB is fourteen
/// full frames, 0.7 s of a host that never stops, far more than the
/// loop's longest turn (a 5.7 s frame dump writes, and every write reads).
pub const RX_KEEP: usize = 8 * 1024;

/// The receive FIFO level above which the interrupt empties it: half the
/// FIFO, so the interrupt may wait 64 bytes, 5.5 ms of the line at
/// 115200 baud, behind a critical section before a byte is lost.
pub const RX_FIFO_THRESHOLD: u16 = 64;

/// The bytes the board writes and the UART has not taken: 8 KiB, 0.7 s of
/// the line, the boot's census burst whole (1507).
pub const TX_RING: usize = 8 * 1024;
/// The records it holds at most: a line or a frame each.
pub const TX_RECORDS: usize = 512;

/// The log's UART, the interrupt's once [`UartLog::new`] handed it over:
/// its receive half empties the FIFO into [`RX_BYTES`], its send half
/// fills the FIFO from [`TX_BYTES`] (1507).
static PORT: Mutex<RefCell<Option<Uart<'static, Blocking>>>> = Mutex::new(RefCell::new(None));
/// The send ring's type.
type Ring = TxRing<TX_RING, TX_RECORDS>;

/// What the board wrote that the UART has not taken: on the heap once it
/// is up ([`UartLog::start_ring`]), not in internal RAM, whose every
/// static byte comes off core 0's stack (the M9's floor, 111,000 B).
/// Before, a write waits for the FIFO as it always did.
static TX_BYTES: Mutex<RefCell<Option<alloc::boxed::Box<Ring>>>> = Mutex::new(RefCell::new(None));
/// What the host sent and the loop has not read.
static RX_BYTES: Mutex<RefCell<RxKeep<RX_KEEP>>> = Mutex::new(RefCell::new(RxKeep::new()));
/// What the host sent while the loop was held in one long op (1594,
/// [`pump_rx`]): moved out of [`RX_BYTES`] before its 8 KiB fill, on
/// the heap, read before the keep by [`UartLog::read_ready`]. Window
/// 19's `bench forget` held core 0's loop for 34 s (the forget and the
/// ring's rebuild in one turn) and lost 10,684 host bytes to the full
/// keep. In 2 KiB blocks, at most 35 s of the line (1603,
/// [`RxHeld`]): the boot's rebuild pumps here too, 25 s of it, and a
/// doubling buffer would have asked the pool for 256 KiB in one piece.
/// Bytes past it are counted lost, as the keep counts its own.
static RX_HELD: Mutex<RefCell<RxHeld>> = Mutex::new(RefCell::new(RxHeld::new()));
static RX_HELD_LOST: AtomicU32 = AtomicU32::new(0);
/// FIFO overflows seen, and bytes the UART flagged.
static RX_OVERRUNS: AtomicU32 = AtomicU32::new(0);
static RX_ERRORS: AtomicU32 = AtomicU32::new(0);
/// Key frames the host sent that the loop took (1595, the bench's
/// `TYPE_KEY_INJECT`): the `[UART]` line's `keys_rx=`, so a key frame lost
/// in a FIFO overrun shows as a press the walker sent and this did not
/// count, without the proxy's log.
static KEYS_RX: AtomicU32 = AtomicU32::new(0);
/// Presses the UI core read into a `KEY` line (1612): the `[UART]` line's
/// `keys_ui=`, beside `keys_rx=`. A walk of injected keys without holds
/// has the two equal; a gap is a press core 0 took that no UI turn
/// handled, and a walker that sent more frames than `keys_rx=` counts lost
/// them on the line.
static KEYS_UI: AtomicU32 = AtomicU32::new(0);

/// Host bytes the UART handed over since boot, wrapping (1597, the
/// `[UART]` line's `rx_bytes=`): the proxy's log sums what it sent, so a
/// window whose sums differ by more than the counted losses lost bytes
/// no counter saw.
static RX_TAKEN: AtomicU32 = AtomicU32::new(0);

/// One key frame taken from the host (`KEYS_RX`).
pub fn count_key_rx() {
    KEYS_RX.fetch_add(1, Ordering::Relaxed);
}

/// One press the UI core read into a `KEY` line (`KEYS_UI`).
pub fn count_key_ui() {
    KEYS_UI.fetch_add(1, Ordering::Relaxed);
}

/// Empty the receive FIFO into the keep now (1595): before a flash op,
/// so the parked spin's window starts with an empty FIFO and the bytes
/// it takes sort strictly after the keep's (1613, [`corepark`]). Until
/// 1613 this was the only cover the op's masked window had, and the
/// line's bytes past the FIFO's 128 were dropped. A board whose log is
/// not the UART's does nothing.
pub fn drain_rx_fifo() {
    critical_section::with(|cs| {
        if let Some(uart) = PORT.borrow_ref_mut(cs).as_mut() {
            pull_rx(uart.split_mut().0, &mut RX_BYTES.borrow_ref_mut(cs));
        }
    });
}

/// Empty the receive FIFO into the keep, counting an overflow (esp-hal
/// has already reset the FIFO and dropped its bytes) and a flagged byte.
/// The read after an error clears the FIFO-full flag, so the interrupt
/// does not come straight back; four reads bound it.
fn pull_rx(rx: &mut UartRx<'static, Blocking>, keep: &mut RxKeep<RX_KEEP>) {
    let mut buf = [0u8; 128];
    for _ in 0..4 {
        match rx.read_buffered(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                RX_TAKEN.fetch_add(n as u32, Ordering::Relaxed);
                keep.push(&buf[..n]);
            }
            Err(RxError::FifoOverflowed) => {
                RX_OVERRUNS.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                RX_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Move what the ring holds into the send FIFO, as much as it takes now;
/// never waits (esp-hal's `write` spins only on a full FIFO, which
/// `write_ready` rules out). The TX-done interrupt is listened to only
/// while bytes wait in the ring: an empty ring has nothing to feed, and
/// an idle line must never keep the interrupt coming.
fn feed_tx(uart: &mut Uart<'static, Blocking>, ring: &mut Ring) {
    while !ring.is_empty() && uart.write_ready() {
        match uart.write(ring.peek()) {
            Ok(0) | Err(_) => break,
            Ok(n) => ring.consume(n),
        }
    }
    match ring.is_empty() {
        true => uart.unlisten(UartInterrupt::TxDone),
        false => uart.listen(UartInterrupt::TxDone),
    }
}

/// The UART0 interrupt: the receive FIFO passed [`RX_FIFO_THRESHOLD`], or
/// the send FIFO ran empty (1507).
#[esp_hal::handler]
fn uart0_rx() {
    stall::isr(stall::Isr::Uart);
    critical_section::with(|cs| {
        if let Some(uart) = PORT.borrow_ref_mut(cs).as_mut() {
            if uart.interrupts().contains(UartInterrupt::TxDone) {
                uart.clear_interrupts(UartInterrupt::TxDone.into());
            }
            if let Some(ring) = TX_BYTES.borrow_ref_mut(cs).as_mut() {
                feed_tx(uart, ring);
            }
            let (rx, _) = uart.split_mut();
            pull_rx(rx, &mut RX_BYTES.borrow_ref_mut(cs));
        }
    });
    stall::isr(stall::Isr::None);
}

/// Keep what the host sent while a flash op had this core's interrupts
/// off (1613): the parked spin's ring, pushed into the keep by
/// [`corepark::hold`] before the UART interrupt resumes, so these bytes
/// sort between the pre-op sweep's and whatever the FIFO holds next.
/// Counted into `rx_bytes=` like every taken byte.
pub(crate) fn absorb_parked(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    RX_TAKEN.fetch_add(bytes.len() as u32, Ordering::Relaxed);
    critical_section::with(|cs| RX_BYTES.borrow_ref_mut(cs).push(bytes));
}

/// Keep what the host sent while the loop is held in one long op
/// (1594): the FIFO and the interrupt's keep emptied into `RX_HELD`,
/// which the loop reads first once it reads again. For the callbacks a
/// long op calls between its steps (the watchdog's feed of `bench
/// forget` and the store's reopen, and the boot's restore, 1603); cheap
/// when nothing came.
pub fn pump_rx() {
    critical_section::with(|cs| {
        let mut keep = RX_BYTES.borrow_ref_mut(cs);
        if let Some(uart) = PORT.borrow_ref_mut(cs).as_mut() {
            pull_rx(uart.split_mut().0, &mut keep);
        }
        if keep.is_empty() {
            return;
        }
        let mut held = RX_HELD.borrow_ref_mut(cs);
        let refused = keep.spill(|bytes| held.push(bytes));
        if refused > 0 {
            RX_HELD_LOST.fetch_add(refused, Ordering::Relaxed);
        }
    });
}

/// The config the log's UART runs with at `baud`: the FIFO-full
/// interrupt at [`RX_FIFO_THRESHOLD`].
pub fn uart_log_config(baud: u32) -> UartConfig {
    UartConfig::default()
        .with_baudrate(baud)
        .with_rx(RxConfig::default().with_fifo_full_threshold(RX_FIFO_THRESHOLD))
}

impl UartLog<'static> {
    /// Log over `uart`, configured with [`uart_log_config`] and pinned:
    /// its receive half goes to the interrupt, which runs from here on.
    pub fn new(mut uart: Uart<'static, Blocking>) -> Self {
        uart.set_interrupt_handler(uart0_rx);
        // Listened to and handed over with interrupts off, so the
        // interrupt never finds a full FIFO and no port to empty it.
        critical_section::with(|cs| {
            uart.listen(UartInterrupt::RxFifoFull);
            PORT.borrow_ref_mut(cs).replace(uart);
        });
        let mut log = Self::with(None);
        log.lossy = false;
        log
    }
}

impl<'d> UartLog<'d> {
    /// A log that only writes, for a panic: no interrupt, nothing read.
    pub fn tx_only(uart: Uart<'d, Blocking>) -> Self {
        let (_rx, tx) = uart.split();
        Self::with(Some(tx))
    }

    fn with(tx: Option<UartTx<'d, Blocking>>) -> Self {
        Self {
            tx,
            lossy: false,
            deframe_drop: 0,
            env_bad: 0,
            env_refused: 0,
            runt: 0,
            frames_said: [0; 3],
            gap_max_ms: 0,
            last_read_ms: None,
            said: None,
            tx_said: 0,
            after_frame: false,
            stack_free: None,
            ui_stack_free: None,
        }
    }

    /// Whether a write into a full ring drops the oldest record (`true`,
    /// the loop's way: it never waits for the line) or waits for room
    /// (`false`, the boot's and a frame dump's: every byte is wanted).
    pub fn set_lossy(&mut self, lossy: bool) {
        self.lossy = lossy;
    }

    /// Records the full ring dropped, and their bytes (1507).
    pub fn tx_dropped(&self) -> (u32, u32) {
        critical_section::with(|cs| {
            TX_BYTES
                .borrow_ref(cs)
                .as_ref()
                .map_or((0, 0), |ring| (ring.dropped(), ring.dropped_bytes()))
        })
    }

    /// The send ring's free bytes and free records (1630): what a writer
    /// that must not make the ring drop sizes its burst by. `(0, 0)`
    /// before the ring is up.
    pub fn tx_room(&self) -> (usize, usize) {
        critical_section::with(|cs| {
            TX_BYTES.borrow_ref(cs).as_ref().map_or((0, 0), |ring| {
                (TX_RING - ring.len(), TX_RECORDS - ring.records())
            })
        })
    }

    /// The records the full ring dropped, by [`Kind`] in [`Kind::ALL`]'s
    /// order (1553).
    pub fn tx_dropped_kinds(&self) -> [u32; 3] {
        critical_section::with(|cs| {
            TX_BYTES
                .borrow_ref(cs)
                .as_ref()
                .map_or([0; 3], |ring| ring.dropped_kinds())
        })
    }

    /// Send through the ring from here on (1507): once the heap is up,
    /// which the ring lives on.
    pub fn start_ring(&mut self) {
        let ring = alloc::boxed::Box::new(Ring::new());
        critical_section::with(|cs| TX_BYTES.borrow_ref_mut(cs).replace(ring));
    }

    /// What the next `[UART]` line says as `stack_free=` (1521b).
    pub fn set_stack_free(&mut self, bytes: usize) {
        self.stack_free = Some(bytes);
    }

    /// What the next `[UART]` line says as `ui_stack_free=` (1582): the
    /// second core's untouched stack, `None` while it is not painted yet.
    /// A board that never calls this says no such key.
    pub fn set_ui_stack_free(&mut self, bytes: Option<usize>) {
        self.ui_stack_free = Some(bytes);
    }

    /// Host bytes lost because the board's keep was full, and the ones
    /// a full park ring counted during a flash op (1613), which only an
    /// op far past the 57 ms bound could produce.
    pub fn rx_lost(&self) -> u32 {
        critical_section::with(|cs| RX_BYTES.borrow_ref(cs).lost())
            .saturating_add(RX_HELD_LOST.load(Ordering::Relaxed))
            .saturating_add(crate::corepark::host_ring_lost())
    }

    /// The loop's HDLC reader dropped a frame (empty or oversized).
    pub fn deframe_dropped(&mut self) {
        self.deframe_drop = self.deframe_drop.saturating_add(1);
    }

    /// A whole frame read as an envelope the loop refused as malformed
    /// (1597, `env_bad=`).
    pub fn envelope_bad(&mut self) {
        self.env_bad = self.env_bad.saturating_add(1);
    }

    /// A whole envelope of a type this image does not take (1597,
    /// `env_refused=`).
    pub fn envelope_refused(&mut self) {
        self.env_refused = self.env_refused.saturating_add(1);
    }

    /// A whole frame that is no envelope and shorter than a Reticulum
    /// packet's header (1597, `runt=`).
    pub fn runt(&mut self) {
        self.runt = self.runt.saturating_add(1);
    }

    /// Every loss on the receive side so far, and the loop's longest gap
    /// since the last `[UART]` line.
    pub fn rx_counts(&self) -> RxCounts {
        RxCounts {
            rx_lost: self.rx_lost(),
            overrun: RX_OVERRUNS.load(Ordering::Relaxed),
            deframe_drop: self.deframe_drop,
            rx_err: RX_ERRORS.load(Ordering::Relaxed),
            loop_gap_max_ms: u32::try_from(self.gap_max_ms).unwrap_or(u32::MAX),
        }
    }

    /// Say `[UART] rx_lost=N overrun=N deframe_drop=N rx_err=N
    /// loop_gap_max_ms=N` when a loss count changed since the last one, or
    /// always when `always` (the banner's turn); the gap starts again.
    pub fn say_rx_counts(&mut self, always: bool) {
        let counts = self.rx_counts();
        let (tx_drop, tx_drop_bytes) = self.tx_dropped();
        let [drop_line, drop_frame, drop_dump] = self.tx_dropped_kinds();
        let frames = [self.env_bad, self.env_refused, self.runt];
        let changed = self.said.is_none_or(|said| counts.losses_differ(&said))
            || tx_drop != self.tx_said
            || frames != self.frames_said;
        if !(always || changed) {
            return;
        }
        self.said = Some(counts);
        self.tx_said = tx_drop;
        self.frames_said = frames;
        self.gap_max_ms = 0;
        // Before the first measure (1521c) the field is there and says so,
        // so every `[UART]` line has the same keys. Through `text` (1553):
        // with the kinds the line outgrows the fixed line's 135 bytes.
        let free = match self.stack_free {
            Some(free) => alloc::format!("{free}"),
            None => alloc::string::String::from("unset"),
        };
        let ui = match self.ui_stack_free {
            Some(Some(free)) => alloc::format!(" ui_stack_free={free}"),
            Some(None) => alloc::string::String::from(" ui_stack_free=unset"),
            None => alloc::string::String::new(),
        };
        let keys = KEYS_RX.load(Ordering::Relaxed);
        let keys_ui = KEYS_UI.load(Ordering::Relaxed);
        let rx_bytes = RX_TAKEN.load(Ordering::Relaxed);
        let [env_bad, env_refused, runt] = frames;
        let body = alloc::format!(
            "{counts} stack_free={free}{ui} tx_drop={tx_drop} tx_drop_bytes={tx_drop_bytes} \
             tx_drop_kind=line:{drop_line},frame:{drop_frame},dump:{drop_dump} keys_rx={keys} keys_ui={keys_ui} \
             rx_bytes={rx_bytes} env_bad={env_bad} env_refused={env_refused} runt={runt}"
        );
        self.text("[UART] ", &body);
    }

    /// One line, shaped by [`leviculum_log_line::format_line`] as
    /// [`UsbLog::line`] shapes it.
    pub fn line(&mut self, prefix: &str, args: core::fmt::Arguments) {
        let mut buf = [0u8; LINE_MAX];
        let line = leviculum_log_line::format_line(&mut buf, prefix, args, uptime_ms());
        self.escaped(line);
    }

    /// One line of any length (1521b): `body` whole, shaped as
    /// [`UartLog::line`] shapes it, in a buffer the size of the line. For
    /// lines that arrive formatted and outgrow `LINE_MAX`: the UI
    /// core's census lines were cut at it (`verdict=over o t=`).
    pub fn text(&mut self, prefix: &str, body: &str) {
        let mut buf = alloc::vec![0u8; prefix.len() + body.len() + 32];
        let line =
            leviculum_log_line::format_line(&mut buf, prefix, format_args!("{body}"), uptime_ms());
        let mut out = alloc::vec::Vec::with_capacity(line.len() + 8);
        if core::mem::take(&mut self.after_frame) {
            out.extend_from_slice(b"\r\n");
        }
        for &byte in line {
            escape_byte(byte, &mut |piece| out.extend_from_slice(piece));
        }
        self.write_all(Kind::Line, &out);
    }

    /// `line`'s bytes with the line's own flow and flag bytes escaped, on a
    /// line of its own after a frame: one record, the line end before it
    /// included (1553), so the ring never drops the one without the other.
    fn escaped(&mut self, line: &[u8]) {
        let mut out = [0u8; 4 * LINE_MAX + 2];
        let mut n = 0;
        if core::mem::take(&mut self.after_frame) {
            out[..2].copy_from_slice(b"\r\n");
            n = 2;
        }
        for &byte in line.iter().take(LINE_MAX) {
            escape_byte(byte, &mut |piece| {
                out[n..n + piece.len()].copy_from_slice(piece);
                n += piece.len();
            });
        }
        self.write_all(Kind::Line, &out[..n]);
    }

    /// One raw line, `text` and its line end, one record (1553): for the
    /// lines read whole by their first word. `text` must be plain ASCII.
    pub fn raw(&mut self, text: &str) {
        let record = self.raw_record(text);
        self.write_all(Kind::Line, &record);
    }

    /// One line of a frame dump, as [`UartLog::raw`] writes it but a
    /// record the ring never drops for another (1553), written once the
    /// ring has room: a dump is a bench path, and its writer may wait.
    pub fn dump_line(&mut self, text: &str) {
        let record = self.raw_record(text);
        self.write_all(Kind::Dump, &record);
    }

    /// Wait until the dump's last line has left the ring (1553), so the
    /// loop's lossy writes after it find room and drop nothing: at most
    /// the ring's 0.7 s of the line. `feed` runs on every pass, the
    /// watchdog's.
    pub fn wait_dump_out(&mut self, feed: &mut dyn FnMut()) {
        if self.tx.is_some() {
            return;
        }
        loop {
            let held = critical_section::with(|cs| {
                let mut port = PORT.borrow_ref_mut(cs);
                let mut ring = TX_BYTES.borrow_ref_mut(cs);
                match (port.as_mut(), ring.as_mut()) {
                    (Some(uart), Some(ring)) => {
                        feed_tx(uart, ring);
                        ring.holds(Kind::Dump)
                    }
                    _ => false,
                }
            });
            if !held {
                return;
            }
            feed();
            core::hint::spin_loop();
        }
    }

    /// `text` and a line end, on a line of its own after a frame.
    fn raw_record(&mut self, text: &str) -> alloc::vec::Vec<u8> {
        let mut record = alloc::vec::Vec::with_capacity(text.len() + 4);
        if core::mem::take(&mut self.after_frame) {
            record.extend_from_slice(b"\r\n");
        }
        record.extend_from_slice(text.as_bytes());
        record.extend_from_slice(b"\r\n");
        record
    }

    /// One HDLC frame of `payload`, flags included (`leviculum_core`'s
    /// codec), between lines, its XON and XOFF bytes escaped
    /// (`leviculum_serial_mux::escape_flow`).
    pub fn frame(&mut self, payload: &[u8]) {
        let mut framed = alloc::vec::Vec::new();
        leviculum_core::framing::hdlc::frame(payload, &mut framed);
        let mut wire = alloc::vec::Vec::with_capacity(framed.len() + 8);
        leviculum_serial_mux::escape_flow(&framed, &mut |piece| wire.extend_from_slice(piece));
        self.write_all(Kind::Frame, &wire);
        self.after_frame = true;
    }

    /// All of `bytes`, one record of `kind`. Into the send ring, the
    /// interrupt's (1507): at once when the log is lossy, a full ring
    /// dropping its oldest record; else, and always for a dump's line
    /// (1553), once the ring has room, which the FIFO is fed from here as
    /// well, so the wait ends with interrupts off too. A log that owns its
    /// send half (a panic's) writes and waits, as before.
    fn write_all(&mut self, kind: Kind, mut bytes: &[u8]) {
        if let Some(tx) = self.tx.as_mut() {
            while !bytes.is_empty() {
                match tx.write(bytes) {
                    Ok(n) => bytes = &bytes[n..],
                    Err(_) => return,
                }
            }
            return;
        }
        let lossy = (self.lossy && kind != Kind::Dump) || bytes.len() > Ring::max_record();
        loop {
            let left = critical_section::with(|cs| {
                let mut port = PORT.borrow_ref_mut(cs);
                let Some(uart) = port.as_mut() else {
                    return 0;
                };
                let mut ring = TX_BYTES.borrow_ref_mut(cs);
                let Some(ring) = ring.as_mut() else {
                    // No ring yet: what the FIFO takes now, the rest on
                    // the next pass.
                    if !uart.write_ready() {
                        return bytes.len();
                    }
                    let n = uart.write(bytes).unwrap_or(bytes.len());
                    return bytes.len() - n;
                };
                let kept = (lossy || ring.fits(bytes.len())) && {
                    ring.push(kind, bytes);
                    true
                };
                feed_tx(uart, ring);
                match kept {
                    true => 0,
                    false => bytes.len(),
                }
            });
            if left == 0 {
                return;
            }
            bytes = &bytes[bytes.len() - left..];
            core::hint::spin_loop();
        }
    }

    /// The bytes the host sent that have arrived, without waiting: how many
    /// went into `buf`, the kept ones first, then what the FIFO holds
    /// under the interrupt's threshold.
    pub fn read_ready(&mut self, buf: &mut [u8]) -> usize {
        let now = uptime_ms();
        if let Some(last) = self.last_read_ms {
            self.gap_max_ms = self.gap_max_ms.max(now.saturating_sub(last));
        }
        self.last_read_ms = Some(now);
        critical_section::with(|cs| {
            // What a long op held first (1594, [`pump_rx`]): it came
            // before anything in the keep now.
            let mut held = RX_HELD.borrow_ref_mut(cs);
            if !held.is_empty() {
                return held.pop_into(buf);
            }
            let mut keep = RX_BYTES.borrow_ref_mut(cs);
            if let Some(uart) = PORT.borrow_ref_mut(cs).as_mut() {
                pull_rx(uart.split_mut().0, &mut keep);
            }
            keep.pop_into(buf)
        })
    }

    /// `[FW_BUILD]` and `[BOARD]`, as [`UsbLog::identify`] says them, for a
    /// board described by its name, tag and heap rather than a
    /// [`BoardConfig`].
    pub fn identify(&mut self, log_prefix: &str, board_name: &str, heap: usize) {
        self.line("[FW_BUILD] ", format_args!("{FW_BUILD_STAMP}"));
        self.line(
            "[BOARD] ",
            format_args!("id={log_prefix} name={board_name} heap={heap}"),
        );
    }
}

/// `byte`, or its `\\xNN` when it is a flow or flag byte, to `out`.
fn escape_byte(byte: u8, out: &mut dyn FnMut(&[u8])) {
    match byte {
        0x7E | 0x7D | 0x11 | 0x13 => {
            out(&[b'\\', b'x', hex_digit(byte >> 4), hex_digit(byte & 0xF)])
        }
        _ => out(&[byte]),
    }
}

fn hex_digit(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        _ => b'a' + nibble - 10,
    }
}

// ---------------------------------------------------------------------
// Panic
// ---------------------------------------------------------------------

/// The UART0 rate a panic is said at, 0 for none: set by a board whose
/// debug log is UART0 ([`say_panics_on_uart0`]).
static PANIC_UART0_BAUD: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Have a panic say its message on UART0 at `baud` before the board stops:
/// a board whose debug log is UART0 behind a bridge (the M9) calls this
/// once its log is up.
pub fn say_panics_on_uart0(baud: u32) {
    PANIC_UART0_BAUD.store(baud, Ordering::Relaxed);
}

/// Halt on panic, after saying why where a board asked for it.
///
/// The nRF crate's handler writes the message into `.uninit` RAM, counts
/// the panic across resets and replays it on the next boot; every one of
/// those depends on machinery this crate does not have yet (a noinit
/// section, a persistent counter, a boot-time replay), so this one does not
/// pretend to. What it can do honestly: on a board that named UART0 as its
/// log ([`say_panics_on_uart0`]), one `[PANIC]` line on a fresh driver,
/// then stop, and leave the board visibly dead rather than quietly wrong.
/// Since 1534 the panic's site goes into [`stall`]'s record first, and a
/// board that armed its loops' watchdogs (the M9) resets itself out of the
/// stop and says the site as `[PREV_RUN] reason=panic ... at=<file>:<line>`.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // The site first, into the record that survives the watchdog's reset
    // (1534): a panic whose line never reaches the UART, because the
    // lock the writer needs is held, still leaves where it was.
    let (file, line) = info
        .location()
        .map_or(("?", 0), |at| (at.file(), at.line()));
    stall::panicked(file, line);
    let baud = PANIC_UART0_BAUD.load(Ordering::Relaxed);
    if baud != 0 {
        // SAFETY: the panicking context owns nothing any more; a fresh
        // driver on UART0, whose pins the board routed at boot, is the only
        // way left to say why.
        let uart0 = unsafe { esp_hal::peripherals::UART0::steal() };
        if let Ok(uart) = Uart::new(uart0, esp_hal::uart::Config::default().with_baudrate(baud)) {
            UartLog::tx_only(uart).line("[PANIC] ", format_args!("{info}"));
        }
    }
    loop {
        core::hint::spin_loop();
    }
}
