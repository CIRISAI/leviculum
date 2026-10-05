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
//! 2. `PLAN wall_bound_ms=…` (`st::PlanLine`), once the start hold is
//!    over, so a capture attached after the flash runner's read-back holds
//!    it: the window one whole run needs, from its start and from boot.
//!    Then bring-up through `qspi::identify_at_boot`, exactly as the SolarNode
//!    firmware does it, so the `[QSPI] JEDEC` line is the familiar one and
//!    the first `BUS` line is the firmware's own bus setting
//!    (`qspi::P25Q16H_BUS`). Then a second `BUS` line: the run moves to
//!    8 MHz at the default RXDELAY before it reads anything, so what
//!    follows does not depend on that constant.
//! 3. `SR`: status registers 1 and 2 and the configure register, read-only.
//!    Any block-protect bit, or CMP, ends the run there with
//!    `RESULT pass=0 reason=block-protected`. No status register is ever
//!    written: on this part bit 6 is BP4 and survives a power cycle
//!    (`qspi::QuadEnable`), so the bus stays single-line, and every
//!    throughput number below is a floor, not the part's ceiling.
//! 4. `CENSUS` × 32: what the part held before the first erase, read at
//!    8 MHz.
//! 5. The plan (`st::PLAN`), every line of it under the same bus clock the
//!    line names, every read at the default RXDELAY:
//!    `ERASE pass=1`; `WRITE pass=1` pattern A at 32 MHz; `READSET set=1`,
//!    five reads at 32 MHz; `BUS` (now 8 MHz); `READSET set=2`, five reads
//!    of the SAME data at 8 MHz; `ERASE pass=2`; `WRITE pass=2` pattern B
//!    (A's complement) at 8 MHz; `READSET set=3` at 8 MHz; `BUS` (back to
//!    32 MHz); `READSET set=4` at 32 MHz. Each read set is followed by its
//!    `MISS` lines (the first 64 mismatched bytes, each with all five
//!    reads), and each written image by its `DIAG` line, which says
//!    whether its errors are on the read side or the program side.
//! 6. The read sweep (`st::SWEEP`, Codeberg #435): pattern B, still on
//!    the part, read five times per chunk at the 8 MHz control and then at
//!    16 and 32 MHz under every `IFTIMING.RXDELAY` from 0 to 7, one
//!    `READSWEEP` line each with `applied=` saying whether the registers
//!    read back as set; then one `SWEEPNARROW` line per faster clock
//!    that read clean only in a run narrower than `st::MIN_CLEAN_RUN`
//!    delays, and `SWEEPBEST`, the fastest clock with a run that wide and
//!    the middle of it (`st::SweepBest`). 73 s of bus time, about 177 to
//!    668 s with the comparison (`st::COMPARE_MS_MAX`, 6.1 s for a clean
//!    point and 35 s for one wrong in every byte); the whole run is
//!    bounded by `st::wall_bound_ms`, 15.4 min, against 134 s before the
//!    sweep, and prints that bound as its `PLAN` line when it starts.
//! 7. `BUS` back at 8 MHz and the default RXDELAY, then `ERASE pass=3`,
//!    verified at 8 MHz, so `final_state` is a measurement. The third
//!    erase is what leaves the part blank, so the SolarNode firmware's
//!    `qspi::log_store` mount reports `STORE state=unformatted` and not
//!    our pattern.
//! 8. `RESULT`. The sweep's red cells are not in it (`Run::verdict`).
//!
//! The run starts [`START_HOLD_S`] after boot and not at once, and the
//! whole report is printed again behind a `REPORT n=<k>` header every
//! [`REPORT_PERIOD_S`]. Both for the same reason: `just
//! flash-solarnode-qspi-selftest` ends in the flash runner's read-back
//! (`tools/fw-readback.sh`, `fw_read_banner`), which holds the debug port
//! open for `FW_READ_WINDOW` = 8 s and consumes every byte that arrives in
//! that window. A capture reader attached at the same time shares the tty
//! with it and gets only what it wins. On 2026-10-02 that took the 32
//! `CENSUS` lines and the `[FW_BUILD]` of t=5002 (the one the runner then
//! confirmed the flash with), and the first 60 s re-print was due after
//! the capture had already stopped.
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
use static_cell::ConstStaticCell;

use leviculum_nrf::boards::solarnode;
use leviculum_nrf::log_critical;
use leviculum_qspi_selftest as st;
use st::{
    Bus, BusClock, Cause, Census, CensusLine, Compare, ErasePass, Failure, MissLine, Op, Pattern,
    PlanLine, ReadSet, Run, Status, SweepBest, SweepCell, Tally, Timing, WritePass,
};

/// One read operation's worth. A multiple of 4, as the peripheral's
/// EasyDMA demands, and a divisor of a census window.
const CHUNK: usize = st::READ_CHUNK_BYTES as usize;

/// One DMA buffer. Static so EasyDMA reads into RAM and the main future
/// stays small; 4-byte aligned because `Qspi` asserts it.
#[repr(align(4))]
struct Buf([u8; CHUNK]);

/// One buffer per read of a read set, 20 KiB: the five reads of a chunk
/// are held side by side and compared with each other. Everything else
/// uses the first.
static BUFS: ConstStaticCell<[Buf; st::READS]> =
    ConstStaticCell::new([const { Buf([0u8; CHUNK]) }; st::READS]);

/// The nRF52840 WDT's RUNSTATUS register (base 0x4001_0000, offset 0x400,
/// `nrf-pac` `wdt::Wdt::runstatus`). Read raw because `embassy-nrf`
/// exports its PAC only under `unstable-pac`.
const WDT_RUNSTATUS: *const u32 = 0x4001_0400 as *const u32;

/// The QSPI `IFCONFIG1` register (base 0x4002_9000, offset 0x600,
/// `nrf-pac` `qspi::Qspi::ifconfig1`): SCKFREQ in 31:28.
const QSPI_IFCONFIG1: *mut u32 = 0x4002_9600 as *mut u32;
/// The QSPI `IFTIMING` register (offset 0x640): RXDELAY in 10:8.
const QSPI_IFTIMING: *mut u32 = 0x4002_9640 as *mut u32;

/// Seconds between re-prints of the whole report. Short enough that a
/// capture which outlives the run by a quarter minute holds one.
const REPORT_PERIOD_S: u64 = 15;

/// Seconds after boot before the run starts: past the flash runner's 8 s
/// read-back window (module docs), with margin for its re-enumeration.
const START_HOLD_S: u64 = 12;

/// The capture window this run needs, printed when it starts and in
/// every report.
const PLAN_LINE: PlanLine = PlanLine {
    hold_ms: START_HOLD_S as u32 * 1000,
};

fn line(args: core::fmt::Arguments) {
    leviculum_nrf::log::log_fmt_critical("[QSPI-TEST] ", args);
}

/// Let the debug writer drain before the next burst of lines. The writer
/// only runs when this task yields, and a report of several hundred lines
/// written without a yield would lap the 8 KiB ring itself.
async fn pace() {
    Timer::after_millis(2).await;
}

/// The bus timing registers, raw.
fn bus() -> Bus {
    // SAFETY: reads of two always-mapped QSPI registers.
    unsafe {
        Bus {
            ifconfig1: core::ptr::read_volatile(QSPI_IFCONFIG1),
            iftiming: core::ptr::read_volatile(QSPI_IFTIMING),
        }
    }
}

/// Set the QSPI clock and input sampling delay to `t` and return the
/// registers as they read back.
///
/// Only `IFCONFIG1.SCKFREQ` and `IFTIMING.RXDELAY` move. The `&mut Qspi`
/// is the proof that no operation is in flight: every one of them is
/// awaited to READY before its borrow ends, and a leaked one (`guarded`)
/// ends the run. The `READSET`/`READSWEEP` line after a switch carries
/// `ms`/`kib_s`, which is where a clock that did not take would show:
/// 2 MiB five times is about 2.7 s at 32 MHz, 5.4 s at 16 and 10.6 s at 8.
fn set_timing(_flash: &mut Qspi<'static>, t: Timing) -> Bus {
    // SAFETY: read-modify-writes of two always-mapped QSPI registers while
    // the peripheral is idle (see above).
    unsafe {
        let v = core::ptr::read_volatile(QSPI_IFCONFIG1);
        core::ptr::write_volatile(QSPI_IFCONFIG1, Bus::with_clock(v, t.clock));
        let v = core::ptr::read_volatile(QSPI_IFTIMING);
        core::ptr::write_volatile(QSPI_IFTIMING, Bus::with_rxdelay(v, t.rxdelay));
    }
    bus()
}

/// Move the bus to `t` unless it is there, printing the `BUS` line of a
/// move.
fn move_to(flash: &mut Qspi<'static>, t: Timing) -> Bus {
    let now = bus();
    if now.carries(t) {
        return now;
    }
    let moved = set_timing(flash, t);
    line(format_args!("{moved}"));
    moved
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
    clock: BusClock,
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
    Ok(WritePass {
        pass,
        pattern,
        clock,
        us,
    })
}

/// Read the whole part [`st::READS`] times against `plan.pattern` at the
/// bus's current setting.
async fn read_set(
    flash: &mut Qspi<'static>,
    bufs: &mut [Buf; st::READS],
    set: u8,
    plan: st::SetPlan,
) -> Result<ReadSet, Failure> {
    let (us, cmp) = read_compare(flash, bufs, plan.pattern).await?;
    Ok(ReadSet { set, plan, us, cmp })
}

/// Read the whole part [`st::READS`] times against `pattern`, chunk by
/// chunk: each chunk is read five times back to back into five buffers,
/// and the five are compared with the pattern and with each other.
/// Returns the summed read time and the comparison.
async fn read_compare(
    flash: &mut Qspi<'static>,
    bufs: &mut [Buf; st::READS],
    pattern: Pattern,
) -> Result<(u64, Compare), Failure> {
    let mut cmp = Compare::default();
    let mut us = 0u64;
    for addr in (0..st::PART_BYTES).step_by(CHUNK) {
        for buf in bufs.iter_mut() {
            us += guarded(
                ReadNorFlash::read(flash, addr, &mut buf.0),
                st::READ_TIMEOUT_MS,
                Op::Read,
                addr,
            )
            .await?;
        }
        let reads: [&[u8]; st::READS] = core::array::from_fn(|r| &bufs[r].0[..]);
        cmp.check(pattern, addr, reads);
    }
    Ok((us, cmp))
}

/// The read sweep over what write pass 2 left on the part, one
/// `READSWEEP` line per point and `SWEEPBEST` at the end.
async fn sweep(
    flash: &mut Qspi<'static>,
    bufs: &mut [Buf; st::READS],
    run: &mut Run,
) -> Result<(), Failure> {
    for (n, want) in st::SWEEP.into_iter().enumerate() {
        let bus = set_timing(flash, want);
        let (us, cmp) = read_compare(flash, bufs, st::SWEEP_PATTERN).await?;
        let cell = SweepCell::new(n as u8, want, bus, us, &cmp);
        run.sweep[n] = Some(cell);
        line(format_args!("{cell}"));
        pace().await;
    }
    print_best(&run.sweep);
    Ok(())
}

/// The `SWEEPNARROW` lines, fastest clock first, then `SWEEPBEST`.
fn print_best(cells: &[Option<SweepCell>]) {
    let best = SweepBest::of(cells);
    for narrow in best.narrow() {
        line(format_args!("{narrow}"));
    }
    line(format_args!("{best}"));
}

/// A read set's line and its `MISS` lines.
async fn print_set(r: &ReadSet) {
    line(format_args!("{r}"));
    for (n, miss) in r.cmp.misses().iter().enumerate() {
        line(format_args!(
            "{}",
            MissLine {
                set: r.set,
                n,
                miss: *miss
            }
        ));
        pace().await;
    }
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
async fn test(
    flash: &mut Qspi<'static>,
    bufs: &mut [Buf; st::READS],
    run: &mut Run,
) -> Result<(), Failure> {
    let status = read_status(flash)?;
    run.status = Some(status);
    line(format_args!("{status}"));
    if status.protection() != st::Protection::Clear {
        return Ok(());
    }

    move_to(flash, st::at_default_rxdelay(st::CENSUS_CLOCK));
    let mut census = Census::new();
    read_all(flash, &mut bufs[0], |addr, got| census.feed(addr, got)).await?;
    run.census = Some(census);
    for (win, window) in census.windows().iter().enumerate() {
        line(format_args!(
            "{}",
            CensusLine {
                win,
                window: *window
            }
        ));
        pace().await;
    }

    run.erase_issued = true;
    for (i, pattern) in [Pattern::A, Pattern::B].into_iter().enumerate() {
        let pass = i as u8 + 1;
        let erase = erase_pass(flash, &mut bufs[0], pass).await?;
        run.erases[i] = Some(erase);
        line(format_args!("{erase}"));
        let clock = st::PLAN[2 * i].write;
        move_to(flash, st::at_default_rxdelay(clock));
        let write = write_pass(flash, &mut bufs[0], pass, pattern, clock).await?;
        run.writes[i] = Some(write);
        line(format_args!("{write}"));
        for k in [2 * i, 2 * i + 1] {
            let plan = st::PLAN[k];
            move_to(flash, st::at_default_rxdelay(plan.read));
            let set = read_set(flash, bufs, k as u8 + 1, plan).await?;
            run.reads[k] = Some(set);
            print_set(&set).await;
        }
        if let Some(diag) = run.diags()[i] {
            line(format_args!("{diag}"));
        }
    }
    sweep(flash, bufs, run).await?;
    let last = st::ERASE_PASSES - 1;
    move_to(flash, st::at_default_rxdelay(st::ERASE_CLOCK[last]));
    let erase = erase_pass(flash, &mut bufs[0], st::ERASE_PASSES as u8).await?;
    run.erases[last] = Some(erase);
    line(format_args!("{erase}"));
    Ok(())
}

/// Every line the run produced, in its order, paced.
async fn print_report(run: &Run) {
    line(format_args!("{PLAN_LINE}"));
    if let Some(bus) = run.bus {
        line(format_args!("{bus}"));
    }
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
            pace().await;
        }
    }
    let diags = run.diags();
    for i in 0..st::ERASE_PASSES {
        if i == st::ERASE_PASSES - 1 && run.sweep.iter().any(Option::is_some) {
            for cell in run.sweep.iter().flatten() {
                line(format_args!("{cell}"));
                pace().await;
            }
            print_best(&run.sweep);
        }
        if let Some(e) = run.erases[i] {
            line(format_args!("{e}"));
        }
        if let Some(w) = run.writes.get(i).copied().flatten() {
            line(format_args!("{w}"));
        }
        for k in [2 * i, 2 * i + 1] {
            if let Some(r) = run.reads.get(k).copied().flatten() {
                print_set(&r).await;
            }
        }
        if let Some(d) = diags.get(i).copied().flatten() {
            line(format_args!("{d}"));
        }
        pace().await;
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
    line(format_args!("HOLD s={START_HOLD_S}"));
    Timer::after_secs(START_HOLD_S).await;

    let start = Instant::now();
    line(format_args!("{PLAN_LINE}"));
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
            let bus = bus();
            run.bus = Some(bus);
            line(format_args!("{bus}"));
            let bufs = BUFS.take();
            if let Err(failure) = test(flash, bufs, &mut run).await {
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
        print_report(&run).await;
    }
}
