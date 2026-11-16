//! Heltec Mesh Node T114 pin mappings and hardware constants
//!
//! nRF52840 + SX1262 LoRa radio + L76K GPS + optional ST7789 TFT display.
//! Pin assignments verified from Heltec Rev 2.0 Pin Map, datasheet,
//! Meshtastic firmware `variants/nrf52840/heltec_mesh_node_t114/variant.h`,
//! and RNode firmware `Boards.h`.
//!
//! Reference: <https://resource.heltec.cn/download/Mesh_Node_T114/Mesh_node_t114_Pin_Map.png>

use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::peripherals;
use embassy_nrf::Peri;

// SX1262 LoRa Radio (SPI0)
/// SX1262 SPI clock
pub type LoRaSck = peripherals::P0_19;
/// SX1262 SPI MOSI
pub type LoRaMosi = peripherals::P0_22;
/// SX1262 SPI MISO
pub type LoRaMiso = peripherals::P0_23;
/// SX1262 SPI chip-select (active low)
pub type LoRaCs = peripherals::P0_24;
/// SX1262 reset (active low)
pub type LoRaReset = peripherals::P0_25;
/// SX1262 busy indicator (high = busy)
pub type LoRaBusy = peripherals::P0_17;
/// SX1262 DIO1 interrupt output
pub type LoRaDio1 = peripherals::P0_20;

// The SX1262's other facts live where they are read: the SPI clock in
// `lora::init`, the TCXO select byte in `CONFIG.lora_tcxo_voltage_reg`
// (1.8 V), the power ceiling and the OCP limit in the transmit-power
// sequence (`leviculum_core::sx126x`, +22 dBm), the BUSY timeout in
// `sx1262.rs`. DIO2 drives the RF switch, there are no RXEN/TXEN pins.
// A copy of any of them here would be a second number to keep in step.

// Green LED
/// Green indicator LED (active LOW)
pub type LedPin = peripherals::P1_03;

// NeoPixel (2x SK6812)
/// NeoPixel data pin (GRB, 800 kHz WS2812 protocol). Nothing drives the
/// two LEDs yet.
pub type NeoPixelPin = peripherals::P0_14;

// UART (NFC pins repurposed as GPIO)
/// UART RX (header P1, NFC pin repurposed)
pub type UartRx = peripherals::P0_09;
/// UART TX (header P1, NFC pin repurposed)
pub type UartTx = peripherals::P0_10;

// GPS (Quectel L76K, powered via VEXT)
/// GPS UART TX (MCU → GPS; Meshtastic `GPS_RX_PIN (32+5)`, named from
/// the module's side there)
pub type GpsTx = peripherals::P1_05;
/// GPS UART RX (GPS → MCU; Meshtastic `GPS_TX_PIN (32+7)`)
pub type GpsRx = peripherals::P1_07;
/// GPS standby control (LOW = allow sleep, HIGH = force wake;
/// `variant.h:170` with `GPS_STANDBY_ACTIVE LOW`, `GPS.h:22-23`)
pub type GpsStandby = peripherals::P1_02;
/// GPS PPS (pulse-per-second) input
pub type GpsPps = peripherals::P1_04;

// I2C0 (RTC footprint, optional PCF8563TS)
/// I2C0 SDA (RTC footprint)
pub type I2c0Sda = peripherals::P0_26;
/// I2C0 SCL (RTC footprint)
pub type I2c0Scl = peripherals::P0_27;

// I2C1 (general purpose, header P1)
/// I2C1 SDA (general purpose, header P1)
pub type I2c1Sda = peripherals::P0_16;
/// I2C1 SCL (general purpose, header P1)
pub type I2c1Scl = peripherals::P0_13;

// TFT Display (ST7789 240x135, optional, SPI1)
/// TFT SPI clock
pub type TftSck = peripherals::P1_08;
/// TFT SPI MOSI
pub type TftMosi = peripherals::P1_09;
/// TFT chip-select (active low)
pub type TftCs = peripherals::P0_11;
/// TFT data/command select
pub type TftDc = peripherals::P0_12;
/// TFT reset (active low)
pub type TftReset = peripherals::P0_02;
/// TFT backlight control (active LOW = on)
pub type TftBacklight = peripherals::P0_15;
/// TFT power enable
pub type TftPowerEn = peripherals::P0_03;

/// The display's wiring for [`crate::st7789`], built from the aliases
/// above, so the binary can only hand over this board's pins.
#[cfg(feature = "bsp-t114")]
#[allow(clippy::too_many_arguments)]
pub fn tft_wiring(
    spi: Peri<'static, peripherals::SPI3>,
    sck: Peri<'static, TftSck>,
    mosi: Peri<'static, TftMosi>,
    cs: Peri<'static, TftCs>,
    dc: Peri<'static, TftDc>,
    rst: Peri<'static, TftReset>,
    vtft: Peri<'static, TftPowerEn>,
    leda: Peri<'static, TftBacklight>,
) -> crate::st7789::TftWiring {
    crate::st7789::TftWiring {
        spi,
        sck: sck.into(),
        mosi: mosi.into(),
        cs: cs.into(),
        dc: dc.into(),
        rst: rst.into(),
        vtft: vtft.into(),
        leda: leda.into(),
    }
}

// QSPI Flash: NONE. There are deliberately no pin aliases here, and
// `CONFIG.qspi_part` is `None`, so `bin/t114.rs` never configures P1.14,
// P1.15, P1.12, P1.13, P0.07 or P0.05 as a flash bus. The six reasons,
// because this error travelled through three projects by copying and
// nobody checked it (Codeberg #384):
//
// 1. Heltec's own board support package has the QSPI pins commented out
//    for HT-n5262, the board id our bootloader reports. The two
//    `EXTERNAL_FLASH_*` defines survive without pins and so do nothing:
//    the manufacturer disabled the bus in their own software.
// 2. The sibling variant HT-n5262G has no QSPI at all and gives (32+14)
//    = P1.14 to `PIN_GPS_RESET` and (32+12) = P1.12 to the display
//    backlight. In this device family the assignment is not stable.
// 3. All six nets are on the expansion header P2 (Heltec datasheet
//    Rev. 1.0 §2.2: 0.05, 0.07, 1.12, 1.13, 1.14, 1.15). Whatever a user
//    plugs in sits on the bus we would be driving.
// 4. The two published pin maps disagree on IO2/IO3 — Heltec's own
//    (commented-out) says P1.00/P1.01, Meshtastic and Heltec's schematic
//    say P0.07/P0.05. Both have been tried on hardware; both are silent.
// 5. Our own measurement (`de6e74ed`): every pin follows our drive, and
//    nothing drives SO during `05h`, `9Fh` or `90h` — before or after
//    the datasheet's own `66h`/`99h` reset. Three units say the same
//    four lines: the rig T114, a second T114 (`183004F712B4A7FE`) and
//    the field Pocket (`ABFAB3F1807E459B`). The `MISO pullup=1
//    pulldown=0` row on those captures is NOT part of this argument: a
//    healthy part holds SO high-impedance while CS# is high, so a
//    working board reads the same, and that row supports no conclusion
//    on its own.
// 6. Heltec's template line is not Heltec's alone. RAK's board support
//    package for the RAK4631 carries `#define EXTERNAL_FLASH_DEVICES
//    IS25LP080D` under the comment "No onboard flash", with the QSPI
//    pins marked "occupied by GPIO's" — the same artefact, on the other
//    vendor, for a module that also answers nothing. An
//    `EXTERNAL_FLASH_*` define is a template default, not a statement
//    that a part is fitted. See `boards/rak4631.rs`.
//
// So: this board has no answering part, and those pins belong to
// something else. Do not "add the missing flash" from a variant header.
// <https://github.com/HelTecAutomation/Heltec_nRF52/blob/master/variants/HT-n5262/variant.h>
// <https://github.com/HelTecAutomation/Heltec_nRF52/blob/master/variants/HT-n5262G/variant.h>

// Battery / ADC
/// Battery voltage sense (AIN2)
pub type BatteryAdc = peripherals::P0_04;
/// ADC divider enable
pub type AdcCtrl = peripherals::P0_06;
/// The level on [`AdcCtrl`] that switches the divider ON. Active HIGH
/// here; the solar node's is the other way round, which is why the
/// polarity travels with the pin (`battery::DividerEnable`).
pub const ADC_CTRL_ACTIVE: Level = Level::High;
/// ADC multiplier: converts raw ADC reading to battery voltage (mV)
pub const ADC_MULTIPLIER: f32 = 4.916;

// External peripheral power
/// VEXT enable (HIGH = on, controls GPS + display power rail, 1s warmup)
pub type VextEnable = peripherals::P0_21;
/// VEXT warmup time in milliseconds
pub const VEXT_WARMUP_MS: u32 = 1000;

// User button
/// User button
pub type UserButton = peripherals::P1_10;

/// Create the green LED output (active low)
pub fn led(pin: Peri<'static, LedPin>) -> Output<'static> {
    // LED is active LOW, start with Level::High (LED off)
    Output::new(pin, Level::High, OutputDrive::Standard)
}

/// Runtime board metadata for shared init code (USB / flash / LoRa).
pub const CONFIG: super::BoardConfig = super::BoardConfig {
    usb_vid: 0x1209,
    usb_pid: 0x0001,
    usb_manufacturer: "leviculum",
    usb_product: "leviculum T114",
    identity_flash_page: 0xEC000,
    radio_config_flash_page: 0xEB000,
    telemetry_flash_page: 0xEA000,
    lora_tcxo_voltage_reg: 0x02, // 1.8 V
    // No QSPI part on this board. See the block above.
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
/// LED1 (green) on P1.03, active low.
pub const PANIC_LED_PORT: u8 = 1;
pub const PANIC_LED_PIN: u8 = 3;
pub const PANIC_LED_ACTIVE_LOW: bool = true;

/// The button, LED and off pins of the 8 s off and the boot-time factory
/// reset (order 1511, `crate::power`). Off: the panel's own rail (VTFT,
/// P0.03) and backlight (LEDA, P0.15), both active low, go high; the VEXT
/// rail (P0.21, active high) that feeds the panel chain and the L76K goes
/// low; the LED (P1.03, active low) goes high. The SX1262 has no switch
/// on this board and stays powered.
#[cfg(feature = "softdevice")]
pub const POWER: crate::power::PowerFacts = crate::power::PowerFacts {
    button_active_high: false,
    led: (PANIC_LED_PORT, PANIC_LED_PIN, PANIC_LED_ACTIVE_LOW),
    off_pins: &[
        crate::power::OffPin {
            port: 0,
            pin: 3,
            high: true,
        },
        crate::power::OffPin {
            port: 0,
            pin: 15,
            high: true,
        },
        crate::power::OffPin {
            port: 0,
            pin: 21,
            high: false,
        },
        crate::power::OffPin {
            port: 1,
            pin: 3,
            high: true,
        },
    ],
};

// Module names, for the spawn lines (D7 of order 1085: every board says
// `<task> task spawned (<module>)` with the module from its board file).
/// The status panel.
pub const DISPLAY_MODULE: &str = "ST7789";
/// The GNSS receiver on UARTE0.
pub const GNSS_MODULE: &str = "L76K";
/// The baud the L76K ships at (Quectel factory default), where the
/// presence machine's sweep starts; the boot line `[GNSS_PRESENCE]
/// state=detecting` names it (D2 of order 1085).
pub const GNSS_BAUD: u32 = 9600;
