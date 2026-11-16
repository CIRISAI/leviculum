//! The GNSS receiver's UART (1477): UART1 at 115200 baud to the M9's
//! AG3352, its receive FIFO emptied by an interrupt into a keep the loop
//! reads, as the log's UART0 is (`crate::UartLog`).
//!
//! The receiver sends about 500 bytes in a burst once a second (GGA, the
//! GSV block and RMC). The FIFO holds 128 bytes, 11 ms of the line; core
//! 0's loop waits for interrupts between turns and a flash op holds them
//! off for up to 57 ms (window 20), so the bytes are taken by the
//! interrupt at half the FIFO and kept for the loop, and a flash op starts
//! with the FIFO emptied ([`drain`], called beside the log's). What a long
//! flash op still overruns is counted, not hidden: a lost sentence is the
//! next second's.
//!
//! What the bytes mean is `loquax_m9::gnss`'s; writes (the init's `$PAIR`
//! frames, a dozen bytes each, a handful per power-up) go straight into
//! the FIFO.

use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, Ordering};

use critical_section::Mutex;
use esp_hal::uart::{Config as UartConfig, RxConfig, RxError, Uart, UartInterrupt};
use esp_hal::Blocking;
use leviculum_serial_mux::rx::RxKeep;

/// The receiver's rate: the AG3352 answered `$PAIR021` at 115200 on the
/// bench (2026-10-26, `docs/src/concepts/thinknode-m9.md`, "GNSS").
pub const BAUD: u32 = 115_200;

/// What the keep holds: four seconds of the receiver's bursts.
pub const KEEP: usize = 2 * 1024;

/// The FIFO level at which the interrupt empties it: half of it.
const THRESHOLD: u16 = 64;

static PORT: Mutex<RefCell<Option<Uart<'static, Blocking>>>> = Mutex::new(RefCell::new(None));
static BYTES: Mutex<RefCell<RxKeep<KEEP>>> = Mutex::new(RefCell::new(RxKeep::new()));
static OVERRUNS: AtomicU32 = AtomicU32::new(0);
static ERRORS: AtomicU32 = AtomicU32::new(0);

/// The config UART1 runs with.
pub fn config() -> UartConfig {
    UartConfig::default()
        .with_baudrate(BAUD)
        .with_rx(RxConfig::default().with_fifo_full_threshold(THRESHOLD))
}

/// Hand `uart`, made with [`config`] and pinned, to the interrupt.
pub fn start(mut uart: Uart<'static, Blocking>) {
    uart.set_interrupt_handler(uart1_rx);
    critical_section::with(|cs| {
        uart.listen(UartInterrupt::RxFifoFull);
        PORT.borrow_ref_mut(cs).replace(uart);
    });
}

/// Empty the FIFO into the keep.
fn pull(uart: &mut Uart<'static, Blocking>, keep: &mut RxKeep<KEEP>) {
    let mut buf = [0u8; 128];
    for _ in 0..4 {
        match uart.read_buffered(&mut buf) {
            Ok(0) => return,
            Ok(n) => keep.push(&buf[..n]),
            Err(RxError::FifoOverflowed) => {
                OVERRUNS.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                ERRORS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[esp_hal::handler]
fn uart1_rx() {
    critical_section::with(|cs| {
        if let Some(uart) = PORT.borrow_ref_mut(cs).as_mut() {
            uart.clear_interrupts(UartInterrupt::RxFifoFull.into());
            pull(uart, &mut BYTES.borrow_ref_mut(cs));
        }
    });
}

/// Keep what the receiver sent while a flash op had core 0's interrupts
/// off (1613): the parked spin's ring, pushed here by
/// `corepark::hold` before this FIFO's interrupt resumes, so the
/// sentence stream stays in order. Until 1613 a burst under an erase
/// lost its tail to the FIFO.
pub(crate) fn absorb_parked(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    critical_section::with(|cs| BYTES.borrow_ref_mut(cs).push(bytes));
}

/// Empty the FIFO now: before a flash op, as the log's is.
pub fn drain() {
    critical_section::with(|cs| {
        if let Some(uart) = PORT.borrow_ref_mut(cs).as_mut() {
            pull(uart, &mut BYTES.borrow_ref_mut(cs));
        }
    });
}

/// The bytes the receiver sent since the last call, into `out`; how many.
pub fn take(out: &mut [u8]) -> usize {
    drain();
    critical_section::with(|cs| BYTES.borrow_ref_mut(cs).pop_into(out))
}

/// Write `frame` to the receiver; whether all of it went.
pub fn write(frame: &[u8]) -> bool {
    critical_section::with(|cs| match PORT.borrow_ref_mut(cs).as_mut() {
        Some(uart) => {
            let mut rest = frame;
            while !rest.is_empty() {
                match uart.write(rest) {
                    Ok(0) | Err(_) => return false,
                    Ok(n) => rest = &rest[n..],
                }
            }
            true
        }
        None => false,
    })
}

/// FIFO overruns, flagged bytes, and bytes the full keep dropped — the
/// last with what a full park ring counted during a flash op (1613).
pub fn losses() -> (u32, u32, u32) {
    let lost = critical_section::with(|cs| BYTES.borrow_ref(cs).lost());
    (
        OVERRUNS.load(Ordering::Relaxed),
        ERRORS.load(Ordering::Relaxed),
        lost.saturating_add(crate::corepark::gnss_ring_lost()),
    )
}
