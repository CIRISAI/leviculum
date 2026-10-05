#![no_std]
//! How long the SolarNode's boot may wait on its QSPI flash, and the line it
//! prints when it stops waiting.
//!
//! # The defect this crate closes (leviculum#435, #296)
//!
//! `bin/solarnode.rs` asks the external flash its JEDEC id and then mounts
//! the record log on it, read-only, before it brings the radio up
//! (`[STG] lora-init`). The nRF52840's QSPI peripheral completes a transfer
//! or it does not, and nothing in `embassy-nrf` gives up for it: a part that
//! stops answering mid-read holds the boot before the radio, and a storage
//! fault becomes a dead node. So the step runs under a deadline, and past it
//! the boot gives the part up and goes on exactly as it does for a board
//! whose part never answered.
//!
//! # Why a crate and not two lines in `qspi.rs`
//!
//! `leviculum-nrf` cross-compiles to `thumbv7em-none-eabihf` and runs no host
//! tests. The budget is arithmetic that is wrong by a factor and fails
//! silently, and the line is a shape a capture reader greps for; both are
//! asserted here (`tests/boot_budget.rs`), and the firmware only uses them.
//! See `docs/src/concepts/firmware-host-test-seam.md`.

use core::fmt;

/// Sector headers the mount reads: one per 4 KiB sector of the P25Q16H's
/// 2 MiB (`RecordLog::mount` → `find_active`, `leviculum-nrf/record-log`).
pub const HEADER_READS: u32 = 2 * 1024 * 1024 / 4096;

/// Bytes in one sector header read (`SECTOR_HEADER_LEN`, record-log).
pub const HEADER_BYTES: u32 = 12;

/// The boot bus clock, in MHz (`qspi::P25Q16H_BUS`, single-line
/// `FASTREAD`).
pub const BUS_MHZ: u32 = 16;

/// What one QSPI read costs on this board beyond its data clocks, in us.
///
/// Measured, not from a datasheet: the self-test capture
/// `/home/lew/rig-run/solarnode-qspi/qspi-selftest-20261005T005318Z.log`,
/// `READSWEEP sck_khz=16000 rxdelay=1`, read 5 x 2 MiB in 4096-byte reads
/// in `ms=5365`. That is 2560 reads at 2095.7 us each, of which 2048 us are
/// the data clocks at 16 MHz; the remaining 47.7 us are the opcode, address
/// and dummy clocks plus the driver's start, interrupt and wake. Rounded up.
/// It over-states the boot's own cost, if anything: the self-test boxes and
/// races every read against a timer, the boot does not.
pub const READ_OVERHEAD_US: u32 = 48;

/// The identify half of the step, in us: the 1 ms wait after the
/// deep-power-down release (`RELEASE_WAIT_CYCLES`, 64 000 cycles at
/// 64 MHz), two custom instructions at [`READ_OVERHEAD_US`] each, and the
/// `[QSPI] JEDEC` line, rounded up. A silent first read earns a second
/// wait and read, but a silent part is never mounted, so the mount below
/// never follows it.
pub const IDENTIFY_US: u32 = 1_100;

/// The whole boot step on this board, in ms: identify plus the mount of a
/// part with no record log on it, which is what the SolarNode's part holds
/// today (its last writer was the self-test, and this firmware never
/// formats).
///
/// An EXPECTED figure, derived from the parts above, not a boot capture:
/// the reviewer's boot captures of 2026-10-05
/// (`/home/lew/rig-run/solarnode-qspi/boot-*.log`) start after the boot,
/// so neither `[QSPI]` line is in them.
///
/// It does NOT cover a formatted, filling store: a mounted log adds the
/// scan of its active sector and `count`, which walks every record on the
/// part, seconds on a full one. The batch that first formats the part has
/// to re-derive this, or the boot will give up a healthy store.
pub const STEP_MS: u32 = expected_step_us().div_ceil(1000);

/// [`STEP_MS`] before rounding, in us.
pub const fn expected_step_us() -> u32 {
    IDENTIFY_US + HEADER_READS * (READ_OVERHEAD_US + HEADER_BYTES * 8 / BUS_MHZ)
}

/// The deadline for a step that takes `step_ms`: four times it, rounded up
/// to a whole second.
pub const fn budget_ms(step_ms: u32) -> u32 {
    (step_ms * 4).div_ceil(1000) * 1000
}

/// How long the boot waits on its QSPI step before it gives the part up.
pub const BOOT_STEP_BUDGET_MS: u32 = budget_ms(STEP_MS);

/// The line the boot prints, after the `[QSPI] ` prefix, when it gave the
/// part up: `state=timeout after_ms=<n>`, `n` the milliseconds from the
/// step's start to the moment the deadline won.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Timeout {
    /// Milliseconds the step had run when it was given up.
    pub after_ms: u64,
}

impl fmt::Display for Timeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "state=timeout after_ms={}", self.after_ms)
    }
}

/// How the race between a step taking `step_ms` and a deadline of
/// `budget_ms` ends: `None` if the step finishes first, the timeout line if
/// the deadline does.
///
/// The step has to finish strictly before the deadline. On the board the
/// boundary millisecond can go either way (`with_timeout` polls the step
/// before the timer, so a step completing in the poll the timer fired in
/// still wins); the model gives it to the deadline, the side that is never
/// optimistic about a part that is slow.
pub fn outcome(step_ms: u64, budget_ms: u32) -> Option<Timeout> {
    if step_ms < u64::from(budget_ms) {
        None
    } else {
        Some(Timeout {
            after_ms: u64::from(budget_ms),
        })
    }
}
