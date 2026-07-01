//! Shared state for RAK19026 baseboard peripherals (display + GNSS + battery).
//!
//! This module is the rendezvous point between the producer tasks (GNSS,
//! battery) and the consumer task (display). It is compiled whenever any
//! one of the three feature flags is enabled; each individual `Watch` /
//! struct definition is then gated on the producing peripheral's feature.

#![allow(dead_code)]

#[cfg(any(feature = "display", feature = "gnss", feature = "battery"))]
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
#[cfg(any(feature = "display", feature = "gnss", feature = "battery"))]
use embassy_sync::watch::Watch;

/// Compact GNSS fix snapshot — what the display task needs and nothing
/// more. Updated by `gnss::gnss_task` when an RMC or GGA sentence parses
/// cleanly. `valid` is the receiver's own validity flag (RMC `Mode::is_valid`
/// or GGA quality != NoFix), not a project-wide policy.
///
/// `latitude` / `longitude` are decimal degrees with sign (negative = S/W,
/// positive = N/E). They keep their last good values across non-valid
/// sentences so the display has something to render even briefly during
/// a fix dip.
#[cfg(feature = "gnss")]
#[derive(Clone, Copy, Debug)]
pub struct GnssFix {
    /// True when the receiver reports a usable fix.
    pub valid: bool,
    /// Number of satellites in use, from the latest GGA.
    pub sat_in_use: u8,
    /// Latitude in decimal degrees (signed). `None` until the first valid
    /// sentence carries position data.
    pub latitude: Option<f64>,
    /// Longitude in decimal degrees (signed). Same convention as latitude.
    pub longitude: Option<f64>,
}

#[cfg(feature = "gnss")]
impl GnssFix {
    pub const fn empty() -> Self {
        Self {
            valid: false,
            sat_in_use: 0,
            latitude: None,
            longitude: None,
        }
    }
}

/// Latest GNSS fix snapshot. Capacity-2 watch: one slot for the producer,
/// one for the display consumer.
#[cfg(feature = "gnss")]
pub static GNSS_FIX: Watch<CriticalSectionRawMutex, GnssFix, 2> = Watch::new();

/// Battery snapshot published by the SAADC task and consumed by the display.
///
/// `voltage_mv` is the pack voltage in millivolts at the battery terminal
/// (already multiplied by the 1.73 hardware divider compensation).
/// `percent` is mapped from `voltage_mv` via the per-cell LiPo OCV curve
/// in `battery.rs`, scaled by the detected cell count.
/// `cell_count` is 1 or 2; on the WisMesh Pocket V2 the value is detected
/// at first boot from the read voltage and persisted, so subsequent
/// boots get a stable result.
#[cfg(feature = "battery")]
#[derive(Clone, Copy, Debug)]
pub struct BatteryState {
    pub voltage_mv: u16,
    pub percent: u8,
    pub cell_count: u8,
}

#[cfg(feature = "battery")]
impl BatteryState {
    pub const fn empty() -> Self {
        Self {
            voltage_mv: 0,
            percent: 0,
            cell_count: 1,
        }
    }
}

/// Latest battery snapshot. Same capacity-2 watch shape as `GNSS_FIX`.
#[cfg(feature = "battery")]
pub static BATTERY_STATE: Watch<CriticalSectionRawMutex, BatteryState, 2> = Watch::new();

/// Requested display power state, written by the user-button task and read
/// by both the display task (to drive `set_display_on`) and the button task
/// itself (to know what the next press should do).
///
/// Capacity-3 watch: button-writer + button-reader (current-state lookup) +
/// display-reader.
#[cfg(feature = "display")]
pub static DISPLAY_ON_REQ: Watch<CriticalSectionRawMutex, bool, 3> = Watch::new();

/// One-shot signals raised by `lora.rs` whenever a packet boundary
/// completes. The LED pulse tasks in `led.rs` block on these and emit a
/// short flash each time. Coalescing is intentional — a Signal stores at
/// most one pending event, so a burst of TX-completes inside an 80 ms
/// window collapses to a single flash. That matches what the eye can
/// resolve anyway.
#[cfg(feature = "display")]
pub static LORA_TX_FLASH: embassy_sync::signal::Signal<CriticalSectionRawMutex, ()> =
    embassy_sync::signal::Signal::new();

#[cfg(feature = "display")]
pub static LORA_RX_FLASH: embassy_sync::signal::Signal<CriticalSectionRawMutex, ()> =
    embassy_sync::signal::Signal::new();
