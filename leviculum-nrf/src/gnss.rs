//! GNSS driver task: baud sweep, presence tri-state, NMEA fold.
//!
//! UARTE0 on P0.15 (RX from chip → MCU) / P0.16 (TX from MCU → chip) on
//! the WisMesh Pocket V2 (RAK19026 VC baseboard, u-blox ZOE-M8Q). The
//! `gnss` cargo feature only says the board routes this UART; whether a
//! receiver is attached and delivering is a runtime question with three
//! answers (Codeberg #240), owned by the pure
//! [`leviculum_gnss_presence::PresenceMachine`]:
//!
//! - the machine sweeps 9600 → 38400 → 115200 until a checksum-clean
//!   sentence locks a baud (so a non-default module still works),
//! - publishes `no-hardware` / `no-fix` / `fix` through
//!   [`GNSS_PRESENCE`] with Fix→NoFix hysteresis,
//! - and forwards parsed RMC/GGA content, which this task folds into
//!   the [`GNSS_FIX`] snapshot exactly as before.
//!
//! This task is deliberately a thin driver: reads via
//! `read_until_idle` (TIMER1 + PPI ch0/ch1 detect the line going quiet
//! between 1 Hz NMEA bursts, so chunks align with sentence boundaries
//! and nothing is lost between reads), a 1 s read timeout to carry
//! time into the machine when the line is silent, and UART
//! reconfiguration when the machine asks for a new baud. All policy —
//! window lengths, sweep order, hysteresis hold — lives host-tested in
//! the pure crate.
//!
//! No UBX-CFG init in this commit. The Meshtastic firmware's full
//! `_message_NAVX5 / _message_PMS / _message_CFG_PM2` chain
//! (`meshtastic/src/gps/ubx.h:38-321`) can be added later if power-save
//! or fix-quality tuning becomes necessary.
//!
//! The PPS pin (P0.17) is configured as a pull-down input but not used —
//! reserved for a future timestamp-capture iteration.

use embassy_executor::Spawner;
use embassy_nrf::gpio::{AnyPin, Input, Pull};
use embassy_nrf::peripherals;
use embassy_nrf::uarte::{self, Uarte};
use embassy_nrf::{bind_interrupts, Peri};
use embassy_time::{with_timeout, Duration, Instant, Timer};

use leviculum_gnss_presence::{Output, PresenceMachine};

use crate::baseboard::{GnssFix, GnssPresenceState, GNSS_FIX, GNSS_PRESENCE};

/// Convert nmea0183's positive-magnitude `Latitude` to signed decimal
/// degrees (negative south).
fn lat_to_f64(lat: &nmea0183::coords::Latitude) -> f64 {
    let mag = lat.as_f64();
    match lat.hemisphere {
        nmea0183::coords::Hemisphere::South => -mag,
        _ => mag,
    }
}

/// Convert nmea0183's positive-magnitude `Longitude` to signed decimal
/// degrees (negative west).
fn lon_to_f64(lon: &nmea0183::coords::Longitude) -> f64 {
    let mag = lon.as_f64();
    match lon.hemisphere {
        nmea0183::coords::Hemisphere::West => -mag,
        _ => mag,
    }
}

bind_interrupts!(pub struct GnssIrqs {
    UARTE0 => uarte::InterruptHandler<peripherals::UARTE0>;
});

/// Map a sweep baud to the UARTE register value. The machine only emits
/// values from `BAUD_SWEEP`; the catch-all keeps this total without an
/// `unwrap`.
fn baudrate_of(baud: u32) -> uarte::Baudrate {
    match baud {
        38_400 => uarte::Baudrate::BAUD38400,
        115_200 => uarte::Baudrate::BAUD115200,
        _ => uarte::Baudrate::BAUD9600,
    }
}

/// Act on one machine output: publish a transition (watch + debug
/// event), record a requested baud change, or fold RMC/GGA content into
/// the `GnssFix` snapshot (the fold is byte-for-byte the pre-#240
/// behaviour — `unix_secs` exists only while a valid RMC does, so the
/// #166 seed gate stays keyed to valid RMC only).
fn apply_output(output: Output, latest: &mut GnssFix, pending_baud: &mut Option<u32>) {
    let sender = GNSS_FIX.sender();
    match output {
        Output::Transition { state, baud } => {
            GNSS_PRESENCE
                .sender()
                .send(GnssPresenceState { state, baud });
            // Banner-class state event, same replay semantics as
            // `[TIME_SOURCE]`: bypasses the runtime-drain gate, last
            // line wins. Rate is bounded by the hysteresis hold and
            // duplicate-suppression in the machine.
            crate::log::log_fmt_critical(
                "[INFO!] ",
                format_args!("[GNSS_PRESENCE] state={} baud={}", state.as_str(), baud),
            );
        }
        Output::SetBaud(baud) => *pending_baud = Some(baud),
        Output::Rmc(rmc) => {
            latest.valid = rmc.mode.is_valid();
            if latest.valid {
                latest.latitude = Some(lat_to_f64(&rmc.latitude));
                latest.longitude = Some(lon_to_f64(&rmc.longitude));
                // RMC UTC is already leap-second-corrected by
                // the receiver — converted as-is, never via
                // raw GPS time (#166, time-and-clocks.md).
                latest.unix_secs = leviculum_gnss_time::unix_secs_from_rmc_utc(&rmc.datetime);
            } else {
                // A stale time claim must not outlive the fix
                // that made it: position keeps last-good for
                // the display, time does not.
                latest.unix_secs = None;
            }
            sender.send(*latest);
        }
        Output::Gga(gga) => {
            latest.sat_in_use = gga.sat_in_use;
            let fix = !matches!(gga.gps_quality, nmea0183::GPSQuality::NoFix);
            latest.valid = fix;
            if fix {
                latest.latitude = Some(lat_to_f64(&gga.latitude));
                latest.longitude = Some(lon_to_f64(&gga.longitude));
            }
            sender.send(*latest);
        }
    }
}

/// Pump UART bytes through the presence machine; publish presence
/// transitions via `GNSS_PRESENCE` and fix snapshots via `GNSS_FIX`.
#[embassy_executor::task]
#[allow(clippy::too_many_arguments)]
pub async fn gnss_task(
    mut uarte0: Peri<'static, peripherals::UARTE0>,
    mut timer1: Peri<'static, peripherals::TIMER1>,
    mut ppi_a: Peri<'static, peripherals::PPI_CH0>,
    mut ppi_b: Peri<'static, peripherals::PPI_CH1>,
    mut rx: Peri<'static, AnyPin>,
    mut tx: Peri<'static, AnyPin>,
    pps: Peri<'static, AnyPin>,
) {
    // Hold the PPS pin low-impedance enough that no spurious capture fires
    // before we wire it up. Drop returns it to its reset state on task exit
    // (which never happens for this task, but the convention is clear).
    let _pps = Input::new(pps, Pull::Down);

    let mut machine = PresenceMachine::new(Instant::now().as_millis());

    // Rolling GnssFix snapshot across sentences. RMC owns "is the
    // receiver happy" (mode is_valid()); GGA owns "how many sats".
    let mut latest = GnssFix::empty();
    let mut pending_baud: Option<u32> = None;

    let mut bytes_total: u32 = 0;
    let mut uart_errors: u32 = 0;
    let mut last_health_log = Instant::now();

    let mut configured_baud = machine.current_baud();
    crate::log::log_fmt(
        "[GNSS] ",
        format_args!("UARTE0 up, sweep start @ {} 8N1", configured_baud),
    );

    // Outer loop: one iteration per UART configuration. The machine
    // requests baud changes through `Output::SetBaud`; recreating the
    // Uarte from reborrowed peripherals is the supported embassy way to
    // change the baud rate (and re-derives the idle timeout from it).
    loop {
        let mut config = uarte::Config::default();
        config.baudrate = baudrate_of(configured_baud);
        let uart = Uarte::new(
            uarte0.reborrow(),
            rx.reborrow(),
            tx.reborrow(),
            GnssIrqs,
            config,
        );
        let (_uart_tx, mut uart_rx) =
            uart.split_with_idle(timer1.reborrow(), ppi_a.reborrow(), ppi_b.reborrow());

        // 256-byte chunk: a full 1 Hz NMEA burst (RMC+GGA+GSA — the
        // GSV tail may split) fits, and the idle detector ends each
        // read at the burst boundary anyway. The 1 s timeout exists to
        // carry time into the machine on a silent line (detection
        // windows, fix hold); when data flows, idle completes the read
        // long before it fires, so the cancel-loses-bytes race is
        // confined to near-silent lines where there is nothing to lose.
        let mut buf = [0u8; 256];
        let new_baud = loop {
            let now_ms = Instant::now().as_millis();
            match with_timeout(Duration::from_secs(1), uart_rx.read_until_idle(&mut buf)).await {
                Ok(Ok(n)) => {
                    bytes_total = bytes_total.saturating_add(n as u32);
                    machine.on_bytes(&buf[..n], now_ms, &mut |o| {
                        apply_output(o, &mut latest, &mut pending_baud)
                    });
                }
                Ok(Err(_e)) => {
                    // Framing/overrun. At a wrong sweep baud this can be
                    // every chunk — count it (the heartbeat reports it)
                    // instead of logging per error, and give the EasyDMA
                    // a moment instead of hot-looping.
                    uart_errors = uart_errors.saturating_add(1);
                    machine.on_uart_error(now_ms, &mut |o| {
                        apply_output(o, &mut latest, &mut pending_baud)
                    });
                    Timer::after(Duration::from_millis(50)).await;
                }
                Err(_timeout) => {
                    machine.poll(now_ms, &mut |o| {
                        apply_output(o, &mut latest, &mut pending_baud)
                    });
                }
            }

            // Heartbeat log every 5 s with cumulative counters. Keeps
            // the debug log readable but proves the GNSS pipe is alive.
            if last_health_log.elapsed().as_secs() >= 5 {
                crate::log::log_fmt(
                    "[GNSS] ",
                    format_args!(
                        "bytes={} sentences={} errs={} valid={} sat={} baud={}",
                        bytes_total,
                        machine.sentences_seen(),
                        uart_errors,
                        latest.valid,
                        latest.sat_in_use,
                        configured_baud,
                    ),
                );
                last_health_log = Instant::now();
            }

            if let Some(b) = pending_baud.take() {
                if b != configured_baud {
                    break b;
                }
            }
        };
        configured_baud = new_baud;
    }
}

/// Convenience wrapper invoked from the bin file.
#[allow(clippy::too_many_arguments)]
pub fn init(
    spawner: &Spawner,
    uarte0: Peri<'static, peripherals::UARTE0>,
    timer1: Peri<'static, peripherals::TIMER1>,
    ppi_a: Peri<'static, peripherals::PPI_CH0>,
    ppi_b: Peri<'static, peripherals::PPI_CH1>,
    rx: Peri<'static, AnyPin>,
    tx: Peri<'static, AnyPin>,
    pps: Peri<'static, AnyPin>,
) {
    spawner.must_spawn(gnss_task(uarte0, timer1, ppi_a, ppi_b, rx, tx, pps));
}
