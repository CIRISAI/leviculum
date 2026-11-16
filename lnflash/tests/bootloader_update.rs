//! RAK's 0.4.4 self-update UF2, read against our flash map before anyone
//! copies it onto a board (order 1609, Codeberg #235).
//!
//! The image is the published file, vendored byte for byte with its
//! provenance (`tests/fixtures/bootloader/rak4631/README.md`). The map
//! is read from the two files that define it, `leviculum-nrf/memory.x` and
//! the RAK4631 board config, so a page moved there is a page this test
//! checks without being edited. Every number asserted is also in
//! `docs/src/concepts/ota-stage-1-ble-dfu.md`, "Bootloader update"; a
//! disagreement is a finding, not a constant to adjust.
//!
//! Needs no hardware and no tool outside cargo.

use lnflash::bootloader_update::{
    plan, protected_regions, regions_from_memory_x, Range, Receiver, Refusal, Region,
    RAK4631_0_4_3, UICR_BLOCK,
};
use lnflash::uf2::{self, Block, Image};

const UPDATE_UF2: &[u8] = include_bytes!(
    "fixtures/bootloader/rak4631/update-wiscore_rak4631_board_bootloader-0.4.4_nosd.uf2"
);
const MEMORY_X: &str = include_str!("../../leviculum-nrf/memory.x");
const RAK4631_BOARD: &str = include_str!("../../leviculum-nrf/src/boards/rak4631.rs");

fn image() -> Image {
    Image::parse(UPDATE_UF2).expect("the vendored update UF2 parses")
}

/// A page the RAK4631 board config names, as `<field>: 0x...,`.
fn board_page(field: &str) -> Region {
    let line = RAK4631_BOARD
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(&format!("{field}:")))
        .unwrap_or_else(|| panic!("rak4631.rs names no {field}"));
    let hex = line
        .split_once("0x")
        .map(|(_, h)| h.trim_end_matches(','))
        .unwrap_or_else(|| panic!("{field} is not a hex literal: {line}"));
    let page = u32::from_str_radix(hex, 16).expect("a hex page address");
    Region {
        name: field.to_string(),
        range: Range::new(page, page + 0x1000),
    }
}

fn region(name: &str) -> Region {
    regions_from_memory_x(MEMORY_X, &[name])
        .pop()
        .unwrap_or_else(|| panic!("memory.x has no {name} region"))
}

/// The library's list (SoftDevice, application, calendar ring, boot record,
/// the band above the window) plus, named, the three pages that make a board
/// itself. Not `STORE`: see `the_staging_window_is_the_top_of_the_record_log`.
fn protected() -> Vec<Region> {
    let mut out = protected_regions(MEMORY_X, &RAK4631_0_4_3);
    assert_eq!(
        out.len(),
        5,
        "FLASH, TIME and BOOT from memory.x, the SoftDevice, the band: {out:?}"
    );
    for field in [
        "identity_flash_page",
        "radio_config_flash_page",
        "telemetry_flash_page",
    ] {
        out.push(board_page(field));
    }
    out
}

#[test]
fn the_file_covers_mbr_bootloader_config_and_uicr_and_nothing_else() {
    let image = image();
    assert_eq!(image.blocks.len(), 136);
    assert_eq!(image.family_id(), Some(uf2::FAMILY_NRF52_BOOTLOADER));
    let spans: Vec<(u32, u32)> = image.spans();
    assert_eq!(
        spans,
        vec![
            (0x0, 0xB00),
            (0xF_4000, 0xF_BB00),
            (0xF_D800, 0xF_D900),
            (0x1000_1000, 0x1000_1100),
        ],
        "MBR, bootloader code, bootloader config page, UICR"
    );
}

#[test]
fn the_real_update_is_accepted_and_names_its_version() {
    let plan = plan(&image(), &RAK4631_0_4_3, &protected()).expect("the 0.4.4 update plans");
    assert_eq!(plan.version.as_deref(), Some("0.4.4"));
    assert_eq!(
        plan.skipped,
        vec![Range::new(0x0, 0xB00)],
        "the MBR blocks, which the running bootloader skips"
    );
    assert_eq!(plan.installed, Range::new(0xF_4000, 0xF_E000));
}

#[test]
fn the_staging_window_is_the_top_of_the_record_log() {
    let plan = plan(&image(), &RAK4631_0_4_3, &protected()).expect("the 0.4.4 update plans");
    assert_eq!(
        plan.staged,
        vec![
            Range::new(0xE_0000, 0xE_7B00),
            Range::new(0xE_9800, 0xE_9900)
        ]
    );
    assert_eq!(
        plan.staged_pages,
        vec![
            Range::new(0xE_0000, 0xE_8000),
            Range::new(0xE_9000, 0xE_A000)
        ],
        "eight pages for the code, one for the config page"
    );
    let store = region("STORE");
    assert_eq!(store.range, Range::new(0xD_A000, 0xE_A000));
    assert_eq!(
        plan.pages_in(&store.range),
        plan.staged_pages,
        "every staged page is a record-log page: 6 to 13 and 15 of 16"
    );
}

#[test]
fn identity_radio_and_telemetry_pages_are_untouched_at_every_step() {
    let plan = plan(&image(), &RAK4631_0_4_3, &protected()).expect("the 0.4.4 update plans");
    for field in [
        "identity_flash_page",
        "radio_config_flash_page",
        "telemetry_flash_page",
    ] {
        let page = board_page(field).range;
        assert!(
            Range::new(RAK4631_0_4_3.user_flash_end, RAK4631_0_4_3.boot_start).overlaps(&page),
            "{field} {page} sits in the band no UF2 writes"
        );
        for staged in &plan.staged_pages {
            assert!(!staged.overlaps(&page), "{field} {page} vs staged {staged}");
        }
        assert!(!plan.installed.overlaps(&page), "{field} vs the MBR copy");
    }
    // The copy stops below the MBR parameter page and the bootloader
    // settings page above it.
    assert!(plan.installed.end <= 0xF_E000);
}

#[test]
fn protecting_the_record_log_refuses_the_real_update() {
    let mut protect = protected();
    protect.push(region("STORE"));
    match plan(&image(), &RAK4631_0_4_3, &protect) {
        Err(Refusal::Protected { name, pages, .. }) => {
            assert_eq!(name, "STORE");
            assert_eq!(pages, Range::new(0xE_0000, 0xE_8000));
        }
        other => panic!("expected a refusal naming STORE, got {other:?}"),
    }
}

fn with_block(addr: u32, family: u32) -> Image {
    let mut image = image();
    image.blocks.push(Block {
        flags: uf2::FLAG_FAMILY_ID,
        target_addr: addr,
        block_no: 0,
        num_blocks: 0,
        family_id: family,
        data: vec![0xA5; 256],
    });
    image.renumber();
    image
}

#[test]
fn a_block_on_the_identity_page_is_refused() {
    let identity = board_page("identity_flash_page").range.start;
    let image = with_block(identity, uf2::FAMILY_NRF52_BOOTLOADER);
    assert_eq!(
        plan(&image, &RAK4631_0_4_3, &protected()),
        Err(Refusal::Outside {
            addr: identity,
            region: Range::new(0xF_4000, 0xF_E000)
        })
    );
}

#[test]
fn a_bundled_softdevice_is_refused() {
    let image = with_block(0x1000, uf2::FAMILY_NRF52_BOOTLOADER);
    assert!(matches!(
        plan(&image, &RAK4631_0_4_3, &protected()),
        Err(Refusal::Outside { addr: 0x1000, .. })
    ));
}

#[test]
fn an_application_image_is_not_a_bootloader_update() {
    let image = Image {
        blocks: vec![Block {
            flags: uf2::FLAG_FAMILY_ID,
            target_addr: 0x2_7000,
            block_no: 0,
            num_blocks: 1,
            family_id: uf2::FAMILY_NRF52840_APP,
            data: vec![0; 256],
        }],
    };
    assert_eq!(
        plan(&image, &RAK4631_0_4_3, &protected()),
        Err(Refusal::NotBootloaderFamily)
    );
}

#[test]
fn a_uicr_naming_another_bootloader_start_is_refused() {
    let mut image = image();
    let uicr = image
        .blocks
        .iter_mut()
        .find(|b| b.target_addr == UICR_BLOCK)
        .expect("the update carries its UICR block");
    uicr.data[0x14..0x18].copy_from_slice(&0xF_3000u32.to_le_bytes());
    assert_eq!(
        plan(&image, &RAK4631_0_4_3, &protected()),
        Err(Refusal::UicrMismatch {
            boot: 0xF_3000,
            mbr: 0xF_E000,
            want_boot: 0xF_4000,
            want_mbr: 0xF_E000
        })
    );
}

#[test]
fn an_update_without_its_uicr_block_is_refused() {
    let mut image = image();
    image.blocks.retain(|b| b.target_addr != UICR_BLOCK);
    image.renumber();
    assert_eq!(
        plan(&image, &RAK4631_0_4_3, &protected()),
        Err(Refusal::NoUicr)
    );
}

#[test]
fn a_receiver_whose_staging_reaches_the_identity_is_refused() {
    // A bootloader with a smaller reserved band (USER_FLASH_END 0xF0000)
    // stages fourteen pages lower instead of twenty, and the same image's
    // code then lands on 0xE6000..0xEDB00, across the identity page. The
    // check reads the receiver, not only the file.
    let receiver = Receiver {
        user_flash_end: 0xF_0000,
        ..RAK4631_0_4_3
    };
    let protect: Vec<Region> = protected()
        .into_iter()
        .filter(|r| r.name != "app data band")
        .collect();
    match plan(&image(), &receiver, &protect) {
        Err(Refusal::Protected { name, .. }) => assert!(
            name.ends_with("_flash_page"),
            "refused for {name}, expected one of the board's own pages"
        ),
        other => panic!("expected a refusal on a board page, got {other:?}"),
    }
}

// ------------------------------------------------- the SWD way back, pinned

const HEX_0_4_4: &str =
    include_str!("fixtures/bootloader/rak4631/wiscore_rak4631_board_bootloader-0.4.4.hex");
const HEX_0_4_3: &str =
    include_str!("fixtures/bootloader/rak4631/wiscore_rak4631_board_bootloader-0.4.3.hex");

fn hex_spans(text: &str) -> Vec<lnflash::ihex::Span> {
    lnflash::ihex::parse(text).expect("a vendored bootloader hex parses")
}

#[test]
fn the_recovery_hex_is_the_bootloader_the_uf2_installs() {
    // The bootloader-only hex the SWD recovery writes must be byte for byte
    // what the UF2 stages, or the way back would be a different bootloader.
    let spans = hex_spans(HEX_0_4_4);
    let ranges: Vec<(u32, u32)> = spans.iter().map(|s| (s.start, s.end())).collect();
    assert_eq!(
        ranges,
        vec![
            (0xF_4000, 0xF_BAB0),
            (0xF_D800, 0xF_D858),
            (0x1000_1014, 0x1000_101C)
        ],
        "bootloader code, config page, the two UICR words: no MBR, no SoftDevice"
    );
    let image = image();
    for span in &spans[..2] {
        for (i, byte) in span.data.iter().enumerate() {
            let addr = span.start + i as u32;
            let got = image
                .read_at(addr, 1)
                .expect("the UF2 covers every hex byte");
            assert_eq!(got[0], *byte, "byte at {addr:#x}");
        }
    }
    let uicr = &spans[2].data;
    assert_eq!(&uicr[0..4], &0xF_4000u32.to_le_bytes());
    assert_eq!(&uicr[4..8], &0xF_E000u32.to_le_bytes());
}

#[test]
fn the_way_back_to_0_4_3_has_the_same_layout() {
    let spans = hex_spans(HEX_0_4_3);
    let ranges: Vec<(u32, u32)> = spans.iter().map(|s| (s.start, s.end())).collect();
    assert_eq!(
        ranges,
        vec![
            (0xF_4000, 0xF_BFE8),
            (0xF_D800, 0xF_D858),
            (0x1000_1014, 0x1000_101C)
        ]
    );
    assert_eq!(
        spans[2].data,
        hex_spans(HEX_0_4_4)[2].data,
        "same UICR words"
    );
    let text: Vec<u8> = spans[0].data.clone();
    let needle = b"UF2 Bootloader 0.4.3";
    assert!(
        text.windows(needle.len()).any(|w| w == needle),
        "the 0.4.3 hex is the build the rig RAK reports"
    );
}
