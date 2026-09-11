//! RAKwireless RAK4631 module pin mappings and hardware constants.
//!
//! nRF52840 + SX1262 LoRa radio module on the RAK19026 VC baseboard
//! used in the WisMesh Pocket V2 (1.3" OLED, u-blox ZOE-M8Q GNSS,
//! LIS3DH accelerometer, battery + USB-C charger).
//!
//! Pin assignments verified against:
//!   * RAKwireless WisBlock vendor source (`PlatformIO/RAK4630/
//!     WisCore_RAK4631_Board/variant.h`) — module-level pinmap.
//!   * Meshtastic firmware (`variants/nrf52840/rak4631/variant.h`) —
//!     SX1262 wiring + RAK19026 baseboard peripherals.
//!   * RAKwireless RAK4631 datasheet:
//!     <https://docs.rakwireless.com/product-categories/wisblock/rak4631/datasheet/>

use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::peripherals;
use embassy_nrf::Peri;

// SX1262 LoRa Radio (module-internal SPI, not exposed on baseboard headers)
/// SX1262 NSS / chip select (active low)
pub type LoRaCs = peripherals::P1_10;
/// SX1262 SPI clock
pub type LoRaSck = peripherals::P1_11;
/// SX1262 SPI MOSI
pub type LoRaMosi = peripherals::P1_12;
/// SX1262 SPI MISO
pub type LoRaMiso = peripherals::P1_13;
/// SX1262 BUSY indicator (high = busy)
pub type LoRaBusy = peripherals::P1_14;
/// SX1262 DIO1 interrupt output
pub type LoRaDio1 = peripherals::P1_15;
/// SX1262 NRESET (active low)
pub type LoRaReset = peripherals::P1_06;
/// SX1262 LoRa front-end power enable (HIGH = on)
pub type LoRaPowerEn = peripherals::P1_05;

/// SX1262 SPI frequency in Hz (4 MHz, matches T114 / Meshtastic).
pub const LORA_SPI_FREQ_HZ: u32 = 4_000_000;
/// SX1262 TCXO voltage supplied via DIO3 (volts). RNode firmware drives the
/// RAK4631 TCXO at 3.3 V (`sx126x.cpp` `enableTCXO`: BOARD_RAK4631 ->
/// MODE_TCXO_3_3V_6X), unlike the Heltec T114 which it drives at 1.8 V. Match
/// the RAK reference here so the SX1262 clock is stable over long airtimes.
pub const LORA_TCXO_VOLTAGE: f32 = 3.3;
/// SX1262 max TX power (dBm). Same SX1262 die as on T114 → +22 dBm.
pub const LORA_MAX_POWER_DBM: i8 = 22;
/// SX1262 uses DIO2 as internal RF switch (no external RXEN/TXEN GPIO).
/// Antenna-switch pad P1.07 exists on the schematic but MUST NOT be driven
/// by the host — DIO2 owns it.
pub const LORA_DIO2_AS_RF_SWITCH: bool = true;

// LEDs (on the RAK4631 module, exposed via baseboard)
/// Green LED1 (active HIGH).
pub type LedPin = peripherals::P1_03;
/// Blue LED2 / notification (active HIGH).
pub type LedNotificationPin = peripherals::P1_04;

// User button (RAK19026 baseboard)
/// User button — active low + pull-up. Shared with NFC1 pin; the RAK
/// UF2 bootloader (`CONFIG_NFCT_PINS_AS_GPIOS` in its Makefile) patches
/// `UICR.NFCPINS` on first boot, persistently freeing P0.09 + P0.10
/// for GPIO use, so no UICR work is needed in firmware.
pub type UserButton = peripherals::P0_09;

// Peripheral 3V3 rail enable (RAK19026 baseboard, gates OLED + GNSS + sensors)
/// 3V3-S rail enable. **HIGH** powers OLED, ZOE-M8Q GNSS, LIS3DH and
/// the NCP5623 RGB driver simultaneously. Do **not** toggle this for
/// GPS-only power saving — Meshtastic's variant.h calls this out
/// explicitly.
pub type Periph3V3En = peripherals::P1_02;

// I²C1 (OLED + LIS3DH + NCP5623 on the baseboard)
/// I²C1 SDA. OLED at 0x3C (SH1106 vs SSD1306 detected at runtime),
/// LIS3DH at 0x18, NCP5623 at 0x38 (baseboard population unconfirmed).
pub type I2c1Sda = peripherals::P0_13;
/// I²C1 SCL.
pub type I2c1Scl = peripherals::P0_14;

// GNSS (u-blox ZOE-M8Q on the RAK19026 VC baseboard)
/// GNSS UART TX (MCU → ZOE-M8Q).
pub type GnssTx = peripherals::P0_16;
/// GNSS UART RX (ZOE-M8Q → MCU).
pub type GnssRx = peripherals::P0_15;
/// GNSS PPS / TIMEPULSE input. Wiring on the integrated VC baseboard
/// is inferred from Meshtastic — verify with a capture once the GNSS
/// UART is up.
pub type GnssPps = peripherals::P0_17;
/// GNSS UART baud rate. ZOE-M8Q ships at 9600 baud (u-blox factory
/// default).
pub const GNSS_BAUD: u32 = 9600;

// Battery (RAK19026 baseboard)
/// Battery voltage sense (AIN3 = P0.05).
pub type BatteryAdc = peripherals::P0_05;
/// ADC multiplier: AREF=3.0V internal, 12-bit, divider 1.5/2.5
/// (multiply ADC volts by 1.73 to recover VBAT in volts).
pub const ADC_MULTIPLIER: f32 = 1.73;

// QSPI Flash: NONE. There are deliberately no pin aliases here, and
// `CONFIG.qspi_part` is `None`, so `bin/rak4631.rs` never configures
// P0.03, P0.26, P0.30, P0.29, P0.28 or P0.02 as a flash bus. RAK says so
// themselves, in the board support package this map was copied out of
// (`RAKWireless/RAK-nRF52-Arduino`,
// `variants/WisCore_RAK4631_Board/variant.h`):
//
// ```c
// // QSPI Pins
// // QSPI occupied by GPIO's
// #define PIN_QSPI_SCK 3
// ...
// // On-board QSPI Flash
// // No onboard flash
// #define EXTERNAL_FLASH_DEVICES IS25LP080D
// ```
//
// The part name is a template line sitting under a comment that denies
// the part: "No onboard flash", and the pins above it are "occupied by
// GPIO's". RAK's RAK4631 datasheet lists the QSPI functions on the
// module pads and names no flash part, and RAK sells flash as a separate
// WisBlock module (RAK15001) — which is what a module with one on board
// would not need. Measured on the field Pocket `ABFAB3F1807E459B`
// (`de6e74ed`): every pin follows our drive, and nothing answers `05h`,
// `9Fh`, `90h` or the `66h`/`99h` reset, before or after it. Same four
// lines as both T114s.
//
// So: this module has no answering part either. Do not "add the missing
// flash" from a variant header — `EXTERNAL_FLASH_DEVICES` without a
// part is exactly how this error travelled through three projects
// (Codeberg #384; the same template artefact is in Heltec's header, see
// `boards/t114.rs`).
// <https://github.com/RAKWireless/RAK-nRF52-Arduino/blob/master/variants/WisCore_RAK4631_Board/variant.h>

/// Create the green LED1 output. **Active HIGH**. Start with Level::Low
/// so the LED is off at boot.
pub fn led(pin: Peri<'static, LedPin>) -> Output<'static> {
    Output::new(pin, Level::Low, OutputDrive::Standard)
}

/// Create the blue LED2 output. **Active HIGH**, identical electrical
/// shape to LED1. Off at boot.
pub fn led_notification(pin: Peri<'static, LedNotificationPin>) -> Output<'static> {
    Output::new(pin, Level::Low, OutputDrive::Standard)
}

/// Runtime board metadata for shared init code (USB / flash / LoRa).
pub const CONFIG: super::BoardConfig = super::BoardConfig {
    usb_vid: 0x1209,
    usb_pid: 0x0002,
    usb_manufacturer: "leviculum",
    usb_product: "leviculum RAK4631",
    log_prefix: "RAK",
    identity_flash_page: 0xEC000,
    radio_config_flash_page: 0xEB000,
    telemetry_flash_page: 0xEA000,
    lora_tcxo_voltage_reg: 0x07, // 3.3 V (RNode MODE_TCXO_3_3V_6X for BOARD_RAK4631)
    lora_spi_freq_hz: LORA_SPI_FREQ_HZ,
    lora_max_power_dbm: LORA_MAX_POWER_DBM,
    // No QSPI part on this module. See the block above.
    qspi_part: None,
};

/// Panic-LED descriptor — port, pin, active-low flag — for `set_panic_led`.
/// LED1 (green) on P1.03, **active high**.
pub const PANIC_LED_PORT: u8 = 1;
pub const PANIC_LED_PIN: u8 = 3;
pub const PANIC_LED_ACTIVE_LOW: bool = false;
