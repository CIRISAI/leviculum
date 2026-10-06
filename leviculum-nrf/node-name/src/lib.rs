#![no_std]
//! The board's node names and the rule that says the BLE surfaces are one
//! reset behind (Codeberg #235, #413).
//!
//! Moved out of `leviculum-nrf/src/name.rs` so a host test can state what
//! an unnamed board is called on each surface and when
//! `NODE_NAME_FLAG_BLE_PENDING` is raised. Every function here takes the
//! state the firmware keeps in its statics as arguments: the identity hash,
//! the operator's name if one is set, and the BLE name this boot came up
//! on. The flash read and the statics stay in `name.rs`, which calls in
//! here and re-exports these items. Policy:
//! `docs/src/concepts/firmware-host-test-seam.md`.
//!
//! The two derived defaults are different strings, not one string and its
//! truncation: the mesh name is `LNode-<hex8>` and the BLE name
//! `LN-<hex8>` ([`leviculum_ble_tx::device_name`]), because eleven bytes
//! of `LNode-<hex8>` would drop half the hex that makes the board
//! identifiable. That is why the pending rule compares BLE name with BLE
//! name and never the mesh name with the BLE name.

use leviculum_ble_tx::gap_name;
use leviculum_core::envelope::{NODE_NAME_FLAG_BLE_PENDING, NODE_NAME_FLAG_STORED};
use leviculum_core::node_name::NodeName;

/// The mesh display name in force: the operator's name, or the derived
/// `LNode-<hex8>`. This is what the next announce carries.
///
/// The derived form is built here rather than in the firmware's
/// `telemetry::announce_app_data` so that the announce path and the
/// control-frame report cannot disagree about what an unnamed board is
/// called.
pub fn mesh_name_for(identity_hash: &[u8; 16], configured: Option<NodeName>) -> NodeName {
    match configured {
        Some(name) => name,
        None => {
            let mut derived = *b"LNode-00000000";
            write_hex8(&mut derived[6..], identity_hash);
            // Ten graphic ASCII bytes at most — inside every bound
            // `decode` checks — so the fallback arm is unreachable.
            NodeName::decode(&derived).unwrap_or(NodeName::EMPTY)
        }
    }
}

/// The BLE name a boot with `configured` comes up on: the GAP device name
/// and the advertisement's Complete Local Name, as
/// [`leviculum_ble_tx::gap_name`] builds them.
///
/// Called with the name configured right now, this is the name a reset
/// would bring up, which [`report_for`] compares against the name this
/// boot came up on to decide `NODE_NAME_FLAG_BLE_PENDING`.
pub fn gap_name_for(identity_hash: &[u8; 16], configured: Option<&NodeName>) -> NodeName {
    let (buf, len) = gap_name(identity_hash, configured.map(NodeName::as_str));
    NodeName::decode(&buf[..len]).unwrap_or(NodeName::EMPTY)
}

/// Everything a
/// [`TYPE_NODE_NAME_REPORT`](leviculum_core::envelope::TYPE_NODE_NAME_REPORT)
/// carries, given the board's state: the flags, the mesh name in force,
/// and `boot_gap`, the BLE name on the air.
///
/// `BLE_PENDING` compares the name BLE came up on against the one it
/// would come up on now — never the mesh name against the BLE name, which
/// differ both by truncation and by having two unrelated derived
/// defaults, and would report a freshly cleared board as pending forever.
pub fn report_for(
    identity_hash: &[u8; 16],
    configured: Option<NodeName>,
    boot_gap: NodeName,
) -> (u8, NodeName, NodeName) {
    let stored = if configured.is_some() {
        NODE_NAME_FLAG_STORED
    } else {
        0
    };
    let pending = if boot_gap == gap_name_for(identity_hash, configured.as_ref()) {
        0
    } else {
        NODE_NAME_FLAG_BLE_PENDING
    };
    (
        stored | pending,
        mesh_name_for(identity_hash, configured),
        boot_gap,
    )
}

/// Write the leading four bytes of `hash` as eight lowercase hex digits.
fn write_hex8(out: &mut [u8], hash: &[u8; 16]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, byte) in hash[..4].iter().enumerate() {
        out[2 * i] = HEX[usize::from(byte >> 4)];
        out[2 * i + 1] = HEX[usize::from(byte & 0x0F)];
    }
}
