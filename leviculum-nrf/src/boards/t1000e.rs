//! SenseCAP T1000-E pin mappings and transceiver facts (Codeberg #406)
//!
//! A card-sized tracker: an nRF52840 and a Semtech LR1110 (LoRa plus GNSS
//! and WiFi scanning, of which this firmware uses the LoRa modem only),
//! an Airoha AG3335 GNSS receiver (driven since #456), a QMA6100P
//! accelerometer (read as moving/still since #457), a buzzer, one LED and
//! one button. No display. It is the first board on this firmware
//! whose radio is not an SX1262, so it is also the first one driven
//! through `leviculum_lr11xx` (`crate::radio` picks the driver for the
//! `bsp-t1000e` build). Same Adafruit nRF52 bootloader family as the other
//! boards: measured on the rig unit (2026-10-18), Board-ID
//! `nRF52840-T1000-E-v1`, SoftDevice S140 7.3.0, bootloader USB 2886:0057.
//!
//! # Every wiring fact here is a foreign-firmware hint
//!
//! Seeed does not publish the T1000-E schematic (the wiki page
//! <https://wiki.seeedstudio.com/t1000_e_intro/> links no design files;
//! checked for the review of Codeberg #451). So nothing below has the
//! standing of a schematic citation, which is the reference this tree
//! holds board files to: the pins come from Meshtastic's board definition,
//! `variants/nrf52840/tracker-t1000-e/variant.h`, and the RF switch table
//! from `rfswitch.h` beside it, with the Seeed wiki corroborating the pin
//! list. Each alias names the `define` it was read from, and
//! `leviculum-nrf/reference-pins.toml` records the LoRa set so
//! `scripts/check-nrf-board-pins.sh` re-reads that header whenever a
//! Meshtastic tree is present.
//!
//! # What first boot refutes
//!
//! The saving grace of an LR1110 is that a wrong transceiver guess fails
//! loudly in `init_radio`: after `Calibrate` the driver reads `GetErrors`
//! and refuses a nonzero word as `Calibration(errors=0x....)`, which the
//! LoRa task prints as `[LORA] init FAILED: ...`. The two guesses in
//! [`LR1110`] that can be wrong and the bit that refutes each:
//!
//! * the TCXO supply (`V1_6`) or its start-up time (300 steps, ~9 ms):
//!   `HF_XOSC_START`, bit 5, `errors=0x0020`. A wrong voltage usually,
//!   a start-up time too short sometimes; raise the steps before doubting
//!   the voltage, since the voltage is the one fact the variant states
//!   (`LR11X0_DIO3_TCXO_VOLTAGE 1.6`).
//! * a 32.768 kHz crystal on DIO10/DIO11 (`LfClk::Xtal`, waited for):
//!   `LF_XOSC_START`, bit 6, `errors=0x0040`. Then the crystal is absent
//!   and the answer is `LfClk::Rc` with `wait_32k_ready: false`.
//!
//! This is the procedure the driver's README documents for the ThinkNode
//! M9 ("Worked example"), where both `Rc` and `Xtal` came up clean. The
//! regulator guess (`DcDc`) is not refuted by `GetErrors`: a board without
//! the inductor runs, badly. It is what Meshtastic sets on every LR1110
//! variant (`src/mesh/LR11x0Interface.cpp:122-124`, "currently all LR1110
//! variant files use the DC-DC regulator option"), which is the best
//! evidence available until a schematic or a current measurement.
//!
//! Note what the variant's `USE_LFXO` is NOT: it selects the nRF52840's own
//! 32 kHz crystal for the MCU's LF clock, a different crystal on a
//! different chip, and says nothing about the LR1110's DIO10/DIO11. The
//! `Xtal` guess rests on the LR1110's GNSS engine, which needs a 32 kHz
//! reference and which this product ships with; it is a guess, and the bit
//! above is how it gets checked.
//!
//! # Transmit power is unverified
//!
//! No Seeed PA characterisation exists that we could find, so the PA table
//! is the Semtech LR1110MB1DXS shield's (same part, same two-PA
//! architecture). The power this board radiates for a configured number is
//! therefore **unverified until a rig measurement**. What keeps that honest
//! in the meantime is the clamp-and-announce contract (#349): the
//! `[SX_TX_POWER]` line states the request, the `SetTxParams` byte and the
//! four `SetPaCfg` bytes of the row that went out, so a capture says which
//! row of which table a number came from.

use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::peripherals;
use embassy_nrf::Peri;
use leviculum_lr11xx::board::{
    PaTable, RfSwitch, Tcxo, LR1110MB1DXS_PA_TABLE, RFSW0, RFSW1, RFSW2, RFSW3,
    SEMTECH_RSSI_CALIBRATION,
};
use leviculum_lr11xx::{Board, Chip, LfClk, RegMode, TcxoVoltage};

// LR1110 LoRa transceiver (variant.h, the `LORA_*`/`PIN_SPI_*` block;
// "SPI shared with nothing else": `SPI_INTERFACES_COUNT 1`)
/// LR1110 SPI clock (`PIN_SPI_SCK (0 + 11)`, `LORA_SCK`)
pub type LoRaSck = peripherals::P0_11;
/// LR1110 SPI MOSI (`PIN_SPI_MOSI (32 + 9)`, `LORA_MOSI`)
pub type LoRaMosi = peripherals::P1_09;
/// LR1110 SPI MISO (`PIN_SPI_MISO (32 + 8)`, `LORA_MISO`)
pub type LoRaMiso = peripherals::P1_08;
/// LR1110 chip-select, active low (`PIN_SPI_NSS (0 + 12)`, `LORA_CS`)
pub type LoRaCs = peripherals::P0_12;
/// LR1110 NRESET, active low (`LORA_RESET (32 + 10)`)
pub type LoRaReset = peripherals::P1_10;
/// LR1110 BUSY, high = busy (`LORA_DIO2 (0 + 7)`, `LR1110_BUSY_PIN`).
/// Meshtastic names the BUSY line "DIO2" in RadioLib's SX126x idiom; on the
/// LR1110 it is the BUSY pin.
pub type LoRaBusy = peripherals::P0_07;
/// LR1110 IRQ, the part's DIO9 (`LORA_DIO1 (32 + 1)`, `LR1110_IRQ_PIN`).
/// "DIO1" again in RadioLib's idiom; the LR1110 raises interrupts on DIO9.
pub type LoRaIrq = peripherals::P1_01;

// The SPI clock is `lora::init`'s, 4 MHz as on every board here (the
// LR1110 takes up to 16 MHz). The power ceiling is the top of the PA
// table below, +22 dBm on the HP PA.

/// The LR1110's transceiver facts, as `leviculum_lr11xx` takes them.
/// Foreign-firmware hints, every field: see the module doc for the source
/// of each and for the `GetErrors` bit that refutes the two that can be
/// wrong at first boot.
pub const LR1110: Board = Board {
    name: "t1000e",
    chip: Chip::Lr1110,
    // `LR11X0_DIO3_TCXO_VOLTAGE 1.6` (variant.h). The start-up time is not
    // stated anywhere we could read: 300 steps (~9 ms) is the Semtech
    // shield's, generous against RadioLib's ~164-step default, and is to be
    // tightened once `GetErrors` stays clean.
    tcxo: Some(Tcxo {
        voltage: TcxoVoltage::V1_6,
        startup_steps: 300,
    }),
    // rfswitch.h, `rfswitch_table`, columns DIO5 DIO6 DIO7 DIO8 =
    // RFSW0..RFSW3: STBY all low; RX DIO5+DIO8; TX DIO5+DIO6+DIO8; TX_HP
    // DIO6+DIO8; TX_HF all low; GNSS DIO7; WIFI all low. DIO8 (RFSW3) is
    // high in every RX and TX mode, which is why the driver names RFSW3
    // and RFSW4 at all (`leviculum_lr11xx::board::RFSW3`).
    rf_switch: RfSwitch {
        enable: RFSW0 | RFSW1 | RFSW2 | RFSW3,
        standby: 0,
        rx: RFSW0 | RFSW3,
        tx: RFSW0 | RFSW1 | RFSW3,
        tx_hp: RFSW1 | RFSW3,
        tx_hf: 0,
        gnss: RFSW2,
        wifi: 0,
    },
    // Meshtastic, `src/mesh/LR11x0Interface.cpp:122-124`, for every LR1110.
    reg_mode: RegMode::DcDc,
    // The refutable guess: bit 6 of `GetErrors` says no (module doc).
    lf_clk: LfClk::Xtal,
    wait_32k_ready: true,
    // Firmware choice, the shield's default.
    rx_boosted: false,
    // The Semtech reference layout's tables; no Seeed figures exist.
    rssi_calibration: Some(SEMTECH_RSSI_CALIBRATION),
    // The LR1110MB1DXS shield's table, -17 to +22 dBm. Radiated power
    // unverified until a rig measurement (module doc).
    pa: PaTable::new(&LR1110MB1DXS_PA_TABLE, -17),
};

// Power rails
/// Sensor supply enable, **active HIGH** (`PIN_3V3_EN (32 + 6)`, "Power to
/// Sensors"). Meshtastic's `initVariant` raises it first thing on every
/// boot and never lowers it; the firmware does the same, because which
/// parts hang on this rail is exactly what the missing schematic would say,
/// and the one configuration known to run the radio is the vendor's.
pub type PowerSensors = peripherals::P1_06;

// GNSS (Airoha AG3335, Codeberg #456). Every line is the variant's
// (`variant.h:107-124`), the levels those of its `initVariant`
// (`variant.cpp`): the hint standing of the rest of this file.
/// UART, MCU receive: the AG3335's TX (`PIN_SERIAL1_RX (0 + 14)`,
/// `GPS_RX_PIN`). Unlike the T114's header, this variant names the GPS
/// lines from the MCU's side, which `gnss::GnssWiring` uses too.
pub type GnssRx = peripherals::P0_14;
/// UART, MCU transmit: the AG3335's RX (`PIN_SERIAL1_TX (0 + 13)`,
/// `GPS_TX_PIN`). 115200 baud (`GPS_BAUDRATE`), one of the presence
/// machine's sweep rates.
pub type GnssTx = peripherals::P0_13;
/// GNSS power enable, **active HIGH** (`PIN_GPS_EN (32 + 11)`,
/// `GPS_EN_ACTIVE HIGH`). The driver's `power_enable`: nothing else on
/// the board hangs on it.
pub type GnssEnable = peripherals::P1_11;
/// The AG3335's sleep request, **held HIGH** = awake (`GPS_SLEEP_INT
/// (32 + 12)`, "always high"). The driver's `standby` line, which it
/// holds high for its whole life, exactly the level this one wants.
pub type GnssSleepInt = peripherals::P1_12;
/// Supply of the AG3335's RTC domain, **held HIGH** (`GPS_VRTC_EN (0 +
/// 8)`, "always high"). Low only in Meshtastic's deep sleep
/// (`src/sleep.cpp:257`), which this firmware has none of.
pub type GnssVrtc = peripherals::P0_08;
/// AG3335 reset, **active HIGH** (`PIN_GPS_RESET (32 + 15)`,
/// `GPS_RESET_MODE HIGH`), held LOW = released. Never pulsed: a reset
/// would throw away the receiver's assistance data on every boot, and the
/// power enable above already gives it a clean start.
pub type GnssReset = peripherals::P1_15;
/// AG3335 RTC wake, **held LOW** (`GPS_RTC_INT (0 + 15)`, "normal is LOW,
/// wake by HIGH"). The GNSS driver's `wake` line: set low here at boot,
/// then handed to the driver, which pulses it high when it powers the
/// receiver up after a sleep (#457 step two), as the Quectel LC29H
/// hardware design (an AG3335 module) pulls WAKEUP to leave Backup mode.
pub type GnssRtcInt = peripherals::P0_15;
/// The AG3335's own reset output, an input with pull-up here
/// (`GPS_RESETB_OUT (32 + 14)`, "always input pull_up"; `initVariant`
/// makes it an input). Read by nothing; configured so the pad is not left
/// floating against a driving output.
pub type GnssResetbOut = peripherals::P1_14;

/// The AG3335 control lines at the levels [`gnss_control`] sets. Kept
/// alive by the caller: dropping an embassy pin returns it to its reset
/// state, which would release VRTC. `rtc_int` is set here, in
/// `initVariant`'s order, and then moved into the GNSS driver's wiring as
/// its `wake` line.
pub struct GnssControl {
    _vrtc: Output<'static>,
    _reset: Output<'static>,
    pub rtc_int: Output<'static>,
    _resetb_out: Input<'static>,
}

/// Set the AG3335's static control lines for the life of the board, in
/// the order of Meshtastic's `initVariant`: VRTC up, reset released, RTC
/// wake low, reset output as an input. The enable and the sleep line go
/// to the GNSS driver as `power_enable` and `standby`, the RTC wake as
/// its `wake`.
pub fn gnss_control(
    vrtc: Peri<'static, GnssVrtc>,
    reset: Peri<'static, GnssReset>,
    rtc_int: Peri<'static, GnssRtcInt>,
    resetb_out: Peri<'static, GnssResetbOut>,
) -> GnssControl {
    GnssControl {
        _vrtc: Output::new(vrtc, Level::High, OutputDrive::Standard),
        _reset: Output::new(reset, Level::Low, OutputDrive::Standard),
        rtc_int: Output::new(rtc_int, Level::Low, OutputDrive::Standard),
        _resetb_out: Input::new(resetb_out, Pull::Up),
    }
}

// Battery (Codeberg #458). The variant's hint standing again: `BATTERY_PIN
// 2 // P0.02/AIN0, BAT_ADC`, `BATTERY_IMMUTABLE`, `ADC_MULTIPLIER (2.0F)`
// (`variant.h:126-128`).
/// Battery voltage sense (`BATTERY_PIN 2`, P0.02 = AIN0, `BAT_ADC`).
///
/// No enable pin: the variant says `BATTERY_IMMUTABLE`, i.e. the divider
/// sits across the pack permanently and Meshtastic switches nothing to
/// read it. So `battery::init` gets `None` for the enable line.
pub type BatteryAdc = peripherals::P0_02;
/// The divider's ratio, terminal volts over pin volts: `ADC_MULTIPLIER
/// (2.0F)`, two equal resistors.
///
/// **Only the ratio**, and it is all there is: without the schematic
/// nobody can say how large the two resistors are, and their size is what
/// decides the SAADC acquisition window (`boards/solarnode.rs` has the
/// argument). So the binary builds the scale with
/// `BatteryScale::for_unsized_divider`, which takes the longest window the
/// part has; 40 µs covers a 2:1 divider of up to 1.6 MΩ a leg, and a
/// smaller one loses nothing by being held longer. The variant's
/// `AREF_VOLTAGE 3.0` is Meshtastic's ADC configuration and is not folded
/// in here (`leviculum_battery_scale` module doc). Through this ratio the
/// measurable range at the terminal is 7200 mV, 1.76 mV a count.
///
/// The variant also names a charger: `EXT_CHRG_DETECT` P1.03 (active
/// low), `EXT_PWR_DETECT` P0.05, `CHARGE_DONE` P1.04. Nothing here reads
/// them yet, so telemetry's `charging` stays nil on this board as on every
/// other; the USB transition is visible only as the pack voltage the
/// charger holds.
pub const ADC_MULTIPLIER: f32 = 2.0;

// Accelerometer (QMA6100P, Codeberg #457). The variant's hint standing again
// (`variant.h:47,63-67`); the registers are the part's datasheet, in
// `leviculum_motion`.
/// I²C SDA (`PIN_WIRE_SDA (0 + 26)`). The variant's only I²C bus, and the
/// part its only device (`HAS_QMA6100P`, "very rare beast, only on this
/// board"; at 0x12, Meshtastic `src/configuration.h:253`).
pub type AccSda = peripherals::P0_26;
/// I²C SCL (`PIN_WIRE_SCL (0 + 27)`).
pub type AccScl = peripherals::P0_27;
/// The accelerometer's rail, **active HIGH** (`PIN_3V3_ACC_EN (32 + 7)`,
/// "Power to Acc"; `initVariant` raises it). Owned by the motion task, which
/// drops it again if the part does not answer.
pub type AccPower = peripherals::P1_07;
/// The part's INT1 (`QMA_6100P_INT_PIN (32 + 2)`). **Not configured**: the
/// task polls the data registers instead, for the reasons in
/// `leviculum_motion`'s module doc (the reference maps the wrong bit of
/// `INT_MAP1`, so no firmware has shown this line working). Step two may
/// take it up once the rig has.
pub type AccInt = peripherals::P1_02;

// LED and button
/// The one LED, **active HIGH** (`PIN_LED1 (0 + 24)`, `LED_STATE_ON 1`).
pub type LedPin = peripherals::P0_24;
/// User button, **active HIGH** with a pull-down (`BUTTON_PIN (0 + 6)`,
/// `BUTTON_ACTIVE_LOW false`, `BUTTON_SENSE_TYPE 0x5`). A double press
/// announces (#459, `crate::button`); the long press is also the product's power
/// button.
pub type UserButton = peripherals::P0_06;

// Buzzer
/// The buzzer's PWM line (`PIN_BUZZER (0 + 25)`, "pwm output"). The
/// feedback grammar's buzzer player drives it ([`buzzer_pwm`], order 892).
pub type BuzzerPin = peripherals::P0_25;
/// The buzzer's enable (`BUZZER_EN_PIN (32 + 5)`). Meshtastic keeps it
/// "always high" and drives the tone on [`BuzzerPin`].
pub type BuzzerEnable = peripherals::P1_05;
/// The PWM instance the buzzer takes. No other part of this firmware uses
/// a PWM peripheral.
pub type BuzzerPwm = peripherals::PWM0;

/// Create the buzzer's enable, **low**: the buzzer is unpowered whatever
/// its PWM line does. Unlike Meshtastic, which holds it high for the life
/// of the board, the feedback player (`crate::feedback::buzzer_task`)
/// raises it only while a pattern plays. The returned pin must be kept
/// alive: dropping an embassy `Output` disconnects it.
pub fn buzzer_enable(pin: Peri<'static, BuzzerEnable>) -> Output<'static> {
    Output::new(pin, Level::Low, OutputDrive::Standard)
}

/// Create the buzzer's PWM on [`BuzzerPin`]: 1 MHz counter clock (16 MHz
/// / 16), so the tone's period fits the 15-bit counter from 31 Hz up, and
/// the line idles low while the generator is off. The player sets the
/// tone.
pub fn buzzer_pwm(
    pwm: Peri<'static, BuzzerPwm>,
    pin: Peri<'static, BuzzerPin>,
) -> embassy_nrf::pwm::SimplePwm<'static> {
    let mut config = embassy_nrf::pwm::SimpleConfig::default();
    config.prescaler = embassy_nrf::pwm::Prescaler::Div16;
    config.ch0_idle_level = Level::Low;
    embassy_nrf::pwm::SimplePwm::new_1ch(pwm, pin, &config)
}

/// Create the LED output. **Active HIGH**; off at boot.
pub fn led(pin: Peri<'static, LedPin>) -> Output<'static> {
    Output::new(pin, Level::Low, OutputDrive::Standard)
}

/// Raise the sensor rail ([`PowerSensors`]) and hold it for the life of
/// the board. The returned pin must be kept alive: dropping an embassy
/// `Output` disconnects it.
pub fn power_sensors(pin: Peri<'static, PowerSensors>) -> Output<'static> {
    Output::new(pin, Level::High, OutputDrive::Standard)
}

/// Runtime board metadata for shared init code (USB / flash / LoRa).
///
/// The three persistence pages are the same three every board uses: they
/// are properties of the Adafruit bootloader's `USER_FLASH_END`
/// (`memory.x`), which this board shares.
///
/// `usb_pid` 0x0004 is the next pid.codes TEST id after the solar node's
/// 0x0003; like those, it is not registered yet.
pub const CONFIG: super::BoardConfig = super::BoardConfig {
    usb_vid: 0x1209,
    usb_pid: 0x0004,
    usb_manufacturer: "leviculum",
    usb_product: "leviculum T1000-E",
    identity_flash_page: 0xEC000,
    radio_config_flash_page: 0xEB000,
    telemetry_flash_page: 0xEA000,
    // The SX1262's `SetDIO3AsTcxoCtrl` byte. Nothing reads it on this
    // board: its binary hands the driver [`LR1110`], whose `tcxo` field is
    // the TCXO fact for this part. The struct is shared, so the field
    // exists; the value is the SX1262 code for the same 1.6 V
    // (datasheet §13.3.6, Table 13-35), so even a misreading would ask for
    // the right voltage.
    lora_tcxo_voltage_reg: 0x00,
    // No external flash on this board: the variant names none.
    qspi_part: None,
};

/// The name each interface reports, indexed by interface id: what
/// `/status` and `lnstatus` print per interface, what `[TRANSPORT]` lines
/// and the node's interface table say. The LoRa entry names this board's
/// radio, an LR1110, which is why the table is the board's and not the
/// orchestration crate's. The binary hands it to
/// `leviculum_firmware::iface_bytes::set_names` once at boot.
pub const IFACE_NAMES: [&str; leviculum_firmware::iface_bytes::IFACE_COUNT] =
    ["serial_usb", "lora_lr1110", "ble"];

/// Panic-LED descriptor — port, pin, active-low flag — for `set_panic_led`.
/// The one LED on P0.24, **active high**.
pub const PANIC_LED_PORT: u8 = 0;
pub const PANIC_LED_PIN: u8 = 24;
pub const PANIC_LED_ACTIVE_LOW: bool = false;

/// The button, LED and off pins of the 8 s off and the boot-time factory
/// reset (order 1511, `crate::power`). Off: the LED (P0.24, active high)
/// and the buzzer's enable ([`BuzzerEnable`], P1.05) go low. The button
/// is active high with a pull-down, so it senses high.
#[cfg(feature = "softdevice")]
pub const POWER: crate::power::PowerFacts = crate::power::PowerFacts {
    button_active_high: true,
    led: (PANIC_LED_PORT, PANIC_LED_PIN, PANIC_LED_ACTIVE_LOW),
    off_pins: &[
        crate::power::OffPin {
            port: 0,
            pin: 24,
            high: false,
        },
        crate::power::OffPin {
            port: 1,
            pin: 5,
            high: false,
        },
    ],
};
