//! What a bootloader self-update UF2 will write, read before it is copied.
//!
//! lnflash never produces such an image ([`uf2::Image::encode`] refuses the
//! family); this module only reads one, so that a person about to drop RAK's
//! `update-*_nosd.uf2` onto a board knows which flash pages it reaches
//! (docs/src/concepts/ota-stage-1-ble-dfu.md, "Bootloader update").
//!
//! # Why the file's own addresses are not the answer
//!
//! A bootloader-family UF2 carries the new bootloader at its final address,
//! `0xF4000` up. The bootloader RUNNING on the board does not write it there.
//! It stages every bootloader block lower down, inside the application
//! window, and only once the last block has arrived asks the MBR to copy the
//! staged image over itself (RAK's 0.4.3 source, which is Adafruit 0.6.2:
//! `src/usb/uf2/ghostfat.c`, the `CFG_UF2_FAMILY_BOOT_ID` arm of
//! `write_block`, and `src/usb/msc_uf2.c`, `SD_MBR_COMMAND_COPY_BL`). The
//! staging offset is `BOOTLOADER_ADDR_END - USER_FLASH_END`, both the running
//! bootloader's constants. So the pages a self-update touches are a property
//! of the image AND of the bootloader receiving it, and [`plan`] takes both.
//!
//! On a RAK4631 that offset is `0xFE000 - 0xEA000 = 0x14000`: the new
//! bootloader lands in `0xE0000..0xEA000` first, the top ten pages of the
//! application window, which on our boards is the record log (`STORE` in
//! `leviculum-nrf/memory.x`). Never the identity: the staging window ends at
//! `USER_FLASH_END` by construction, and the identity, radio and telemetry
//! pages sit at or above it.
//!
//! # What is refused
//!
//! An image that is not entirely bootloader family, a block outside the MBR,
//! the bootloader region and the UICR word pair, a UICR that names another
//! bootloader start or MBR parameter page than the running bootloader's (it
//! would abort on that itself, after having staged everything before it), an
//! image without the UICR block (likewise), and any staged page inside a
//! region the caller protects. The MBR blocks are skipped by the running
//! bootloader and only reported. A SoftDevice inside the image is refused by
//! the "outside" rule: it would sit at `0x1000` and up.

use crate::uf2::{self, Image};

/// First flash address the bootloader treats as application: everything
/// below is the MBR and is skipped, not written (`USER_FLASH_START` =
/// `MBR_SIZE`, docs/src/concepts/lnode-flashing.md).
pub const USER_FLASH_START: u32 = 0x1000;

/// The UICR page a bootloader UF2 carries its two address words in. The
/// running bootloader accepts exactly this block address (`in_uicr_space`).
pub const UICR_BLOCK: u32 = 0x1000_1000;
/// Offset of `NRF_UICR->NRFFW[0]`, the bootloader start, in that block.
const UICR_BOOT_OFFSET: usize = 0x14;
/// Offset of `NRF_UICR->NRFFW[1]`, the MBR parameter page, in that block.
const UICR_MBR_PARAMS_OFFSET: usize = 0x18;

/// nRF52840 flash page: the unit the bootloader erases and rewrites
/// (`flash_nrf5x_write` caches one 4 KiB page and rewrites it whole).
pub const FLASH_PAGE: u32 = 0x1000;

/// The constants of the bootloader that will RECEIVE the update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receiver {
    /// `USER_FLASH_END`: the top of the window it writes applications to,
    /// measured from `CURRENT.UF2` (docs/src/concepts/lnode-flashing.md).
    pub user_flash_end: u32,
    /// `BOOTLOADER_ADDR_START`, also `NRF_UICR->NRFFW[0]`.
    pub boot_start: u32,
    /// `BOOTLOADER_ADDR_END` = `BOOTLOADER_MBR_PARAMS_PAGE_ADDRESS`, also
    /// `NRF_UICR->NRFFW[1]`.
    pub mbr_params: u32,
}

/// RAK's 0.4.3 on the RAK4631, the build on the rig RAK. The window top is
/// the measured `CURRENT.UF2` span; the bootloader start and parameter page
/// are the UICR words of its own hex (`0x10001014`, `0x10001018`), and the
/// source's `linker/nrf52840.ld` agrees on both.
pub const RAK4631_0_4_3: Receiver = Receiver {
    user_flash_end: 0xE_A000,
    boot_start: 0xF_4000,
    mbr_params: 0xF_E000,
};

impl Receiver {
    /// How far below its final address a bootloader block is staged.
    pub fn staging_offset(&self) -> u32 {
        self.mbr_params - self.user_flash_end
    }

    /// The region the final MBR copy overwrites: `DFU_BL_IMAGE_MAX_SIZE`
    /// bytes from the bootloader start.
    pub fn installed(&self) -> Range {
        Range::new(self.boot_start, self.mbr_params)
    }

    fn in_bootloader(&self, addr: u32) -> bool {
        self.boot_start <= addr && addr < self.mbr_params
    }
}

/// A half-open flash address range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Range {
    pub start: u32,
    pub end: u32,
}

impl Range {
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    pub fn overlaps(&self, other: &Range) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// The same range widened to whole flash pages.
    pub fn pages(&self) -> Range {
        Range::new(
            self.start & !(FLASH_PAGE - 1),
            self.end.div_ceil(FLASH_PAGE) * FLASH_PAGE,
        )
    }
}

impl std::fmt::Display for Range {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#07x}..{:#07x}", self.start, self.end)
    }
}

/// A region the caller names, for refusal or for the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub name: String,
    pub range: Range,
}

/// Everything the update will do to flash, in the order it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// `UF2 Bootloader <version>` as the new image will print it in
    /// `INFO_UF2.TXT`, if the image carries the string.
    pub version: Option<String>,
    /// The file's own address spans.
    pub literal: Vec<Range>,
    /// Spans below [`USER_FLASH_START`], skipped by the receiver.
    pub skipped: Vec<Range>,
    /// Byte spans written while staging, at their staged addresses.
    pub staged: Vec<Range>,
    /// The same, widened to the pages erased and rewritten.
    pub staged_pages: Vec<Range>,
    /// What the MBR copy overwrites at the end.
    pub installed: Range,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error(
        "not a bootloader self-update: the image does not carry family {:#010x} on every block",
        uf2::FAMILY_NRF52_BOOTLOADER
    )]
    NotBootloaderFamily,
    #[error("an image with no blocks")]
    Empty,
    #[error("block at {addr:#010x} is outside the MBR, the bootloader region {region} and the UICR block")]
    Outside { addr: u32, region: Range },
    #[error("no UICR block at {UICR_BLOCK:#010x}: the receiving bootloader aborts such an update after staging it")]
    NoUicr,
    #[error("UICR names bootloader {boot:#x} and MBR parameters {mbr:#x}, the receiver is at {want_boot:#x} and {want_mbr:#x}")]
    UicrMismatch {
        boot: u32,
        mbr: u32,
        want_boot: u32,
        want_mbr: u32,
    },
    #[error("staging writes page(s) {pages} inside {name} {region}")]
    Protected {
        name: String,
        region: Range,
        pages: Range,
    },
}

/// Read what `image` will do to a board whose bootloader is `receiver`, and
/// refuse it if any page it erases lies in one of `protected`.
pub fn plan(image: &Image, receiver: &Receiver, protected: &[Region]) -> Result<Plan, Refusal> {
    if image.blocks.is_empty() {
        return Err(Refusal::Empty);
    }
    if image.family_id() != Some(uf2::FAMILY_NRF52_BOOTLOADER) {
        return Err(Refusal::NotBootloaderFamily);
    }

    let offset = receiver.staging_offset();
    let mut skipped = Vec::new();
    let mut staged = Vec::new();
    let mut uicr = None;
    for block in &image.blocks {
        let range = Range::new(block.target_addr, block.end());
        if block.target_addr == UICR_BLOCK {
            let word = |at: usize| {
                block
                    .data
                    .get(at..at + 4)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            };
            uicr = Some((word(UICR_BOOT_OFFSET), word(UICR_MBR_PARAMS_OFFSET)));
        } else if block.end() <= USER_FLASH_START {
            skipped.push(range);
        } else if receiver.in_bootloader(block.target_addr) && block.end() <= receiver.mbr_params {
            staged.push(Range::new(range.start - offset, range.end - offset));
        } else {
            return Err(Refusal::Outside {
                addr: block.target_addr,
                region: receiver.installed(),
            });
        }
    }

    match uicr {
        None => return Err(Refusal::NoUicr),
        Some((Some(boot), Some(mbr))) => {
            if boot != receiver.boot_start || mbr != receiver.mbr_params {
                return Err(Refusal::UicrMismatch {
                    boot,
                    mbr,
                    want_boot: receiver.boot_start,
                    want_mbr: receiver.mbr_params,
                });
            }
        }
        Some(_) => return Err(Refusal::NoUicr),
    }

    let staged = merge(staged);
    let staged_pages = merge(staged.iter().map(Range::pages).collect());
    for region in protected {
        for pages in &staged_pages {
            if pages.overlaps(&region.range) {
                return Err(Refusal::Protected {
                    name: region.name.clone(),
                    region: region.range,
                    pages: *pages,
                });
            }
        }
    }

    Ok(Plan {
        version: version_string(image),
        literal: image
            .spans()
            .into_iter()
            .map(|(start, end)| Range::new(start, end))
            .collect(),
        skipped: merge(skipped),
        staged,
        staged_pages,
        installed: receiver.installed(),
    })
}

impl Plan {
    /// The staged pages that fall inside `region`, for the report: the pages
    /// of a region the caller let through and whose content is therefore
    /// lost.
    pub fn pages_in(&self, region: &Range) -> Vec<Range> {
        self.staged_pages
            .iter()
            .filter(|p| p.overlaps(region))
            .map(|p| Range::new(p.start.max(region.start), p.end.min(region.end)))
            .collect()
    }
}

/// The regions of `memory.x` named `names`, read from its `NAME : ORIGIN =
/// 0x..., LENGTH = 0x...` lines. One reader so that the recipe and the test
/// protect exactly what the linker places.
pub fn regions_from_memory_x(text: &str, names: &[&str]) -> Vec<Region> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !names.contains(&name) {
            continue;
        }
        let field = |key: &str| {
            let at = rest.find(key)? + key.len();
            let tail = rest[at..].trim_start().strip_prefix('=')?.trim_start();
            let hex = tail.strip_prefix("0x")?;
            let digits: String = hex
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == '_')
                .filter(|c| *c != '_')
                .collect();
            u32::from_str_radix(&digits, 16).ok()
        };
        if let (Some(origin), Some(length)) = (field("ORIGIN"), field("LENGTH")) {
            out.push(Region {
                name: name.to_string(),
                range: Range::new(origin, origin + length),
            });
        }
    }
    out
}

/// What a bootloader update must leave alone on an nRF52840 LNode: the
/// SoftDevice (`USER_FLASH_START` up to the application), the application
/// image, the calendar ring and the boot record from `memory_x`, and the band
/// between the receiver's window top and its own start, where the identity,
/// radio and telemetry pages live. `STORE` is not in it: a RAK4631 update
/// stages into the record log and cannot do otherwise, and [`Plan::pages_in`]
/// reports the pages it costs.
pub fn protected_regions(memory_x: &str, receiver: &Receiver) -> Vec<Region> {
    let mut out = regions_from_memory_x(memory_x, &["FLASH", "TIME", "BOOT"]);
    if let Some(flash) = out.iter().find(|r| r.name == "FLASH") {
        out.push(Region {
            name: "SoftDevice".into(),
            range: Range::new(USER_FLASH_START, flash.range.start),
        });
    }
    out.push(Region {
        name: "app data band".into(),
        range: Range::new(receiver.user_flash_end, receiver.boot_start),
    });
    out
}

fn merge(mut ranges: Vec<Range>) -> Vec<Range> {
    ranges.sort_unstable();
    let mut out: Vec<Range> = Vec::new();
    for range in ranges {
        match out.last_mut() {
            Some(prev) if prev.end >= range.start => prev.end = prev.end.max(range.end),
            _ => out.push(range),
        }
    }
    out
}

fn version_string(image: &Image) -> Option<String> {
    const NEEDLE: &[u8] = b"UF2 Bootloader ";
    let mut bytes = Vec::new();
    let mut blocks: Vec<&uf2::Block> = image.blocks.iter().collect();
    blocks.sort_by_key(|b| b.target_addr);
    for block in blocks {
        bytes.extend_from_slice(&block.data);
    }
    let at = bytes.windows(NEEDLE.len()).position(|w| w == NEEDLE)? + NEEDLE.len();
    let tail = &bytes[at..];
    let end = tail
        .iter()
        .position(|b| *b == b'\r' || *b == b'\n' || *b == 0 || *b == b' ')?;
    String::from_utf8(tail[..end].to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_range_widens_to_whole_pages() {
        assert_eq!(
            Range::new(0xE_7000, 0xE_7B00).pages(),
            Range::new(0xE_7000, 0xE_8000)
        );
        assert_eq!(
            Range::new(0xE_9800, 0xE_9900).pages(),
            Range::new(0xE_9000, 0xE_A000)
        );
    }

    #[test]
    fn the_rak_receiver_stages_twenty_pages_below_its_target() {
        assert_eq!(RAK4631_0_4_3.staging_offset(), 0x1_4000);
        assert_eq!(RAK4631_0_4_3.installed(), Range::new(0xF_4000, 0xF_E000));
    }

    #[test]
    fn memory_x_lines_are_read_with_their_lengths() {
        let text = "    STORE : ORIGIN = 0x000DA000, LENGTH = 0x10000\n\
                    /* FLASH : ORIGIN = 0x1, LENGTH = 0x1 in a comment */\n\
                    FLASH : ORIGIN = 0x00027000, LENGTH = 0xB0000\n";
        let regions = regions_from_memory_x(text, &["STORE", "FLASH"]);
        assert_eq!(
            regions,
            vec![
                Region {
                    name: "STORE".into(),
                    range: Range::new(0xD_A000, 0xE_A000)
                },
                Region {
                    name: "FLASH".into(),
                    range: Range::new(0x2_7000, 0xD_7000)
                },
            ]
        );
    }
}
