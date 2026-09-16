//! Board-specific pin mappings and shared board metadata for the ESP32
//! class.
//!
//! # What "class" means here, and why the split is drawn now
//!
//! This crate exists to serve four boards (Heltec V3, Heltec V4, two
//! T-Beams) that differ in SoC family (ESP32 vs ESP32-S3), in radio part
//! and in front end, but agree on everything the firmware does above the
//! pin: one `esp_hal::init`, one clock configuration, one USB serial the
//! banner goes out of, one SX1262 command sequence set out of
//! [`leviculum_core::sx126x`]. The split therefore runs exactly along
//! "does this fact change when the board changes":
//!
//!  * **per board** — which GPIO carries which net, which of them the
//!    radio uses, where the LED sits and in which direction it conducts,
//!    the battery divider ratio, whether a GNSS receiver is fitted. All
//!    of it in `boards/<board>.rs`, every constant carrying the document
//!    it was read out of.
//!  * **per class** — the order init runs in, the clock, bringing up the
//!    USB Serial/JTAG peripheral, formatting and emitting the boot
//!    banner. All of it in `lib.rs`, taking a `&'static BoardConfig` so
//!    one code path serves every binary target.
//!
//! Adding the second board is then: one `boards/<name>.rs` with its pins
//! and its `CONFIG`, one `src/bin/<name>.rs` that hands those pins to the
//! shared init, one `[[bin]]` stanza and one `Justfile` recipe. No new
//! idiom, and nothing in `lib.rs` moves.
//!
//! One build per pinout family remains the policy
//! (`docs/src/concepts/board-support-scope.md`): a board file describes a
//! wiring, not a stock-keeping unit, and two boards that agree on every
//! net share one binary.

pub mod heltec_v4;

/// Runtime board metadata consumed by the shared init code.
///
/// Every board module exposes a `pub const CONFIG: BoardConfig` with its
/// own values; shared init functions take `&'static BoardConfig` so the
/// same code path serves every binary target.
///
/// Deliberately small. A field here is a fact some shared function reads;
/// a constant only the board's own module uses stays in that module,
/// where it sits next to the schematic citation it came from. The nRF
/// crate learned this the hard way — a `BoardConfig` that grew fields
/// nothing consumed grew fields nothing could check either.
pub struct BoardConfig {
    /// Short tag used in identity log lines (e.g. "V4").
    pub log_prefix: &'static str,
    /// Human-readable board name, emitted once at boot.
    pub board_name: &'static str,
    /// SX1262 SPI bus frequency in Hz.
    pub lora_spi_freq_hz: u32,
    /// SX1262 maximum TX power in dBm at the chip's own output, before
    /// any external front end.
    pub lora_max_power_dbm: i8,
    /// Whether a GNSS receiver is fitted on the board itself.
    ///
    /// Not "is there a GNSS header" — the V4 has one and reports `false`
    /// here, because what is soldered decides whether the firmware may
    /// assume a receiver answers.
    pub gnss_onboard: bool,
}
