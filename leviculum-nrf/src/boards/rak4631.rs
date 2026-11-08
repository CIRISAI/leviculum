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

// The TCXO runs at 3.3 V (`CONFIG.lora_tcxo_voltage_reg`): RNode firmware
// drives the RAK4631 TCXO there (`sx126x.cpp` `enableTCXO`: BOARD_RAK4631 ->
// MODE_TCXO_3_3V_6X), unlike the Heltec T114 which it drives at 1.8 V, so the
// SX1262 clock is stable over long airtimes. DIO2 drives the RF switch, there
// is no RXEN/TXEN GPIO: the antenna-switch pad P1.07 exists on the schematic
// but MUST NOT be driven by the host — DIO2 owns it. The SPI clock is
// `lora::init`'s, the power ceiling the transmit-power sequence's
// (`leviculum_core::sx126x`, the same die and +22 dBm as on the T114).

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
/// GNSS UART baud rate. ZOE-M8Q ships at 9600 baud (u-blox factory
/// default), where the presence machine's sweep starts; the boot line
/// `[GNSS_PRESENCE] state=detecting` names it.
pub const GNSS_BAUD: u32 = 9600;

// GNSS PPS / TIMEPULSE: NONE, and this is the second alias on this board
// that had to be deleted rather than corrected (Codeberg #394; the QSPI
// flash below is the first). There was a `GnssPps = P0_17` here, and
// `bin/rak4631.rs` handed it to the GNSS task, on the strength of a
// Meshtastic line that says so in as many words:
//
// ```c
// // RAK1910 GPS module
// // If using the wisblock GPS module and pluged into Port A on WisBlock base
// // IO1 is hooked to PPS (pin 12 on header) = gpio 17
// #define PIN_GPS_PPS (17) // Pulse per second input from the GPS
// ```
//
// Read the comment above the define and it is about a different receiver
// on a different baseboard: the RAK1910 is a plug-in WisBlock module, and
// the sentence describes what *that* card does to the Port A header. It
// says nothing about the ZOE-M8Q soldered onto a VC board, and our own
// doc comment admitted as much — "inferred from Meshtastic, verify with a
// capture". Nobody captured; the inference stood from 2026-04-28 to
// 2026-09-24.
//
// The RAK19026 VC schematic (RAKwireless, 11/26/2024, five sheets on the
// WisMesh Base Board VC datasheet page) says the opposite twice over:
//
//   * Sheet 3, U6 `ZOE-M8Q-0`, pin C3 `TIMEPULSE` → net `1PPS` → `R39`,
//     marked `0/NC`: an unfitted link. The pad behind it is `IO3`, and
//     `WB_IO3 = 21` in RAK's own `WisCore_RAK4631_Board/variant.h`. So
//     TIMEPULSE reaches no MCU pin on an assembled board, and the pin it
//     would have reached is P0.21, not P0.17. The receiver's other two
//     control nets are depopulated the same way — `STANDBY_GPS` through
//     `R44` (`0/NC`, IO4) and `RESET_GPS` through `R45` (`0/NC`, IO6) —
//     while both UART links are fitted (`R43`, `0`). On this board the
//     ZOE-M8Q is a UART and nothing else.
//   * Sheet 4, U7 `LIS3DH`, pin 11 `INT1` → `R53`, marked `0`: fitted,
//     onto net `IO1`, and `WB_IO1 = 17`. P0.17 is the accelerometer's
//     interrupt output.
//
// The same header cross-checks itself on this board elsewhere: `WB_IO2 =
// 34` is P1.02, the 3V3-S enable above, and `WB_IO5 = 9` is P0.09, the
// user key drawn as K3 on sheet 4. So no alias here, and nothing in
// `bin/rak4631.rs` configures P0.17: it belongs to whatever eventually
// drives the LIS3DH, which is the part the movement flag would use.
// `leviculum-std/tests/rak4631_gnss_pulse_pin.rs` keeps it that way.
// <https://docs.rakwireless.com/product-categories/meshtastic/wismesh-base/datasheet-vc/>

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

/// Raise the RAK19026 baseboard's 3V3-S rail ([`Periph3V3En`], P1.02):
/// **HIGH** powers the OLED, the ZOE-M8Q, the LIS3DH and the NCP5623, per
/// Meshtastic's RAK4631 init (`variants/.../variant.cpp:initVariant`, and
/// `src/platform/nrf52/main-nrf52.cpp:453`, which drives it LOW only at
/// shutdown). The display task sleeps 500 ms after creating its TWIM, so
/// the OLED's own power-on reset has finished by the first probe. The
/// binary holds the returned output for the life of the board.
pub fn periph_3v3(pin: Peri<'static, Periph3V3En>) -> Output<'static> {
    Output::new(pin, Level::High, OutputDrive::Standard)
}

/// Raise the SX1262 front-end power ([`LoRaPowerEn`], P1.05). It must be
/// HIGH before any SPI traffic to the radio, so the binary raises it before
/// `lora::init`. The chip sits on the module, not the baseboard, so this is
/// independent of [`periph_3v3`]. Held for the life of the board.
pub fn lora_power(pin: Peri<'static, LoRaPowerEn>) -> Output<'static> {
    Output::new(pin, Level::High, OutputDrive::Standard)
}

// Module names, for the spawn lines (D7 of order 1085: every board says
// `<task> task spawned (<module>)` with the module from its board file).
/// The status panel on TWISPI0, driven as an SSD1306.
pub const DISPLAY_MODULE: &str = "SSD1306";
/// The GNSS receiver on UARTE0.
pub const GNSS_MODULE: &str = "ZOE-M8Q";

/// Runtime board metadata for shared init code (USB / flash / LoRa).
pub const CONFIG: super::BoardConfig = super::BoardConfig {
    usb_vid: 0x1209,
    usb_pid: 0x0002,
    usb_manufacturer: "leviculum",
    usb_product: "leviculum RAK4631",
    identity_flash_page: 0xEC000,
    radio_config_flash_page: 0xEB000,
    telemetry_flash_page: 0xEA000,
    lora_tcxo_voltage_reg: 0x07, // 3.3 V (RNode MODE_TCXO_3_3V_6X for BOARD_RAK4631)
    // No QSPI part on this module. See the block above.
    qspi_part: None,
};

/// The name each interface reports, indexed by interface id: what
/// `/status` and `lnstatus` print per interface, what `[TRANSPORT]` lines
/// and the node's interface table say. The LoRa entry names this board's
/// radio, an SX1262, which is why the table is the board's and not the
/// orchestration crate's. The binary hands it to
/// `leviculum_firmware::iface_bytes::set_names` once at boot.
pub const IFACE_NAMES: [&str; leviculum_firmware::iface_bytes::IFACE_COUNT] =
    ["serial_usb", "lora_sx1262", "ble"];

/// Panic-LED descriptor — port, pin, active-low flag — for `set_panic_led`.
/// LED1 (green) on P1.03, **active high**.
pub const PANIC_LED_PORT: u8 = 1;
pub const PANIC_LED_PIN: u8 = 3;
pub const PANIC_LED_ACTIVE_LOW: bool = false;

/// HardFault-LED descriptor for `set_hardfault_led`: LED2 (blue) on P1.04,
/// **active high**, so a HardFault reads apart from the green panic LED.
pub const HARDFAULT_LED_PORT: u8 = 1;
pub const HARDFAULT_LED_PIN: u8 = 4;
pub const HARDFAULT_LED_ACTIVE_LOW: bool = false;

/// The button, LED and off pins of the 8 s off and the boot-time factory
/// reset (order 1511, `crate::power`). Off: the 3V3-S rail
/// ([`Periph3V3En`], P1.02) that feeds the OLED, the GNSS and the sensors
/// goes low, as Meshtastic drives it at shutdown
/// (`src/platform/nrf52/main-nrf52.cpp:453`); the SX1262 front-end power
/// ([`LoRaPowerEn`], P1.05) goes low; both LEDs (P1.03, P1.04, active high)
/// go low. The OLED has no backlight of its own: its rail is 3V3-S.
#[cfg(feature = "softdevice")]
pub const POWER: crate::power::PowerFacts = crate::power::PowerFacts {
    button_active_high: false,
    led: (PANIC_LED_PORT, PANIC_LED_PIN, PANIC_LED_ACTIVE_LOW),
    off_pins: &[
        crate::power::OffPin {
            port: 1,
            pin: 2,
            high: false,
        },
        crate::power::OffPin {
            port: 1,
            pin: 5,
            high: false,
        },
        crate::power::OffPin {
            port: 1,
            pin: 3,
            high: false,
        },
        crate::power::OffPin {
            port: 1,
            pin: 4,
            high: false,
        },
    ],
};
