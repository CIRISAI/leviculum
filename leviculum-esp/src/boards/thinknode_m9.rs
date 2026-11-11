//! Elecrow ThinkNode M9 pin map: the nets the Loquax image uses (order
//! 1361, slice four of `docs/src/concepts/handheld-messenger.md`).
//!
//! ESP32-S3R8 (8 MB PSRAM, 16 MB flash) + LR1110 + a 2.4-inch ST7789 panel
//! mounted landscape + a 37-key keyboard behind an I2C controller, and a
//! CH340K bridge between the SoC's UART0 and the USB-C port.
//!
//! # Where these numbers come from
//!
//! Not from a schematic: Elecrow has published none, and the bring-up plan
//! asks for one before radio work (`docs/src/concepts/thinknode-m9.md`,
//! "Bring-up plan", step 1). Every net below is carried from that page,
//! which names its source per line (the Meshtastic and MeshCore variants
//! agreeing, the wiki), and every one of them has already driven this
//! unit: the map measurement firmware lit the panel through exactly these
//! pins (`leviculum-mapfill/m9/src/main.rs`), and the community port's
//! probe build found the keyboard controller on this bus. The radio's
//! nets are only parked here, never driven: this image puts nothing on the
//! air.

use esp_hal::peripherals;
use leviculum_lr11xx::board::{PaTable, RfSwitch, Tcxo, LR1110MB1DXS_PA_TABLE, RFSW0, RFSW1};
use leviculum_lr11xx::{Board, Chip, LfClk, RegMode, TcxoVoltage};

// ---------------------------------------------------------------------
// The shared SPI bus: panel, LR1110, SD card (`thinknode-m9.md`,
// "Storage": SCK 40, MOSI 47, MISO 38)
// ---------------------------------------------------------------------

/// SPI2 clock.
pub type SpiSck<'d> = peripherals::GPIO40<'d>;
/// SPI2 MOSI.
pub type SpiMosi<'d> = peripherals::GPIO47<'d>;
/// SPI2 MISO.
pub type SpiMiso<'d> = peripherals::GPIO38<'d>;
/// The LR1110's chip select, held high so it ignores the panel's traffic.
pub type LoRaNss<'d> = peripherals::GPIO39<'d>;
/// The LR1110's BUSY (`thinknode-m9.md`, "Radio": BUSY 41).
pub type LoRaBusy<'d> = peripherals::GPIO41<'d>;
/// The LR1110's IRQ, its DIO9 (IRQ 42).
pub type LoRaIrq<'d> = peripherals::GPIO42<'d>;
/// The LR1110's NRESET, active low (NRESET 45).
pub type LoRaReset<'d> = peripherals::GPIO45<'d>;
/// The SD card's chip select, held high for the same reason
/// (`thinknode-m9.md`, "Storage": GPIO48).
pub type SdCs<'d> = peripherals::GPIO48<'d>;

// ---------------------------------------------------------------------
// The panel (`thinknode-m9.md`, "Display": CS 16, DC 15, RESET 14,
// backlight 17 on when low)
// ---------------------------------------------------------------------

pub type LcdCs<'d> = peripherals::GPIO16<'d>;
pub type LcdDc<'d> = peripherals::GPIO15<'d>;
pub type LcdReset<'d> = peripherals::GPIO14<'d>;
pub type LcdBacklight<'d> = peripherals::GPIO17<'d>;
/// The panel's tearing-effect output (1507): GPIO19 in both variants
/// (`thinknode-m9.md`, "Display": TE 19). No schematic says it; a line
/// that never moves leaves the draws paced by the clock
/// (`loquax_m9::pace`), and the boot says which.
pub type LcdTe<'d> = peripherals::GPIO19<'d>;

/// The panel's clock: 40 MHz, at which a full frame flushed in 30.8 ms
/// from SRAM and 31.5 ms from PSRAM, 99.8 % of the line rate
/// (`docs/drafts/map-measurement-2026-10.md`, "Device rows, measured
/// 2026-10-26").
pub const LCD_SPI_MHZ: u32 = 40;

/// MADCTL for the panel as mounted: MY|MV, 320 wide and 240 high, the
/// community port's `[DISP] ST7789 320x240 madctl=0xb0`. The map firmware's
/// 0x00 drew portrait, rotated 90 degrees against the host
/// (`thinknode-m9.md`, "Display"); whether 0xB0 also mirrors is what the
/// first frame of text on this image shows.
pub const LCD_MADCTL: u8 = 0xB0;

/// Peripheral power, on when low (`thinknode-m9.md`, "Display" and
/// "Power": `VEXT_ENABLE`, GPIO18). Powers the panel.
pub type VextEnable<'d> = peripherals::GPIO18<'d>;

// ---------------------------------------------------------------------
// The keyboard (`thinknode-m9.md`, "Keyboard" and "Two hardware
// revisions")
// ---------------------------------------------------------------------

/// The keyboard controller's own I2C bus: SDA 20.
pub type KbSda<'d> = peripherals::GPIO20<'d>;
/// SCL 21.
pub type KbScl<'d> = peripherals::GPIO21<'d>;
/// The controller's address on V1.1, our unit.
pub const KB_ADDR_V11: u8 = 0x6D;
/// Its address on V1.0.
pub const KB_ADDR_V10: u8 = 0x6C;
/// The bus clock: standard mode. Nothing states the controller's
/// ceiling, so the image asks for the rate every I2C part takes.
pub const KB_I2C_KHZ: u32 = 100;

// ---------------------------------------------------------------------
// GNSS (`thinknode-m9.md`, "GNSS": an Airoha AG3352, 1477)
// ---------------------------------------------------------------------

/// UART1 TX, board to receiver. The two public variants name the pins in
/// opposite directions; these are the community port's, whose probe build
/// got `$PAIR021,AG3352` back on our unit (2026-10-26).
pub type GnssTx<'d> = peripherals::GPIO3<'d>;
/// UART1 RX, receiver to board.
pub type GnssRx<'d> = peripherals::GPIO2<'d>;
/// The receiver's power enable: active low on V1.0, active high on V1.1
/// (Meshtastic's `variant.cpp`, the keyboard controller's address tells
/// the revision, [`gnss_enable_active_high`]).
pub type GnssEnable<'d> = peripherals::GPIO11<'d>;
/// The AG3352's wake line, held high as the Meshtastic variant and the
/// community port hold it (the page keeps open whether it is an input).
pub type GnssWake<'d> = peripherals::GPIO10<'d>;

/// Whether the GNSS enable is active high: on V1.1, whose keyboard
/// controller answers at [`KB_ADDR_V11`]; V1.0's at [`KB_ADDR_V10`] is
/// active low. A board where neither answered is taken as our V1.1.
pub fn gnss_enable_active_high(kb_address: u8) -> bool {
    kb_address != KB_ADDR_V10
}

// ---------------------------------------------------------------------
// The host link (`thinknode-m9.md`, "USB and the rest": a CH340K on
// UART0)
// ---------------------------------------------------------------------

/// UART0 TX, to the CH340K.
pub type UartTx<'d> = peripherals::GPIO43<'d>;
/// UART0 RX, from the CH340K.
pub type UartRx<'d> = peripherals::GPIO44<'d>;
/// The rate periculum and the map firmware open the port at.
pub const UART_BAUD: u32 = 115_200;

/// The PSRAM the UI heap lives in: 8 MB, octal, which came up with
/// 8,388,608 bytes in the map measurement (`thinknode-m9.md`, "The board
/// as the UI sees it").
pub const PSRAM_BYTES: usize = 8 << 20;

/// The UI core's own heap pool at the top of the PSRAM (1594), out of
/// [`PSRAM_BYTES`]; core 0 keeps the rest. 2 MiB: the plan's UI budget is
/// 1.1 MB on the 64-bit host (`handheld-messenger.md`, "Memory budgets,
/// measured"), and window 19 (2026-11-09, bench image of fbe1e3f6,
/// `bench/board.log`) measured the whole board at 1.36 MB in use at its
/// busiest (`[OPEN_HEAP] heap_free=7027384` of 8,388,608), the UI core's
/// censused share (renderer 62,096, map 218,048, the kept picture
/// 153,600, a draw's peak 82,487) about 0.5 MB of it. So the pool holds
/// the UI core's plan twice and the whole board's use as well; a request
/// it cannot serve falls back to core 0's pool (`pools.rs`), costing a
/// shared lock, never a failure.
pub const UI_HEAP_BYTES: usize = 2 << 20;

/// Short tag in the identity lines.
pub const LOG_PREFIX: &str = "M9";
/// Board name in the identity lines.
pub const BOARD_NAME: &str = "Elecrow ThinkNode M9";

// ---------------------------------------------------------------------
// The radio (`thinknode-m9.md`, "Radio"; order 1478): the LR1110's facts
// as the driver takes them, and the PHY the board comes up on
// ---------------------------------------------------------------------

/// The LR1110's SPI clock: it shares the bus with the panel's 40 MHz, and
/// takes at most 16 MHz itself (`leviculum-lr11xx` README, the M9's worked
/// example); 4 MHz as on every nRF board that drives one (`lora::init`).
pub const LORA_SPI_HZ: u32 = 4_000_000;

/// The M9's LR1110 as `leviculum_lr11xx` takes it (1478). Every field is
/// the page's or a bench probe's, none from a schematic (Elecrow has
/// published none):
///
/// - TCXO 3.3 V on DIO3, both variants; RadioLib's 5 ms start-up (164
///   steps of the 32.768 kHz clock), the README's worked example.
/// - The RF switch on DIO5 (RFSW0) and DIO6 (RFSW1), the same table in
///   both variants, DIO5 first: standby LL, RX HL, TX HH, TX_HP LH. Both
///   PA paths are wired; DIO7 and DIO8 are reserved nets, enabled for
///   nothing.
/// - The LDO, not the DC-DC: the bench probe initialised clean in DC-DC
///   mode, but whether the inductor is fitted is open (Meshtastic runs
///   LDO), and the LDO is the mode that works either way.
/// - The 32.768 kHz crystal: `LfClk::Xtal` with `wait_32k_ready` came up
///   with a clean `GetErrors` on our unit (2026-10-26), where a missing
///   crystal says bit 6.
/// - The PA table: unknown without the schematic or an FCC report; the
///   Semtech LR1110MB1DXS shield's, as the T1000-E runs, -17 to +22 dBm.
///   The community port's probe build clamps a requested 22 dBm to 17;
///   radiated power is unmeasured until the rig measures it.
pub const LR1110: Board = Board {
    name: "thinknode_m9",
    chip: Chip::Lr1110,
    tcxo: Some(Tcxo {
        voltage: TcxoVoltage::V3_3,
        startup_steps: 164,
    }),
    rf_switch: RfSwitch {
        enable: RFSW0 | RFSW1,
        standby: 0,
        rx: RFSW0,
        tx: RFSW0 | RFSW1,
        tx_hp: RFSW1,
        tx_hf: 0,
        gnss: 0,
        wifi: 0,
    },
    reg_mode: RegMode::Ldo,
    lf_clk: LfClk::Xtal,
    wait_32k_ready: true,
    rx_boosted: false,
    rssi_calibration: Some(leviculum_lr11xx::board::SEMTECH_RSSI_CALIBRATION),
    pa: PaTable::new(&LR1110MB1DXS_PA_TABLE, -17),
};

/// The PHY the radio comes up on (`project_lora_default_phy`): the
/// ReticulumNet NL channel every Leviculum board's default is, 869.463 MHz,
/// BW 125 kHz, SF8, CR 4/5, as `leviculum-nrf/src/lora.rs`'s
/// `RadioConfig::eu_medium`.
pub const LORA_FREQUENCY_HZ: u32 = 869_463_000;
/// Bandwidth, Hz.
pub const LORA_BANDWIDTH_HZ: u32 = 125_000;
/// Spreading factor.
pub const LORA_SF: u8 = 8;
/// Coding rate denominator, 4/5.
pub const LORA_CR_DENOM: u8 = 5;
