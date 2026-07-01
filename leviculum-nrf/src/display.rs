//! 1.3" OLED status display on the WisMesh Pocket V2 (RAK19026 VC baseboard).
//!
//! Builds an I²C bus on TWISPI0 with SDA on P0.13 / SCL on P0.14, runtime-
//! probes for an OLED at 0x3C (and 0x3D as fallback), and drives an SSD1306
//! status screen at ~1 Hz.
//!
//! Chip-detection follows Meshtastic's `src/detect/ScanI2CTwoWire.cpp:52-87`
//! exactly: send register address `0x00`, read one byte, mask the lower
//! nibble, and classify per the lookup table. SH1106 detection is logged
//! but not currently rendered; the only sh1106 crate on crates.io still
//! pins embedded-hal 0.2.3 which is not directly usable with embassy-nrf
//! 0.9's TWIM. SH1106 hardware is uncommon on RAK19026 VC and adding it
//! is a follow-up.

extern crate alloc;

use core::sync::atomic::Ordering;

use embassy_executor::Spawner;
use embassy_nrf::peripherals;
use embassy_nrf::twim::{self, Twim};
use embassy_nrf::{bind_interrupts, Peri};
use embassy_time::{Duration, Timer};

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};

bind_interrupts!(pub struct DisplayIrqs {
    TWISPI0 => twim::InterruptHandler<peripherals::TWISPI0>;
});

/// I²C addresses Meshtastic probes for an OLED. Order matters — 0x3C first.
const PROBE_ADDRS: [u8; 2] = [0x3C, 0x3D];

/// What the chip-probe found. Only the first variant is renderable today.
#[derive(Clone, Copy, Debug)]
enum DetectedKind {
    Ssd1306,
    Sh1106,
    None,
}

#[derive(Clone, Copy, Debug)]
struct Detected {
    kind: DetectedKind,
    addr: u8,
}

/// Address-only ACK probe — matches Meshtastic's `scanPort` quick-test
/// (`beginTransmission(addr); endTransmission()` with no payload). A
/// SSD1306/SH1106 in display-off mode will still ACK its own address even
/// when it would NACK a register read; using `write_read` for the probe
/// (as we did initially) loses those slaves.
async fn ack_probe(twim: &mut Twim<'_>, addr: u8) -> bool {
    twim.write(addr, &[]).await.is_ok()
}

/// Read the SSD1306/SH1106 status byte at register 0x00 with the
/// stabilization loop from `meshtastic/src/detect/ScanI2CTwoWire.cpp:52-87`.
/// Only called after `ack_probe` confirmed the slave is alive.
async fn classify(twim: &mut Twim<'_>, addr: u8) -> DetectedKind {
    let mut r: u8 = 0;
    let mut prev: u8 = 0xFF;
    let mut tries: u8 = 0;
    while r != prev && tries < 4 {
        prev = r;
        let mut buf = [0u8; 1];
        if twim.write_read(addr, &[0x00], &mut buf).await.is_err() {
            return DetectedKind::None;
        }
        r = buf[0] & 0x0F;
        tries += 1;
    }
    match r {
        0x00 | 0x08 => DetectedKind::Sh1106,
        0x03..=0x07 => DetectedKind::Ssd1306,
        _ => DetectedKind::None,
    }
}

/// Probe both standard addresses; the first hit wins.
///
/// If the slave ACKs its address but `classify()` returns `None` (typical
/// when an SSD1306 is in display-off and won't honour a status read), we
/// default to SSD1306. The RAK19026 VC's RAK-vendor sample driver hard-
/// codes SSD1306, so this is the correct default for this carrier; SH1106
/// detection is best-effort and currently not rendered anyway.
async fn detect(twim: &mut Twim<'_>) -> Detected {
    for &addr in &PROBE_ADDRS {
        if ack_probe(twim, addr).await {
            let kind = classify(twim, addr).await;
            let kind = match kind {
                DetectedKind::None => DetectedKind::Ssd1306,
                k => k,
            };
            return Detected { kind, addr };
        }
    }
    Detected {
        kind: DetectedKind::None,
        addr: 0,
    }
}

/// Format the identity hash as 10 hex chars (5 bytes).
fn ident_short(hash: &[u8; 16]) -> heapless::String<10> {
    let mut s: heapless::String<10> = heapless::String::new();
    let hex = b"0123456789abcdef";
    for b in &hash[..5] {
        let _ = s.push(hex[(*b >> 4) as usize] as char);
        let _ = s.push(hex[(*b & 0x0F) as usize] as char);
    }
    s
}

#[embassy_executor::task]
pub async fn display_task(
    twispi0: Peri<'static, peripherals::TWISPI0>,
    sda: Peri<'static, peripherals::P0_13>,
    scl: Peri<'static, peripherals::P0_14>,
    identity_hash: [u8; 16],
) {
    static mut TX_BUF: [u8; 64] = [0u8; 64];

    let mut config = twim::Config::default();
    config.frequency = twim::Frequency::K400;
    // The RAK19026 VC baseboard exposes I²C1 (SDA P0.13 / SCL P0.14) without
    // external pull-up resistors — internal nRF52840 pull-ups are required.
    config.sda_pullup = true;
    config.scl_pullup = true;

    // SAFETY: TX_BUF is owned exclusively by this single, never-respawned
    // task. We hand it to Twim by `&mut` for the lifetime of the task.
    let tx_buf: &'static mut [u8] = unsafe { &mut *core::ptr::addr_of_mut!(TX_BUF) };
    let mut twim = Twim::new(twispi0, DisplayIrqs, sda, scl, config, tx_buf);

    // OLEDs (and several baseboard sensors) need a few hundred ms after the
    // 3V3-S rail comes up before they ACK on I²C. Give the bus 500 ms quiet
    // time so the very first probe doesn't see a still-resetting chip.
    Timer::after(Duration::from_millis(500)).await;

    let detected = detect(&mut twim).await;
    crate::log::log_fmt(
        "[DISP] ",
        format_args!("OLED probe: {:?} at 0x{:02X}", detected.kind, detected.addr),
    );

    let mut display = match detected.kind {
        DetectedKind::Ssd1306 => {
            let interface = I2CDisplayInterface::new_custom_address(twim, detected.addr);
            let mut d = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
                .into_buffered_graphics_mode();
            if let Err(e) = d.init() {
                crate::log::log_fmt("[DISP] ", format_args!("init failed: {:?}", e));
                return;
            }
            crate::log::log_fmt("[DISP] ", format_args!("SSD1306 init ok"));
            d
        }
        DetectedKind::Sh1106 => {
            crate::log::log_fmt(
                "[DISP] ",
                format_args!("SH1106 detected — not rendered (driver pending)"),
            );
            return;
        }
        DetectedKind::None => {
            crate::log::log_fmt(
                "[DISP] ",
                format_args!("no OLED found on TWISPI0 — display task exiting"),
            );
            return;
        }
    };

    let text_style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();
    let id_short = ident_short(&identity_hash);

    // Receivers for shared baseboard state — held outside the loop so the
    // last value survives between producer updates.
    #[cfg(feature = "gnss")]
    let mut gnss_rx = crate::baseboard::GNSS_FIX
        .receiver()
        .expect("gnss watch capacity");
    #[cfg(feature = "battery")]
    let mut bat_rx = crate::baseboard::BATTERY_STATE
        .receiver()
        .expect("battery watch capacity");

    // Display power state, controlled by `crate::button` via DISPLAY_ON_REQ.
    // We seed the watch to `true` so the button task can read the current
    // state before the user has ever pressed anything.
    let display_req_sender = crate::baseboard::DISPLAY_ON_REQ.sender();
    display_req_sender.send(true);
    let mut display_req_rx = crate::baseboard::DISPLAY_ON_REQ
        .receiver()
        .expect("DISPLAY_ON_REQ watch capacity (display reader)");
    let mut display_on = true;

    // Frame key — when nothing relevant has changed since last render, we
    // skip the I²C flush entirely. lat/lon are quantized to 5 decimal
    // places (≈1 m); receiver-jitter below that level no longer flips the
    // frame. The heartbeat phase bumps the key every 5 s so the screen
    // refreshes itself slowly (lifetime indicator).
    #[derive(PartialEq, Eq)]
    struct FrameKey {
        rx: u32,
        tx: u32,
        sat: u8,
        valid: bool,
        // Latitude/longitude quantized to 5dp via int(round(value * 1e5)).
        // Option<i64> survives the no-fix transition.
        lat_e5: Option<i64>,
        lon_e5: Option<i64>,
        // Battery percent — None when feature off; quantized to 1 % so
        // ADC-noise doesn't trigger every-render flushes.
        bat_pct: Option<u8>,
        // Pack voltage rounded to 100 mV so noise on the LSDigit doesn't
        // wake the renderer. Same Option-treatment as bat_pct.
        bat_dv: Option<u16>,
        heartbeat: bool,
    }

    let mut last_key: Option<FrameKey> = None;
    let mut tick: u32 = 0;

    loop {
        tick = tick.wrapping_add(1);
        let heartbeat = (tick / 5) & 1 != 0; // toggles every 5 seconds

        // Honour the latest power-state request from the button task. We
        // only act on actual changes — `try_changed()` returns `Some(_)`
        // exactly once per new value.
        if let Some(req) = display_req_rx.try_changed() {
            if req != display_on {
                if let Err(e) = display.set_display_on(req) {
                    crate::log::log_fmt(
                        "[DISP] ",
                        format_args!("set_display_on({}) failed: {:?}", req, e),
                    );
                } else {
                    crate::log::log_fmt(
                        "[DISP] ",
                        format_args!("display power → {}", if req { "on" } else { "off" }),
                    );
                    display_on = req;
                }
            }
        }

        // While the screen is off there is no point computing or flushing
        // a frame. Tick at the same 1 Hz cadence so the request poll above
        // still runs.
        if !display_on {
            Timer::after(Duration::from_secs(1)).await;
            continue;
        }

        let rx = crate::lora::LORA_RX_COUNT.load(Ordering::Relaxed);
        let tx = crate::lora::LORA_TX_COUNT.load(Ordering::Relaxed);

        let mut line2 = heapless::String::<24>::new();
        let _ = core::fmt::write(&mut line2, format_args!("ID: {}", id_short));

        let mut line3 = heapless::String::<24>::new();
        let _ = core::fmt::write(&mut line3, format_args!("RX: {:<5} TX: {:<5}", rx, tx));

        let mut line4_buf = heapless::String::<24>::new();
        #[allow(unused_assignments, unused_mut)]
        let mut bat_pct: Option<u8> = None;
        #[allow(unused_assignments, unused_mut)]
        let mut bat_dv: Option<u16> = None;
        #[cfg(feature = "battery")]
        {
            match bat_rx.try_get() {
                Some(b) if b.voltage_mv > 0 => {
                    bat_pct = Some(b.percent);
                    // Voltage in deci-volts (e.g. 3.92 V → 39 dV) so the
                    // frame key only flips on visible 100 mV changes.
                    bat_dv = Some(b.voltage_mv / 100);
                    let v_int = b.voltage_mv / 1000;
                    let v_frac = (b.voltage_mv % 1000) / 10;
                    let _ = core::fmt::write(
                        &mut line4_buf,
                        format_args!("Bat: {:>3}% {}.{:02}V", b.percent, v_int, v_frac),
                    );
                }
                _ => {
                    let _ = core::fmt::write(&mut line4_buf, format_args!("Bat: --"));
                }
            }
        }
        #[cfg(not(feature = "battery"))]
        {
            let _ = core::fmt::write(&mut line4_buf, format_args!("Bat: -- (no feature)"));
        }

        // Defaults — overwritten if `gnss` is on.
        let mut line5_buf = heapless::String::<24>::new();
        let mut line6_buf = heapless::String::<24>::new();
        #[allow(unused_assignments, unused_mut)]
        let mut sat: u8 = 0;
        #[allow(unused_assignments, unused_mut)]
        let mut valid = false;
        #[allow(unused_assignments, unused_mut)]
        let mut lat_e5: Option<i64> = None;
        #[allow(unused_assignments, unused_mut)]
        let mut lon_e5: Option<i64> = None;

        #[cfg(feature = "gnss")]
        {
            let fix_opt = gnss_rx.try_get();
            let (label, sats) = match fix_opt {
                Some(f) if f.valid => ("fix", f.sat_in_use),
                Some(f) => ("search", f.sat_in_use),
                None => ("init", 0),
            };
            sat = sats;
            valid = matches!(fix_opt, Some(f) if f.valid);
            let _ = core::fmt::write(&mut line5_buf, format_args!("GPS: {} sat {}", sats, label));
            match fix_opt.and_then(|f| f.latitude.zip(f.longitude)) {
                Some((lat, lon)) => {
                    // 5dp in the displayed string and the frame key.
                    // `as i64` truncates toward zero; that's fine for change
                    // detection — a single LSB flip from rounding semantics
                    // would just trigger one extra render, and at 5dp each
                    // unit equals ~1.1 m so coordinate jitter rarely flips
                    // the truncated key at all.
                    let _ = core::fmt::write(&mut line6_buf, format_args!("{:.5},{:.5}", lat, lon));
                    lat_e5 = Some((lat * 1e5) as i64);
                    lon_e5 = Some((lon * 1e5) as i64);
                }
                None => {
                    let _ = core::fmt::write(&mut line6_buf, format_args!("(no fix)"));
                }
            }
        }
        #[cfg(not(feature = "gnss"))]
        {
            let _ = core::fmt::write(&mut line5_buf, format_args!("GPS: -- (no feature)"));
            let _ = core::fmt::write(&mut line6_buf, format_args!(""));
        }

        let key = FrameKey {
            rx,
            tx,
            sat,
            valid,
            lat_e5,
            lon_e5,
            bat_pct,
            bat_dv,
            heartbeat,
        };
        if last_key.as_ref() == Some(&key) {
            // Nothing meaningful changed since last render and the
            // heartbeat phase is the same. Skip the I²C flush entirely.
            Timer::after(Duration::from_secs(1)).await;
            continue;
        }

        let _ = display.clear(BinaryColor::Off);

        let lines: [&str; 6] = [
            "leviculum RAK4631",
            line2.as_str(),
            line3.as_str(),
            line4_buf.as_str(),
            line5_buf.as_str(),
            line6_buf.as_str(),
        ];
        for (i, s) in lines.iter().enumerate() {
            let y = (i as i32) * 10;
            let _ = Text::with_baseline(s, Point::new(0, y), text_style, Baseline::Top)
                .draw(&mut display);
        }

        // Heartbeat marker — a 2×2 dot in the top-right corner, drawn only
        // during the "on" phase. Tells you "the firmware loop is alive"
        // without redrawing the rest of the screen at any visible rate.
        if heartbeat {
            let _ = Rectangle::new(Point::new(125, 0), Size::new(2, 2))
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                .draw(&mut display);
        }

        if let Err(e) = display.flush() {
            crate::log::log_fmt("[DISP] ", format_args!("flush failed: {:?}", e));
        }
        last_key = Some(key);

        Timer::after(Duration::from_secs(1)).await;
    }
}

/// Convenience wrapper invoked from the bin file.
pub fn init(
    spawner: &Spawner,
    twispi0: Peri<'static, peripherals::TWISPI0>,
    sda: Peri<'static, peripherals::P0_13>,
    scl: Peri<'static, peripherals::P0_14>,
    identity_hash: [u8; 16],
) {
    spawner.must_spawn(display_task(twispi0, sda, scl, identity_hash));
}
