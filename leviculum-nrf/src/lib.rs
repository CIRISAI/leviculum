//! Reticulum firmware support for nRF52840-based boards
//!
//! Provides heap allocator setup, board-specific pin mappings,
//! USB CDC-ACM debug logging, and Reticulum transport interface.

#![no_std]

extern crate alloc;

// Bug #32 spike: exactly one BSP feature must be enabled. Mutex
// enforced at compile time so any binary forgets-to-select-a-BSP or
// sets-both fails fast at `cargo build`, not at link or runtime.
#[cfg(all(feature = "bsp-rak4631", feature = "bsp-t114"))]
compile_error!("`bsp-rak4631` and `bsp-t114` are mutually exclusive — pick exactly one");

#[cfg(not(any(feature = "bsp-rak4631", feature = "bsp-t114")))]
compile_error!("must enable exactly one of `bsp-rak4631` or `bsp-t114`");

pub mod ble;
pub mod boards;
pub mod clock;
pub mod flash;
pub mod interface;
pub mod log;
pub mod lora;
pub mod radio_store;
pub mod rng;
// T114 ST7789 status display — rides with the BSP (not the V2's
// `display` feature): the panel is write-only, presence detection is
// impossible, and blind-driving it is safe on panel-less boards, so one
// t114 UF2 serves both populations. See `st7789.rs` module docs.
#[cfg(feature = "bsp-t114")]
pub mod st7789;
pub mod sx1262;
pub mod usb;

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
            "rng={:?} usbd={:?} twispi0={:?} saadc={:?} spi2={:?} spim3={:?} uarte0={:?} gpiote={:?} rtc1={:?}",
            Interrupt::RNG.get_priority(),
            Interrupt::USBD.get_priority(),
            Interrupt::TWISPI0.get_priority(),
            Interrupt::SAADC.get_priority(),
            Interrupt::SPI2.get_priority(),
            Interrupt::SPIM3.get_priority(),
            Interrupt::UARTE0.get_priority(),
            Interrupt::GPIOTE.get_priority(),
            Interrupt::RTC1.get_priority(),
        ),
    );
}

/// Read, print, and clear POWER.RESETREAS.
///
/// MUST run before `Softdevice::enable` — once the SD owns POWER this
/// register is only reachable via `sd_power_reset_reason_get/clr`.
/// Cleared after the print (write-1-to-clear: writing back the read
/// value clears exactly the latched bits) so every boot reports only
/// its own cause. An all-zero raw value on a boot that was clearly a
/// reset (not first power-up) is itself a diagnosis: POR and brownout
/// latch NO bit, so raw=0x0 in a boot loop points at the supply.
pub fn log_reset_reason() {
    // embassy-nrf 0.9 keeps its pac crate-private, so raw volatile
    // access: POWER.RESETREAS at 0x40000000 + 0x400. Bit layout from
    // the nRF52840 product spec (matches nrf-pac's Resetreas): 0
    // RESETPIN, 1 DOG, 2 SREQ, 3 LOCKUP, 16 OFF, 17 LPCOMP, 18 DIF,
    // 19 NFC, 20 VBUS.
    const POWER_RESETREAS: *mut u32 = 0x4000_0400 as *mut u32;
    let raw = unsafe { core::ptr::read_volatile(POWER_RESETREAS) };
    log::log_fmt_critical(
        "[RESET_REASON] ",
        format_args!(
            "raw=0x{:08x} resetpin={} dog={} sreq={} lockup={} off={} lpcomp={} dif={} nfc={} vbus={}",
            raw,
            (raw & 1 << 0 != 0) as u8,
            (raw & 1 << 1 != 0) as u8,
            (raw & 1 << 2 != 0) as u8,
            (raw & 1 << 3 != 0) as u8,
            (raw & 1 << 16 != 0) as u8,
            (raw & 1 << 17 != 0) as u8,
            (raw & 1 << 18 != 0) as u8,
            (raw & 1 << 19 != 0) as u8,
            (raw & 1 << 20 != 0) as u8,
        ),
    );
    unsafe { core::ptr::write_volatile(POWER_RESETREAS, raw) };
}

// RAK19026 baseboard peripherals — each gated on its own feature so the
// bare nRF52840 + SX1262 build (T114, RAK4631 module without baseboard)
// stays unchanged.
#[cfg(any(feature = "display", feature = "gnss", feature = "battery"))]
pub mod baseboard;
#[cfg(feature = "battery")]
pub mod battery;
#[cfg(feature = "display")]
pub mod button;
#[cfg(feature = "display")]
pub mod display;
#[cfg(feature = "gnss")]
pub mod gnss;
#[cfg(feature = "display")]
pub mod led;

/// Install the tracing subscriber that routes `leviculum-core` log events
/// to the CDC-ACM debug port via LOG_CHANNEL.
///
/// Call once at startup before any tracing macros fire. Without this,
/// all `tracing::debug!()` / `tracing::info!()` etc. from leviculum-core
/// are silently dropped (no subscriber registered).
pub fn init_tracing() {
    use tracing_core::dispatcher;
    let subscriber = log::TracingSubscriber;
    let dispatch = dispatcher::Dispatch::new(subscriber);
    let _ = dispatcher::set_global_default(dispatch);
}

use core::mem::MaybeUninit;
use embedded_alloc::LlffHeap as Heap;

#[global_allocator]
static HEAP: Heap = Heap::empty();

/// Heap size in bytes (64 KiB).
///
/// Bumped DOWN from 96 KiB after the Bug #32 RNG fix. Sequence:
/// 1. Direct-RNG-access panic (~26 s cycle) was the dominant fault.
/// 2. RNG fix landed (commit f093099) → device runs longer, exposing
///    a SECOND failure mode: stack overflow into BSS, manifesting as
///    HardFault in `embassy_sync::AtomicWaker::wake` (Cell::replace
///    on corrupted memory), `embedded_alloc::llff:78` allocator
///    panic, or PC=0x0 jumps. Stack peak runs into __ebss territory.
/// 3. Reducing heap from 96 KiB to 64 KiB shifts __ebss DOWN by 32
///    KiB, giving stack 32 KiB more headroom. Observed `heap_used`
///    peak is ~50 KiB (78% of 64 KiB pool); fits with 14 KiB margin.
///    With 96 KiB pool, stack peak crashed into BSS within ~25 s.
///
/// Earlier "stack_free=0 was a measurement artifact" diagnosis (in
/// Stage 1B's commit message) was WRONG — the artifact was real
/// for the OLD stack_free implementation (commit 42f05e2 fixes that),
/// but stack overflow at HEAP=96K is also real. Both bugs coexist.
const HEAP_SIZE: usize = 96 * 1024;

/// Return (used, free) heap bytes at this instant.
pub fn heap_stats() -> (usize, usize) {
    (HEAP.used(), HEAP.free())
}

/// High-watermark of heap usage since boot (bytes). Updated by
/// [`heap_watermark_task`]; resets with every boot — the periodic
/// `[HEAP]` log lines (which land in the persistent tail) carry the
/// history across resets. Codeberg #65 instrumentation.
static HEAP_WATERMARK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Sample the heap, update the high-watermark, and return
/// `(used, free, watermark)`.
pub fn heap_watermark_sample() -> (usize, usize, usize) {
    let (used, free) = heap_stats();
    let watermark = HEAP_WATERMARK
        .fetch_max(used, core::sync::atomic::Ordering::Relaxed)
        .max(used);
    (used, free, watermark)
}

/// Periodic heap telemetry (Codeberg #65): one `[HEAP]` line every 30 s
/// into the normal log path, which feeds both the USB debug capture and
/// the persistent tail in `.uninit` — so an OOM panic's post-mortem boot
/// replays the heap trajectory leading up to it. Strictly additive: the
/// cadence is coarse and the sampling is two atomic loads; the alloc
/// path itself is untouched.
#[embassy_executor::task]
pub async fn heap_watermark_task() {
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(30)).await;
        let (used, free, watermark) = heap_watermark_sample();
        crate::log::log_fmt(
            "[HEAP] ",
            format_args!("used={used} free={free} watermark={watermark} size={HEAP_SIZE}"),
        );
    }
}

/// Persistent panic counter (Codeberg #65): lives in `.uninit` next to
/// the post-mortems and — unlike them — is NOT cleared by the boot-time
/// read, so it accumulates across panic/HardFault soft-reset cycles
/// until power loss. Cold boots start at 0 via the magic check.
#[repr(C)]
struct PanicCountRaw {
    magic: u32,
    count: u32,
}

const PANIC_COUNT_MAGIC: u32 = 0xC01D_FACE;

#[link_section = ".uninit"]
static mut PANIC_COUNT: core::mem::MaybeUninit<PanicCountRaw> = core::mem::MaybeUninit::uninit();

/// Increment the persistent panic counter and return the new value.
/// Called on every panic and HardFault path before the soft-reset.
/// Volatile raw-pointer access only — runs in fault context.
fn bump_panic_count() -> u32 {
    unsafe {
        let p = core::ptr::addr_of_mut!(PANIC_COUNT).cast::<PanicCountRaw>();
        let magic = core::ptr::read_volatile(core::ptr::addr_of!((*p).magic));
        let prev = if magic == PANIC_COUNT_MAGIC {
            core::ptr::read_volatile(core::ptr::addr_of!((*p).count))
        } else {
            0
        };
        let next = prev.wrapping_add(1);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).count), next);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).magic), PANIC_COUNT_MAGIC);
        next
    }
}

/// Read the persistent panic counter WITHOUT clearing it (boot banner,
/// debug captures). 0 on cold boot or never-panicked.
pub fn panic_count() -> u32 {
    unsafe {
        let p = core::ptr::addr_of!(PANIC_COUNT).cast::<PanicCountRaw>();
        if core::ptr::read_volatile(core::ptr::addr_of!((*p).magic)) == PANIC_COUNT_MAGIC {
            core::ptr::read_volatile(core::ptr::addr_of!((*p).count))
        } else {
            0
        }
    }
}

/// Sentinel word `paint_stack` fills the unused stack with. Distinct
/// from both `.uninit` post-mortem magics, so a stray match cannot be
/// mistaken for a valid record.
const STACK_CANARY: u32 = 0xDEAD_BEEF;

/// Low and high address of the stack region, straight from the linker.
///
/// **flip-link layout** (this crate links with `flip-link`, see
/// `.cargo/config.toml`): the stack sits at the BOTTOM of RAM, below
/// `.data`/`.bss`, and grows DOWN toward `_stack_end`:
///
/// ```text
///   _stack_end  = ORIGIN(RAM) = 0x200030e0   <- SoftDevice RAM floor
///        |  stack, grows DOWN  ^
///   _stack_start = __sdata     |             <- SP at reset
///        .data / .bss
///   __ebss
///        .uninit (post-mortems, persistent tail)
///   RAM end     = 0x20040000
/// ```
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

/// Paint the unused stack with [`STACK_CANARY`]. Call once at the start
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
    let (lo, hi) = stack_region();
    let limit = stack_pointer().saturating_sub(SP_MARGIN).min(hi);
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
        log_stack("tick");
    }
}

/// Initialize the heap allocator
///
/// Must be called once before any `alloc` usage (Vec, String, etc.).
/// Typically called at the start of the firmware entry point.
pub fn init_heap() {
    static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
    // SAFETY: Called once at startup before any concurrent access.
    // Use addr_of! to avoid creating a reference to the static mut.
    unsafe {
        let heap_start = core::ptr::addr_of!(HEAP_MEM) as usize;
        HEAP.init(heap_start, HEAP_SIZE);
    }
}

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

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

/// Maximum bytes of panic message preserved across the post-mortem soft-reset.
/// Bumped to 1024 (was 256) so the SoftDevice's "memory access violation"
/// message — which encodes PC + PREGION at the END of the string — survives
/// the buffer cap. The 256-char limit was cutting off precisely the
/// diagnostic data we need to pinpoint the offending instruction.
pub const PANIC_MSG_MAX: usize = 1024;

/// Snapshot returned by `take_panic_postmortem()`.
#[derive(Clone, Copy)]
pub struct PanicPostMortem {
    /// Bytes valid in `bytes` (== `total` unless the message overflowed
    /// the buffer, in which case these are the LAST `PANIC_MSG_MAX`
    /// bytes of it).
    pub len: usize,
    /// Total bytes the panic formatter produced. `total > len` means
    /// the front of the message was dropped — never the tail, where SD
    /// fault panics carry PC/PREGION.
    pub total: usize,
    pub bytes: [u8; PANIC_MSG_MAX],
}

#[repr(C)]
struct PanicPmRaw {
    magic: u32,
    len: u32,
    bytes: [u8; PANIC_MSG_MAX],
}

/// Record version, encoded in the magic. `.uninit` survives a reflash,
/// so a record written by an older image is still there on the first
/// boot of a new one. `len` used to mean "bytes stored, clamped"; it now
/// means "total bytes produced", with `bytes` a ring keeping the last
/// `PANIC_MSG_MAX` of them — the same word read the old way linearises
/// an overlong message at the wrong offset. Bump this whenever the
/// record's layout or the meaning of a field changes, so stale records
/// are rejected instead of misparsed.
const PANIC_PM_MAGIC: u32 = 0xBADD_CAF1;

#[link_section = ".uninit"]
static mut PANIC_PM: core::mem::MaybeUninit<PanicPmRaw> = core::mem::MaybeUninit::uninit();

/// Read and clear the panic message captured before the last soft-reset.
/// Returns `Some(_)` exactly once after a panic, `None` otherwise.
pub fn take_panic_postmortem() -> Option<PanicPostMortem> {
    unsafe {
        let p = core::ptr::addr_of_mut!(PANIC_PM).cast::<PanicPmRaw>();
        let magic = core::ptr::read_volatile(core::ptr::addr_of!((*p).magic));
        if magic != PANIC_PM_MAGIC {
            return None;
        }
        let total = core::ptr::read_volatile(core::ptr::addr_of!((*p).len)) as usize;
        // The panic handler writes the buffer as a ring keeping the LAST
        // `PANIC_MSG_MAX` bytes; on overflow the oldest surviving byte
        // sits at `total % PANIC_MSG_MAX`. Linearise on the way out.
        let (start, len) = if total > PANIC_MSG_MAX {
            (total % PANIC_MSG_MAX, PANIC_MSG_MAX)
        } else {
            (0, total)
        };
        let mut bytes = [0u8; PANIC_MSG_MAX];
        let src = core::ptr::addr_of!((*p).bytes).cast::<u8>();
        for (i, slot) in bytes.iter_mut().enumerate().take(len) {
            *slot = core::ptr::read_volatile(src.add((start + i) % PANIC_MSG_MAX));
        }
        // Clear magic so subsequent boots don't re-log.
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).magic), 0);
        Some(PanicPostMortem { len, total, bytes })
    }
}

#[cfg(not(test))]
mod panic_handler {
    use core::panic::PanicInfo;
    use core::sync::atomic::Ordering;

    /// Ring writer for `core::fmt::write` — keeps the LAST `buf.len()`
    /// bytes of everything written (`pos` counts total bytes, storage
    /// wraps). On overflow the front of the message is sacrificed, not
    /// the tail: SD fault panics put PC/PREGION at the END, behind an
    /// expendable source-path prefix. Used inside the panic handler
    /// where allocation must not happen.
    struct ByteWriter<'a> {
        buf: &'a mut [u8],
        pos: usize,
    }
    impl core::fmt::Write for ByteWriter<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for &b in s.as_bytes() {
                self.buf[self.pos % self.buf.len()] = b;
                self.pos += 1;
            }
            Ok(())
        }
    }

    #[panic_handler]
    fn panic(info: &PanicInfo) -> ! {
        // Codeberg #65: count every panic across soft-reset cycles.
        super::bump_panic_count();
        // Capture the panic message into `.uninit` so the next boot can
        // log it. LOG_CHANNEL is unusable here — the executor is dead
        // and would never drain it.
        unsafe {
            let p = core::ptr::addr_of_mut!(super::PANIC_PM).cast::<super::PanicPmRaw>();
            let buf_ptr = core::ptr::addr_of_mut!((*p).bytes).cast::<u8>();
            let buf_slice = core::slice::from_raw_parts_mut(buf_ptr, super::PANIC_MSG_MAX);
            let mut writer = ByteWriter {
                buf: buf_slice,
                pos: 0,
            };
            let _ = core::fmt::write(&mut writer, format_args!("{}", info));
            core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).len), writer.pos as u32);
            // Magic last — partial write must not appear valid.
            core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).magic), super::PANIC_PM_MAGIC);
        }

        if !super::PANIC_LED_ARMED.load(Ordering::Relaxed) {
            // No LED configured — reset immediately so the next boot
            // logs the post-mortem.
            cortex_m::peripheral::SCB::sys_reset();
        }

        // Direct register pokes to avoid any allocation or HAL state.
        // P0_BASE = 0x5000_0000, P1_BASE = 0x5000_0300.
        let port = super::PANIC_LED_PORT.load(Ordering::Relaxed);
        let pin = super::PANIC_LED_PIN.load(Ordering::Relaxed);
        let active_low = super::PANIC_LED_ACTIVE_LOW.load(Ordering::Relaxed);
        let port_base: u32 = if port == 1 { 0x5000_0300 } else { 0x5000_0000 };
        let pin_mask: u32 = 1u32 << (pin & 31);
        let dirset = port_base + 0x518;
        let outset = port_base + 0x508;
        let outclr = port_base + 0x50C;
        // (set_on, set_off): write to OUTCLR to drive low, OUTSET to drive high
        let (reg_on, reg_off) = if active_low {
            (outclr, outset)
        } else {
            (outset, outclr)
        };

        unsafe {
            core::ptr::write_volatile(dirset as *mut u32, pin_mask);
        }

        // Blink ~25 cycles (~5 s of visual indication), then reset so the
        // next boot can log the captured panic message.
        for _ in 0..25u32 {
            unsafe {
                core::ptr::write_volatile(reg_on as *mut u32, pin_mask);
            }
            for _ in 0..2_000_000u32 {
                cortex_m::asm::nop();
            }
            unsafe {
                core::ptr::write_volatile(reg_off as *mut u32, pin_mask);
            }
            for _ in 0..2_000_000u32 {
                cortex_m::asm::nop();
            }
        }
        cortex_m::peripheral::SCB::sys_reset();
    }
}

/// HardFault post-mortem snapshot saved into a `.uninit` static so it
/// survives `sys_reset()`. The next boot reads it via
/// `take_hardfault_postmortem()` to log the faulting PC and the
/// register set, allowing post-hoc address-to-source resolution with
/// `arm-none-eabi-addr2line`.
#[repr(C)]
pub struct HardfaultPostMortem {
    /// `PM_MAGIC` when valid. Set on fault, cleared after one successful read.
    magic: u32,
    pub pc: u32,
    pub lr: u32,
    pub r0: u32,
    pub r1: u32,
    pub r2: u32,
    pub r3: u32,
    pub r12: u32,
    pub xpsr: u32,
}

/// Distinct from `paint_stack`'s `0xDEADBEEF` canary so a stray canary
/// word cannot masquerade as a valid PM. Versioned like
/// [`PANIC_PM_MAGIC`]: bump it if this record's layout ever changes, so
/// a record left in `.uninit` by an older image is rejected rather than
/// misparsed.
const HARDFAULT_PM_MAGIC: u32 = 0xC0FF_EE12;

/// Survives `sys_reset` because `.uninit` is `NOLOAD` in cortex-m-rt's
/// link.x — values in RAM are not zeroed by the runtime startup.
#[link_section = ".uninit"]
static mut HARDFAULT_PM: core::mem::MaybeUninit<HardfaultPostMortem> =
    core::mem::MaybeUninit::uninit();

/// Read and clear the HardFault post-mortem captured before the last
/// soft-reset. Returns `Some(_)` once after a HardFault, `None`
/// otherwise (and `None` on every subsequent call until the next fault).
pub fn take_hardfault_postmortem() -> Option<HardfaultPostMortem> {
    unsafe {
        let p = core::ptr::addr_of_mut!(HARDFAULT_PM).cast::<HardfaultPostMortem>();
        let magic = core::ptr::read_volatile(core::ptr::addr_of!((*p).magic));
        if magic != HARDFAULT_PM_MAGIC {
            return None;
        }
        let pm = HardfaultPostMortem {
            magic: 0,
            pc: core::ptr::read_volatile(core::ptr::addr_of!((*p).pc)),
            lr: core::ptr::read_volatile(core::ptr::addr_of!((*p).lr)),
            r0: core::ptr::read_volatile(core::ptr::addr_of!((*p).r0)),
            r1: core::ptr::read_volatile(core::ptr::addr_of!((*p).r1)),
            r2: core::ptr::read_volatile(core::ptr::addr_of!((*p).r2)),
            r3: core::ptr::read_volatile(core::ptr::addr_of!((*p).r3)),
            r12: core::ptr::read_volatile(core::ptr::addr_of!((*p).r12)),
            xpsr: core::ptr::read_volatile(core::ptr::addr_of!((*p).xpsr)),
        };
        // Invalidate so we don't re-log on subsequent boots.
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).magic), 0);
        Some(pm)
    }
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
    bump_panic_count();
    // Save register snapshot first — even if the LED arming is absent
    // (unlikely on a configured board), the post-mortem is the
    // diagnostic of record.
    let p = core::ptr::addr_of_mut!(HARDFAULT_PM).cast::<HardfaultPostMortem>();
    core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).pc), ef.pc());
    core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).lr), ef.lr());
    core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).r0), ef.r0());
    core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).r1), ef.r1());
    core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).r2), ef.r2());
    core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).r3), ef.r3());
    core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).r12), ef.r12());
    core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).xpsr), ef.xpsr());
    // Magic last so a partial write can't masquerade as a valid PM.
    core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).magic), HARDFAULT_PM_MAGIC);

    if !HARDFAULT_LED_ARMED.load(Ordering::Relaxed) {
        // No LED configured — reset immediately so the next boot can
        // log the post-mortem.
        cortex_m::peripheral::SCB::sys_reset();
    }

    let port = HARDFAULT_LED_PORT.load(Ordering::Relaxed);
    let pin = HARDFAULT_LED_PIN.load(Ordering::Relaxed);
    let active_low = HARDFAULT_LED_ACTIVE_LOW.load(Ordering::Relaxed);
    let port_base: u32 = if port == 1 { 0x5000_0300 } else { 0x5000_0000 };
    let pin_mask: u32 = 1u32 << (pin & 31);
    let dirset = port_base + 0x518;
    let outset = port_base + 0x508;
    let outclr = port_base + 0x50C;
    let (reg_on, reg_off) = if active_low {
        (outclr, outset)
    } else {
        (outset, outclr)
    };

    core::ptr::write_volatile(dirset as *mut u32, pin_mask);

    // Blink ~25 cycles for ~5 s of visual indication, then reset so the
    // next boot can log the captured post-mortem.
    for _ in 0..25u32 {
        core::ptr::write_volatile(reg_on as *mut u32, pin_mask);
        for _ in 0..2_000_000u32 {
            cortex_m::asm::nop();
        }
        core::ptr::write_volatile(reg_off as *mut u32, pin_mask);
        for _ in 0..2_000_000u32 {
            cortex_m::asm::nop();
        }
    }
    cortex_m::peripheral::SCB::sys_reset();
}
