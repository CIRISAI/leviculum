//! SolarNode external-flash self-test: erase, program and read back the
//! whole P25Q16H, with timings (OTA stage 2,
//! `docs/src/concepts/ota-stage-2-mesh-image.md`).
//!
//! **This binary ERASES the whole external flash.** It is a measurement
//! instrument, not firmware: no bundle and no `lnflash` entry carries it,
//! and the one recipe that flashes it (`just flash-solarnode-qspi-selftest`)
//! says so in its description. It boots, tests, prints, idles. No
//! SoftDevice, no radio, no Reticulum, no BLE — and it runs the whole test
//! again on every boot, so a board left on it is erased on every boot.
//!
//! The sequence, every line `[QSPI-TEST] …` on the debug CDC port:
//!
//! 1. `START`: the datasheet runtime bounds, and whether a watchdog is
//!    running (none is armed by anything in this tree; the line is the
//!    measurement that the bootloader did not arm one either).
//! 2. Bring-up through `qspi::identify_at_boot`, exactly as the SolarNode
//!    firmware does it, so the `[QSPI] JEDEC` line is the familiar one.
//! 3. `SR`: status registers 1 and 2 and the configure register, read-only.
//!    Any block-protect bit, or CMP, ends the run there with
//!    `RESULT pass=0 reason=block-protected`. No status register is ever
//!    written: on this part bit 6 is BP4 and survives a power cycle
//!    (`qspi::QuadEnable`), so the bus stays single-line, and every
//!    throughput number below is a floor, not the part's ceiling.
//! 4. `CENSUS` × 32: what the part held before the first erase.
//! 5. `ERASE pass=1`, `WRITE`/`READ pass=1` pattern A, `ERASE pass=2`,
//!    `WRITE`/`READ pass=2` pattern B (A's complement), `ERASE pass=3`.
//!    The third erase is what leaves the part blank, so a later
//!    `qspi::log_store` mount reports `STORE state=unformatted` and not
//!    our pattern.
//! 6. `RESULT`.
//!
//! Then the whole report again, behind a `REPORT n=<k>` header, every
//! 60 s: the log ring is 8 KiB and the flash runner's own read-back drains
//! the first seconds of output to a host that is not capturing, so the
//! census would otherwise be gone by the time a person attaches.
//!
//! Every decision — the pattern, the comparison, the census, the
//! protection rule, the verdict, the line shapes — is in
//! `leviculum-qspi-selftest`, host-tested. This file is the bus.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use core::future::Future;
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_nrf::qspi::{self, Qspi};
use embassy_time::{Duration, Instant, Timer};
use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};
use static_cell::StaticCell;

use leviculum_nrf::boards::solarnode;
use leviculum_nrf::log_critical;
use leviculum_qspi_selftest as st;
use st::{
    Cause, Census, CensusLine, ErasePass, Failure, Op, Pattern, ReadPass, Run, Status, Tally,
    WritePass,
};

/// One read operation's worth. A multiple of 4, as the peripheral's
/// EasyDMA demands, and a divisor of a census window.
const CHUNK: usize = st::READ_CHUNK_BYTES as usize;

/// The DMA buffer. Static so EasyDMA reads into RAM and the main future
/// stays small; 4-byte aligned because `Qspi` asserts it.
#[repr(align(4))]
struct Buf([u8; CHUNK]);

static BUF: StaticCell<Buf> = StaticCell::new();

/// The nRF52840 WDT's RUNSTATUS register (base 0x4001_0000, offset 0x400,
/// `nrf-pac` `wdt::Wdt::runstatus`). Read raw because `embassy-nrf`
/// exports its PAC only under `unstable-pac`.
const WDT_RUNSTATUS: *const u32 = 0x4001_0400 as *const u32;

/// Seconds between re-prints of the whole report.
const REPORT_PERIOD_S: u64 = 60;

fn line(args: core::fmt::Arguments) {
    leviculum_nrf::log::log_fmt_critical("[QSPI-TEST] ", args);
}

fn us_since(start: Instant) -> u64 {
    start.elapsed().as_micros()
}

/// The build stamp every 5 s, as the firmware prints it, so the flash
/// runner's read-back recognises the image while the test is still running.
#[embassy_executor::task]
async fn fw_build_banner() {
    loop {
        log_critical!("[FW_BUILD] {}", leviculum_nrf::FW_BUILD_STAMP);
        Timer::after_secs(5).await;
    }
}

/// Run one flash operation under a timeout.
///
/// The future is boxed so a timeout can LEAK it rather than drop it: every
/// `Qspi` operation future carries an `OnDrop` that spins on the
/// peripheral's READY event (`embassy-nrf-0.9.0/src/qspi.rs`, `erase`,
/// `read_raw`, `write_raw`), and READY is exactly what a hung operation
/// never raises, so dropping it would hang here instead of reporting.
/// After a timeout the run stops and nothing touches the bus or the DMA
/// buffer again.
async fn guarded<F>(fut: F, limit_ms: u32, op: Op, addr: u32) -> Result<u64, Failure>
where
    F: Future<Output = Result<(), qspi::Error>>,
{
    let start = Instant::now();
    let mut fut = Box::pin(fut);
    match select(
        fut.as_mut(),
        Timer::after(Duration::from_millis(limit_ms.into())),
    )
    .await
    {
        Either::First(Ok(())) => Ok(us_since(start)),
        Either::First(Err(_)) => Err(Failure {
            op,
            addr,
            cause: Cause::Driver,
        }),
        Either::Second(()) => {
            core::mem::forget(fut);
            Err(Failure {
                op,
                addr,
                cause: Cause::Timeout,
            })
        }
    }
}

/// Read the whole part in [`CHUNK`]s, handing each to `check`. Returns the
/// summed read time, without the time `check` takes.
async fn read_all(
    flash: &mut Qspi<'static>,
    buf: &mut Buf,
    mut check: impl FnMut(u32, &[u8]),
) -> Result<u64, Failure> {
    let mut us = 0u64;
    for addr in (0..st::PART_BYTES).step_by(CHUNK) {
        us += guarded(
            ReadNorFlash::read(flash, addr, &mut buf.0),
            st::READ_TIMEOUT_MS,
            Op::Read,
            addr,
        )
        .await?;
        check(addr, &buf.0);
    }
    Ok(us)
}

/// Erase every sector, then read the part back and count what is not
/// `0xFF`.
async fn erase_pass(
    flash: &mut Qspi<'static>,
    buf: &mut Buf,
    pass: u8,
) -> Result<ErasePass, Failure> {
    let mut us = 0u64;
    let mut max_sector_us = 0u64;
    for sector in 0..st::SECTORS {
        let addr = sector * st::SECTOR_BYTES;
        let took = guarded(
            NorFlash::erase(flash, addr, addr + st::SECTOR_BYTES),
            st::ERASE_TIMEOUT_MS,
            Op::Erase,
            addr,
        )
        .await?;
        us += took;
        max_sector_us = max_sector_us.max(took);
    }
    let mut not_ff = Tally::default();
    read_all(flash, buf, |addr, got| not_ff.check_erased(addr, got)).await?;
    Ok(ErasePass {
        pass,
        us,
        max_sector_us,
        not_ff,
    })
}

/// Program the whole part with `pattern`, one 256-byte page per program
/// operation and never across a page boundary.
///
/// One page per operation on purpose. The nRF52840 QSPI peripheral is
/// given a page size (`IFCONFIG0.PPSIZE`, 256 bytes as `embassy-nrf`
/// configures it) and is meant to split a longer write itself, but that
/// is the peripheral's behaviour and not something this test may assume
/// while it is the thing being measured. Writing page by page makes every
/// program operation one `PP` of one page whatever the peripheral does
/// with longer ones; the cost is one interrupt per page against a `tPP`
/// of 2 ms.
async fn write_pass(
    flash: &mut Qspi<'static>,
    buf: &mut Buf,
    pass: u8,
    pattern: Pattern,
) -> Result<WritePass, Failure> {
    let page = st::PAGE_BYTES as usize;
    let mut us = 0u64;
    for n in 0..st::PAGES {
        let addr = n * st::PAGE_BYTES;
        st::fill(pattern, addr, &mut buf.0[..page]);
        us += guarded(
            NorFlash::write(flash, addr, &buf.0[..page]),
            st::PROGRAM_TIMEOUT_MS,
            Op::Write,
            addr,
        )
        .await?;
    }
    Ok(WritePass { pass, pattern, us })
}

/// Read the whole part back against `pattern`.
async fn read_pass(
    flash: &mut Qspi<'static>,
    buf: &mut Buf,
    pass: u8,
    pattern: Pattern,
) -> Result<ReadPass, Failure> {
    let mut tally = Tally::default();
    let us = read_all(flash, buf, |addr, got| {
        tally.check_pattern(pattern, addr, got)
    })
    .await?;
    Ok(ReadPass { pass, us, tally })
}

/// Status register 1, 2 and the configure register, read-only. A refused
/// SR1 read is a failure; SR2 and CR become `na`.
fn read_status(flash: &mut Qspi<'static>) -> Result<Status, Failure> {
    let mut read = |op: u8| {
        let mut v = [0u8; 1];
        flash
            .blocking_custom_instruction(op, &[], &mut v)
            .ok()
            .map(|()| v[0])
    };
    let sr1 = read(st::OP_RDSR).ok_or(Failure {
        op: Op::Status,
        addr: 0,
        cause: Cause::Driver,
    })?;
    Ok(Status {
        sr1,
        sr2: read(st::OP_RDSR2),
        cr: read(st::OP_RDCR),
    })
}

/// The test proper, from status read to final erase. Fills `run` as it
/// goes, so a failure leaves everything before it on the report.
async fn test(flash: &mut Qspi<'static>, buf: &mut Buf, run: &mut Run) -> Result<(), Failure> {
    let status = read_status(flash)?;
    run.status = Some(status);
    line(format_args!("{status}"));
    if status.protection() != st::Protection::Clear {
        return Ok(());
    }

    let mut census = Census::new();
    read_all(flash, buf, |addr, got| census.feed(addr, got)).await?;
    run.census = Some(census);
    for (win, window) in census.windows().iter().enumerate() {
        line(format_args!(
            "{}",
            CensusLine {
                win,
                window: *window
            }
        ));
    }

    run.erase_issued = true;
    for (i, pattern) in [Pattern::A, Pattern::B].into_iter().enumerate() {
        let pass = i as u8 + 1;
        let erase = erase_pass(flash, buf, pass).await?;
        run.erases[i] = Some(erase);
        line(format_args!("{erase}"));
        let write = write_pass(flash, buf, pass, pattern).await?;
        run.writes[i] = Some(write);
        line(format_args!("{write}"));
        let read = read_pass(flash, buf, pass, pattern).await?;
        run.reads[i] = Some(read);
        line(format_args!("{read}"));
    }
    let last = st::ERASE_PASSES - 1;
    let erase = erase_pass(flash, buf, st::ERASE_PASSES as u8).await?;
    run.erases[last] = Some(erase);
    line(format_args!("{erase}"));
    Ok(())
}

/// Every line the run produced, in its order.
fn print_report(run: &Run) {
    if let Some(status) = run.status {
        line(format_args!("{status}"));
    }
    if let Some(census) = run.census {
        for (win, window) in census.windows().iter().enumerate() {
            line(format_args!(
                "{}",
                CensusLine {
                    win,
                    window: *window
                }
            ));
        }
    }
    for i in 0..st::ERASE_PASSES {
        if let Some(e) = run.erases[i] {
            line(format_args!("{e}"));
        }
        if let Some(w) = run.writes.get(i).copied().flatten() {
            line(format_args!("{w}"));
        }
        if let Some(r) = run.reads.get(i).copied().flatten() {
            line(format_args!("{r}"));
        }
    }
    if let Some(f) = run.failure {
        line(format_args!("{f}"));
    }
    line(format_args!("{}", run.verdict()));
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = embassy_nrf::config::Config::default();
    config.hfclk_source = embassy_nrf::config::HfclkSource::ExternalXtal;
    config.gpiote_interrupt_priority = embassy_nrf::interrupt::Priority::P2;
    config.time_interrupt_priority = embassy_nrf::interrupt::Priority::P2;
    let p = embassy_nrf::init(config);

    leviculum_nrf::init_heap();
    let vbus = leviculum_nrf::init_vbus();
    let _serial = leviculum_nrf::usb::init(&spawner, p.USBD, vbus, &solarnode::CONFIG);
    spawner.must_spawn(fw_build_banner());

    log_critical!("leviculum SolarNode QSPI self-test: ERASES the whole external flash");

    // SAFETY: a read of a read-only, always-mapped peripheral register.
    let wdt_running = unsafe { core::ptr::read_volatile(WDT_RUNSTATUS) } & 1;
    line(format_args!(
        "START part=P25Q16H bytes={} bus=single-line typical_ms={} worst_case_ms={} \
         timeout_bound_ms={} wdt_running={wdt_running}",
        st::PART_BYTES,
        st::typical_ms(),
        st::worst_case_ms(),
        st::timeout_bound_ms(),
    ));

    let start = Instant::now();
    let mut run = Run::default();
    let flash = match solarnode::CONFIG.qspi_part {
        Some(part) => leviculum_nrf::qspi::identify_at_boot(
            p.QSPI,
            p.P0_21.into(), // SCK
            p.P0_25.into(), // CSN
            p.P0_20.into(), // IO0 / DI
            p.P0_24.into(), // IO1 / DO
            p.P0_22.into(), // IO2 / WP#
            p.P0_23.into(), // IO3 / HOLD#
            part,
        ),
        None => None,
    };
    // Held to the end of `main`, which never returns: dropping it would
    // deactivate the peripheral under a leaked operation (`guarded`).
    let mut flash = flash;
    match flash.as_mut() {
        Some(flash) => {
            let buf = BUF.init(Buf([0u8; CHUNK]));
            if let Err(failure) = test(flash, buf, &mut run).await {
                run.failure = Some(failure);
                line(format_args!("{failure}"));
            }
        }
        None => run.no_part = true,
    }
    run.total_us = us_since(start);
    line(format_args!("{}", run.verdict()));

    let mut n = 0u32;
    loop {
        Timer::after_secs(REPORT_PERIOD_S).await;
        n = n.wrapping_add(1);
        line(format_args!("REPORT n={n}"));
        print_report(&run);
    }
}
