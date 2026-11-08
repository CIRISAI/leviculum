//! The flash and a DMA out of PSRAM never at once (1507), and the DMA
//! never starved by the flash (1563): the board's one
//! [`loquax_m9::mspi::Arbiter`] on its clock. The protocol, why it is
//! safe and why it yields are in that module, which the host steps a
//! microsecond at a time.

use loquax_m9::mspi::Arbiter;

use crate::uptime_us;

pub use loquax_m9::mspi::transfer_us;

/// The memory SPI bus's two claims.
static BUS: Arbiter = Arbiter::new();

fn now() -> u32 {
    uptime_us() as u32
}

/// The UI core may start a DMA out of PSRAM of `us` microseconds: `true`
/// when no flash op is announced, and the flash will wait for it; `false`
/// asks the I/O core for the next gap.
pub fn dma_begin(us: u32) -> bool {
    BUS.dma_begin(now(), us)
}

/// The DMA the UI core started has ended before its announced time.
pub fn dma_done() {
    BUS.dma_done();
}

/// Run the flash op `f` once no DMA out of PSRAM runs, and none starts
/// until it is done, after the gap a DMA that asked is owed.
pub fn flash<R>(f: impl FnOnce() -> R) -> R {
    BUS.flash(now, f)
}
