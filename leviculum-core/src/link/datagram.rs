//! Unordered, unreliable link datagrams (+ciris, leviculum#102).
//!
//! A Channel is ordered and reliable: a frame behind a lost one waits for its
//! retransmit, or since leviculum#94 for its deadline, so one loss on a live
//! media stream turns into a burst of late or expired frames. A datagram is
//! the other shape: one encrypted link packet, sent once, never acknowledged,
//! retransmitted or reordered, delivered the moment it decrypts. It carries a
//! per-link sequence number so the receiver can see loss and order and do
//! its own jitter buffering.
//!
//! On the wire it is a link DATA packet with context
//! [`crate::packet::PacketContext::Datagram`] (`0xF0`, a byte the reference
//! does not assign), whose link-encrypted plaintext is
//!
//! ```text
//! [sequence: u32 big-endian][payload]
//! ```
//!
//! A peer that does not know the context drops it at its own local gate:
//! leviculum's at `NodeCore`'s unknown-context drop (Codeberg #332), Python's
//! by falling off the `elif` chain in `Link.receive`. So the sender must
//! know the peer speaks it, and the Channel stays the fallback.
//!
//! No proof is ever requested or sent for one, whatever the destination's
//! proof strategy: a datagram has no delivery to confirm.

use alloc::vec::Vec;

/// Bytes a datagram's sequence number takes from the link MDU.
pub const DATAGRAM_HEADER_LEN: usize = 4;

/// The plaintext of a datagram: `sequence` then `payload`.
pub fn wrap(sequence: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(DATAGRAM_HEADER_LEN + payload.len());
    out.extend_from_slice(&sequence.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Split a datagram's plaintext into its sequence number and payload, or
/// `None` when it is too short to hold a sequence number.
pub fn unwrap(plaintext: &[u8]) -> Option<(u32, &[u8])> {
    let (head, payload) = plaintext.split_first_chunk::<DATAGRAM_HEADER_LEN>()?;
    Some((u32::from_be_bytes(*head), payload))
}

/// What a link has sent and received as datagrams.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DatagramStats {
    /// Datagrams handed to the interface.
    pub sent: u64,
    /// Their payload bytes, sequence numbers excluded.
    pub sent_bytes: u64,
    /// Datagrams that decrypted and were delivered.
    pub received: u64,
    /// Their payload bytes, sequence numbers excluded.
    pub received_bytes: u64,
    /// Datagrams that did not decrypt or were too short to carry a sequence
    /// number. None of them was delivered.
    pub refused: u64,
}

/// One link's datagram state: the next sequence number and the counters.
#[derive(Debug, Default, Clone)]
pub(crate) struct DatagramState {
    next_sequence: u32,
    pub(crate) stats: DatagramStats,
}

impl DatagramState {
    /// The sequence number for the next datagram, wrapping after `u32::MAX`.
    pub(crate) fn take_sequence(&mut self) -> u32 {
        let sequence = self.next_sequence;
        self.next_sequence = sequence.wrapping_add(1);
        sequence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_datagram_round_trips_its_sequence_and_payload() {
        let wire = wrap(0x0102_0304, b"frame");
        assert_eq!(&wire[..DATAGRAM_HEADER_LEN], &[1, 2, 3, 4]);
        assert_eq!(unwrap(&wire), Some((0x0102_0304, &b"frame"[..])));
        assert_eq!(unwrap(&wrap(7, b"")), Some((7, &b""[..])));
    }

    #[test]
    fn a_plaintext_shorter_than_a_sequence_number_is_refused() {
        assert_eq!(unwrap(&[1, 2, 3]), None);
        assert_eq!(unwrap(&[]), None);
    }

    #[test]
    fn sequence_numbers_count_up_and_wrap() {
        let mut state = DatagramState::default();
        assert_eq!(state.take_sequence(), 0);
        assert_eq!(state.take_sequence(), 1);
        state.next_sequence = u32::MAX;
        assert_eq!(state.take_sequence(), u32::MAX);
        assert_eq!(state.take_sequence(), 0);
    }
}
