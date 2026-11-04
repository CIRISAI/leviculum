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

extern crate alloc;

pub mod boards;
pub mod flash;
pub mod handheld;
pub mod serial_iface;
pub mod sx1262;

use core::alloc::{GlobalAlloc, Layout};
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};

use embedded_alloc::LlffHeap as Heap;
use esp_hal::{
    clock::CpuClock,
    peripherals::{Peripherals, USB_DEVICE},
    time::{Duration, Instant},
    uart::Uart,
    usb::usb_serial_jtag::UsbSerialJtag,
    Blocking,
};

use boards::BoardConfig;

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

/// The allocator, counted: what is live now and the most that was since
/// [`heap_reset_peak`]. The count is what a board's census reads
/// (`heap-budget`-style, order 1361); two atomics per call, against a
/// linked-list search that costs far more.
struct CountedHeap {
    heap: Heap,
    live: AtomicUsize,
    peak: AtomicUsize,
}

// SAFETY: every call goes to the inner allocator unchanged; the counts are
// bookkeeping beside it and never decide what is handed out.
unsafe impl GlobalAlloc for CountedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract, passed through.
        let ptr = unsafe { self.heap.alloc(layout) };
        if !ptr.is_null() {
            let live = self.live.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            self.peak.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.live.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller's contract, passed through.
        unsafe { self.heap.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static HEAP: CountedHeap = CountedHeap {
    heap: Heap::empty(),
    live: AtomicUsize::new(0),
    peak: AtomicUsize::new(0),
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

/// Bytes the allocator still has.
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
pub const HEAP_SIZE: usize = 64 * 1024;

/// Initialise the heap allocator. Called once by [`init`].
fn init_heap() {
    static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
    // SAFETY: called once at startup, before anything concurrent exists.
    // `addr_of!` avoids forming a reference to the `static mut`.
    unsafe {
        let heap_start = core::ptr::addr_of!(HEAP_MEM) as usize;
        HEAP.heap.init(heap_start, HEAP_SIZE);
    }
}

/// Give the allocator `size` bytes at `start` instead of the [`HEAP_SIZE`]
/// array: a board whose heap lives elsewhere, the M9's UI in PSRAM (order
/// 1361), calls this after [`init_soc`] and before anything allocates.
///
/// # Safety
///
/// Called once, instead of [`init`]'s heap, before any allocation; the
/// region is mapped, writable, and used by nothing else for as long as the
/// image runs.
pub unsafe fn init_heap_at(start: usize, size: usize) {
    // SAFETY: the caller's contract.
    unsafe { HEAP.heap.init(start, size) }
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
/// keeps `SwFlowControl` in its config and never programs it. Instead
/// nothing the host sends waits in the 128-byte receive FIFO while the
/// board writes: every write drains the FIFO into [`RX_KEEP`] bytes of
/// memory between FIFO-sized pieces, so a frame that arrives during a
/// 5.7 s frame dump is read when the dump is done, not lost.
pub struct UartLog<'d> {
    uart: Uart<'d, Blocking>,
    /// What the host sent while the board was writing.
    rx: alloc::collections::VecDeque<u8>,
    /// Bytes lost because `rx` was full.
    rx_lost: u32,
}

/// The most host bytes kept while the board writes: a dump's 5.7 s at
/// 115200 baud is at most 66 KB from a host that never stops, far more
/// than any host sends; 8 KiB is fourteen full frames.
pub const RX_KEEP: usize = 8 * 1024;

/// How much is written between two reads of the receive FIFO: less than
/// the FIFO's 128 bytes arrive in the time this many leave.
const TX_PIECE: usize = 96;

impl<'d> UartLog<'d> {
    /// Log over `uart`, already configured and pinned.
    pub fn new(uart: Uart<'d, Blocking>) -> Self {
        Self {
            uart,
            rx: alloc::collections::VecDeque::new(),
            rx_lost: 0,
        }
    }

    /// Host bytes lost because the board's keep was full.
    pub fn rx_lost(&self) -> u32 {
        self.rx_lost
    }

    /// One line, shaped by [`leviculum_log_line::format_line`] as
    /// [`UsbLog::line`] shapes it.
    pub fn line(&mut self, prefix: &str, args: core::fmt::Arguments) {
        let mut buf = [0u8; LINE_MAX];
        let line = leviculum_log_line::format_line(&mut buf, prefix, args, uptime_ms());
        let mut out = [0u8; 4 * LINE_MAX];
        let mut n = 0;
        for byte in line {
            match byte {
                0x7E | 0x7D | 0x11 | 0x13 => {
                    for (i, b) in [b'\\', b'x', hex_digit(byte >> 4), hex_digit(byte & 0xF)]
                        .into_iter()
                        .enumerate()
                    {
                        out[n + i] = b;
                    }
                    n += 4;
                }
                _ => {
                    out[n] = *byte;
                    n += 1;
                }
            }
        }
        self.write_all(&out[..n]);
    }

    /// One raw line, `text` and a line end, as it is: for the lines read
    /// whole by their first word. `text` must be plain ASCII.
    pub fn raw(&mut self, text: &str) {
        self.write_all(text.as_bytes());
        self.write_all(b"\r\n");
    }

    /// One HDLC frame of `payload`, flags included (`leviculum_core`'s
    /// codec), between lines, its XON and XOFF bytes escaped
    /// (`leviculum_serial_mux::escape_flow`).
    pub fn frame(&mut self, payload: &[u8]) {
        let mut framed = alloc::vec::Vec::new();
        leviculum_core::framing::hdlc::frame(payload, &mut framed);
        let mut wire = alloc::vec::Vec::with_capacity(framed.len() + 8);
        leviculum_serial_mux::escape_flow(&framed, &mut |piece| wire.extend_from_slice(piece));
        self.write_all(&wire);
    }

    /// All of `bytes`: esp-hal's `write` places what fits in the FIFO and
    /// says how much. The receive FIFO is drained between pieces. A TX
    /// error drops the rest of the write, never the boot.
    fn write_all(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            self.drain_rx();
            let piece = bytes.len().min(TX_PIECE);
            match self.uart.write(&bytes[..piece]) {
                Ok(n) => bytes = &bytes[n..],
                Err(_) => return,
            }
        }
        self.drain_rx();
    }

    /// Move what the receive FIFO holds into the keep.
    fn drain_rx(&mut self) {
        let mut buf = [0u8; 128];
        loop {
            let n = self.uart.read_buffered(&mut buf).unwrap_or(0);
            if n == 0 {
                return;
            }
            for &byte in &buf[..n] {
                if self.rx.len() >= RX_KEEP {
                    self.rx_lost = self.rx_lost.saturating_add(1);
                } else {
                    self.rx.push_back(byte);
                }
            }
        }
    }

    /// The bytes the host sent that have arrived, without waiting: how many
    /// went into `buf`, the kept ones first.
    pub fn read_ready(&mut self, buf: &mut [u8]) -> usize {
        self.drain_rx();
        let n = buf.len().min(self.rx.len());
        for (slot, byte) in buf.iter_mut().zip(self.rx.drain(..n)) {
            *slot = byte;
        }
        n
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
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let baud = PANIC_UART0_BAUD.load(Ordering::Relaxed);
    if baud != 0 {
        // SAFETY: the panicking context owns nothing any more; a fresh
        // driver on UART0, whose pins the board routed at boot, is the only
        // way left to say why.
        let uart0 = unsafe { esp_hal::peripherals::UART0::steal() };
        if let Ok(uart) = Uart::new(uart0, esp_hal::uart::Config::default().with_baudrate(baud)) {
            UartLog::new(uart).line("[PANIC] ", format_args!("{info}"));
        }
    }
    loop {
        core::hint::spin_loop();
    }
}
