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

/// Short tag in the identity lines.
pub const LOG_PREFIX: &str = "M9";
/// Board name in the identity lines.
pub const BOARD_NAME: &str = "Elecrow ThinkNode M9";
