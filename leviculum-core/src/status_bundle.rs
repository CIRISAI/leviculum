//! The `/status` bundle a transport instance answers `rnstatus -R` with
//! (Codeberg #235).
//!
//! A remote status query is `rnstatus -R <transport hash> -i <identity>`: the
//! client links to `rnstransport.remote.management`, identifies, and issues a
//! `/status` request. The response Python builds is
//! `[get_interface_stats()]`, plus `get_link_count()` when the request asked
//! for link stats (`Transport.remote_status_handler`, `Transport.py:2814`).
//!
//! `leviculum-std` builds that bundle from its own interface inventory
//! (`rpc::handlers::build_interface_stats`), which is a different world: a
//! `BTreeMap` of listeners and their spawned children, IFAC configs, RNode
//! radio rows, `serde_pickle` values. None of it exists on a board, and none
//! of it is `no_std`. This module is the board's builder — the same wire
//! format, assembled from a slice the caller fills.
//!
//! # What the reference reads
//!
//! The vendored `rnstatus` reads these keys **without guarding** them, so an
//! answer that omits one raises `KeyError` in the reference tool and the query
//! fails — Priority 1, compatibility clause:
//!
//! | key | reader |
//! |-----|--------|
//! | `interfaces` (top level) | `rnstatus.py:361` |
//! | `name` | `rnstatus.py:391`, `456` |
//! | `status` | `rnstatus.py:418` |
//! | `mode` | `rnstatus.py:421` |
//! | `clients` | `rnstatus.py:428` |
//! | `rxb` | `rnstatus.py:575` |
//! | `txb` | `rnstatus.py:576` |
//! | `rxb`/`txb`/`rxs`/`txs` (top level) | `rnstatus.py:648-657`, under `-t` |
//!
//! And these it guards with `in`, so they are optional but carry real
//! information when present: `bitrate` (`rnstatus.py:471`), per-interface
//! `rxs`/`txs` (`rnstatus.py:634`), `transport_id` (`rnstatus.py:660`),
//! `transport_uptime` (`rnstatus.py:667`).
//!
//! Everything else the reference can print — announce and path-request
//! frequencies, burst state, IFAC signatures, airtime, I2P tunnel state — is
//! guarded the same way and is deliberately absent: a board that has never
//! measured a frequency sample would have to invent one, and an invented
//! number is worse than a missing line. The bundle carries what the board
//! honestly knows.
//!
//! Key order follows `Reticulum.get_interface_stats`
//! (`reference/Reticulum/RNS/Reticulum.py:1326-1515`) for the subset emitted,
//! so the golden in `tests` is a transcription of the reference rather than a
//! second design.

use alloc::vec::Vec;

use crate::msgpack::{
    read_array_len, read_bool, write_array_header, write_bin, write_bool, write_fixarray_header,
    write_fixmap_header, write_float64, write_nil, write_str, write_uint,
};

/// One interface row of the bundle.
///
/// Borrowed throughout: a firmware caller builds this on the stack from its
/// interface objects and its byte counters, and nothing here allocates until
/// [`encode_status`] runs.
pub struct InterfaceStatus<'a> {
    /// Displayed as the interface's heading (`rnstatus.py:456`).
    pub name: &'a str,
    /// `true` -> "Status: Up" (`rnstatus.py:418`).
    pub status: bool,
    /// `Interface.MODE_*`; `leviculum_core::traits::InterfaceMode::as_u8`
    /// already carries the reference's numbering (`Interface.py:45-50`).
    pub mode: u8,
    /// Connected clients, `None` for an interface that has no such notion.
    /// Read unguarded by the reference, so it is always emitted — as nil when
    /// absent, which is what Python puts there (`Reticulum.py:1340`).
    pub clients: Option<u64>,
    /// Bytes received on this interface since boot.
    pub rxb: u64,
    /// Bytes transmitted on this interface since boot.
    pub txb: u64,
    /// Current receive speed in bytes/s over the emitter's sampling window.
    pub rxs: f64,
    /// Current transmit speed in bytes/s over the emitter's sampling window.
    pub txs: f64,
    /// On-air / link bitrate in bits/s, `None` when the medium has no
    /// meaningful figure.
    pub bitrate: Option<u64>,
}

/// The whole `/status` answer: the interface rows plus the transport-level
/// fields the reference prints under them.
pub struct StatusBundle<'a> {
    /// One row per interface. Totals are summed from these rows, so an
    /// interface that is in the list is in the totals by construction.
    pub interfaces: &'a [InterfaceStatus<'a>],
    /// The transport identity hash (`Identity::hash`), or `None` on a node
    /// that is not a transport instance. Python omits the whole
    /// transport block in that case (`Reticulum.py:1500`).
    pub transport_id: Option<&'a [u8]>,
    /// Seconds since this node started.
    pub transport_uptime: f64,
}

/// Number of keys in one interface map. Stays ≤ 15 so the fixmap header
/// holds; a 16th key needs `map16`, which the crate's writer does not have.
const INTERFACE_KEYS: u8 = 9;

/// Number of keys in the top-level map with `transport_id` present.
const TOP_KEYS_WITH_TRANSPORT: u8 = 7;

impl StatusBundle<'_> {
    /// Sum of the per-interface byte and speed counters.
    ///
    /// Python keeps separate transport-level totals
    /// (`RNS.Transport.traffic_rxb`), which on a board would be a second set
    /// of counters to keep in step with the first. Summing the rows instead
    /// means the totals cannot disagree with the interfaces printed above
    /// them.
    fn totals(&self) -> (u64, u64, f64, f64) {
        let mut rxb = 0u64;
        let mut txb = 0u64;
        let mut rxs = 0.0;
        let mut txs = 0.0;
        for iface in self.interfaces {
            rxb = rxb.saturating_add(iface.rxb);
            txb = txb.saturating_add(iface.txb);
            rxs += iface.rxs;
            txs += iface.txs;
        }
        (rxb, txb, rxs, txs)
    }
}

/// Whether a `/status` request asked for link stats (`rnstatus -R -l`).
///
/// Mirrors Python `Transport.remote_status_handler`: the payload is
/// `[include_lstats]`, and only a list whose first element is literally
/// `True` turns them on. Nil, an empty list, a non-list and a non-bool first
/// element all mean no.
pub fn request_wants_link_stats(data: &[u8]) -> bool {
    let mut pos = 0;
    let Some(len) = read_array_len(data, &mut pos) else {
        return false;
    };
    if len == 0 {
        return false;
    }
    read_bool(data, &mut pos).unwrap_or(false)
}

/// Encode the stats map alone — the value Python's `get_interface_stats()`
/// returns, msgpack-packed.
pub fn encode_status(bundle: &StatusBundle) -> Vec<u8> {
    let mut buf = Vec::new();
    write_status(&mut buf, bundle);
    buf
}

/// Encode the full `/status` response: `[stats]`, or `[stats, link_count]`
/// when the requester asked for link stats.
///
/// `rnstatus` reads `response[0]` as the stats and `response[1]`, if present,
/// as the link count (`rnstatus.py:108-113`).
pub fn encode_status_response(bundle: &StatusBundle, link_count: Option<u64>) -> Vec<u8> {
    let mut buf = Vec::new();
    write_fixarray_header(&mut buf, if link_count.is_some() { 2 } else { 1 });
    write_status(&mut buf, bundle);
    if let Some(count) = link_count {
        write_uint(&mut buf, count);
    }
    buf
}

fn write_status(buf: &mut Vec<u8>, bundle: &StatusBundle) {
    let (rxb, txb, rxs, txs) = bundle.totals();
    let keys = if bundle.transport_id.is_some() {
        TOP_KEYS_WITH_TRANSPORT
    } else {
        TOP_KEYS_WITH_TRANSPORT - 2
    };
    write_fixmap_header(buf, keys);

    write_str(buf, "interfaces");
    // Not the fixarray writer: the row count comes from the caller's slice,
    // and a board that grows a 16th interface must widen the header rather
    // than emit a frame the reference cannot parse.
    write_array_header(buf, bundle.interfaces.len());
    for iface in bundle.interfaces {
        write_interface(buf, iface);
    }

    write_str(buf, "rxb");
    write_uint(buf, rxb);
    write_str(buf, "txb");
    write_uint(buf, txb);
    write_str(buf, "rxs");
    write_float64(buf, rxs);
    write_str(buf, "txs");
    write_float64(buf, txs);

    // Python emits the transport block only for a transport instance
    // (`Reticulum.py:1500`), and `rnstatus` guards every key in it, so
    // leaving it out on a non-transport node is the reference's own shape.
    if let Some(transport_id) = bundle.transport_id {
        write_str(buf, "transport_id");
        write_bin(buf, transport_id);
        write_str(buf, "transport_uptime");
        write_float64(buf, bundle.transport_uptime);
    }
}

fn write_interface(buf: &mut Vec<u8>, iface: &InterfaceStatus) {
    write_fixmap_header(buf, INTERFACE_KEYS);

    write_str(buf, "clients");
    match iface.clients {
        Some(clients) => write_uint(buf, clients),
        None => write_nil(buf),
    }
    write_str(buf, "bitrate");
    match iface.bitrate {
        Some(bitrate) => write_uint(buf, bitrate),
        None => write_nil(buf),
    }
    write_str(buf, "rxs");
    write_float64(buf, iface.rxs);
    write_str(buf, "txs");
    write_float64(buf, iface.txs);
    write_str(buf, "name");
    write_str(buf, iface.name);
    write_str(buf, "rxb");
    write_uint(buf, iface.rxb);
    write_str(buf, "txb");
    write_uint(buf, iface.txb);
    write_str(buf, "status");
    write_bool(buf, iface.status);
    write_str(buf, "mode");
    write_uint(buf, u64::from(iface.mode));
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::String;

    /// The fixture the golden below was generated from. Kept in one place so
    /// the generator script and the test cannot drift.
    fn fixture() -> ([InterfaceStatus<'static>; 3], [u8; 16]) {
        let interfaces = [
            InterfaceStatus {
                name: "serial_usb",
                status: true,
                mode: 6,
                clients: None,
                rxb: 4096,
                txb: 1234,
                rxs: 0.0,
                txs: 0.0,
                bitrate: Some(1_000_000),
            },
            InterfaceStatus {
                name: "lora_sx1262",
                status: true,
                mode: 6,
                clients: None,
                rxb: 98765,
                txb: 43210,
                rxs: 12.5,
                txs: 3.25,
                bitrate: Some(3125),
            },
            InterfaceStatus {
                name: "ble",
                status: false,
                mode: 6,
                clients: Some(2),
                rxb: 0,
                txb: 0,
                rxs: 0.0,
                txs: 0.0,
                bitrate: None,
            },
        ];
        let transport_id = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        (interfaces, transport_id)
    }

    fn hex(bytes: &[u8]) -> String {
        let mut out = String::new();
        for b in bytes {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }

    /// The reference packer's own bytes for [`fixture`], as
    /// `RNS.vendor.umsgpack.packb` produces them. Regenerate with
    /// `PYTHONPATH=reference/Reticulum python3
    /// leviculum-core/tests/status_bundle_golden_gen.py`.
    const GOLDEN_STATUS_HEX: &str = concat!(
        "87aa696e74657266616365739389a7636c69656e7473c0a762697472617465ce000f4240",
        "a3727873cb0000000000000000a3747873cb0000000000000000a46e616d65aa73657269",
        "616c5f757362a3727862cd1000a3747862cd04d2a6737461747573c3a46d6f64650689a7",
        "636c69656e7473c0a762697472617465cd0c35a3727873cb4029000000000000a3747873",
        "cb400a000000000000a46e616d65ab6c6f72615f737831323632a3727862ce000181cda3",
        "747862cda8caa6737461747573c3a46d6f64650689a7636c69656e747302a76269747261",
        "7465c0a3727873cb0000000000000000a3747873cb0000000000000000a46e616d65a362",
        "6c65a372786200a374786200a6737461747573c2a46d6f646506a3727862ce000191cda3",
        "747862cdad9ca3727873cb4029000000000000a3747873cb400a000000000000ac747261",
        "6e73706f72745f6964c41000112233445566778899aabbccddeeffb07472616e73706f72",
        "745f757074696d65cb40ac210000000000",
    );

    /// Byte-for-byte against what the reference's own packer produces for the
    /// same dict.
    ///
    /// Byte equality is the stronger claim: a bundle that equals
    /// `umsgpack.packb(...)` output decodes to exactly the dict the reference
    /// packed, so a separate "does it decode" test would add nothing.
    #[test]
    fn status_bundle_matches_the_reference_packer() {
        let (interfaces, transport_id) = fixture();
        let bundle = StatusBundle {
            interfaces: &interfaces,
            transport_id: Some(&transport_id),
            transport_uptime: 3600.5,
        };
        assert_eq!(hex(&encode_status(&bundle)), GOLDEN_STATUS_HEX);
    }

    /// Only `[true, …]` asks for link stats; everything else, including the
    /// nil an argument-less request carries, does not.
    #[test]
    fn link_stats_are_requested_only_by_a_leading_true() {
        assert!(request_wants_link_stats(&[0x91, 0xc3]));
        assert!(request_wants_link_stats(&[0x92, 0xc3, 0xc2]));
        assert!(!request_wants_link_stats(&[0x91, 0xc2]));
        assert!(!request_wants_link_stats(&[0x90]));
        assert!(!request_wants_link_stats(&[0xc0]));
        assert!(!request_wants_link_stats(&[]));
        // A first element that is an integer, not a bool.
        assert!(!request_wants_link_stats(&[0x91, 0x01]));
    }

    /// The `/status` response wrapper: `[stats]` without link stats,
    /// `[stats, n]` with them (`rnstatus.py:108-113`).
    #[test]
    fn response_wraps_the_stats_in_the_list_the_reference_unpacks() {
        let (interfaces, transport_id) = fixture();
        let bundle = StatusBundle {
            interfaces: &interfaces,
            transport_id: Some(&transport_id),
            transport_uptime: 3600.5,
        };
        let stats = encode_status(&bundle);

        let plain = encode_status_response(&bundle, None);
        assert_eq!(plain[0], 0x91, "one-element fixarray");
        assert_eq!(&plain[1..], &stats[..]);

        let with_links = encode_status_response(&bundle, Some(3));
        assert_eq!(with_links[0], 0x92, "two-element fixarray");
        assert_eq!(&with_links[1..with_links.len() - 1], &stats[..]);
        assert_eq!(*with_links.last().expect("non-empty"), 0x03);
    }

    /// The totals are the sum of the rows above them, not a second counter.
    #[test]
    fn totals_are_the_sum_of_the_interface_rows() {
        let (interfaces, _) = fixture();
        let bundle = StatusBundle {
            interfaces: &interfaces,
            transport_id: None,
            transport_uptime: 0.0,
        };
        let (rxb, txb, rxs, txs) = bundle.totals();
        assert_eq!(rxb, 4096 + 98765);
        assert_eq!(txb, 1234 + 43210);
        assert_eq!(rxs, 12.5);
        assert_eq!(txs, 3.25);
    }

    /// A node with no transport identity drops the two transport keys and
    /// says so in its map header, as Python does (`Reticulum.py:1500`).
    #[test]
    fn a_non_transport_node_omits_the_transport_block() {
        let (interfaces, _) = fixture();
        let bundle = StatusBundle {
            interfaces: &interfaces,
            transport_id: None,
            transport_uptime: 0.0,
        };
        let encoded = encode_status(&bundle);
        assert_eq!(encoded[0], 0x80 | 5, "five-key fixmap");
        assert!(!hex(&encoded).contains(&hex(b"transport_id")));
    }
}
