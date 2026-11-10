//! A core with nothing to do waits for its next interrupt (1507, move 9
//! of the 2026-11-07 structure review's M9 list).
//!
//! Before 1507 both of the M9's loops spun: core 0 between its
//! keyboard ticks and UART bytes, core 1 between the panel's frames. A
//! spinning Xtensa core runs its pipeline and fetches its loop from the
//! cache all the time. `WAITI 0` (Xtensa ISA, "WAITI") stops the core's
//! clock until an interrupt at any level arrives, and the core goes on
//! after the handler: nothing is lost that an interrupt would have
//! brought, and the next one is at most a timer period away.
//!
//! This is the clock-gated idle, not the chip's light sleep: light sleep
//! stops the APB clock under the UART, and its wake on UART edges loses
//! the bytes that wake it (ESP32-S3 TRM, "UART wakeup"), which on the
//! M9's one line is the start of an lnsd frame; and the keyboard is
//! polled every 10 ms (1463), more often than light sleep pays for its
//! entry and exit.
//!
//! What each core spent waiting is counted ([`waited_us`]), so the board
//! says how idle its cores were (`[IDLE]`), a number the bench reads
//! where no meter measures the current.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::uptime_us;

/// Microseconds each core spent in [`wait`] since the last
/// [`waited_us`], core 0 first.
static WAITED_US: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// Wait for the next interrupt on this core, `core` 0 or 1, and count the
/// time. The caller must not be inside a critical section: `WAITI 0`
/// opens every interrupt level, and a section with its interrupts off
/// would be one no longer. A caller that waits must have an interrupt
/// coming on this core (a timer); this function cannot tell.
pub fn wait(core: usize) {
    let t0 = uptime_us();
    #[cfg(target_arch = "xtensa")]
    // SAFETY: WAITI changes PS.INTLEVEL to 0, which is where thread code
    // runs outside a critical section; it touches no memory. No
    // `nomem`: the compiler must not move the loop's loads of what an
    // interrupt stores across it.
    unsafe {
        core::arch::asm!("waiti 0", options(nostack));
    }
    #[cfg(not(target_arch = "xtensa"))]
    core::hint::spin_loop();
    let us = uptime_us().saturating_sub(t0);
    if let Some(waited) = WAITED_US.get(core) {
        waited.fetch_add(u32::try_from(us).unwrap_or(u32::MAX), Ordering::Relaxed);
    }
}

/// What each core waited since the last call, microseconds, core 0
/// first; the counts start again.
pub fn waited_us() -> [u32; 2] {
    [
        WAITED_US[0].swap(0, Ordering::Relaxed),
        WAITED_US[1].swap(0, Ordering::Relaxed),
    ]
}
