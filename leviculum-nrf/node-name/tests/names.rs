//! The node names and the BLE-pending flag, asserted on the host
//! (Codeberg #235, #413).
//!
//! "The board is one reset behind" was read off captures by hand in every
//! BLE room since September: the `[NAME ]` line and the control frame's
//! report carry the mesh name, the BLE name on the air and the flags, and
//! whether those agree was checked by eye. Here each state an operator can
//! reach (unnamed, named after boot, named and reset, cleared after boot,
//! named longer than BLE can carry) is one test against a fixed identity.
//!
//! The fixed identity's hash starts `e1 9b 2b 38`, so its derived names are
//! the `LNode-e19b2b38` / `LN-e19b2b38` pair the captures show.
//!
//! Run: `cargo test -p leviculum-node-name --test names -- --nocapture`

use leviculum_ble_tx::{DEVICE_NAME_LEN, GAP_DEVNAME_DEFAULT_LEN};
use leviculum_core::envelope::{NODE_NAME_FLAG_BLE_PENDING, NODE_NAME_FLAG_STORED};
use leviculum_core::node_name::{NodeName, BLE_NAME_MAX_LEN};
use leviculum_node_name::{gap_name_for, mesh_name_for, report_for};

/// The fixed identity. Only the leading four bytes reach a name; the rest
/// is deliberately not zero so a derivation that read past them would show.
const IDENTITY: [u8; 16] = [
    0xe1, 0x9b, 0x2b, 0x38, 0x5c, 0x07, 0xd2, 0x41, 0x9a, 0x6e, 0x13, 0xf0, 0x88, 0x24, 0xb7, 0x55,
];

fn name(text: &str) -> NodeName {
    NodeName::parse(text.as_bytes()).expect("a valid operator name")
}

/// The BLE name the boot came up on, as `note_boot_name` records it.
fn booted_with(configured: Option<&NodeName>) -> NodeName {
    gap_name_for(&IDENTITY, configured)
}

#[test]
fn the_fixed_identity_derives_the_names_the_captures_show() {
    let mesh = mesh_name_for(&IDENTITY, None);
    let ble = gap_name_for(&IDENTITY, None);
    println!("mesh={mesh} ble={ble}");
    assert_eq!(mesh.as_str(), "LNode-e19b2b38");
    assert_eq!(ble.as_str(), "LN-e19b2b38");
    // The derived BLE name fills the limit exactly, by construction.
    assert_eq!(ble.len(), DEVICE_NAME_LEN);
}

#[test]
fn only_the_leading_four_bytes_reach_a_derived_name() {
    let mut other = IDENTITY;
    for byte in &mut other[4..] {
        *byte = !*byte;
    }
    assert_eq!(mesh_name_for(&other, None), mesh_name_for(&IDENTITY, None));
    assert_eq!(gap_name_for(&other, None), gap_name_for(&IDENTITY, None));
}

#[test]
fn every_nibble_renders_as_lowercase_hex_in_both_names() {
    let mut hash = [0u8; 16];
    hash[..4].copy_from_slice(&[0x01, 0x23, 0xab, 0xef]);
    assert_eq!(mesh_name_for(&hash, None).as_str(), "LNode-0123abef");
    assert_eq!(gap_name_for(&hash, None).as_str(), "LN-0123abef");
    hash[..4].copy_from_slice(&[0x45, 0x67, 0x89, 0xcd]);
    assert_eq!(mesh_name_for(&hash, None).as_str(), "LNode-456789cd");
    assert_eq!(gap_name_for(&hash, None).as_str(), "LN-456789cd");
}

#[test]
fn an_operator_name_overrides_both_derived_names() {
    let set = name("Balkon-Nord");
    assert_eq!(mesh_name_for(&IDENTITY, Some(set)).as_str(), "Balkon-Nord");
    assert_eq!(gap_name_for(&IDENTITY, Some(&set)).as_str(), "Balkon-Nord");
}

#[test]
fn an_unnamed_board_that_booted_unnamed_is_not_pending() {
    let (flags, mesh, ble) = report_for(&IDENTITY, None, booted_with(None));
    assert_eq!(flags, 0);
    assert_eq!(mesh.as_str(), "LNode-e19b2b38");
    assert_eq!(ble.as_str(), "LN-e19b2b38");
    // The two names differ and the flag is still clear: the rule compares
    // BLE with BLE, or a freshly cleared board would be pending forever.
    assert_ne!(mesh, ble);
}

#[test]
fn a_name_set_after_boot_is_one_reset_behind_on_ble() {
    let set = name("Balkon-Nord");
    let (flags, mesh, ble) = report_for(&IDENTITY, Some(set), booted_with(None));
    assert_eq!(flags, NODE_NAME_FLAG_STORED | NODE_NAME_FLAG_BLE_PENDING);
    // The mesh adopts the name at once, BLE still carries the boot's name.
    assert_eq!(mesh.as_str(), "Balkon-Nord");
    assert_eq!(ble.as_str(), "LN-e19b2b38");
}

#[test]
fn the_reset_after_naming_clears_the_pending_flag() {
    let set = name("Balkon-Nord");
    let (flags, mesh, ble) = report_for(&IDENTITY, Some(set), booted_with(Some(&set)));
    assert_eq!(flags, NODE_NAME_FLAG_STORED);
    assert_eq!(mesh.as_str(), "Balkon-Nord");
    assert_eq!(ble.as_str(), "Balkon-Nord");
}

#[test]
fn a_name_cleared_after_boot_is_pending_until_the_reset() {
    let set = name("Balkon-Nord");
    let (flags, mesh, ble) = report_for(&IDENTITY, None, booted_with(Some(&set)));
    assert_eq!(flags, NODE_NAME_FLAG_BLE_PENDING);
    assert_eq!(mesh.as_str(), "LNode-e19b2b38");
    assert_eq!(ble.as_str(), "Balkon-Nord");

    let (flags, _, ble) = report_for(&IDENTITY, None, booted_with(None));
    assert_eq!(flags, 0);
    assert_eq!(ble.as_str(), "LN-e19b2b38");
}

#[test]
fn a_rename_between_two_names_is_pending() {
    let before = name("Balkon-Nord");
    let after = name("Keller");
    let (flags, mesh, ble) = report_for(&IDENTITY, Some(after), booted_with(Some(&before)));
    assert_eq!(flags, NODE_NAME_FLAG_STORED | NODE_NAME_FLAG_BLE_PENDING);
    assert_eq!(mesh.as_str(), "Keller");
    assert_eq!(ble.as_str(), "Balkon-Nord");
}

/// The limit the BLE name is cut at: [`DEVICE_NAME_LEN`], 11 bytes
/// (`leviculum-nrf/ble-tx/src/lib.rs:143`).
///
/// The SoftDevice imposes the ceiling it sits under, not the 11 itself.
/// The GAP device name is reserved in the SoftDevice's own attribute table
/// (`max_len: DEVICE_NAME_LEN`, `vloc` `BLE_GATTS_VLOC_STACK`,
/// `leviculum-nrf/src/ble/mod.rs:849`), which by default has room for
/// `BLE_GAP_DEVNAME_DEFAULT_LEN` = 31 bytes (`nrf-softdevice-s140`
/// bindings at rev `5949a5b`, `src/bindings.rs:639`, mirrored as
/// [`GAP_DEVNAME_DEFAULT_LEN`]); the advertisement side is bounded by the
/// 31-byte legacy scan-response PDU. Eleven is ours, chosen under both.
#[test]
fn the_ble_name_limit_is_eleven_bytes_under_the_softdevice_ceiling() {
    assert_eq!(DEVICE_NAME_LEN, 11);
    assert_eq!(BLE_NAME_MAX_LEN, DEVICE_NAME_LEN);
    assert_eq!(GAP_DEVNAME_DEFAULT_LEN, 31);
    const _: () = assert!(DEVICE_NAME_LEN <= GAP_DEVNAME_DEFAULT_LEN);
}

#[test]
fn a_name_at_the_limit_reaches_ble_whole() {
    let set = name("Balkon-Nord");
    assert_eq!(set.len(), DEVICE_NAME_LEN);
    assert_eq!(gap_name_for(&IDENTITY, Some(&set)), set);
}

#[test]
fn a_name_past_the_limit_reaches_ble_as_its_prefix() {
    let set = name("Balkon-Nordseite");
    assert_eq!(gap_name_for(&IDENTITY, Some(&set)).as_str(), "Balkon-Nord");
    // The mesh carries the whole name.
    assert_eq!(
        mesh_name_for(&IDENTITY, Some(set)).as_str(),
        "Balkon-Nordseite"
    );

    let one_over = name("Balkon-Nordx");
    assert_eq!(one_over.len(), DEVICE_NAME_LEN + 1);
    assert_eq!(
        gap_name_for(&IDENTITY, Some(&one_over)).as_str(),
        "Balkon-Nord"
    );
}

#[test]
fn the_cut_never_splits_a_codepoint() {
    // `ö` occupies bytes 11 and 12, so byte 11 is not a boundary and the
    // cut falls back to the ten bytes before it.
    let set = name("Balkon-Norö");
    assert_eq!(set.len(), DEVICE_NAME_LEN + 1);
    assert_eq!(gap_name_for(&IDENTITY, Some(&set)).as_str(), "Balkon-Nor");
}

#[test]
fn the_cut_never_ends_on_whitespace() {
    let set = name("Balkon-Nor Süd");
    assert_eq!(gap_name_for(&IDENTITY, Some(&set)).as_str(), "Balkon-Nor");
}

#[test]
fn a_long_name_after_its_reset_is_not_pending() {
    // Mesh and BLE differ by the truncation, and the flag is clear.
    let set = name("Balkon-Nordseite");
    let (flags, mesh, ble) = report_for(&IDENTITY, Some(set), booted_with(Some(&set)));
    assert_eq!(flags, NODE_NAME_FLAG_STORED);
    assert_eq!(mesh.as_str(), "Balkon-Nordseite");
    assert_eq!(ble.as_str(), "Balkon-Nord");
}

#[test]
fn a_rename_that_keeps_the_ble_prefix_is_not_pending() {
    // A reset would bring BLE up on the same eleven bytes, so there is
    // nothing for the reset to change and the flag says so.
    let before = name("Balkon-Nordseite");
    let after = name("Balkon-Nordwand");
    let (flags, mesh, ble) = report_for(&IDENTITY, Some(after), booted_with(Some(&before)));
    assert_eq!(flags, NODE_NAME_FLAG_STORED);
    assert_eq!(mesh.as_str(), "Balkon-Nordwand");
    assert_eq!(ble.as_str(), "Balkon-Nord");
}
