//! The flash and a DMA out of PSRAM never at once (1507).
//!
//! The ESP32-S3 reaches its flash and its PSRAM over the one memory SPI
//! bus, and a flash write or erase runs with that bus's cache path off
//! (`flash.rs`, `parked`). The M9's panel flush is a DMA straight out of
//! the frame in PSRAM since 1507: one that ran across an erase would read
//! a bus that is not there. The structure review's M9 list says it as a
//! rule, "a flash writer in the gaps, never during a flush"
//! (`struktur-review-loquax-20261107.md`, 2.9).
//!
//! Two cores, two flags, no lock either side can die holding: the UI
//! core announces a transfer by the time it ends ([`dma_begin`]) and backs
//! off when a flash op is announced; the I/O core announces its op
//! ([`flash`]) and waits for the announced end to pass. Both announce
//! before they look (sequentially consistent), so at least one of them
//! sees the other. The end is a time, not a release: a UI core busy
//! folding a long page never holds the I/O core's write up past the
//! transfer it started.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::uptime_us;

/// A flash write or erase is announced or running.
static FLASH: AtomicBool = AtomicBool::new(false);
/// When the last announced DMA ends, microseconds on the wrapping `u32`
/// clock; 0 when none was.
static DMA_UNTIL: AtomicU32 = AtomicU32::new(0);

fn now() -> u32 {
    uptime_us() as u32
}

/// `until` lies after `now` on the wrapping clock.
fn ahead(until: u32, now: u32) -> bool {
    until != 0 && (until.wrapping_sub(now) as i32) > 0
}

/// How long `bytes` take on the panel's bus at `spi_mhz`, doubled, and a
/// tenth of a millisecond besides: the end the flash waits for.
pub fn transfer_us(bytes: usize, spi_mhz: u32) -> u32 {
    let bits = (bytes as u64) * 8;
    let us = bits / u64::from(spi_mhz.max(1));
    u32::try_from(us * 2 + 100).unwrap_or(u32::MAX)
}

/// The UI core may start a DMA out of PSRAM of `us` microseconds: `true`
/// when no flash op is announced, and the flash will wait for it.
pub fn dma_begin(us: u32) -> bool {
    let until = now().wrapping_add(us).max(1);
    DMA_UNTIL.store(until, Ordering::SeqCst);
    if FLASH.load(Ordering::SeqCst) {
        DMA_UNTIL.store(0, Ordering::SeqCst);
        return false;
    }
    true
}

/// The DMA the UI core started has ended before its announced time.
pub fn dma_done() {
    DMA_UNTIL.store(0, Ordering::SeqCst);
}

/// Run the flash op `f` once no DMA out of PSRAM runs, and none starts
/// until it is done.
pub fn flash<R>(f: impl FnOnce() -> R) -> R {
    FLASH.store(true, Ordering::SeqCst);
    while ahead(DMA_UNTIL.load(Ordering::SeqCst), now()) {
        core::hint::spin_loop();
    }
    let r = f();
    FLASH.store(false, Ordering::SeqCst);
    r
}
