//! The QSPI NOR flash both boards carry and neither has ever driven.
//!
//! 2 MB of MX25R1635F on the T114, 1 MB of IS25LP080D on the WisMesh
//! Pocket V2. Codeberg #384 and
//! `docs/src/concepts/propagation-node-on-a-board.md` ask what to put in
//! it; this module is only the part that gets there — the peripheral, the
//! part's identity, and the `embedded_storage` NorFlash surface the record
//! log ([`leviculum_record_log`]) wants underneath it.
//!
//! # The storage trait shape
//!
//! There is nothing to wrap. `embassy_nrf::qspi::Qspi` already implements
//! `ReadNorFlash`/`NorFlash` (and their async twins) with `ERASE_SIZE`
//! 4096, `WRITE_SIZE` 4 and `READ_SIZE` 4, which is exactly the shape
//! `flash.rs` already uses for the internal NVMC and the SoftDevice flash.
//! So this module builds a `Qspi`, proves it is talking to the part we
//! think it is, and hands it back. Anything that adds a layer here adds
//! flash and a place for a bug.
//!
//! What it does *not* do is give the caller a device before it knows what
//! is on the other end. [`identify_at_boot`] returns `None` on a JEDEC id
//! that is not the board's part, and dropping the `Qspi` deactivates the
//! peripheral: **a board with an unexpected part says so and writes
//! nothing.** That is deliberate — the store's whole safety argument is
//! about 4 KB sectors, a 0xFF erased state and program-once bits, and none
//! of those are true of a part we have not identified.
//!
//! # Why the two frequencies differ
//!
//! `Speed::M8` on the T114 and `Speed::M32` on the Pocket, and the
//! asymmetry is the parts', not ours. The Macronix part is the low-power
//! one: in its default ultra-low-power mode its quad read tops out at
//! 8 MHz. The ISSI part allows 133 MHz, so the nRF52840's own 32 MHz
//! ceiling is what binds there. That is 4 MB/s against 16 MB/s, which the
//! concept paper turns into a 0.5 s full-store scan on the T114 and 0.07 s
//! on the Pocket — the number that makes an on-flash directory affordable
//! and a RAM index unnecessary.
//!
//! # Quad enable
//!
//! Both parts power up in single-line SPI mode with the QE bit of their
//! status register clear, and both put QE in bit 6. The `READ4IO`/`PP4IO`
//! opcodes this driver configures do not work until it is set, so
//! [`identify_at_boot`] sets it if it is not already. The write-enable the
//! part needs first is supplied by the QSPI peripheral itself: every
//! custom instruction it issues has `CINSTRCONF.WREN` set
//! (`custom_instruction_start`, `embassy-nrf-0.9.0/src/qspi.rs`), and
//! `WIPWAIT` makes it wait for the part to finish. One byte of data, so
//! the Macronix configuration registers — which hold the ultra-low-power
//! bit — keep their values.

use embassy_nrf::qspi::{self, Config, Frequency, Qspi};
use embassy_nrf::{bind_interrupts, peripherals, Peri};

use embassy_nrf::gpio::AnyPin;

bind_interrupts!(pub struct QspiIrqs {
    QSPI => qspi::InterruptHandler<peripherals::QSPI>;
});

/// Read JEDEC ID (opcode 0x9F): manufacturer, memory type, capacity.
const CMD_READ_JEDEC_ID: u8 = 0x9F;
/// Read Status Register (opcode 0x05).
const CMD_READ_STATUS: u8 = 0x05;
/// Write Status Register (opcode 0x01).
const CMD_WRITE_STATUS: u8 = 0x01;
/// Quad Enable, bit 6 of the status register on both parts.
const STATUS_QE: u8 = 0x40;

/// The two bus speeds we use, in a form a `const` board table can hold.
///
/// `embassy_nrf::qspi::Frequency` is neither `Copy` nor constructible out
/// of a `&'static` struct, so the board table carries this and converts on
/// the way in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed {
    /// 8 MHz — the MX25R1635F's quad-read ceiling in ultra-low-power mode.
    M8,
    /// 32 MHz — the nRF52840 QSPI's own ceiling.
    M32,
}

impl Speed {
    fn frequency(self) -> Frequency {
        match self {
            Speed::M8 => Frequency::M8,
            Speed::M32 => Frequency::M32,
        }
    }

    /// The bus clock in MHz, for the boot log line.
    pub fn mhz(self) -> u32 {
        match self {
            Speed::M8 => 8,
            Speed::M32 => 32,
        }
    }
}

/// The part a board is expected to carry.
pub struct FlashPart {
    /// Datasheet name, for the boot line.
    pub name: &'static str,
    /// The three bytes opcode 0x9F answers with.
    pub jedec: [u8; 3],
    /// Density in bytes. Also the bound `Qspi` enforces on every access.
    pub capacity: u32,
    /// Bus clock this part is driven at.
    pub speed: Speed,
}

/// Macronix MX25R1635F, 16 Mbit, on the Heltec Mesh Node T114.
pub const MX25R1635F: FlashPart = FlashPart {
    name: "MX25R1635F",
    jedec: [0xC2, 0x28, 0x15],
    capacity: 2 * 1024 * 1024,
    speed: Speed::M8,
};

/// ISSI IS25LP080D, 8 Mbit, on the RAK4631 module (WisMesh Pocket V2).
pub const IS25LP080D: FlashPart = FlashPart {
    name: "IS25LP080D",
    jedec: [0x9D, 0x60, 0x14],
    capacity: 1024 * 1024,
    speed: Speed::M32,
};

/// Bring the QSPI up, identify the part, and hand back a NorFlash device.
///
/// `None` means the part did not answer with the JEDEC id this board
/// expects. The `Qspi` is dropped on that path, which deactivates the
/// peripheral and leaves the pins deconfigured, so nothing can write to a
/// part whose geometry we do not know.
///
/// Emits one `[QSPI]` line either way, ungated like `SD_RAM_FLOOR`: a
/// board that comes up with an unexpected part has to say so in a boot
/// capture, before any host has attached to the debug port.
#[allow(clippy::too_many_arguments)]
pub fn identify_at_boot(
    qspi: Peri<'static, peripherals::QSPI>,
    sck: Peri<'static, AnyPin>,
    csn: Peri<'static, AnyPin>,
    io0: Peri<'static, AnyPin>,
    io1: Peri<'static, AnyPin>,
    io2: Peri<'static, AnyPin>,
    io3: Peri<'static, AnyPin>,
    part: &'static FlashPart,
) -> Option<Qspi<'static>> {
    let mut config = Config::default();
    config.frequency = part.speed.frequency();
    config.capacity = part.capacity;

    // `Qspi::new` drives IO3 (the part's HOLD#/RESET#) high before it
    // activates the interface, which is what the T114 board comment asks
    // for; every pin goes out high first (`config_pin!`,
    // `embassy-nrf-0.9.0/src/qspi.rs`).
    let mut flash = Qspi::new(qspi, QspiIrqs, sck, csn, io0, io1, io2, io3, config);

    let mut jedec = [0u8; 3];
    // Custom instructions run on the single-line SPI path regardless of
    // the quad read/write opcodes configured above, so this works before
    // QE is set.
    if flash
        .blocking_custom_instruction(CMD_READ_JEDEC_ID, &[], &mut jedec)
        .is_err()
    {
        log_part(part, &[0, 0, 0], false, "read-failed");
        return None;
    }

    if jedec != part.jedec {
        log_part(part, &jedec, false, "unexpected-part");
        return None;
    }

    let mut status = [0u8; 1];
    if flash
        .blocking_custom_instruction(CMD_READ_STATUS, &[], &mut status)
        .is_err()
    {
        log_part(part, &jedec, false, "status-read-failed");
        return None;
    }
    if status[0] & STATUS_QE == 0 {
        let want = status[0] | STATUS_QE;
        if flash
            .blocking_custom_instruction(CMD_WRITE_STATUS, &[want], &mut [])
            .is_err()
        {
            log_part(part, &jedec, false, "quad-enable-failed");
            return None;
        }
        // Read it back rather than assume: the quad opcodes this driver is
        // configured with are silently wrong if QE did not take, and the
        // symptom would be garbage data rather than an error.
        let mut check = [0u8; 1];
        if flash
            .blocking_custom_instruction(CMD_READ_STATUS, &[], &mut check)
            .is_err()
            || check[0] & STATUS_QE == 0
        {
            log_part(part, &jedec, false, "quad-enable-refused");
            return None;
        }
    }

    log_part(part, &jedec, true, "ok");
    Some(flash)
}

/// The one boot line. Same shape as `SD_RAM_FLOOR`: every value that went
/// into the verdict is on it, so the verdict is checkable from a capture.
fn log_part(part: &FlashPart, jedec: &[u8; 3], matched: bool, state: &str) {
    crate::log::log_fmt_critical(
        "[QSPI] ",
        format_args!(
            "JEDEC id={:02x}:{:02x}:{:02x} expect={:02x}:{:02x}:{:02x} part={} bytes={} clk={}MHz match={} state={}",
            jedec[0],
            jedec[1],
            jedec[2],
            part.jedec[0],
            part.jedec[1],
            part.jedec[2],
            part.name,
            part.capacity,
            part.speed.mhz(),
            u8::from(matched),
            state,
        ),
    );
}

/// Bytes of the part read back and summarised by [`log_head`].
const HEAD_PROBE_LEN: usize = 256;

/// A 4-byte-aligned read buffer. The QSPI peripheral DMAs into it and
/// asserts `ptr % 4 == 0` (`start_read`, `embassy-nrf-0.9.0/src/qspi.rs`).
#[repr(align(4))]
struct Aligned<const N: usize>([u8; N]);

/// Read the first 256 bytes over the quad path and say what came back.
///
/// This is the only thing in part 1 that exercises `READ4IO` at the
/// configured clock, so it is what a boot capture has to show before the
/// bus is believed. It is also the honest answer to "is there already
/// something on these boards' flash?", which matters before anything
/// formats them: `nonff=0/256` is a blank part (or a bus that answers with
/// pull-ups — the two look alike, which is why the JEDEC line above is the
/// separate proof that a part is there at all), and anything else is data
/// somebody wrote.
pub fn log_head(flash: &mut Qspi<'static>) {
    let mut buf = Aligned([0u8; HEAD_PROBE_LEN]);
    if flash.blocking_read(0, &mut buf.0).is_err() {
        crate::log::log_fmt_critical("[QSPI] ", format_args!("HEAD state=read-failed"));
        return;
    }
    // FNV-1a over the window: a number a capture can be diffed on, not a
    // cryptographic claim.
    let mut digest: u32 = 0x811C_9DC5;
    let mut nonff = 0u32;
    for byte in buf.0.iter() {
        digest = (digest ^ u32::from(*byte)).wrapping_mul(0x0100_0193);
        if *byte != 0xFF {
            nonff += 1;
        }
    }
    crate::log::log_fmt_critical(
        "[QSPI] ",
        format_args!(
            "HEAD fnv1a={digest:08x} nonff={nonff}/{HEAD_PROBE_LEN} b0={:02x}{:02x}{:02x}{:02x}",
            buf.0[0], buf.0[1], buf.0[2], buf.0[3]
        ),
    );
}

/// Mount the record log over the whole part, read-only, and say what is
/// there.
///
/// **Read-only on purpose.** These boards have carried other people's
/// firmware, and part 1 of #384 has no business erasing whatever that left
/// behind; `RecordLog::mount` returns `None` on a region it did not write
/// rather than formatting it. Formatting is a decision for the batch that
/// actually stores something.
///
/// Consumes the device: nothing in part 1 keeps the QSPI, and dropping it
/// deactivates the peripheral (and, on the way out, works around
/// nRF52840 anomaly 122) instead of holding the part out of its standby
/// current for a store no one has mounted yet.
pub fn log_store(flash: Qspi<'static>, part: &FlashPart) {
    use leviculum_record_log::{RecordLog, SECTOR_SIZE};

    let sectors = part.capacity / SECTOR_SIZE;
    match RecordLog::mount(flash, 0, part.capacity) {
        Ok(Some(mut log)) => {
            let active = log.active_sector();
            let seq = log.sequence();
            match log.count() {
                Ok((live, purged)) => crate::log::log_fmt_critical(
                    "[QSPI] ",
                    format_args!(
                        "STORE state=mounted sectors={sectors} active={active} seq={seq} \
                         live={live} purged={purged}"
                    ),
                ),
                Err(_) => crate::log::log_fmt_critical(
                    "[QSPI] ",
                    format_args!("STORE state=scan-failed sectors={sectors} active={active}"),
                ),
            }
        }
        Ok(None) => crate::log::log_fmt_critical(
            "[QSPI] ",
            format_args!("STORE state=unformatted sectors={sectors} note=this-batch-never-formats"),
        ),
        Err(_) => crate::log::log_fmt_critical(
            "[QSPI] ",
            format_args!("STORE state=mount-failed sectors={sectors}"),
        ),
    }
}
