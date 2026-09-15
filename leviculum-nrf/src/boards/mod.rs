//! Board-specific pin mappings and shared board metadata.
//!
//! # The antenna switch is a two-shaped thing
//!
//! Every board here sets `SetDIO2AsRfSwitch` — the SX1262 raises DIO2
//! for the duration of a transmit and the board's switch follows it.
//! On the T114 and the RAK4631 that is the whole story, and both board
//! files say so out loud: the RAK's antenna-switch pad exists on the
//! schematic and must NOT be driven, because DIO2 owns it.
//!
//! The Wio-SX1262 on the solar node splits the job. DIO2 still steers
//! the transmit side, but the receive side is a host GPIO
//! ([`solarnode::LoRaRxEnable`]) that has to be asserted for a listening
//! window and released before a key-up — with both asserted at once the
//! switch is in neither position and the transmit goes nowhere.
//!
//! So a board's front end is not one boolean any more. It is: DIO2,
//! always; plus an OPTIONAL host-driven RX-enable pin, which a board
//! module declares as a type alias exactly like every other pin it
//! owns, and which its binary hands to [`crate::lora::init`] as
//! `Some(..)`. The pin cannot live in [`BoardConfig`] — a `Peri` is a
//! moved handle, not a `const` — and it is deliberately not mirrored
//! there as a flag either: the driver takes the pin itself, so a second
//! copy of the fact could only ever be a copy that disagrees. The
//! switching lives in `sx1262.rs` and `lora.rs`, i.e. in the interface,
//! because knowing that this medium has a front end to steer is exactly
//! the interface's business (`docs/src/concepts/interface-isolation.md`).
//! Nothing about it reaches `transport.rs`.

pub mod rak4631;
pub mod solarnode;
pub mod t114;

/// Runtime board metadata consumed by shared init code (USB, flash, LoRa).
///
/// Every board module exposes a `pub const CONFIG: BoardConfig` with its
/// own values; shared init functions take `&'static BoardConfig` so the
/// same code path serves every binary target.
pub struct BoardConfig {
    /// USB Vendor ID advertised by the firmware.
    pub usb_vid: u16,
    /// USB Product ID advertised by the firmware.
    pub usb_pid: u16,
    /// USB iManufacturer string.
    pub usb_manufacturer: &'static str,
    /// USB iProduct string.
    pub usb_product: &'static str,
    /// Short tag used in identity log lines (e.g. "T114", "RAK").
    pub log_prefix: &'static str,
    /// Internal-flash byte address of the page reserved for identity storage.
    pub identity_flash_page: u32,
    /// Internal-flash byte address of the page reserved for the persisted
    /// LoRa radio configuration (see [`crate::radio_store`]). Distinct from
    /// [`identity_flash_page`](Self::identity_flash_page) and, like it,
    /// outside the linker's FLASH region (`memory.x`).
    pub radio_config_flash_page: u32,
    /// Internal-flash byte address of the page holding the persisted
    /// telemetry target (Codeberg #236) and the user-set fixed position —
    /// two records, layout named in [`crate::telemetry`]. Third page in
    /// the same band as the two above, chosen for the same reason:
    /// `0xEA000` is the bootloader's `USER_FLASH_END`, so a UF2 flash
    /// declines every block from there upwards and the configuration
    /// survives a firmware update.
    pub telemetry_flash_page: u32,
    /// SX1262 TCXO voltage select byte for `SetDIO3AsTcxoCtrl`
    /// (0x02 = 1.8 V, see datasheet §13.3.6).
    pub lora_tcxo_voltage_reg: u8,
    /// SX1262 SPI bus frequency in Hz.
    pub lora_spi_freq_hz: u32,
    /// SX1262 maximum TX power in dBm.
    pub lora_max_power_dbm: i8,
    /// The QSPI NOR part this board carries: its JEDEC id, its density and
    /// the bus clock it is driven at. The boot probe refuses to hand out a
    /// device whose id does not match this ([`crate::qspi`]).
    ///
    /// `None` means no part is mounted, and then nothing may configure
    /// this board's QSPI pins at all. It covers two different boards.
    /// On the T114 and the RAK4631 there is no part to mount: those six
    /// nets are on the expansion header, two of them have other
    /// functions in the sibling variant, and driving them blind is not
    /// free — the evidence is in [`t114`] and [`rak4631`] where the pin
    /// aliases used to be (Codeberg #384). On the solar node a
    /// P25Q16H really is fitted, on the XIAO module, and nothing in
    /// this firmware uses it yet; see [`solarnode`].
    pub qspi_part: Option<&'static crate::qspi::FlashPart>,
}
