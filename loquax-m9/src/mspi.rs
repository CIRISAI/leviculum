//! The flash and a DMA out of PSRAM never at once (1507), and the DMA
//! never starved by the flash (1563).
//!
//! The ESP32-S3 reaches its flash and its PSRAM over the one memory SPI
//! bus, and a flash write or erase runs with that bus's cache path off
//! (`leviculum-esp`'s `flash.rs`, `parked`). The M9's panel flush is a DMA
//! straight out of the frame in PSRAM since 1507: one that ran across an
//! erase would read a bus that is not there. The structure review's M9
//! list says it as a rule, "a flash writer in the gaps, never during a
//! flush" (`struktur-review-loquax-20261107.md`, 2.9).
//!
//! Two cores, no lock either side can die holding: the UI core announces
//! a transfer by the time it ends ([`Arbiter::dma_begin`]) and backs off
//! when a flash op is announced; the I/O core announces its op and waits
//! for the announced end to pass ([`Arbiter::flash`]). Both announce
//! before they look (sequentially consistent), so at least one of them
//! sees the other. The end is a time, not a release: a UI core busy
//! folding a long page never holds the I/O core's write up past the
//! transfer it started.
//!
//! Since 1563 the DMA that backed off is owed a gap. During a full pull
//! the I/O core writes flash op after op, and each op parks the UI core
//! (esp-storage's AutoPark): the gap between one op's end and the next
//! one's announce is a few instructions on core 0, while the UI core,
//! unparked, first refills its cache. It never saw the flash idle: a
//! flush of 12,800 bytes took 2 to 4.4 s and the draw stage ran past
//! MWDT1's 5 s four times (`[PREV_RUN] reason=wdt1 core1_stage=draw`,
//! bench 2026-11-08 13:11 to 13:16). Now a DMA that found the flash busy
//! says when it first asked ([`Arbiter::dma_begin`]), and the I/O core,
//! before its next op, waits for that DMA to start, at most [`YIELD_US`]
//! after it asked ([`Arbiter::flash_yields`]): one op per chunk at worst,
//! and a UI core that went on to other work costs the I/O core one bounded
//! wait, not one per op.
//!
//! The clock is the caller's: microseconds on a wrapping `u32`, so the
//! host drives both cores step by step (`tests`).

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// How long the I/O core holds its next flash op for a DMA that asked,
/// from the moment it first asked: the UI core's wake from a park and a
/// cache refill, many times over, and short beside a page's write.
pub const YIELD_US: u32 = 2_000;

/// How long `bytes` take on the panel's bus at `spi_mhz`, doubled, and a
/// tenth of a millisecond besides: the end the flash waits for.
pub fn transfer_us(bytes: usize, spi_mhz: u32) -> u32 {
    let bits = (bytes as u64) * 8;
    let us = bits / u64::from(spi_mhz.max(1));
    u32::try_from(us * 2 + 100).unwrap_or(u32::MAX)
}

/// `until` lies after `now` on the wrapping clock.
fn ahead(until: u32, now: u32) -> bool {
    until != 0 && (until.wrapping_sub(now) as i32) > 0
}

/// The two cores' claims on the memory SPI bus.
#[derive(Debug, Default)]
pub struct Arbiter {
    /// A flash write or erase is announced or running.
    flash: AtomicBool,
    /// When the last announced DMA ends; 0 when none was.
    dma_until: AtomicU32,
    /// When a DMA that found the flash busy first asked; 0 when none
    /// waits (1563).
    asked: AtomicU32,
}

impl Arbiter {
    pub const fn new() -> Self {
        Self {
            flash: AtomicBool::new(false),
            dma_until: AtomicU32::new(0),
            asked: AtomicU32::new(0),
        }
    }

    /// The UI core may start a DMA out of PSRAM of `us` microseconds at
    /// `now`: `true` when no flash op is announced, and the flash will
    /// wait for it. `false` says the DMA asks for the next gap.
    pub fn dma_begin(&self, now: u32, us: u32) -> bool {
        let until = now.wrapping_add(us).max(1);
        self.dma_until.store(until, Ordering::SeqCst);
        if self.flash.load(Ordering::SeqCst) {
            self.dma_until.store(0, Ordering::SeqCst);
            // The first ask's time stands: a UI core that keeps asking
            // does not hold the flash past one bound.
            let _ = self
                .asked
                .compare_exchange(0, now.max(1), Ordering::SeqCst, Ordering::SeqCst);
            return false;
        }
        // Started, after the end is announced: the flash that sees the
        // ask gone sees the transfer.
        self.asked.store(0, Ordering::SeqCst);
        true
    }

    /// The DMA the UI core started has ended before its announced time.
    pub fn dma_done(&self) {
        self.dma_until.store(0, Ordering::SeqCst);
    }

    /// Whether the I/O core holds its next op at `now` for a DMA that
    /// asked: until it started, or [`YIELD_US`] after it asked, when the
    /// ask is dropped.
    pub fn flash_yields(&self, now: u32) -> bool {
        let asked = self.asked.load(Ordering::SeqCst);
        if asked == 0 {
            return false;
        }
        if ahead(asked.wrapping_add(YIELD_US).max(1), now) {
            return true;
        }
        let _ = self
            .asked
            .compare_exchange(asked, 0, Ordering::SeqCst, Ordering::SeqCst);
        false
    }

    /// The I/O core announces its op.
    pub fn flash_announce(&self) {
        self.flash.store(true, Ordering::SeqCst);
    }

    /// Whether the announced op still waits at `now` for a DMA's end.
    pub fn flash_waits(&self, now: u32) -> bool {
        ahead(self.dma_until.load(Ordering::SeqCst), now)
    }

    /// The I/O core's op is done.
    pub fn flash_done(&self) {
        self.flash.store(false, Ordering::SeqCst);
    }

    /// Run the flash op `f` once no DMA out of PSRAM runs, and none starts
    /// until it is done, after the gap a DMA that asked is owed. `now`
    /// is the clock.
    pub fn flash<R>(&self, now: impl Fn() -> u32, f: impl FnOnce() -> R) -> R {
        while self.flash_yields(now()) {
            core::hint::spin_loop();
        }
        self.flash_announce();
        while self.flash_waits(now()) {
            core::hint::spin_loop();
        }
        let r = f();
        self.flash_done();
        r
    }
}

#[cfg(test)]
mod tests;
