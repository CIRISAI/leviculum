//! BLE fragmentation and defragmentation for Columba-compatible BLE interfaces.
//!
//! Implements the Columba Protocol v2.2 fragment format. Reticulum packets are
//! split into BLE-MTU-sized fragments with a 5-byte header, and reassembled on
//! the receiving side.
//!
//! # Fragment Format
//!
//! ```text
//! [Type:1][Sequence:2 BE][Total:2 BE][Payload...]
//! ```
//!
//! | Type | Value | Meaning |
//! |------|-------|---------|
//! | LONE | 0x00 | Complete packet in single fragment |
//! | START | 0x01 | First fragment of multi-fragment packet |
//! | CONTINUE | 0x02 | Middle fragment |
//! | END | 0x03 | Last fragment |
//!
//! # Usage
//!
//! ```
//! use leviculum_core::framing::ble::*;
//!
//! // Fragment a packet for sending
//! let packet = b"Hello, Reticulum!";
//! let fragments = fragment_packet(packet, DEFAULT_MTU);
//! assert_eq!(fragments.len(), 1); // fits in one LONE fragment
//!
//! // Defragment received data
//! let mut defrag = BleDefragmenter::new();
//! for frag in &fragments {
//!     if let DefragResult::Complete(data) = defrag.process(frag, 1000) {
//!         assert_eq!(&data, packet);
//!     }
//! }
//! ```

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

// Constants
/// Fragment header size in bytes.
pub const FRAGMENT_HEADER_SIZE: usize = 5;

/// Single-fragment packet (no fragmentation needed).
pub const FRAGMENT_TYPE_LONE: u8 = 0x00;
/// First fragment of a multi-fragment packet.
pub const FRAGMENT_TYPE_START: u8 = 0x01;
/// Middle fragment of a multi-fragment packet.
pub const FRAGMENT_TYPE_CONTINUE: u8 = 0x02;
/// Last fragment of a multi-fragment packet.
pub const FRAGMENT_TYPE_END: u8 = 0x03;

/// BLE 4.0 minimum MTU.
pub const MIN_MTU: usize = 23;
/// Typical negotiated MTU for most devices.
pub const DEFAULT_MTU: usize = 185;
/// BLE 5.0 maximum MTU.
pub const MAX_MTU: usize = 517;
/// ATT protocol header overhead subtracted from MTU.
pub const ATT_HEADER_SIZE: usize = 3;

/// Timeout for incomplete fragment reassembly (milliseconds).
/// Sliding window: reset on each received fragment.
pub const REASSEMBLY_TIMEOUT_MS: u64 = 30_000;
/// Application-level keepalive interval (milliseconds).
pub const KEEPALIVE_INTERVAL_MS: u64 = 15_000;
/// Keepalive packet content (single zero byte).
pub const KEEPALIVE_BYTE: u8 = 0x00;

// Fragmentation (stateless)
/// Maximum payload bytes per fragment for a given BLE MTU.
///
/// Returns 0 if the MTU is too small to carry any payload.
pub const fn payload_per_fragment(mtu: usize) -> usize {
    let overhead = ATT_HEADER_SIZE + FRAGMENT_HEADER_SIZE;
    mtu.saturating_sub(overhead)
}

/// Number of fragments needed to send `data_len` bytes at the given BLE MTU.
///
/// Always returns at least 1 (a zero-length packet produces one LONE fragment).
pub fn fragment_count(data_len: usize, mtu: usize) -> usize {
    let ppf = payload_per_fragment(mtu);
    if ppf == 0 {
        return 1; // degenerate MTU, send empty LONE
    }
    if data_len == 0 {
        return 1;
    }
    data_len.div_ceil(ppf)
}

/// Build the 5-byte header for fragment `index` of `total` fragments.
///
/// The type byte is determined by position:
/// - total == 1: LONE
/// - index == 0: START
/// - index == total - 1: END
/// - otherwise: CONTINUE
pub fn build_fragment_header(index: usize, total: usize) -> [u8; FRAGMENT_HEADER_SIZE] {
    let ftype = if total == 1 {
        FRAGMENT_TYPE_LONE
    } else if index == 0 {
        FRAGMENT_TYPE_START
    } else if index == total - 1 {
        FRAGMENT_TYPE_END
    } else {
        FRAGMENT_TYPE_CONTINUE
    };
    let seq = index as u16;
    let tot = total as u16;
    [
        ftype,
        (seq >> 8) as u8,
        seq as u8,
        (tot >> 8) as u8,
        tot as u8,
    ]
}

/// Get the payload slice for fragment `index` from `data`.
///
/// Returns the byte range of `data` that belongs to this fragment.
pub fn fragment_payload(data: &[u8], index: usize, mtu: usize) -> &[u8] {
    let ppf = payload_per_fragment(mtu);
    if ppf == 0 {
        return &[];
    }
    let start = index * ppf;
    let end = (start + ppf).min(data.len());
    if start >= data.len() {
        &[]
    } else {
        &data[start..end]
    }
}

/// Fragment a Reticulum packet into BLE fragments (convenience, uses alloc).
///
/// Each returned `Vec<u8>` is a complete fragment: 5-byte header + payload,
/// ready to write to the BLE TX characteristic.
pub fn fragment_packet(data: &[u8], mtu: usize) -> Vec<Vec<u8>> {
    let total = fragment_count(data.len(), mtu);
    let mut fragments = Vec::with_capacity(total);
    for i in 0..total {
        let header = build_fragment_header(i, total);
        let payload = fragment_payload(data, i, mtu);
        let mut frag = Vec::with_capacity(FRAGMENT_HEADER_SIZE + payload.len());
        frag.extend_from_slice(&header);
        frag.extend_from_slice(payload);
        fragments.push(frag);
    }
    fragments
}

// Defragmentation (stateful, per-peer)
/// Result of processing a received BLE fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefragResult {
    /// More fragments needed to complete the packet.
    NeedMore,
    /// All fragments received; contains the reassembled packet.
    Complete(Vec<u8>),
    /// Invalid fragment (bad type, too short, sequence out of range).
    Error,
}

/// Per-peer BLE fragment reassembler.
///
/// Accumulates fragments keyed by sequence number. Returns `Complete` when
/// all fragments (0..total-1) have been received. Supports out-of-order
/// delivery. The caller must provide the current time for timeout tracking.
///
/// # Cleanup
///
/// Call [`is_timed_out`](BleDefragmenter::is_timed_out) periodically and
/// [`reset`](BleDefragmenter::reset) when timed out, or rely on the automatic
/// reset when a new START/LONE fragment arrives while a previous reassembly
/// is in progress.
pub struct BleDefragmenter {
    fragments: BTreeMap<u16, Vec<u8>>,
    expected_total: u16,
    last_fragment_ms: u64,
}

impl BleDefragmenter {
    /// Create a new defragmenter with no pending reassembly.
    pub fn new() -> Self {
        Self {
            fragments: BTreeMap::new(),
            expected_total: 0,
            last_fragment_ms: 0,
        }
    }

    /// Process a received BLE fragment.
    ///
    /// `fragment` must be at least [`FRAGMENT_HEADER_SIZE`] bytes (5).
    /// `now_ms` is the current monotonic time in milliseconds.
    pub fn process(&mut self, fragment: &[u8], now_ms: u64) -> DefragResult {
        if fragment.len() < FRAGMENT_HEADER_SIZE {
            return DefragResult::Error;
        }

        let ftype = fragment[0];
        let seq = u16::from_be_bytes([fragment[1], fragment[2]]);
        let total = u16::from_be_bytes([fragment[3], fragment[4]]);
        let payload = &fragment[FRAGMENT_HEADER_SIZE..];

        // Validate fragment type
        if ftype > FRAGMENT_TYPE_END {
            return DefragResult::Error;
        }

        // Validate total > 0
        if total == 0 {
            return DefragResult::Error;
        }

        // Validate sequence < total
        if seq >= total {
            return DefragResult::Error;
        }

        // LONE fragment, complete packet in one piece
        if ftype == FRAGMENT_TYPE_LONE {
            self.reset();
            return DefragResult::Complete(payload.to_vec());
        }

        // New multi-fragment sequence starting, reset any previous partial
        if ftype == FRAGMENT_TYPE_START && seq == 0 {
            self.fragments.clear();
            self.expected_total = total;
        }

        // Check consistency with current reassembly
        if total != self.expected_total {
            // Fragment from a different packet or corrupted, discard
            self.reset();
            return DefragResult::Error;
        }

        // Store fragment payload
        self.fragments.insert(seq, payload.to_vec());
        self.last_fragment_ms = now_ms;

        // Check if all fragments received
        if self.fragments.len() == self.expected_total as usize {
            // Reassemble in sequence order
            let mut packet = Vec::new();
            for i in 0..self.expected_total {
                match self.fragments.get(&i) {
                    Some(p) => packet.extend_from_slice(p),
                    None => {
                        // Should not happen, len check passed but gap found
                        self.reset();
                        return DefragResult::Error;
                    }
                }
            }
            self.reset();
            DefragResult::Complete(packet)
        } else {
            DefragResult::NeedMore
        }
    }

    /// Check if the current reassembly has timed out.
    ///
    /// Returns `true` if fragments are pending and the timeout has elapsed
    /// since the last fragment was received.
    pub fn is_timed_out(&self, now_ms: u64) -> bool {
        !self.fragments.is_empty()
            && now_ms.saturating_sub(self.last_fragment_ms) >= REASSEMBLY_TIMEOUT_MS
    }

    /// Discard any in-progress reassembly.
    pub fn reset(&mut self) {
        self.fragments.clear();
        self.expected_total = 0;
        self.last_fragment_ms = 0;
    }
}

impl Default for BleDefragmenter {
    fn default() -> Self {
        Self::new()
    }
}

// Tests
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn test_lone_fragment_roundtrip() {
        let data = b"Hello, Reticulum!";
        let frags = fragment_packet(data, DEFAULT_MTU);
        assert_eq!(frags.len(), 1);

        // Verify header: LONE, seq=0, total=1
        assert_eq!(frags[0][0], FRAGMENT_TYPE_LONE);
        assert_eq!(u16::from_be_bytes([frags[0][1], frags[0][2]]), 0);
        assert_eq!(u16::from_be_bytes([frags[0][3], frags[0][4]]), 1);

        let mut defrag = BleDefragmenter::new();
        match defrag.process(&frags[0], 1000) {
            DefragResult::Complete(result) => assert_eq!(result, data),
            other => panic!("Expected Complete, got {:?}", other),
        }
    }

    #[test]
    fn test_multi_fragment_roundtrip() {
        // 500 bytes at MTU 185 → payload_per_fragment = 177 → ceil(500/177) = 3 fragments
        let data: Vec<u8> = (0..500).map(|i| (i % 256) as u8).collect();
        let frags = fragment_packet(&data, DEFAULT_MTU);
        assert_eq!(frags.len(), 3);

        // Verify fragment types
        assert_eq!(frags[0][0], FRAGMENT_TYPE_START);
        assert_eq!(frags[1][0], FRAGMENT_TYPE_CONTINUE);
        assert_eq!(frags[2][0], FRAGMENT_TYPE_END);

        // Verify total count in each header
        for frag in &frags {
            assert_eq!(u16::from_be_bytes([frag[3], frag[4]]), 3);
        }

        // Reassemble in order
        let mut defrag = BleDefragmenter::new();
        assert_eq!(defrag.process(&frags[0], 1000), DefragResult::NeedMore);
        assert_eq!(defrag.process(&frags[1], 1000), DefragResult::NeedMore);
        match defrag.process(&frags[2], 1000) {
            DefragResult::Complete(result) => assert_eq!(result, data),
            other => panic!("Expected Complete, got {:?}", other),
        }
    }

    #[test]
    fn test_out_of_order_reassembly() {
        let data: Vec<u8> = (0..500).map(|i| (i % 256) as u8).collect();
        let frags = fragment_packet(&data, DEFAULT_MTU);
        assert_eq!(frags.len(), 3);

        // Deliver out of order: START, END, CONTINUE
        let mut defrag = BleDefragmenter::new();
        assert_eq!(defrag.process(&frags[0], 1000), DefragResult::NeedMore);
        assert_eq!(defrag.process(&frags[2], 1000), DefragResult::NeedMore);
        match defrag.process(&frags[1], 1000) {
            DefragResult::Complete(result) => assert_eq!(result, data),
            other => panic!("Expected Complete, got {:?}", other),
        }
    }

    #[test]
    fn test_mtu_boundary_exact_fit() {
        // Payload that exactly fills one fragment: payload_per_fragment(185) = 177
        let ppf = payload_per_fragment(DEFAULT_MTU);
        let data: Vec<u8> = (0..ppf).map(|i| (i % 256) as u8).collect();
        let frags = fragment_packet(&data, DEFAULT_MTU);
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0][0], FRAGMENT_TYPE_LONE);

        // One byte over → splits into 2 fragments
        let data2: Vec<u8> = (0..ppf + 1).map(|i| (i % 256) as u8).collect();
        let frags2 = fragment_packet(&data2, DEFAULT_MTU);
        assert_eq!(frags2.len(), 2);
        assert_eq!(frags2[0][0], FRAGMENT_TYPE_START);
        assert_eq!(frags2[1][0], FRAGMENT_TYPE_END);
    }

    #[test]
    fn test_large_packet() {
        let data: Vec<u8> = vec![0xAA; 500];
        let frags = fragment_packet(&data, DEFAULT_MTU);
        let ppf = payload_per_fragment(DEFAULT_MTU);

        // Verify payload sizes
        assert_eq!(frags[0].len() - FRAGMENT_HEADER_SIZE, ppf); // full
        assert_eq!(frags[1].len() - FRAGMENT_HEADER_SIZE, ppf); // full
        assert_eq!(frags[2].len() - FRAGMENT_HEADER_SIZE, 500 - 2 * ppf); // remainder

        // Reassemble and verify
        let mut defrag = BleDefragmenter::new();
        let mut result = DefragResult::NeedMore;
        for frag in &frags {
            result = defrag.process(frag, 1000);
        }
        match result {
            DefragResult::Complete(r) => assert_eq!(r, data),
            other => panic!("Expected Complete, got {:?}", other),
        }
    }

    #[test]
    fn test_timeout() {
        let data: Vec<u8> = (0..500).map(|i| (i % 256) as u8).collect();
        let frags = fragment_packet(&data, DEFAULT_MTU);

        let mut defrag = BleDefragmenter::new();
        assert_eq!(defrag.process(&frags[0], 1000), DefragResult::NeedMore);
        assert!(!defrag.is_timed_out(1000));
        assert!(!defrag.is_timed_out(30_999));
        assert!(defrag.is_timed_out(31_000));

        // After timeout, reset and try again
        defrag.reset();
        assert!(!defrag.is_timed_out(31_000)); // no pending fragments
    }

    #[test]
    fn test_zero_length_payload() {
        let data: &[u8] = &[];
        let frags = fragment_packet(data, DEFAULT_MTU);
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].len(), FRAGMENT_HEADER_SIZE); // header only

        let mut defrag = BleDefragmenter::new();
        match defrag.process(&frags[0], 1000) {
            DefragResult::Complete(result) => assert!(result.is_empty()),
            other => panic!("Expected Complete, got {:?}", other),
        }
    }

    #[test]
    fn test_header_encoding() {
        let header = build_fragment_header(0x0102, 0x0304);
        // index > 0 and index < total-1 → CONTINUE
        assert_eq!(header[0], FRAGMENT_TYPE_CONTINUE);
        // Sequence: big-endian 0x0102
        assert_eq!(header[1], 0x01);
        assert_eq!(header[2], 0x02);
        // Total: big-endian 0x0304
        assert_eq!(header[3], 0x03);
        assert_eq!(header[4], 0x04);
    }

    #[test]
    fn test_too_short_fragment() {
        let mut defrag = BleDefragmenter::new();
        // Less than 5 bytes → Error
        assert_eq!(defrag.process(&[0x00, 0x01], 1000), DefragResult::Error);
        assert_eq!(defrag.process(&[], 1000), DefragResult::Error);
    }

    #[test]
    fn test_invalid_fragment_type() {
        let mut defrag = BleDefragmenter::new();
        let invalid = [0x04, 0x00, 0x00, 0x00, 0x01]; // type 4 doesn't exist
        assert_eq!(defrag.process(&invalid, 1000), DefragResult::Error);
    }

    #[test]
    fn test_sequence_exceeds_total() {
        let mut defrag = BleDefragmenter::new();
        // seq=1, total=1 → seq >= total → Error
        let bad = [FRAGMENT_TYPE_CONTINUE, 0x00, 0x01, 0x00, 0x01];
        assert_eq!(defrag.process(&bad, 1000), DefragResult::Error);
    }

    #[test]
    fn test_zero_total() {
        let mut defrag = BleDefragmenter::new();
        let bad = [FRAGMENT_TYPE_LONE, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(defrag.process(&bad, 1000), DefragResult::Error);
    }

    #[test]
    fn test_min_mtu() {
        // MIN_MTU 23 → payload = 23 - 3 - 5 = 15 bytes per fragment
        let data: Vec<u8> = (0..50).collect();
        let frags = fragment_packet(&data, MIN_MTU);
        let ppf = payload_per_fragment(MIN_MTU);
        assert_eq!(ppf, 15);
        assert_eq!(frags.len(), 4); // ceil(50/15) = 4

        let mut defrag = BleDefragmenter::new();
        let mut result = DefragResult::NeedMore;
        for frag in &frags {
            result = defrag.process(frag, 1000);
        }
        match result {
            DefragResult::Complete(r) => assert_eq!(r, data),
            other => panic!("Expected Complete, got {:?}", other),
        }
    }

    #[test]
    fn test_max_mtu() {
        // MAX_MTU 517 → payload = 517 - 3 - 5 = 509 bytes per fragment
        // A 500-byte packet fits in a single LONE fragment
        let data: Vec<u8> = vec![0xBB; 500];
        let frags = fragment_packet(&data, MAX_MTU);
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0][0], FRAGMENT_TYPE_LONE);

        let mut defrag = BleDefragmenter::new();
        match defrag.process(&frags[0], 1000) {
            DefragResult::Complete(r) => assert_eq!(r, data),
            other => panic!("Expected Complete, got {:?}", other),
        }
    }

    #[test]
    fn test_new_start_resets_previous() {
        let data1: Vec<u8> = (0..500).map(|i| (i % 256) as u8).collect();
        let data2: Vec<u8> = vec![0xFF; 500];
        let frags1 = fragment_packet(&data1, DEFAULT_MTU);
        let frags2 = fragment_packet(&data2, DEFAULT_MTU);

        let mut defrag = BleDefragmenter::new();
        // Start reassembling packet 1, then abandon it with packet 2's START
        assert_eq!(defrag.process(&frags1[0], 1000), DefragResult::NeedMore);
        assert_eq!(defrag.process(&frags2[0], 2000), DefragResult::NeedMore);
        assert_eq!(defrag.process(&frags2[1], 2000), DefragResult::NeedMore);
        match defrag.process(&frags2[2], 2000) {
            DefragResult::Complete(r) => assert_eq!(r, data2),
            other => panic!("Expected Complete, got {:?}", other),
        }
    }

    #[test]
    fn test_payload_per_fragment_values() {
        assert_eq!(payload_per_fragment(MIN_MTU), 15);
        assert_eq!(payload_per_fragment(DEFAULT_MTU), 177);
        assert_eq!(payload_per_fragment(MAX_MTU), 509);
        assert_eq!(payload_per_fragment(8), 0); // MTU too small
        assert_eq!(payload_per_fragment(0), 0);
    }
}
