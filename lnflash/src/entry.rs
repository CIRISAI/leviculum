//! The **enter** axis: getting a board into its bootloader.
//!
//! Two mechanisms, and only one of them is ours to control.
//!
//! The **1200-baud touch** opens a CDC port at exactly 1200 baud. Our
//! firmware answers the resulting `SET_LINE_CODING` by writing
//! `DFU_MAGIC_UF2_RESET` to `GPREGRET` and resetting; the bootloader reads
//! that retained register on the next boot and stays in mass storage. It
//! only exists if the running firmware implements it. Ours does. Stock
//! Meshtastic does not, and for Meshcore, microReticulum and RNode firmware
//! on nRF we have not measured it.
//!
//! The **double-tap** is the bootloader's own mechanism, works regardless of
//! what is running, and needs a human. **This is the load-bearing limit on
//! any fully automatic tool**: there is no universal software trigger, so a
//! tool is automatic for boards already carrying our firmware — every
//! re-flash — and must fall back to one clearly announced key press
//! otherwise (docs/src/concepts/lnode-flashing.md, "Getting into the
//! bootloader").
//!
//! The touch is done with `tcsetattr`, not by shelling out to `stty`.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::sys::Fd;
use crate::usb::{Device, Sysfs, UsbId};

/// Measured latency from the touch to the bootloader appearing on USB: 5 s
/// on the T114, 3 s on the RAK. The wait is generous because the cost of
/// waiting too long is a slow run and the cost of waiting too little is
/// telling the user their board did not respond when it did.
pub const BOOTLOADER_APPEARS_WITHIN: Duration = Duration::from_secs(20);

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Open a CDC port at 1200 baud and close it again.
///
/// The reset is a side effect of the line-coding change, so there is nothing
/// to write and nothing to read; the port simply goes away underneath us,
/// and an error from the close is the expected outcome rather than a fault.
pub fn touch_1200(port: &Path) -> io::Result<()> {
    let fd = Fd::open_serial(port)?;
    fd.set_baud_1200()?;
    // Let the host controller actually issue SET_LINE_CODING before the
    // close tears the port down.
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}

/// Wait for a device on one of `ids` that is the same physical board as
/// `was`, or for any such device if `was` is `None`.
///
/// Correlation matters when more than one board is attached: "a UF2 drive
/// appeared" is not the same claim as "the board I touched came back", and
/// only the second one licenses a write.
pub fn wait_for_bootloader(
    sysfs: &Sysfs,
    ids: &[UsbId],
    was: Option<&Device>,
    within: Duration,
) -> io::Result<Option<Device>> {
    let deadline = Instant::now() + within;
    loop {
        let found = sysfs.devices_matching(ids)?.into_iter().find(|candidate| {
            was.is_none_or(|before| candidate.is_same_board(before))
                // A board with no serial in either mode can only be tracked
                // by its port, which `is_same_board` already falls back to.
                && candidate.block_device().is_some()
        });
        if found.is_some() {
            return Ok(found);
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Wait for a device to leave the bus.
///
/// Needed because a board that reboots is still listed in sysfs for a
/// moment afterwards. Without this, "wait for the bootloader" answers
/// instantly with the pre-reboot entry, and the next mount lands on a
/// device that is in the middle of going away. Returns `false` if it never
/// left, which is itself a fact the caller may act on.
pub fn wait_until_gone(sysfs: &Sysfs, device: &Device, within: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + within;
    loop {
        let still_there = sysfs
            .devices()?
            .iter()
            .any(|d| d.name == device.name && d.id == device.id);
        if !still_there {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Wait for an application to come back on one of `ids`, for the verify
/// step. Same correlation rule, no drive expected.
pub fn wait_for_application(
    sysfs: &Sysfs,
    ids: &[UsbId],
    was: Option<&Device>,
    within: Duration,
) -> io::Result<Option<Device>> {
    let deadline = Instant::now() + within;
    loop {
        let found = sysfs
            .devices_matching(ids)?
            .into_iter()
            .find(|candidate| was.is_none_or(|before| candidate.is_same_board(before)));
        if found.is_some() {
            return Ok(found);
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// What to tell a user who has to reach for the board.
///
/// Named here rather than written inline at the call site because it is the
/// one instruction in the tool a person has to act on, and it should read
/// the same every time.
pub fn double_tap_instruction(what: &str) -> String {
    format!(
        "{what} has to be put into its bootloader by hand:\n  \
         press RESET twice, quickly — the second press within about half a second of the first.\n  \
         A drive appears when it worked."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_double_tap_instruction_says_what_to_press_and_what_success_looks_like() {
        let text = double_tap_instruction("the board on 3-2.4");
        assert!(text.contains("3-2.4"));
        assert!(text.contains("RESET twice"));
        assert!(text.contains("drive appears"));
    }

    #[test]
    fn waiting_finds_a_bootloader_that_is_already_there_without_sleeping() {
        let sysfs = Sysfs::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sysfs"));
        let started = Instant::now();
        let found = wait_for_bootloader(
            &sysfs,
            &[UsbId::new(0x239a, 0x0071)],
            None,
            Duration::from_secs(30),
        )
        .unwrap()
        .unwrap();
        assert_eq!(found.name, "3-2.4");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn waiting_correlates_to_the_board_that_was_touched() {
        let sysfs = Sysfs::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sysfs"));
        let t114 = sysfs
            .devices_matching(&[UsbId::new(0x1209, 0x0001)])
            .unwrap()
            .remove(0);
        let rak = sysfs
            .devices_matching(&[UsbId::new(0x1209, 0x0002)])
            .unwrap()
            .remove(0);

        // The T114's bootloader is on the bus, with the word-swapped serial.
        assert!(wait_for_bootloader(
            &sysfs,
            &[UsbId::new(0x239a, 0x0071)],
            Some(&t114),
            Duration::from_millis(1)
        )
        .unwrap()
        .is_some());

        // The RAK was not touched, so that same drive is not its bootloader
        // — which is exactly the confusion commit 362c1c2d records.
        assert!(wait_for_bootloader(
            &sysfs,
            &[UsbId::new(0x239a, 0x0071)],
            Some(&rak),
            Duration::from_millis(1)
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn a_device_still_on_the_bus_is_not_reported_as_gone() {
        // The trap this closes: a board that just rebooted is still listed
        // for a moment, so "wait for the bootloader" would answer instantly
        // with the entry that is on its way out.
        let sysfs = Sysfs::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sysfs"));
        let boot = sysfs
            .devices_matching(&[UsbId::new(0x239a, 0x0071)])
            .unwrap()
            .remove(0);
        assert!(!wait_until_gone(&sysfs, &boot, Duration::from_millis(300)).unwrap());
    }

    #[test]
    fn a_device_that_is_not_there_reads_as_gone_immediately() {
        let sysfs = Sysfs::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sysfs"));
        let mut absent = sysfs
            .devices_matching(&[UsbId::new(0x239a, 0x0071)])
            .unwrap()
            .remove(0);
        absent.name = "9-9.9".into();
        let started = Instant::now();
        assert!(wait_until_gone(&sysfs, &absent, Duration::from_secs(30)).unwrap());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn waiting_times_out_rather_than_hanging_when_nothing_appears() {
        let sysfs = Sysfs::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sysfs"));
        let started = Instant::now();
        let found = wait_for_bootloader(
            &sysfs,
            &[UsbId::new(0x239a, 0x0029)], // the RAK bootloader, not present
            None,
            Duration::from_millis(300),
        )
        .unwrap();
        assert!(found.is_none());
        assert!(started.elapsed() >= Duration::from_millis(300));
    }
}
