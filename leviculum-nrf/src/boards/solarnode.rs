//! SenseCAP Solar Node P1-Pro pin mappings and hardware constants
//!
//! Not one integrated PCB but three Seeed modules on a carrier board:
//! a XIAO nRF52840 Plus (nRF52840, 1 MB flash, 256 KB RAM, P25Q16H QSPI
//! part on the module), a Wio-SX1262 radio and a XIAO L76K GNSS. Same
//! MCU family, same radio die and the same Adafruit UF2 bootloader as
//! the T114 and the RAK4631, so `sx1262.rs`, `lora.rs`, `interface.rs`,
//! the dual-CDC transport, the SoftDevice path and the flash identity
//! store are the shared ones. There is no display: this board carries
//! no ST7789 and no `leviculum-screen` path.
//!
//! Pin assignments resolved from Meshtastic's own board definition,
//! `variants/nrf52840/seeed_solar_node/variant.cpp` — the
//! `g_ADigitalPinMap` array — together with `variant.h` beside it. The
//! `D`-numbers in the header are indices into that array, so every
//! P0/P1 below is read out of the mapping table rather than inferred
//! from a convention. Where the header's own comments disagree with the
//! table (it calls `PIN_LED1` "P1.15", which is the radio's MOSI), the
//! table wins.
//!
//! Corroborated against the sibling variant for the same two modules,
//! `variants/nrf52840/seeed_xiao_nrf52840_kit`, which wires the
//! Wio-SX1262 identically and states the battery divider's resistors.
//!
//! Measured on the unit (2026-09-15, Codeberg #233): bootloader 0.9.2,
//! Board-ID `nRF52840-SeeedXiao-v1`, SoftDevice S140 7.3.0.
//!
//! References:
//!   * <https://files.seeedstudio.com/products/SenseCAP/Wio_SX1262/Wio-SX1262%20for%20XIAO%20V1.0_SCH.pdf>
//!   * Meshtastic `variants/nrf52840/seeed_solar_node/{variant.h,variant.cpp}`

use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::peripherals;
use embassy_nrf::Peri;

// SX1262 LoRa radio, on the Wio-SX1262 module (SPI0 pads of the XIAO)
/// SX1262 SPI clock (`D8`)
pub type LoRaSck = peripherals::P1_13;
/// SX1262 SPI MOSI (`D10`)
pub type LoRaMosi = peripherals::P1_15;
/// SX1262 SPI MISO (`D9`)
pub type LoRaMiso = peripherals::P1_14;
/// SX1262 chip-select, active low (`SX126X_CS D4`)
pub type LoRaCs = peripherals::P0_04;
/// SX1262 reset, active low (`SX126X_RESET D2`)
pub type LoRaReset = peripherals::P0_28;
/// SX1262 busy indicator, high = busy (`SX126X_BUSY D3`)
pub type LoRaBusy = peripherals::P0_29;
/// SX1262 DIO1 interrupt output (`SX126X_DIO1 D1`)
pub type LoRaDio1 = peripherals::P0_03;
/// SX1262 external RX enable (`SX126X_RXEN D5`), high while the chip is
/// listening and low before every key-up.
///
/// This is the pin the other two boards do not have, and the reason the
/// LoRa driver carries an optional RX-enable line at all: on the
/// Wio-SX1262 the antenna switch is steered from **both** ends. DIO2
/// owns the transmit side (`SX126X_DIO2_AS_RF_SWITCH`, set on every
/// board we build) while the receive side is a host GPIO, and
/// `SX126X_TXEN` is `RADIOLIB_NC` — there is no second host line. The
/// switching itself lives in `sx1262.rs`, next to the commands that
/// change the chip's state; see [`crate::lora`] and the front-end note
/// in [`super`].
pub type LoRaRxEnable = peripherals::P0_05;

/// SX1262 SPI frequency in Hz (4 MHz, as on the T114 and the RAK4631).
pub const LORA_SPI_FREQ_HZ: u32 = 4_000_000;
/// SX1262 TCXO voltage supplied via DIO3 (volts).
/// `SX126X_DIO3_TCXO_VOLTAGE 1.8` in the variant, the same value the
/// T114 uses.
pub const LORA_TCXO_VOLTAGE: f32 = 1.8;
/// SX1262 max TX power (dBm). Same die, same +22 dBm.
pub const LORA_MAX_POWER_DBM: i8 = 22;
/// DIO2 drives the RF switch — but unlike the other two boards it does
/// not drive all of it. See [`LoRaRxEnable`].
pub const LORA_DIO2_AS_RF_SWITCH: bool = true;

// LEDs (on the carrier board)
/// Green LED, **active HIGH** (`PIN_LED1` = index 12 = P0.19).
///
/// Active high because the solar node's own variant says so
/// (`LED_STATE_ON 1`) and its `initVariant` writes LOW to leave both
/// LEDs off at boot. Note that the sibling XIAO kit variant is the
/// other way round (`LED_STATE_ON (0)`, a common-anode RGB part), so
/// this is a per-carrier-board fact and not a XIAO one.
pub type LedPin = peripherals::P0_19;
/// Blue LED, active HIGH (`PIN_LED2` = index 11 = P0.15).
pub type LedNotificationPin = peripherals::P0_15;

// Buttons
/// User button (`D13`), active low. This is the XIAO's program button.
pub type UserButton = peripherals::P1_01;
/// Second button / touch pad (index 20 = P1.07).
pub type TouchButton = peripherals::P1_07;

// GNSS (XIAO L76K)
/// GNSS UART TX — **MCU → L76K** (`D6`, index 6 = P1.11).
///
/// Named from the MCU's side, as every other board file in this tree
/// is. Meshtastic's `GPS_TX_PIN`/`GPS_RX_PIN` naming is inconsistent
/// across variants (the T114's own comments say the opposite of what
/// its code does, see `boards/t114.rs`), so the direction is not taken
/// from that name. Three independent statements in the two variants
/// for these same two modules settle it, and all three say D6 is the
/// **MCU's** transmitter:
///
/// 1. `seeed_solar_node/variant.cpp`, the mapping table itself:
///    `43, // D6  P1.11 (UART_TX) GNSS_TX`. `UART_TX` is the XIAO pad
///    name, which is a property of the module and not of a variant
///    author's habit.
/// 2. `seeed_solar_node/variant.h:120` with `:115`:
///    `#define PIN_SERIAL1_TX GPS_TX_PIN` and `#define GPS_TX_PIN D6`.
///    In the Adafruit nRF52 core `PIN_SERIAL1_TX` is the pin the MCU
///    drives. (The trailing comment on that same line 115 reads `// 44`,
///    which is P1.12 and therefore D7 — a fourth reminder that in this
///    header the comments are not the map. The table is.)
/// 3. `seeed_xiao_nrf52840_kit/variant.h:181-182`, the sibling carrier
///    for the same XIAO and the same L76K, spells it out:
///    `#define GPS_TX_PIN D6 // This is data from the MCU`,
///    `#define GPS_RX_PIN D7 // This is data from the GNSS module`.
///
/// It stays worth knowing on the bench that a swapped pair produces
/// silence rather than an error: a receiver that settles on
/// `no-hardware` while [`GnssEnable`] is high is a reason to try the
/// pair the other way round before concluding the module is dead.
pub type GnssTx = peripherals::P1_11;
/// GNSS UART RX — L76K → MCU (`D7`, index 7 = P1.12). See [`GnssTx`]
/// for the citations that fix the direction of the pair.
pub type GnssRx = peripherals::P1_12;
/// GNSS standby / wakeup control (`D0`, index 0 = P0.02,
/// `PIN_GPS_STANDBY`).
///
/// **HIGH is awake.** The variant defines no `GPS_STANDBY_ACTIVE`, so
/// Meshtastic's default applies (`src/gps/GPS.h:22-23`,
/// `#define GPS_STANDBY_ACTIVE LOW`), and `GPS::writePinStandby` writes
/// that level for standby and its inverse for awake
/// (`src/gps/GPS.cpp:904-916`). The driver therefore holds this pin
/// high for the life of the task, exactly as on the T114.
pub type GnssStandby = peripherals::P0_02;
/// GNSS reset (`D17`, index 17 = P1.03).
///
/// Declared for completeness and deliberately **not driven**: the
/// solar node's `initVariant` never configures it either, so the pin
/// stays in its reset state (input, disconnected) and the module comes
/// up on its own internal reset. Asserting it at boot would be a forced
/// restart on every power-up, which is the ephemeris-wiping cold start
/// `gnss-init` argues against sending as a command.
pub type GnssReset = peripherals::P1_03;
/// GNSS power enable (`D18`, index 18 = P1.05, `GPS_EN`).
///
/// **HIGH powers the receiver**, and this switch belongs to the
/// receiver alone — nothing else on the carrier goes dark with it,
/// which is why the driver owns it rather than a board-level rail
/// helper (contrast the T114's shared VEXT, [`crate::vext`]).
/// `initVariant` in `seeed_solar_node/variant.cpp` writes it LOW while
/// it configures the QSPI CS, the battery divider and the two LEDs, and
/// then ends by writing it HIGH — so upstream's own order is *enable
/// last*, after the rest of the board is set up.
///
/// **Order relative to the UART:** our driver raises this pin as its
/// first act and builds the `Uarte` a few statements later, which is
/// the opposite order and is the safe one. The L76K's TX line is
/// unpowered until this pin is high, so a UART opened first would see a
/// floating input; opening it after means the first bytes the sweep
/// reads are bytes the module actually sent. Nothing needs to wait for
/// the rail beyond that: the presence machine sweeps for as long as it
/// takes and re-sweeps on sentence starvation, so a receiver still
/// booting is a few silent seconds, not a missed window.
///
/// The board has no `VGNSS_Ctrl` net. That name belongs to the Heltec
/// V4's ESP32 carrier (`leviculum-esp/src/boards/heltec_v4.rs`), where
/// it gates a GNSS *header* supply through Q7; the analogue here is
/// this pin and nothing else.
pub type GnssEnable = peripherals::P1_05;
/// GNSS UART baud rate. The L76K ships at 9600
/// (`seeed_solar_node/variant.h`, `#define GPS_BAUDRATE 9600`, the same
/// value the T114's variant carries), which is where the presence
/// machine's sweep starts.
///
/// Unlike the other boards in this tree there is **no PPS line broken
/// out**: the carrier exposes wakeup, reset and enable only, and P0.31
/// is the battery ADC. So this board is an NMEA-only time source
/// (#166), which is the weaker of the two kinds.
pub const GNSS_BAUD: u32 = 9600;

// Grove / I²C, on the NFC pins
/// Grove SDA (`D14` = P0.09, NFC1).
///
/// The same trap the RAK4631 board layer documents: these two pads are
/// the nRF52840's NFC antenna pins, and they are GPIO only while
/// `UICR.NFCPINS` says so. It is deliberate here, and the build side is
/// covered — `embassy-nrf` is pulled with `nfc-pins-as-gpio`, which
/// writes the UICR at startup. Confirm the UICR state on a fresh unit
/// rather than assuming the bootloader patched it.
pub type GroveSda = peripherals::P0_09;
/// Grove SCL (`D15` = P0.10, NFC2). See [`GroveSda`].
pub type GroveScl = peripherals::P0_10;

// QSPI flash: a P25Q16H (2 MB) IS fitted, on the XIAO module itself —
// unlike the T114 and the RAK4631, whose variant headers name a part
// their boards do not carry (Codeberg #384). `CONFIG.qspi_part` is
// nevertheless `None`, which here means "not mounted yet" rather than
// "not there": nothing in this firmware uses it, the record log lives
// in internal flash, and a part that is probed but unused would only
// add a boot stage that can hang. The pins, for whoever mounts it
// (`g_ADigitalPinMap` indices 21-26): SCK P0.21, CS P0.25, IO0 P0.20,
// IO1 P0.24, IO2 P0.22, IO3 P0.23. They are deliberately not aliases
// here — nothing may configure them as a bus while the part is unused.

// Battery
/// Battery voltage sense (`PIN_VBAT` = `D16` = P0.31, AIN7). The
/// divider is the XIAO module's own, not the carrier board's.
pub type BatteryAdc = peripherals::P0_31;
/// Divider enable (`BAT_READ` = `D19` = P0.14), **active LOW**.
///
/// The polarity is the one thing about this pin that cannot be guessed
/// from the solar node's own variant, which drives it LOW once in
/// `initVariant` and never touches it again — leaving it *enabled* for
/// the life of the board, since Meshtastic defines no `ADC_CTRL` here
/// and its `battery_adcEnable()` is therefore empty. The sibling XIAO
/// kit variant states it outright for the same P0.14 on the same
/// module: `#define ADC_CTRL VBAT_ENABLE` with
/// `#define ADC_CTRL_ENABLED LOW // ... sink`.
///
/// We switch it per sample rather than leaving it on, as on the T114:
/// a 1.5 MΩ divider across the pack is a leak a solar node pays for all
/// winter.
pub type AdcCtrl = peripherals::P0_14;
/// The level on [`AdcCtrl`] that switches the divider ON.
pub const ADC_CTRL_ACTIVE: Level = Level::Low;
/// The battery divider, as the two resistors it is: R17 = 1 MΩ from the
/// terminal to [`BatteryAdc`], R18 = 510 kΩ from the pin down to
/// [`AdcCtrl`].
///
/// Both values are stated in `seeed_xiao_nrf52840_kit/variant.h:202`
/// beside its `ADC_MULTIPLIER`, so the factor is
/// (1000 + 510) / 510 = 2.9608. The solar node's own variant rounds it
/// to `ADC_MULTIPLIER 3.3`, and that 3.3 is not this number: it is
/// paired there with `AREF_VOLTAGE 3.3` while the variant defines no
/// `VBAT_AR_INTERNAL`, so Meshtastic reads the pin at the 3.6 V full
/// scale its `analogReference(AR_INTERNAL)` default sets and converts as
/// if it were 3.3 V. Their product, 3.3 × 3.3/3.6 = 3.03, lands within
/// 2 % of the resistors — which is why nobody noticed. We take the
/// resistors. Our own full scale is not a board property at all; it is
/// the chip's internal 0.6 V reference divided by the gain
/// `leviculum_battery_scale::CONFIGURED_GAIN` sets, and that crate's
/// module doc argues why it must not be multiplied in twice. Through
/// this divider at that gain the board's whole measurable range is
/// 10 660 mV, 2.6 mV per count.
///
/// **The two resistors and not their ratio**, unlike the T114's and the
/// RAK's. The ratio decides the conversion, but their magnitude decides
/// how long the SAADC must hold the input before converting: 1 MΩ ∥
/// 510 kΩ is 338 kΩ of source, where the nRF52840 specifies the 10 µs
/// window every board was sampled with for 100 kΩ (PS v1.8 §6.23). The
/// T114's 490 kΩ chain presents 80 kΩ and is inside it; this one is not,
/// so `leviculum_battery_scale::BatteryScale::for_divider` derives 20 µs
/// from these numbers rather than a constant being written down here.
/// A multiplier could not have carried that.
///
/// # What is on the other side of it: one cell, four of them in parallel
///
/// The divider hangs on the XIAO module's `VBAT` net, and the part that
/// charges that net is a single-cell one. On the module's own schematic
/// (Seeed EAGLE sheet 1/1, title `Seeed Studio XIAO nRF52840 v1.1`,
/// grid A5-A6, published as
/// `files.seeedstudio.com/wiki/XIAO-BLE/Seeed-Studio-XIAO-nRF52840-Sense-v1.1.pdf`)
/// `U2 BQ25100` takes `IN` from `VBUS` and puts `OUT` on `VBAT`, which
/// is also the `BAT`/`GND` battery pads; from that node `1M 1%` runs to
/// `P0.31_AIN7_BAT` and `510k 1%` on to `P0.14_READ_BAT`, which is these
/// two resistors and the polarity [`ADC_CTRL_ACTIVE`] states, in the
/// schematic's own words beside them: *"Set P0.14 to output Sink only to
/// enable BAT voltage read;"*. The bq2510x is a 250 mA linear charger
/// for **one** cell with its charge voltage fixed at 4.2 V. (That sheet
/// is the plain XIAO nRF52840 rather than the Plus this carrier fits;
/// the Plus is corroborated part-for-part by the sibling variant that
/// also names the resistors, `seeed_xiao_nrf52840_kit/variant.h:201-206`
/// — `R17=1M, R18=510k`, `ADC_CTRL VBAT_ENABLE`, `ADC_CTRL_ENABLED LOW`,
/// `BQ25101 ~CHG`.)
///
/// The carrier charges the same net through a second single-cell part:
/// Seeed's hardware overview for this product names the *"Charging
/// Management Chip"* as `CN3165 (0.99A)`, a solar-input linear charger
/// whose regulation voltage is internally fixed at 4.2 V, and gives the
/// only two supplies as *"Type-C: 5V 1A"* and *"Solar power supply: 5V
/// 1A"*. Nothing on the board steps up, so no series pack can be
/// charged here at all.
///
/// Therefore the four cells the P1-Pro ships with — Seeed: *"4 x 18650
/// lithium (NMC) batteries (3350mAh each)"* — are **1S4P**: 13.4 Ah at
/// one cell's voltage, which is the ~49 Wh of #233. The same 49 Wh is
/// also what 2S2P would give, so the energy figure does not decide this
/// and the charger does. Measured on the rig unit, eleven captures
/// between 2026-09-18 and 2026-09-23: `[BAT] init pack_mv=` 4123 to
/// 4154, `cells=1S`, holding just under the CN3165's 4.2 V on USB.
///
/// This stays prose and does NOT become a constant. `battery.rs`
/// classifies the pack from its own first reading and is given no cell
/// count to trust, which is the property that catches a divider that
/// never settled; a number here would be a second source of truth for
/// the same question. What it is for is the reader deciding what a
/// plausible reading on this board looks like: the band a 1S
/// classification implies is `leviculum_battery_scale::pack_band_mv(1)`,
/// 2500 to 4330 mV, and anything asserting a 2S window against this
/// board is asserting a pack the product does not have.
pub const BATTERY_DIVIDER: leviculum_battery_scale::Divider =
    leviculum_battery_scale::Divider::new(1_000_000, 510_000);

/// Create the green LED output. **Active HIGH**; off at boot.
pub fn led(pin: Peri<'static, LedPin>) -> Output<'static> {
    Output::new(pin, Level::Low, OutputDrive::Standard)
}

/// Create the blue LED output. **Active HIGH**, same shape as
/// [`led`]. Off at boot.
pub fn led_notification(pin: Peri<'static, LedNotificationPin>) -> Output<'static> {
    Output::new(pin, Level::Low, OutputDrive::Standard)
}

/// Runtime board metadata for shared init code (USB / flash / LoRa).
///
/// The three persistence pages are the same three every board uses:
/// they are properties of the Adafruit bootloader's `USER_FLASH_END`
/// (`memory.x`), which this board shares, not of the carrier.
pub const CONFIG: super::BoardConfig = super::BoardConfig {
    usb_vid: 0x1209,
    usb_pid: 0x0003,
    usb_manufacturer: "leviculum",
    usb_product: "leviculum SolarNode",
    log_prefix: "SOLAR",
    identity_flash_page: 0xEC000,
    radio_config_flash_page: 0xEB000,
    telemetry_flash_page: 0xEA000,
    lora_tcxo_voltage_reg: 0x02, // 1.8 V
    lora_spi_freq_hz: LORA_SPI_FREQ_HZ,
    lora_max_power_dbm: LORA_MAX_POWER_DBM,
    // Fitted but unmounted. See the QSPI block above.
    qspi_part: None,
};

/// Panic-LED descriptor — port, pin, active-low flag — for `set_panic_led`.
/// Green LED on P0.19, **active high**.
pub const PANIC_LED_PORT: u8 = 0;
pub const PANIC_LED_PIN: u8 = 19;
pub const PANIC_LED_ACTIVE_LOW: bool = false;
