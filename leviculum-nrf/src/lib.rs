//! Reticulum firmware support for nRF52840-based boards
//!
//! Provides heap allocator setup, board-specific pin mappings,
//! USB CDC-ACM debug logging, and Reticulum transport interface.
//!
//! # Nothing here is tested
//!
//! This crate cannot run a host test — both BSP features route through
//! `softdevice`, one of them is mandatory (the `compile_error!` pair
//! below), and `nrf-softdevice-s140` does not compile for a host triple.
//! Every `#[test]` in this workspace lives in one of the pure member
//! crates beside it, and every assertion written here instead would need
//! a board and an hour of the one rig.
//!
//! So a decision — a cadence, a threshold, an ordering, a predicate, an
//! arithmetic budget, a byte-exact line other tools grep — belongs in a
//! member crate, and this crate calls it. What stays is what takes a pin,
//! a register, a SoftDevice syscall or an `embassy_time::Instant`. The
//! policy, the seam's current extent and the decisions still stranded in
//! `src/` are in `docs/src/concepts/firmware-host-test-seam.md`.

#![no_std]

extern crate alloc;

// Bug #32 spike: exactly one BSP feature must be enabled. Mutex
// enforced at compile time so any binary forgets-to-select-a-BSP or
// sets-both fails fast at `cargo build`, not at link or runtime.
#[cfg(any(
    all(feature = "bsp-rak4631", feature = "bsp-t114"),
    all(feature = "bsp-rak4631", feature = "bsp-solarnode"),
    all(feature = "bsp-rak4631", feature = "bsp-t1000e"),
    all(feature = "bsp-t114", feature = "bsp-solarnode"),
    all(feature = "bsp-t114", feature = "bsp-t1000e"),
    all(feature = "bsp-solarnode", feature = "bsp-t1000e"),
))]
compile_error!(
    "`bsp-rak4631`, `bsp-t114`, `bsp-solarnode` and `bsp-t1000e` are mutually exclusive — pick exactly one"
);

#[cfg(not(any(
    feature = "bsp-rak4631",
    feature = "bsp-t114",
    feature = "bsp-solarnode",
    feature = "bsp-t1000e"
)))]
compile_error!(
    "must enable exactly one of `bsp-rak4631`, `bsp-t114`, `bsp-solarnode` or `bsp-t1000e`"
);

// The radio driver follows from the BSP; a build that names both, or a
// BSP that names neither, is a Cargo.toml mistake.
#[cfg(all(feature = "radio-sx1262", feature = "radio-lr11xx"))]
compile_error!("`radio-sx1262` and `radio-lr11xx` are mutually exclusive — the BSP picks one");

pub mod announce;
pub mod banner;
pub mod ble;
pub mod boards;
pub mod boot_count;
pub mod boot_trace;
/// The user button (#459), the GPIO half: the pin and its pull. The
/// gestures and what they do are `leviculum_firmware::button`'s (976).
pub mod button;
pub mod chtm;
pub mod events;
/// The feedback grammar's players (#459, order 892): the LED, the
/// notification LED and the T1000-E's buzzer answer the button.
pub mod feedback;
pub mod flash;
/// The `[HEAP_CENSUS]` line (Codeberg #388): who holds the heap the
/// `[HEAP]` line watches. Gated like [`pn`] — the engine is one of the
/// owners it walks, and every shipped build enables the SoftDevice.
#[cfg(feature = "softdevice")]
pub mod heap_census;
/// The debug log moved to the orchestration stratum (#469 step 1); the
/// path stays, so every `crate::log::` call site and the three macros read
/// as they did. The CDC writer that drains its ring stays in [`usb`].
pub use leviculum_firmware::log;
pub use leviculum_firmware::{info, log_critical, warn};
pub mod lora;
pub mod media;
/// Remote management on the board (#235): the identity allow-list
/// `rnstatus -R` / `lnstatus -R` are gated on, read at boot from the
/// telemetry page and writable over USB only.
pub mod mgmt;
pub mod name;
/// The propagation-node role (#384 part 3): the board twin of lnpnd's
/// engine, over the record-log adapters of `leviculum-pn-store`.
#[cfg(feature = "softdevice")]
pub mod pn;
pub mod position;
// The 8 s off and the boot-time factory reset (order 1511).
#[cfg(feature = "softdevice")]
pub mod power;
pub mod qspi;
// The LoRa transceiver driver this build links, behind one set of names
// for `lora`. Documented inside, where its links resolve.
pub mod radio;
pub mod radio_store;
/// The board's message store (#384): the record log mounted on the flash
/// region `memory.x` reserves behind the image. Nothing is stored in it yet —
/// the module carries the mount and the bench instrument that exercises it.
pub mod record_store;
/// The board's `/dfu` verb (OTA stage 1, #235): enter BLE DFU on an
/// allow-listed command from the mesh.
pub mod remote_dfu;
pub mod remote_status;
pub mod rng;
// T114 ST7789 status display — rides with the BSP (not the V2's
// `display` feature): the panel is write-only, presence detection is
// impossible, and blind-driving it is safe on panel-less boards, so one
// t114 UF2 serves both populations. See `st7789.rs` module docs.
#[cfg(feature = "bsp-t114")]
pub mod st7789;
// #384 size harness, behind a throwaway non-default feature. See the module
// docs: no shipped build enables it and none of it is in the default image.
#[cfg(any(feature = "store-spike-record-log", feature = "store-spike-sequential"))]
pub mod store_spike;
pub mod sx1262;
/// Telemetry reporting (Codeberg #236): the target lifecycle, the flash
/// record, and the wiring from this board's sensors into an LXMF report.
pub mod telemetry;
/// The learned calendar, checkpointed to flash across a power cycle
/// (Codeberg #166 item 4).
pub mod time_floor;
/// Periodic `[TRANSPORT]` counter line (Codeberg #344): what the transport
/// did with the packets it was asked to relay, on a firmware whose `tracing`
/// call sites are compiled out.
pub mod transport_stats;
pub mod usb;
/// The T114's switched peripheral rail (VEXT), shared by the display and
/// the GNSS receiver and therefore owned by neither (Codeberg #69).
pub mod vext;

// Shared SoftwareVbusDetect, fed by the SoftDevice's SoC POWER events
// from `ble::softdevice_task` and read by `embassy_nrf::usb::Driver` via
// `usb::init`. We use the software variant because the SoftDevice
// reserves exclusive access to the POWER peripheral and embassy-nrf's
// HardwareVbusDetect would conflict (per
// `embassy_nrf::usb::vbus_detect::HardwareVbusDetect` doc comment).
//
// The static is initialized lazily by `init_vbus()` — call once early
// in `main()` before either `usb::init` or `ble::init`.
pub use embassy_nrf::usb::vbus_detect::SoftwareVbusDetect;
use static_cell::StaticCell;

static VBUS_CELL: StaticCell<SoftwareVbusDetect> = StaticCell::new();

/// Initialize and return a reference to the shared `SoftwareVbusDetect`.
///
/// **Diagnostic workaround:** initial state is `(true, true)` so the USB
/// driver enumerates immediately at boot without waiting for the
/// SoftDevice's first `PowerUsbDetected` SoC event. Reason: while the
/// nrf-softdevice integration is being debugged, `Softdevice::enable`
/// sometimes panics before any SoC event fires, leaving USB stuck
/// waiting and hiding the panic message from our logs. With
/// `(true, true)` the cable-already-plugged-in case (which is our
/// flash workflow) just works; cable-removed scenarios will show a
/// stale "USB connected" state until the SD events kick in. Revert
/// to `(false, false)` once boot is reliable.
pub fn init_vbus() -> &'static SoftwareVbusDetect {
    VBUS_CELL.init(SoftwareVbusDetect::new(true, true))
}

/// Set the eight peripheral-IRQ priorities to non-SoftDevice-reserved
/// values before `Softdevice::enable`. The Nordic S140 reserves
/// architectural priorities 0, 1, and 4 for its own use; using any of
/// those for an application IRQ causes UB at runtime (silent fault or
/// hang) once the SoftDevice is active.
///
/// We pick P5 for everything — well below the SoftDevice's reserved
/// band, plenty of headroom above the SoftDevice's lowest priority (7).
/// GPIOTE and RTC1 are already set to P2 via `embassy_nrf::config::Config`
/// in main(); they are *also* re-asserted here for symmetry and to give
/// the boot log a single source of truth.
///
/// Call once early in main(), before `Softdevice::enable`.
///
/// nRF52840 has only 3 priority bits; cortex-m's `set_priority` takes
/// the priority in the upper-3-bits encoding (so P5 → `5 << 5 = 0xA0`).
/// On most cortex-m crate versions `set_priority(irq, n)` accepts the
/// raw priority byte and shifts internally; we pass the priority value
/// 0..=7 directly.
pub fn set_irq_priorities() {
    use embassy_nrf::interrupt::{Interrupt, InterruptExt, Priority};

    // P5: well below the SoftDevice's reserved 0/1/4 band, well above
    // its highest application priority (7).
    Interrupt::RNG.set_priority(Priority::P5);
    Interrupt::USBD.set_priority(Priority::P5);
    Interrupt::TWISPI0.set_priority(Priority::P5);
    Interrupt::SAADC.set_priority(Priority::P5);
    Interrupt::SPI2.set_priority(Priority::P5);
    Interrupt::SPIM3.set_priority(Priority::P5);
    Interrupt::UARTE0.set_priority(Priority::P5);
    // QSPI: `Qspi::new` enables this IRQ whether or not anything awaits it,
    // so it gets a priority here like every other peripheral we own rather
    // than keeping the reset default of 0 — which is inside the
    // SoftDevice's reserved band.
    Interrupt::QSPI.set_priority(Priority::P5);

    // GPIOTE + RTC1 already at P2 via embassy_nrf config; assert
    // explicitly so the [NVIC_PRIO] log line reflects the same state
    // regardless of who set them last.
    Interrupt::GPIOTE.set_priority(Priority::P2);
    Interrupt::RTC1.set_priority(Priority::P2);
}

/// Read the NVIC priority registers for the eight IRQs we manage and
/// emit a single `[NVIC_PRIO]` log line. Call once after
/// `set_irq_priorities()` for verification.
pub fn log_irq_priorities() {
    use embassy_nrf::interrupt::{Interrupt, InterruptExt};

    log::log_fmt_critical(
        "[NVIC_PRIO] ",
        format_args!(
            "rng={:?} usbd={:?} twispi0={:?} saadc={:?} spi2={:?} spim3={:?} uarte0={:?} qspi={:?} gpiote={:?} rtc1={:?}",
            Interrupt::RNG.get_priority(),
            Interrupt::USBD.get_priority(),
            Interrupt::TWISPI0.get_priority(),
            Interrupt::SAADC.get_priority(),
            Interrupt::SPI2.get_priority(),
            Interrupt::SPIM3.get_priority(),
            Interrupt::UARTE0.get_priority(),
            Interrupt::QSPI.get_priority(),
            Interrupt::GPIOTE.get_priority(),
            Interrupt::RTC1.get_priority(),
        ),
    );
}

/// Print a POWER.RESETREAS value the caller already holds.
///
/// The register read (and its mandatory clear) moved to
/// [`boot_trace::capture`], which runs first thing in `main` — before
/// `Softdevice::enable`, after which POWER belongs to the SD and the
/// register is only reachable via `sd_power_reset_reason_get/clr`. This
/// keeps one read for both the `BOOT_TRACE` line and this decoded one;
/// the decode itself is host-tested in `leviculum-boot-trace`.
pub fn log_reset_reason(raw: u32) {
    log::log_fmt_critical(
        "[RESET_REASON] ",
        format_args!("{}", leviculum_boot_trace::ResetReasons::decode(raw)),
    );
}

// RAK19026 baseboard peripherals — each gated on its own feature so the
// bare nRF52840 + SX1262 build (T114, RAK4631 module without baseboard)
// stays unchanged. The state they share is `leviculum_firmware::board_state`
// (959); these are the tasks that feed and read it.
#[cfg(feature = "battery")]
pub mod battery;
#[cfg(feature = "display")]
pub mod display;
// The page facts both status panels hand up: the Pocket's SSD1306
// (`display`) and the T114's ST7789, which rides with its BSP.
#[cfg(feature = "gnss")]
pub mod gnss;
#[cfg(feature = "display")]
pub mod led;
#[cfg(any(feature = "display", feature = "bsp-t114"))]
pub mod panel_facts;
// The T1000-E's accelerometer (#457). Its own feature, not part of the
// baseboard set above: the only board carrying the part is not a RAK one.
#[cfg(feature = "motion")]
pub mod motion;
#[cfg(all(feature = "motion", feature = "display"))]
compile_error!(
    "`motion` and `display` both bind TWISPI0's interrupt; give one of them TWISPI1 first"
);

use core::mem::MaybeUninit;

// The global allocator, `heap_stats` and the watermark live in the
// orchestration stratum (961, `leviculum_firmware::heap`); what stays here
// is the pool's memory ([`init_heap`]) and the executor task that runs the
// `[HEAP]` loop. The pool size and the incoming-resource caps are terms of
// the boot heap budget (#388), named from `leviculum_heap_budget`.
use leviculum_heap_budget::HEAP_SIZE;

/// Periodic heap telemetry (Codeberg #65): one `[HEAP]` line every 30 s,
/// [`leviculum_firmware::heap::watermark_loop`] on this executor.
#[embassy_executor::task]
pub async fn heap_watermark_task() {
    leviculum_firmware::heap::watermark_loop().await
}

/// Sentinel word `paint_stack` fills the unused stack with. Distinct
/// from both `.retained` post-mortem magics, so a stray match cannot be
/// mistaken for a valid record.
const STACK_CANARY: u32 = 0xDEAD_BEEF;

/// Low and high address of the stack region, straight from the linker.
///
/// **flip-link layout** (this crate links with `flip-link`, see
/// `.cargo/config.toml`): the stack sits at the BOTTOM of RAM, below
/// `.data`/`.bss`, and grows DOWN toward `_stack_end`:
///
/// ```text
///   __sretained = ORIGIN(RETAINED)           <- SoftDevice RAM ceiling
///        .retained (post-mortems, persistent tail, boot trace)
///   _stack_end  = ORIGIN(RAM)                <- stack floor
///        |  stack, grows DOWN  ^
///   _stack_start = __sdata     |             <- SP at reset
///        .data / .bss / .uninit (empty)
///   RAM end     = 0x20040000                 <- bootloader stack top
/// ```
///
/// The cross-boot records live BELOW the stack in their own `RETAINED`
/// region (memory.x) rather than in `.uninit` above `.bss`: flip-link
/// packs RAM sections against the top of RAM, and the Adafruit
/// bootloader — which every reset runs before the app — starts its own
/// stack at exactly 0x20040000, clobbering whatever lives up there.
///
/// That is the exact inverse of the classic cortex-m-rt layout, where
/// the stack is at the TOP and `__ebss` is its floor. The pre-flip-link
/// implementation of these helpers scanned UPWARD from `__ebss` — i.e.
/// straight through `.uninit` — and reported numbers that had nothing to
/// do with the stack. Use these symbols, never `__ebss`.
///
/// Returns `(low, high)` = `(_stack_end, _stack_start)`.
pub fn stack_region() -> (usize, usize) {
    extern "C" {
        static _stack_end: u8;
        static _stack_start: u8;
    }
    (
        core::ptr::addr_of!(_stack_end) as usize,
        core::ptr::addr_of!(_stack_start) as usize,
    )
}

/// Current stack pointer. Only meaningful relative to
/// [`stack_region`] — an SP approaching `_stack_end` is an overflow in
/// progress.
pub fn stack_pointer() -> usize {
    let sp: usize;
    // SAFETY: reads a core register, no memory effects.
    unsafe { core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
    sp
}

/// Minimum free stack bytes since boot: the distance from `_stack_end`
/// (the low end, where an overflow lands first) up to the lowest word
/// the stack has ever touched.
///
/// Requires [`paint_stack`] to have run at boot. Scans `STACK_CANARY`
/// words UPWARD from `_stack_end`; the first non-canary word is the
/// deepest the stack has ever reached.
///
/// A monotonically shrinking value across the periodic `[STACK]` lines
/// is a stack overflow in progress; reaching 0 means the stack has
/// already run into the SoftDevice's RAM.
///
/// Conservative by construction: an interrupt taken while `paint_stack`
/// was running could leave non-canary words in the region, which only
/// ever makes the reported figure SMALLER than the truth, never larger.
pub fn stack_min_free() -> usize {
    let (lo, hi) = stack_region();
    let mut p = lo as *const u32;
    let mut untouched = 0usize;
    while (p as usize) < hi {
        if unsafe { core::ptr::read_volatile(p) } != STACK_CANARY {
            break;
        }
        untouched += 1;
        p = unsafe { p.add(1) };
    }
    untouched * 4
}

/// Bytes [`paint_stack`] actually covered. This is the CEILING on
/// [`stack_min_free`]: everything above the paint limit was already in
/// use when the paint ran and can never read back as free.
static STACK_PAINTED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Paint the unused stack with `STACK_CANARY`. Call once at the start
/// of main, before any heavy work.
///
/// Paints `[_stack_end, SP - SP_MARGIN)` — everything below the live
/// frame. The margin keeps this function's own frame and any interrupt
/// frame stacked on top of it out of the painted range.
///
/// Note that main's own frame is ALREADY allocated at this point (the
/// `async fn main` body is one big `TaskStorage::poll` with a single
/// entry `sub sp`), so the painted extent is much smaller than the
/// region: `painted ≈ SP_at_paint - _stack_end`. `[STACK]` reports both,
/// so the reviewer can read `min_free` against its own ceiling.
///
/// # Safety
/// Must be called before any concurrent tasks or interrupts use the
/// stack below `SP - SP_MARGIN`.
pub unsafe fn paint_stack() {
    /// Headroom below the live SP left unpainted: this function's frame
    /// plus room for an interrupt frame taken mid-paint.
    const SP_MARGIN: usize = 1024;
    paint_below_sp(SP_MARGIN);
}

/// Headroom left unpainted by [`reset_stack_watermark`].
///
/// Four times [`paint_stack`]'s boot-time margin, because the situation
/// is not the same one. At boot nothing else is running; a runtime
/// repaint happens with the SoftDevice enabled, the LoRa driver armed
/// and USB enumerated, and every one of those takes interrupts that push
/// their frames below the current SP while the repaint is in flight.
/// Nordic's guidance for the S140 is that the application reserve on the
/// order of 1.5 KiB of stack for the SoftDevice's own interrupt
/// handlers; 4 KiB is that with room for one of ours nested on top.
const RESET_SP_MARGIN: usize = 4096;

/// Paint `[_stack_end, SP - margin)` and record how far it reached.
///
/// # Safety
/// Nothing may be using the stack below `SP - margin`.
unsafe fn paint_below_sp(margin: usize) {
    let (lo, hi) = stack_region();
    let limit = stack_pointer().saturating_sub(margin).min(hi);
    let mut p = lo as *mut u32;
    while (p as usize) < limit {
        core::ptr::write_volatile(p, STACK_CANARY);
        p = p.add(1);
    }
    STACK_PAINTED.store(
        limit.saturating_sub(lo),
        core::sync::atomic::Ordering::Relaxed,
    );
}

/// Restart the stack high-water measurement from here.
///
/// The canary scheme records the deepest the stack has *ever* been since
/// the paint, which makes the boot transient — the deepest single moment
/// on most boards — the number every later reading reports. Peak stack
/// under a named load (BLE traffic, LoRa relay, telemetry) is therefore
/// not readable at all from a boot-painted stack: it is hidden behind
/// whatever `main` did on the way up.
///
/// This repaints everything below the live frame, so the next
/// `[STACK] min_free` is the peak of the phase that *follows* the reset.
/// Drive one phase, read it, reset, drive the next. It emits two lines:
/// `tag=pre-reset` (the peak of the phase just ended, the number to
/// record) and `tag=reset` (the new ceiling, i.e. `painted`).
///
/// Reachable over the debug CDC port with the byte `s`
/// (`usb::debug_reader_task`), alongside the `p` post-mortem query.
///
/// # Safety
/// Everything below `SP - RESET_SP_MARGIN` must be dead. That holds
/// for the embassy executor — task futures live in their own storage and
/// are polled on this one stack, so at any instant only the frames at or
/// above the current SP are live — and the margin covers an interrupt
/// frame taken while the repaint runs. It does NOT hold for an interrupt
/// handler that nests deeper than the margin; nothing in this firmware
/// does, but a handler added later that does would have its frame
/// overwritten. The reported `painted` is the ceiling on the reading, as
/// it is at boot.
pub unsafe fn reset_stack_watermark() {
    log_stack("pre-reset");
    paint_below_sp(RESET_SP_MARGIN);
    log_stack("reset");
}

/// Emit one `[STACK]` telemetry line.
///
/// Exact format (one line, `log_fmt_critical` so it bypasses the
/// runtime-drain gate and always reaches the persistent tail):
///
/// ```text
/// [STACK] min_free=<dec> painted=<dec> region=0x<lo>..0x<hi> size=<dec> peak_used=<dec> sp=0x<hex> tag=<label>
/// ```
///
/// `min_free` is the diagnostic of record — bytes above `_stack_end`
/// never touched. `painted` is its ceiling (see [`paint_stack`]);
/// `min_free == painted` means nothing has yet gone below the paint
/// limit, `min_free → 0` means the stack is about to run into the
/// SoftDevice's RAM. `peak_used = size - min_free`, `sp` is the
/// instantaneous depth at the sampling point.
pub fn log_stack(tag: &str) {
    let (lo, hi) = stack_region();
    let size = hi.saturating_sub(lo);
    let min_free = stack_min_free();
    log::log_fmt_critical(
        "[STACK] ",
        format_args!(
            "min_free={} painted={} region=0x{:08x}..0x{:08x} size={} peak_used={} sp=0x{:08x} tag={}",
            min_free,
            STACK_PAINTED.load(core::sync::atomic::Ordering::Relaxed),
            lo,
            hi,
            size,
            size.saturating_sub(min_free),
            stack_pointer(),
            tag,
        ),
    );
}

/// Periodic `[STACK]` telemetry. 2 s cadence: fast enough that a
/// collapsing `min_free` is visible as a trajectory in the ~2 KiB
/// persistent tail before the crash truncates it, coarse enough not to
/// lap the tail on its own.
#[embassy_executor::task]
pub async fn stack_watermark_task() {
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(2)).await;
        leviculum_firmware::thread_watch::beat_now();
        log_stack("tick");
    }
}

/// Initialize the heap allocator
///
/// Must be called once before any `alloc` usage (Vec, String, etc.).
/// Typically called at the start of the firmware entry point.
pub fn init_heap() {
    static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
    // SAFETY: Called once at startup before any concurrent access, and
    // HEAP_MEM is named nowhere else, so the allocator owns it for good.
    // Use addr_of! to avoid creating a reference to the static mut.
    unsafe {
        let heap_start = core::ptr::addr_of!(HEAP_MEM) as usize;
        leviculum_firmware::heap::init(heap_start, HEAP_SIZE);
    }
}

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use leviculum_boot_trace::postmortem;

// The post-mortem store moved to the orchestration stratum (#469 step 1);
// the handlers that fill it stay here, below, because catching a fault and
// resetting the core are this architecture's.
pub use leviculum_firmware::postmortem::{
    log_panic_count, log_postmortems, postmortem_query, take_hardfault_postmortem,
    take_panic_postmortem, HardfaultPostMortem, PanicPostMortem,
};

static PANIC_LED_ARMED: AtomicBool = AtomicBool::new(false);
static PANIC_LED_PORT: AtomicU8 = AtomicU8::new(0);
static PANIC_LED_PIN: AtomicU8 = AtomicU8::new(0);
static PANIC_LED_ACTIVE_LOW: AtomicBool = AtomicBool::new(false);

/// Arm the panic-handler LED. Until this is called, a panic skips GPIO
/// and just halts.
///
/// `port` is 0 (P0) or 1 (P1). `pin` is 0..=31. `active_low` is true if
/// the LED lights when the GPIO is driven low.
pub fn set_panic_led(port: u8, pin: u8, active_low: bool) {
    PANIC_LED_PORT.store(port, Ordering::Relaxed);
    PANIC_LED_PIN.store(pin, Ordering::Relaxed);
    PANIC_LED_ACTIVE_LOW.store(active_low, Ordering::Relaxed);
    PANIC_LED_ARMED.store(true, Ordering::Relaxed);
}

/// The build identity this image carries, as ONE contiguous string literal.
///
/// It is printed verbatim after `[FW_BUILD] ` by the boot line and by the
/// periodic banner task, so the bytes a reader sees on the debug port are
/// literally these bytes. Being a single literal is the point: the flash
/// runner greps the same text out of the flat image it is about to write
/// (`leviculum-nrf/tools/fw-readback.sh`, `fw_image_stamp`), so what it
/// expects to read back is taken from the image rather than from the working
/// tree. Two `env!` arguments formatted at the call site would put the sha in
/// its own rodata entry with nothing next to it to grep for, and whether it
/// landed beside the `[FW_BUILD]` literal would be a property of rustc's
/// constant pooling — true today, not promised tomorrow.
///
/// `dirty` is part of the stamp, not decoration: two images built from the
/// same commit with different working trees are different images, and a rig
/// that confirmed only the sha would call them the same one (Codeberg #343).
///
/// `version` is the crate's, the number scripts/bump-touched.py moves at
/// every landing that touched this firmware (1226). It comes last, so every
/// reader that matches `git_sha=<sha> dirty=<bool>` reads it unchanged.
pub const FW_BUILD_STAMP: &str = concat!(
    "git_sha=",
    env!("LEVICULUM_GIT_SHA"),
    " dirty=",
    env!("LEVICULUM_GIT_DIRTY"),
    " version=",
    env!("CARGO_PKG_VERSION")
);

static HARDFAULT_LED_ARMED: AtomicBool = AtomicBool::new(false);
static HARDFAULT_LED_PORT: AtomicU8 = AtomicU8::new(0);
static HARDFAULT_LED_PIN: AtomicU8 = AtomicU8::new(0);
static HARDFAULT_LED_ACTIVE_LOW: AtomicBool = AtomicBool::new(false);

/// Arm the HardFault-handler LED. Same pattern as `set_panic_led` but
/// for the cortex-m HardFault exception path. Picking a different LED
/// (e.g. blue while panic uses green) lets the operator distinguish a
/// `panic!()` from a HardFault at a glance: blue blink ≈ HardFault,
/// green blink ≈ panic, all dark ≈ async deadlock or DefaultHandler
/// halt with no LED armed.
pub fn set_hardfault_led(port: u8, pin: u8, active_low: bool) {
    HARDFAULT_LED_PORT.store(port, Ordering::Relaxed);
    HARDFAULT_LED_PIN.store(pin, Ordering::Relaxed);
    HARDFAULT_LED_ACTIVE_LOW.store(active_low, Ordering::Relaxed);
    HARDFAULT_LED_ARMED.store(true, Ordering::Relaxed);
}

#[cfg(not(test))]
mod panic_handler {
    use core::panic::PanicInfo;
    use core::sync::atomic::Ordering;

    #[panic_handler]
    fn panic(info: &PanicInfo) -> ! {
        // Codeberg #65: count every panic across soft-reset cycles.
        leviculum_firmware::postmortem::bump_panic_count();
        // Capture the panic message into `.retained` so the next boot can
        // log it.
        leviculum_firmware::postmortem::record_panic(info);

        // No LED configured: reset immediately so the next boot logs the
        // post-mortem. Else blink (~5 s of visual indication), then reset.
        let super::postmortem::FaultExit::BlinkThenReset { cycles } =
            super::postmortem::fault_exit(super::PANIC_LED_ARMED.load(Ordering::Relaxed))
        else {
            cortex_m::peripheral::SCB::sys_reset();
        };

        // Direct register pokes to avoid any allocation or HAL state.
        let led = super::postmortem::led_registers(
            super::PANIC_LED_PORT.load(Ordering::Relaxed),
            super::PANIC_LED_PIN.load(Ordering::Relaxed),
            super::PANIC_LED_ACTIVE_LOW.load(Ordering::Relaxed),
        );
        unsafe { super::blink_then_reset(led, cycles) }
    }
}
/// The boot diagnostics every board binary logs once USB is up, in one
/// place so the bins cannot drift apart again: the RAK4631 bin once logged
/// neither the reset reason nor the stack trajectory the T114 did (review
/// finding L-0031), on the board that needed them most. All lines are
/// critical, so they pass the runtime-drain gate and land in the persistent
/// tail before any host is attached.
///
/// In order: the boot stack watermark, the panic count, how far the
/// previous boot got and what reset it (both from
/// [`boot_trace::capture`] at the top of `main`, where POWER was still
/// ours; after `Softdevice::enable` it belongs to the SD), the boot count
/// (the half of that question retained RAM cannot answer across a power
/// loss, #380), the IRQ priorities, the post-mortem block, and last
/// boot's persistent tail as `[PERSISTENT_LOG]` lines.
pub fn log_boot_diagnostics(
    boot: &leviculum_firmware::boot_trace::Captured,
    boot_count: &boot_count::Recorded,
    hardfault_pm: Option<&HardfaultPostMortem>,
    panic_pm: Option<&PanicPostMortem>,
    persistent_log: Option<log::PersistentLogSnapshot>,
) {
    log_stack("boot");
    log_panic_count();
    leviculum_firmware::boot_trace::log_prev(boot);
    log_reset_reason(boot.reset_reason);
    // `WAKE reason=` and, on a boot that found the button held,
    // `FACTORY_RESET done=` (order 1511).
    #[cfg(feature = "softdevice")]
    power::log_banner(boot.reset_reason);
    boot_count::log_banner(boot_count);
    log_irq_priorities();
    // Shared boot/query formatter: the same block is retrievable at any
    // later time via the debug-port query (`p` byte, postmortem_query).
    log_postmortems(hardfault_pm, panic_pm);
    if let Some(snap) = persistent_log {
        // Which lines of last boot's tail are replayed:
        // `leviculum_boot_trace::tail::replay_lines`.
        for line in
            leviculum_boot_trace::tail::replay_lines(&snap.bytes[..snap.len], snap.went_round)
        {
            crate::log_critical!("[PERSISTENT_LOG] {}", line);
        }
    }
}

/// The periodic diagnostics every board binary runs: `[HEAP]` every 30 s
/// and `[STACK]` every 2 s. Shared with [`log_boot_diagnostics`] for the
/// same reason: a board that does not spawn the stack watermark replays
/// no stack trajectory after a crash.
pub fn spawn_diagnostics_tasks(spawner: &embassy_executor::Spawner) {
    spawner.must_spawn(heap_watermark_task());
    spawner.must_spawn(stack_watermark_task());
}

/// Cortex-M HardFault handler — overrides the cortex-m-rt default which
/// would just `wfi` forever and leave us with no visual cue. If a board
/// armed `set_hardfault_led`, blink that LED briefly so the operator
/// sees the fault in real time, capture an `ExceptionFrame` snapshot to
/// `.uninit` RAM that survives the soft-reset, then `sys_reset` so the
/// next boot can log the post-mortem from a working executor.
#[cfg(not(test))]
#[cortex_m_rt::exception]
unsafe fn HardFault(ef: &cortex_m_rt::ExceptionFrame) -> ! {
    // Codeberg #65: count every HardFault across soft-reset cycles.
    leviculum_firmware::postmortem::bump_panic_count();
    // Save register snapshot first — even if the LED arming is absent
    // (unlikely on a configured board), the post-mortem is the
    // diagnostic of record.
    leviculum_firmware::postmortem::record_hardfault(
        &leviculum_firmware::postmortem::HardfaultRegisters {
            pc: ef.pc(),
            lr: ef.lr(),
            r0: ef.r0(),
            r1: ef.r1(),
            r2: ef.r2(),
            r3: ef.r3(),
            r12: ef.r12(),
            xpsr: ef.xpsr(),
        },
    );

    // No LED configured: reset immediately so the next boot can log the
    // post-mortem. Else blink (~5 s of visual indication), then reset.
    let postmortem::FaultExit::BlinkThenReset { cycles } =
        postmortem::fault_exit(HARDFAULT_LED_ARMED.load(Ordering::Relaxed))
    else {
        cortex_m::peripheral::SCB::sys_reset();
    };
    let led = postmortem::led_registers(
        HARDFAULT_LED_PORT.load(Ordering::Relaxed),
        HARDFAULT_LED_PIN.load(Ordering::Relaxed),
        HARDFAULT_LED_ACTIVE_LOW.load(Ordering::Relaxed),
    );
    blink_then_reset(led, cycles)
}

/// The fault blink both handlers end in: `cycles` on/off periods of the LED
/// `led` names, then `sys_reset` so the next boot can log the captured
/// post-mortem. Direct register pokes, no allocation, no HAL state.
///
/// # Safety
///
/// Writes the GPIO registers `led` names; only for a fault handler, which
/// owns the core until the reset.
#[cfg(not(test))]
unsafe fn blink_then_reset(led: postmortem::LedRegisters, cycles: u32) -> ! {
    core::ptr::write_volatile(led.dirset as *mut u32, led.mask);
    for _ in 0..cycles {
        core::ptr::write_volatile(led.on as *mut u32, led.mask);
        for _ in 0..2_000_000u32 {
            cortex_m::asm::nop();
        }
        core::ptr::write_volatile(led.off as *mut u32, led.mask);
        for _ in 0..2_000_000u32 {
            cortex_m::asm::nop();
        }
    }
    cortex_m::peripheral::SCB::sys_reset();
}

// The node loop, once (#469 step 7): the skeleton a board binary runs
// between its own wiring. Gated like `pn`, which it drives.
#[cfg(feature = "softdevice")]
pub mod node_loop;
