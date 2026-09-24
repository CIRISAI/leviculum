//! How often this board restarted while nobody was watching (Codeberg
//! #380).
//!
//! The non-volatile half of the boot instrumentation. [`crate::boot_trace`]
//! keeps its breadcrumbs in retained RAM, which answers what the previous
//! boot was doing and is gone by definition after a power loss — and a
//! Pocket V2 that restarted twice on a field walk is exactly a power
//! loss: every boot of that walk read `reset_reason=0x00000000` with
//! `prev_magic=absent`. A count that survives the supply going away has
//! to live in flash, so this module appends one 16-byte record per boot
//! to the page `memory.x` reserves as `BOOT` (`0xD9000`), read from the
//! linker through [`page`] and named by no constant anywhere else — the
//! same terms [`crate::record_store`] holds its region on, and for the
//! same reason: a board config carrying its own copy of the address
//! could disagree with the linker, and that disagreement is a module
//! that erases the running image's own code.
//!
//! **One page erase covers 256 boots**; the layout, the free-slot scan,
//! the wrap that carries the boot number past an erase and the wear
//! arithmetic are in [`leviculum_boot_count`], where a host test can cut
//! the power at every word boundary. This module only moves bytes.
//!
//! # Which side of the SoftDevice line this write sits on
//!
//! [`crate::radio_store`] draws that line: loading at boot is a plain
//! read of memory-mapped flash, while writing under a live SoftDevice
//! must go through `sd_flash_page_erase` / `sd_flash_write`, because the
//! SD owns the flash timing and a direct NVMC erase stalls the CPU for
//! milliseconds and breaks the BLE radio schedule.
//!
//! **The boot write sits on the other side: before `Softdevice::enable`,
//! straight to the NVMC.** Not because it is simpler — though it is, with
//! no channel, no task and no retry loop — but because of what it is
//! counting. A board that boot-loops on a sagging pack dies EARLY, and
//! most of the ways it can die are before the SoftDevice is up: the HFXO
//! wait, the flash reads, USB. A record written after `Softdevice::enable`
//! would be missing from precisely the boots the operator needs counted,
//! and the count would understate a boot loop exactly in proportion to
//! how bad it was. Writing at the same point the media profile and the
//! node name are read costs those boots nothing.
//!
//! What the early write gives up: a board that dies between power-on and
//! this call — inside `embassy_nrf::init`'s HFXO wait, say — leaves no
//! record, so that boot is never counted and the next boot's `n=` is one
//! lower than the truth. That window is the first few milliseconds of
//! `main` and it is not silent: the boot that dies inside it is the one
//! [`crate::boot_trace`] reports on the NEXT boot's `BOOT_TRACE` line as
//! `prev_phase=enter-main`. The two lines are read together, and between
//! them a boot that was too short to be counted still says so.
//!
//! The erase, when the page fills, is the one expensive operation here —
//! a page erase costs milliseconds, during which the CPU stalls on every
//! flash fetch. It is paid before `usb::init`, so it delays enumeration
//! rather than interrupting it, on one boot in 256.
//!
//! # Why the count is not in the telemetry report yet
//!
//! It belongs there, for the reason the pack voltage does: the report is
//! the only surface that reaches an operator who is still out there,
//! while the debug line needs a cable. **The report has no field for it.**
//! [`leviculum_lxmf::telemetry`] carries six of Sideband's sensors —
//! time, location, battery, physical link, temperature, power production
//! — and a restart count is none of them. The format is Sideband's and
//! not ours, so the gap is stated here rather than filled by minting a
//! sensor ID: `docs/src/concepts/telemetry.md` §"The extension ladder"
//! is explicit that a field number we invent and one app understands is
//! a fork of the format with a friendlier name.
//!
//! The proposal, which is rung 1 of that ladder and therefore not an
//! extension at all: Sideband defines a **free-text information sensor**
//! among its twenty-four, and `boots=<n>` is exactly the kind of
//! statement it exists for. What is missing before it can be written is
//! its SID and the shape of its packed value, and neither can be read
//! from this tree — no Sideband checkout lives under `reference/`, which
//! holds only Reticulum, LXMF, LXST and the RNode firmware. Adding the
//! sensor is then one field, one encode arm and one decode arm in the
//! codec, plus one line here; a viewer that ignores the sensor shows
//! the report exactly as it does today, which is the "failure mode on a
//! viewer that does not participate" the same document asks for.

use embassy_nrf::nvmc::Nvmc;
use embedded_storage::nor_flash::NorFlash;
use leviculum_boot_count::{encode, plan, BootCountLine, BootRecord, PAGE_SIZE};

/// What this boot's record says, and whether it reached the page.
pub struct Recorded {
    /// The record as it was written (or as it would have been, had the
    /// write not failed).
    pub record: BootRecord,
    /// Slots of the page spent including this one — the `since_erase=`
    /// of the line.
    pub since_erase: u32,
    /// False if the flash refused the erase or the write. The line is
    /// emitted either way; a second line says the count did not stick.
    pub persisted: bool,
}

/// The page this log owns, from `memory.x`'s `BOOT` region.
///
/// One symbol, no Rust constant beside it: the map is the single place
/// the address is decided, and the `ASSERT`s next to it are what refuse
/// a link in which the page overlaps the image below or the record log
/// above. Absolute symbol (value, not content), so what is taken is its
/// ADDRESS — the same shape [`crate::record_store::region`] reads.
pub fn page() -> u32 {
    extern "C" {
        static __sboot_record: u8;
    }
    core::ptr::addr_of!(__sboot_record) as u32
}

/// Append this boot's record to the page.
///
/// Call once, from `main`, **before `Softdevice::enable`** (see the
/// module comment) and before `usb::init`, next to the other
/// boot-time persistence reads. Blocks: the NVMC write is synchronous,
/// which is the whole reason it belongs this early.
///
/// `boot` is the capture [`crate::boot_trace::capture`] took as the first
/// statement of `main` — this reads its already-cleared `RESETREAS` and
/// whether the retained record survived, and writes to nothing of its.
pub fn record_at_boot(mut nvmc: Nvmc<'_>, boot: &crate::boot_trace::Captured) -> Recorded {
    let retained = matches!(boot.prev, leviculum_boot_trace::PrevBoot::Present { .. });
    let page = page();

    // SAFETY: `page` is the base of `memory.x`'s BOOT region, a whole
    // 4 KiB page inside the 1 MiB flash map that the linker hands to no
    // section and that the ASSERTs there keep clear of both the image
    // below and the record log above. Internal flash is readable as
    // normal memory on this part — the same read `radio_store::load`
    // does, legal before `Softdevice::enable`. The slice is dropped
    // before anything writes.
    let mapped = unsafe { core::slice::from_raw_parts(page as *const u8, PAGE_SIZE) };
    let plan = plan(mapped, boot.reset_reason, retained);

    let encoded = Aligned(encode(&plan.record));
    let result = (|| {
        if plan.erase_first {
            nvmc.erase(page, page + PAGE_SIZE as u32)?;
        }
        nvmc.write(page + plan.offset as u32, &encoded.0)
    })();

    Recorded {
        record: plan.record,
        since_erase: plan.since_erase,
        persisted: result.is_ok(),
    }
}

/// 4-byte-aligned record buffer. The NVMC writes whole 32-bit words and
/// reads the source through a `*const u32` (`embassy_nrf::nvmc`), so the
/// buffer it is handed is aligned for the same reason `radio_store`'s is.
#[repr(align(4))]
struct Aligned([u8; leviculum_boot_count::RECORD_SIZE]);

/// Emit the `BOOT_COUNT` line:
///
/// ```text
/// BOOT_COUNT n=42 reset_reason=0x00000004 retained=1 since_erase=17 t=118
/// ```
///
/// Ungated (`log_fmt_critical`, like `[QSPI]` and `[MEDIA]`): the drain
/// gate stays shut until DTR-assert or 30 s of uptime, and a field board
/// on battery has no host to open it — which is precisely the run whose
/// restart count is worth keeping. Shape frozen in
/// `docs/src/structured-event-logs.md` and byte-pinned by
/// [`leviculum_boot_count`]'s host tests.
///
/// Call after `usb::init`, beside `BOOT_TRACE` and `[RESET_REASON]`; the
/// three are read together.
pub fn log_banner(recorded: &Recorded) {
    crate::log::log_fmt_critical(
        "",
        format_args!(
            "{}",
            BootCountLine {
                record: recorded.record,
                since_erase: recorded.since_erase,
            }
        ),
    );
    if !recorded.persisted {
        // The number on the line above is what this boot IS; it is just
        // not what the next boot will read, so say so rather than let a
        // repeated `n=` look like a board that stopped counting.
        crate::log::log_fmt_critical(
            "",
            format_args!("BOOT_COUNT_WRITE_FAILED n={}", recorded.record.boot),
        );
    }
}
