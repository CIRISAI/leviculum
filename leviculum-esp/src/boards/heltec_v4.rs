//! Heltec WiFi LoRa 32 V4 pin map and hardware constants.
//!
//! ESP32-S3R2 (U7) + SX1262 (U9) + KCT8103L front end (U8) + W25Q128 SPI
//! flash (U6) + 0.96" SSD1315 OLED, USB-C straight onto the SoC's own USB
//! Serial/JTAG pins.
//!
//! # Where these numbers come from
//!
//! Every net below was read off the manufacturer's schematic, by tracing
//! the wire from the ESP32-S3R2 pin to the part at the other end, not from
//! a pin table and not from another project's variant header. Two sheets
//! exist and both were traced:
//!
//!  * rev 4.3 — <https://resource.heltec.cn/download/WiFi_LoRa_32_V4/Schematic/HTIT-WB32LAF_V4.3.pdf>
//!  * rev 4.2 — <https://resource.heltec.cn/download/WiFi_LoRa_32_V4/Schematic/WiFi_LoRa_32_V4.2.pdf>
//!
//! and the datasheet, for the header pin table and the two supply rules:
//!
//!  * <https://resource.heltec.cn/download/WiFi_LoRa_32_V4/datasheet/WiFi_LoRa_32_V4.2.0.pdf>
//!    (V4.2.0, 2025-09-11)
//!
//! The citation form used below is `<sheet>: U7.<pin> <-> U9.<pin>`: the
//! net carries a label at each end, so a claim here can be checked by
//! looking at two pins rather than by trusting a summary.
//!
//! The datasheet states the board is "form factor and pin compatible with
//! WiFi LoRa 32 V3", and the seven radio nets below do come out identical
//! to the V3's. That is a corroboration, not the source: it is stated here
//! so a reader who knows the V3 numbers can see the agreement, and if the
//! two ever disagree the schematic wins.
//!
//! # The radio has a front end, and the two sheets disagree about it
//!
//! The V4 is the high-power variant: the SX1262 does not reach the antenna
//! directly but through a KCT8103L PA/LNA module (U8), which has three
//! control inputs — CSD (shutdown), CTX (transmit enable), CPS (path
//! select). Who drives which is the one place where the two published
//! sheets do NOT agree:
//!
//! | control | rev 4.2 | rev 4.3 |
//! |---------|---------|---------|
//! | CSD (U8.4) | GPIO2 (U7.7) | GPIO2 (U7.7) |
//! | CTX (U8.6) | SX1262 DIO2 (U9.12) | GPIO5 (U7.10) |
//! | CPS (U8.5) | GPIO46 (U7.52) | SX1262 DIO2 (U9.12) |
//!
//! CSD agrees; CTX and CPS are swapped between the revisions, and with
//! them the whole question of whether DIO2 steers the switch (as it does
//! on every nRF board we run) or a host GPIO does. That decides the
//! transmit path, so it is not a thing to average: the two candidate pins
//! are declared below as [`UNRESOLVED_FEM_CTX`] and [`UNRESOLVED_FEM_CPS`]
//! with the evidence attached, and the board revision has to be read off
//! the physical board before either is wired up. Step 1 puts nothing on
//! the air and therefore does not need the answer.

use esp_hal::peripherals;

// ---------------------------------------------------------------------
// SX1262 LoRa radio, on SPI2
// ---------------------------------------------------------------------

/// SX1262 SPI chip-select, net `LoRa_NSS` (active low).
///
/// rev 4.2 and rev 4.3: U7.13 <-> U9.19 (NSS).
pub type LoRaNss<'d> = peripherals::GPIO8<'d>;

/// SX1262 SPI clock, net `LoRa_SCK`.
///
/// rev 4.2 and rev 4.3: U7.14 <-> U9.18 (SCK).
pub type LoRaSck<'d> = peripherals::GPIO9<'d>;

/// SX1262 SPI MOSI, net `LoRa_MOSI`.
///
/// rev 4.2 and rev 4.3: U7.15 <-> U9.17 (MOSI).
pub type LoRaMosi<'d> = peripherals::GPIO10<'d>;

/// SX1262 SPI MISO, net `LoRa_MISO`.
///
/// rev 4.2 and rev 4.3: U7.16 <-> U9.16 (MISO).
pub type LoRaMiso<'d> = peripherals::GPIO11<'d>;

/// SX1262 reset, net `LoRa_RST` (active low).
///
/// rev 4.2 and rev 4.3: U7.17 <-> U9.15 (NRESET). The sheets show a 10K
/// (R42) standing beside this net; which rail it ties to was not read out
/// with enough confidence to claim a reset state for an undriven GPIO, so
/// the firmware drives the pin itself rather than relying on one.
pub type LoRaReset<'d> = peripherals::GPIO12<'d>;

/// SX1262 busy indicator, net `LoRa_BUSY` (high = busy).
///
/// rev 4.2 and rev 4.3: U7.18 <-> U9.14 (BUSY).
pub type LoRaBusy<'d> = peripherals::GPIO13<'d>;

/// SX1262 DIO1 interrupt output, net `DIO1`.
///
/// rev 4.2 and rev 4.3: U7.19 <-> U9.13 (DIO1).
pub type LoRaDio1<'d> = peripherals::GPIO14<'d>;

/// SX1262 SPI bus frequency in Hz.
///
/// The SX1262 datasheet allows 16 MHz; 8 MHz is taken here as the
/// conservative starting point for a bus that has never been run on this
/// board. Raising it is a measurement, not an edit.
pub const LORA_SPI_FREQ_HZ: u32 = 8_000_000;

/// SX1262 maximum TX power in dBm at the chip's own output.
///
/// This is the SX1262 pin, NOT what leaves the antenna: U8 (KCT8103L)
/// sits between them and the datasheet advertises 28 +/- 1 dBm at the
/// connector. Whatever the transmit path ends up programming has to
/// account for the PA, and that calculation belongs with the code that
/// enables the PA, not to a constant named `max_power`.
pub const LORA_MAX_POWER_DBM: i8 = 22;

/// The 32 MHz TCXO (X1) is supplied from the SX1262's DIO3 through the
/// ferrite bead L13, so the firmware must issue `SetDIO3AsTcxoCtrl`
/// before the chip can be calibrated (rev 4.3: U9.6 -> L13 -> X1.4 VDD,
/// decoupled by C39).
pub const LORA_TCXO_ON_DIO3: bool = true;

/// The DIO3 output voltage the TCXO needs is **not established**.
///
/// The schematic names X1 only as "32MHz" with no part number, and the
/// datasheet does not mention the TCXO at all, so the supply voltage
/// cannot be read out of either document. `SetDIO3AsTcxoCtrl` takes it as
/// an argument, and a guess there is a radio that either does not start
/// or runs its reference out of spec — so the value is absent rather than
/// wrong. Step 2 either finds the part marking on the board or measures
/// the rail.
pub const UNRESOLVED_TCXO_VOLTAGE: () = ();

// ---------------------------------------------------------------------
// KCT8103L PA/LNA front end (U8)
// ---------------------------------------------------------------------

/// Front-end shutdown, net `PA_CSD`.
///
/// The one FEM control both sheets agree on: rev 4.2 and rev 4.3,
/// U7.7 <-> U8.4 (CSD).
pub type FemShutdown<'d> = peripherals::GPIO2<'d>;

/// Front-end transmit enable (`PA_CTX`) — **unresolved**, see the module
/// documentation. rev 4.2 drives U8.6 from the SX1262's DIO2 (U9.12);
/// rev 4.3 drives it from GPIO5 (U7.10).
pub const UNRESOLVED_FEM_CTX: () = ();

/// Front-end path select (`PA_CPS`) — **unresolved**, see the module
/// documentation. rev 4.2 drives U8.5 from GPIO46 (U7.52); rev 4.3
/// drives it from the SX1262's DIO2 (U9.12).
pub const UNRESOLVED_FEM_CPS: () = ();

/// Front-end regulator enable, net `VFEM_Ctrl`: gates the TLV75733 (U3)
/// that supplies `Vfem`.
///
/// rev 4.2 and rev 4.3: U7.12 <-> U3.3 (EN). Also exposed on the header
/// as GPIO7 (datasheet pin table row 18, "GPIO7, ADC1_CH6, TOUCH7,
/// VFEM_Control").
pub type FemSupplyEnable<'d> = peripherals::GPIO7<'d>;

// ---------------------------------------------------------------------
// Indicators and board supplies
// ---------------------------------------------------------------------

/// White status LED (LED3), net `LED`. **Active high**: the GPIO feeds
/// R34 (330R) into the LED's anode and the cathode goes to ground, so a
/// high level lights it.
///
/// rev 4.2 and rev 4.3: U7.40 -> R34 -> LED3 -> GND. Also exposed on the
/// header as GPIO35 (datasheet pin table row 10, "GPIO35, ..., LED").
///
/// The red LED2 beside it is the charger's CHRG output (U4.7), not a GPIO.
pub type LedPin<'d> = peripherals::GPIO35<'d>;

/// True when driving [`LedPin`] high lights the LED.
pub const LED_ACTIVE_HIGH: bool = true;

/// External-peripheral supply enable, net `Vext_Ctrl`.
///
/// **Active low**: datasheet §2.4, "When using VE for external power
/// supply, the VextCtrl(GPIO36) pin needs to be pulled low." The
/// schematic agrees — U7.41 drives the gate of the P-channel Q2 through
/// R10/R8.
pub type VextEnable<'d> = peripherals::GPIO36<'d>;

/// True when driving [`VextEnable`] low turns the Vext rail on.
pub const VEXT_ACTIVE_LOW: bool = true;

/// GNSS-header supply enable, net `VGNSS_Ctrl` (rev 4.2 and rev 4.3:
/// U7.39, gate of Q7 through R21/R22).
pub type GnssSupplyEnable<'d> = peripherals::GPIO34<'d>;

// ---------------------------------------------------------------------
// Battery measurement
// ---------------------------------------------------------------------

/// Battery-divider tap, net `ADC_IN` (ADC1_CH0).
///
/// rev 4.2 and rev 4.3: U7.6, between R27 (390K, to VBAT through Q5) and
/// R28 (100K, to GND). Datasheet pin table row 12 names the same pin
/// "GPIO1, ADC1_CH0, TOUCH1, VBAT_Read1".
pub type BatterySensePin<'d> = peripherals::GPIO1<'d>;

/// Divider enable, net `ADC_Ctrl`: switches Q5 so the divider only draws
/// current while a reading is being taken.
///
/// rev 4.2 and rev 4.3: U7.42. Datasheet §2.4: "ADC1_CH0 is used to read
/// the lithium battery voltage, the ADC_CTRL(37) pin needs to be pulled
/// high."
pub type BatterySenseEnable<'d> = peripherals::GPIO37<'d>;

/// High-side resistor of the battery divider, in ohms (R27).
pub const BATTERY_DIVIDER_HIGH_OHMS: u32 = 390_000;
/// Low-side resistor of the battery divider, in ohms (R28).
pub const BATTERY_DIVIDER_LOW_OHMS: u32 = 100_000;

// The datasheet prints the relation as `VBAT = 100 / (100+390) *
// VADC_IN1`, which is the reciprocal of the divider it draws one page
// earlier: 390K from the battery into 100K to ground puts VBAT/4.9 on the
// pin, so recovering VBAT means multiplying by 4.9, not dividing. The
// resistor values above are the schematic's and the arithmetic is done
// from them; the datasheet line is noted so the next reader does not have
// to rediscover that it is inverted.

// ---------------------------------------------------------------------
// Buttons and the rest of the fixed wiring
// ---------------------------------------------------------------------

/// The `PRG` button, net `USER_Key`: switch S2 (`USER_SW`) pulls the net
/// to ground, so the pressed level is low.
///
/// rev 4.2 and rev 4.3: U7.5. This is GPIO0, the ROM's boot-mode strap:
/// held low at reset the SoC enters serial download instead of running
/// the image.
pub type UserButton<'d> = peripherals::GPIO0<'d>;

/// OLED I2C data, net `OLED_SDA` (rev 4.2 and rev 4.3: U7.23; datasheet
/// pin table, "GPIO17, OLED_SDA").
pub type OledSda<'d> = peripherals::GPIO17<'d>;
/// OLED I2C clock, net `OLED_SCL` (rev 4.2 and rev 4.3: U7.24; datasheet
/// pin table, "GPIO18, OLED_SCL").
pub type OledScl<'d> = peripherals::GPIO18<'d>;
/// OLED reset, net `OLED_RST` (rev 4.2 and rev 4.3: U7.27; datasheet pin
/// table, "GPIO21, OLED RST").
pub type OledReset<'d> = peripherals::GPIO21<'d>;

// USB is not a pin alias here. The Type-C connector (P1) runs through
// R17/R18 (22R) straight to GPIO20 (D+) and GPIO19 (D-) — there is no
// USB-to-UART bridge on this board, U1..U3 are regulators and U4 is the
// charger. Those two pins therefore belong to the SoC's USB Serial/JTAG
// peripheral, which takes them itself and never appears as a GPIO;
// `lib.rs` opens it through `peripherals.USB_DEVICE`. Confirmed on both
// sheets and in the datasheet pin table ("GPIO20, ..., USB_D+",
// "GPIO19, ..., USB_D-").

/// The board's entry in the shared metadata table.
pub const CONFIG: super::BoardConfig = super::BoardConfig {
    log_prefix: "V4",
    board_name: "Heltec WiFi LoRa 32 V4",
    lora_spi_freq_hz: LORA_SPI_FREQ_HZ,
    lora_max_power_dbm: LORA_MAX_POWER_DBM,
    // The V4 has a GNSS *header* (P3, SH1.25-8Pin) and a switched supply
    // for it, but nothing is soldered on the board. Whatever a user plugs
    // in is a runtime discovery, not a build-time fact.
    gnss_onboard: false,
};
