//! Bounds for GATT values a peer controls (Codeberg #387).
//!
//! `nrf-softdevice`'s `GattValue` impl for `heapless::Vec<u8, N>` is
//! `unwrap!(Self::from_slice(data))` (gatt_traits.rs:97): a peer write
//! longer than the characteristic's declared width panics the board.
//! The field T114 hit exactly that on 2026-09-12 with two phones
//! connected — twice, `PANIC_COUNT` 1 and 2, post-mortem naming that
//! line. The declared width was 251, but the write path is fed by the
//! SoftDevice's event buffer, not by the attribute's `max_len`: a
//! single ATT write at our granted MTU carries up to
//! [`GATT_VALUE_MAX`] bytes, and a queued (long) write executed in one
//! event can carry up to the `evt-max-size-512` buffer's 494 data
//! bytes. Whatever the blob delivers, a conversion that panics on
//! length is the wrong tool for peer-controlled bytes.
//!
//! [`GattBytes`] is the replacement value type: its wire conversion
//! **never fails** — data beyond the bound is truncated into the buffer
//! and the original wire length is kept, so the firmware's handler can
//! *reject* the write (drop it whole, log it, count it) instead of
//! either panicking or silently feeding a truncated fragment to the
//! defragmenter. Truncation is not a delivery option: a cut Columba
//! fragment would complete a reassembly with garbage. The policy —
//! oversize means dropped — lives with the handler; this type's job is
//! to survive the bytes and report them honestly.
//!
//! The bound itself is derived, not chosen: a Columba fragment fills
//! the negotiated ATT MTU exactly (5-byte fragment header plus
//! `payload_per_fragment` of payload = MTU − 3), so against our
//! [`ATT_MTU`] grant of 256 a **legitimate** peer writes 253-byte
//! values. The previous width of 251 — the BLE 4.2 link-layer DLE
//! payload, a bound from one layer below ATT — was 2 bytes short of
//! its own protocol's fragment size, which is how a healthy phone
//! produced the panic.

use leviculum_core::framing::ble::{payload_per_fragment, ATT_HEADER_SIZE, FRAGMENT_HEADER_SIZE};

/// The ATT MTU the firmware grants (`ble_gatt_conn_cfg_t::att_mtu`).
/// The negotiated MTU of any link to a board is `min(peer, this)`;
/// the firmware's `CONN_GATT` reads it from here so the GATT value
/// bound below cannot drift from the grant that produces the writes.
pub const ATT_MTU: usize = 256;

/// Widest GATT value a peer can legitimately put on the wire against
/// our MTU grant: one ATT Write Request/Command/Notification carries
/// MTU − 3 bytes of value. This is also exactly the size of a Columba
/// fragment at that MTU (see the const assert below), so the
/// characteristic width and the protocol agree by construction.
pub const GATT_VALUE_MAX: usize = ATT_MTU - ATT_HEADER_SIZE;

// A Columba fragment at our granted MTU fills one ATT write exactly:
// FRAGMENT_HEADER_SIZE + payload_per_fragment(ATT_MTU) = ATT_MTU − 3.
// If the framing overheads ever move, this stops the bound from
// silently becoming too small again.
const _: () = assert!(GATT_VALUE_MAX == FRAGMENT_HEADER_SIZE + payload_per_fragment(ATT_MTU));

/// A bounded byte value from a GATT peer whose construction cannot
/// fail: at most `N` bytes are kept, and the length the peer actually
/// put on the wire is carried alongside so [`Self::oversize`] can turn
/// "too long" into a *decision* instead of a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GattBytes<const N: usize> {
    buf: [u8; N],
    /// Bytes of `buf` in use, `<= N`.
    len: u16,
    /// The peer's length as received, before truncation. Saturated at
    /// `u16::MAX`, far above anything an ATT event buffer can carry.
    wire_len: u16,
}

impl<const N: usize> GattBytes<N> {
    /// Inbound: accept whatever the stack hands over. Never fails —
    /// beyond-bound data is truncated and marked, and the handler
    /// decides what a marked value is worth (in this firmware:
    /// nothing; it is dropped and logged).
    pub fn from_wire(data: &[u8]) -> Self {
        let keep = data.len().min(N);
        let mut buf = [0u8; N];
        buf[..keep].copy_from_slice(&data[..keep]);
        Self {
            buf,
            len: keep as u16,
            wire_len: data.len().try_into().unwrap_or(u16::MAX),
        }
    }

    /// Outbound: a value of our own making. `None` if it does not fit
    /// — nothing oversize is ever offered to the stack, so the peer's
    /// side of this defect cannot originate here.
    pub fn new(data: &[u8]) -> Option<Self> {
        (data.len() <= N).then(|| Self::from_wire(data))
    }

    /// The kept bytes. For an oversize value this is the truncated
    /// prefix — call [`Self::oversize`] first; a truncated Columba
    /// fragment must never reach the defragmenter.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..usize::from(self.len)]
    }

    /// The length the peer put on the wire, before any truncation.
    pub fn wire_len(&self) -> usize {
        usize::from(self.wire_len)
    }

    /// Whether the peer wrote more than the bound. `true` means
    /// [`Self::as_slice`] is a truncated prefix, not the value.
    pub fn oversize(&self) -> bool {
        self.wire_len() > N
    }
}

/// Which GATT surface an oversize value arrived on: a peer's write to
/// our server's RX characteristic, or a peer's notification into our
/// client. Same defect class, one line grammar each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OversizeFrom {
    /// `BLE_GATT_WRITE_OVERSIZE` — peripheral role, inbound write.
    Write,
    /// `BLE_GATT_NOTIFY_OVERSIZE` — central role, inbound notification.
    Notify,
}

/// The structured line for a dropped oversize value (#387). Formatted
/// here so the exact grammar is pinned by a host test rather than
/// transcribed into one; the firmware appends its own ` t=<ms>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OversizeLine {
    pub from: OversizeFrom,
    /// SoftDevice connection handle the value arrived on.
    pub conn: u16,
    /// The peer's wire length.
    pub len: usize,
    /// The bound it exceeded ([`GATT_VALUE_MAX`] in this firmware).
    pub max: usize,
}

impl core::fmt::Display for OversizeLine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match self.from {
            OversizeFrom::Write => "BLE_GATT_WRITE_OVERSIZE",
            OversizeFrom::Notify => "BLE_GATT_NOTIFY_OVERSIZE",
        };
        write!(
            f,
            "{} conn={} len={} max={}",
            name, self.conn, self.len, self.max
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect, isolated (#387): the vendored `GattValue` impl for
    /// `heapless::Vec<u8, N>` is `unwrap!(Self::from_slice(data))`
    /// (nrf-softdevice/src/ble/gatt_traits.rs:97) — the exact call and
    /// input below, which is why this test's panic message is the
    /// field post-mortem's. The real write path (SoftDevice event →
    /// `gatt_server::run` → generated `on_write` → `from_gatt`) cannot
    /// run on the host — nrf-softdevice compiles only against the S140
    /// blob for thumbv7em (SVC inline asm) — so the conversion seam is
    /// the closest host-drivable point, and it is where the fix lives.
    #[test]
    #[should_panic]
    fn the_vendored_conversion_panics_on_a_write_one_past_the_bound() {
        let write = [0u8; GATT_VALUE_MAX + 1];
        heapless::Vec::<u8, GATT_VALUE_MAX>::from_slice(&write).unwrap();
    }

    /// The fix: the same one-past-the-bound write becomes a marked
    /// value the handler drops, not a panic.
    #[test]
    fn an_oversize_write_is_marked_and_truncated_never_a_panic() {
        let write = [7u8; GATT_VALUE_MAX + 1];
        let value = GattBytes::<GATT_VALUE_MAX>::from_wire(&write);
        assert!(value.oversize());
        assert_eq!(value.wire_len(), GATT_VALUE_MAX + 1);
        assert_eq!(value.as_slice().len(), GATT_VALUE_MAX);
    }

    /// The widest event the `evt-max-size-512` buffer can deliver
    /// (512 − 18 bytes of event header) survives too: the bound on
    /// what reaches `from_gatt` is the event buffer, not `max_len`.
    #[test]
    fn the_widest_deliverable_event_survives() {
        let write = [7u8; 512 - 18];
        let value = GattBytes::<GATT_VALUE_MAX>::from_wire(&write);
        assert!(value.oversize());
        assert_eq!(value.wire_len(), 494);
    }

    /// The bound is the legitimate Columba fragment size at our MTU
    /// grant: 253 = 256 − 3. The 251 it replaces was the field panic's
    /// other half — a healthy phone filling the negotiated MTU was
    /// already 2 bytes over the old width.
    #[test]
    fn a_full_columba_fragment_at_our_mtu_fits_exactly() {
        assert_eq!(GATT_VALUE_MAX, 253);
        let fragment = [7u8; GATT_VALUE_MAX];
        let value = GattBytes::<GATT_VALUE_MAX>::from_wire(&fragment);
        assert!(!value.oversize());
        assert_eq!(value.as_slice(), &fragment);
    }

    /// Outbound construction refuses what would not fit, so our own
    /// writes and notifications can never hand a peer this defect.
    #[test]
    fn outbound_values_refuse_to_be_oversize() {
        assert!(GattBytes::<GATT_VALUE_MAX>::new(&[7u8; GATT_VALUE_MAX + 1]).is_none());
        let value = GattBytes::<GATT_VALUE_MAX>::new(&[7u8; 16]);
        assert_eq!(value.map(|v| v.as_slice().len()), Some(16));
    }

    #[test]
    fn empty_and_keepalive_writes_pass_unchanged() {
        let empty = GattBytes::<GATT_VALUE_MAX>::from_wire(&[]);
        assert!(!empty.oversize());
        assert_eq!(empty.as_slice(), &[] as &[u8]);
        let keepalive = GattBytes::<GATT_VALUE_MAX>::from_wire(&[0x00]);
        assert_eq!(keepalive.as_slice(), &[0x00]);
    }

    /// The two drop lines, verbatim (#387): what a dropped oversize
    /// write and a dropped oversize notification put in the log. The
    /// firmware appends ` t=<ms>`; everything before it is this.
    #[test]
    fn the_oversize_lines_are_the_documented_grammar_verbatim() {
        let write = OversizeLine {
            from: OversizeFrom::Write,
            conn: 1,
            len: 253,
            max: 251,
        };
        assert_eq!(
            write.to_string(),
            "BLE_GATT_WRITE_OVERSIZE conn=1 len=253 max=251"
        );

        let notify = OversizeLine {
            from: OversizeFrom::Notify,
            conn: 2,
            len: 494,
            max: 253,
        };
        assert_eq!(
            notify.to_string(),
            "BLE_GATT_NOTIFY_OVERSIZE conn=2 len=494 max=253"
        );
    }
}
