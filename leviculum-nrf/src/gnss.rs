//! u-blox ZOE-M8Q GNSS on the WisMesh Pocket V2 (RAK19026 VC baseboard).
//!
//! UARTE0 on P0.15 (RX from chip → MCU) / P0.16 (TX from MCU → chip),
//! 9600 8N1, the ZOE-M8Q factory default. We rely on that default and
//! consume RMC + GGA from the standard NMEA stream — no UBX-CFG init in
//! this commit. The Meshtastic firmware's full
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

use crate::baseboard::{GnssFix, GNSS_FIX};

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

/// Pump bytes from the GNSS UART into an `nmea0183::Parser` and republish
/// fixes through `GNSS_FIX`.
#[embassy_executor::task]
pub async fn gnss_task(
    uarte0: Peri<'static, peripherals::UARTE0>,
    rx: Peri<'static, AnyPin>,
    tx: Peri<'static, AnyPin>,
    pps: Peri<'static, AnyPin>,
) {
    // Hold the PPS pin low-impedance enough that no spurious capture fires
    // before we wire it up. Drop returns it to its reset state on task exit
    // (which never happens for this task, but the convention is clear).
    let _pps = Input::new(pps, Pull::Down);

    let mut config = uarte::Config::default();
    config.baudrate = uarte::Baudrate::BAUD9600;
    let mut uart = Uarte::new(uarte0, rx, tx, GnssIrqs, config);

    let mut parser = nmea0183::Parser::new();
    let sender = GNSS_FIX.sender();

    // Track the rolling state across sentences. RMC owns "is the receiver
    // happy" (mode is_valid()); GGA owns "how many sats". Together they
    // form the GnssFix snapshot we publish.
    let mut latest = GnssFix::empty();

    // 256-byte chunk: at 9600 baud (~960 B/s) this fills in ~270 ms, well
    // above the per-byte parse cost. Smaller buffers (64 B at 67 ms) caused
    // EasyDMA Overrun under sustained NMEA flow during early bring-up.
    let mut buf = [0u8; 256];
    let mut bytes_total: u32 = 0;
    let mut sentences_seen: u32 = 0;
    let mut last_health_log = embassy_time::Instant::now();

    crate::log::log_fmt("[GNSS] ", format_args!("UARTE0 up @ 9600 8N1, buf=256"));

    loop {
        match uart.read(&mut buf).await {
            Ok(()) => {
                bytes_total = bytes_total.saturating_add(buf.len() as u32);
                for b in buf.iter() {
                    if let Some(result) = parser.parse_from_byte(*b) {
                        sentences_seen = sentences_seen.saturating_add(1);
                        match result {
                            Ok(nmea0183::ParseResult::RMC(Some(rmc))) => {
                                latest.valid = rmc.mode.is_valid();
                                if latest.valid {
                                    latest.latitude = Some(lat_to_f64(&rmc.latitude));
                                    latest.longitude = Some(lon_to_f64(&rmc.longitude));
                                }
                                sender.send(latest);
                            }
                            Ok(nmea0183::ParseResult::GGA(Some(gga))) => {
                                latest.sat_in_use = gga.sat_in_use;
                                let fix = !matches!(gga.gps_quality, nmea0183::GPSQuality::NoFix);
                                latest.valid = fix;
                                if fix {
                                    latest.latitude = Some(lat_to_f64(&gga.latitude));
                                    latest.longitude = Some(lon_to_f64(&gga.longitude));
                                }
                                sender.send(latest);
                            }
                            // Receiver up but no solution / non-fix sentence —
                            // we counted it; nothing else to do.
                            _ => {}
                        }
                    }
                }
                // Heartbeat log every 5 s with cumulative counters. Keeps the
                // debug log readable but proves the GNSS pipe is alive.
                if last_health_log.elapsed().as_secs() >= 5 {
                    crate::log::log_fmt(
                        "[GNSS] ",
                        format_args!(
                            "bytes={} sentences={} valid={} sat={}",
                            bytes_total, sentences_seen, latest.valid, latest.sat_in_use
                        ),
                    );
                    last_health_log = embassy_time::Instant::now();
                }
            }
            Err(e) => {
                crate::log::log_fmt("[GNSS] ", format_args!("UART read err: {:?}", e));
                // Brief pause to let the EasyDMA recover instead of hot-looping
                // on overruns.
                embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;
            }
        }
    }
}

/// Convenience wrapper invoked from the bin file.
pub fn init(
    spawner: &Spawner,
    uarte0: Peri<'static, peripherals::UARTE0>,
    rx: Peri<'static, AnyPin>,
    tx: Peri<'static, AnyPin>,
    pps: Peri<'static, AnyPin>,
) {
    spawner.must_spawn(gnss_task(uarte0, rx, tx, pps));
}
