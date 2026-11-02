//! Channel messages with a deadline (+ciris, leviculum#94).
//!
//! Live media must drop a stale frame, not retransmit it or deliver it late.
//! A channel is reliable and strictly ordered, so a frame cannot simply be
//! left out: the receiver would wait for its sequence number forever. Two
//! system messages keep the sequence whole:
//!
//! - [`MSG_TIMED`] carries a frame with the sender's send time and a time to
//!   live. The receiver estimates how old it is on arrival (against the
//!   smallest one-way offset it has seen, so the two clocks need not agree)
//!   and drops it instead of delivering it if it is older than its TTL, which
//!   is what happens to a frame that waited in the reorder buffer behind a
//!   gap.
//! - [`MSG_EXPIRED`] is what the sender retransmits in a frame's place once
//!   its deadline has passed: same sequence number, no payload. The receiver
//!   consumes it in order and delivers nothing.
//!
//! Both are leviculum-only (0.33.0+ciris.1 on): a Python or rns-rs receiver
//! does not know them, so a host sends timed frames only to peers that do.

use alloc::vec::Vec;

/// A frame with a deadline: `msgtype (u16) | sent_ms (u32) | ttl_ms (u32) |
/// data`, all big-endian. `sent_ms` is the sender's clock, truncated.
pub const MSG_TIMED: u16 = 0xFD01;
/// What replaces an expired frame on the wire: no payload.
pub const MSG_EXPIRED: u16 = 0xFD00;
/// Bytes [`MSG_TIMED`] adds in front of the frame.
pub const TIMED_HEADER_LEN: usize = 10;

/// Whether a message type is one of these two (the node's, not the host's,
/// except for the frame a timed message carries).
#[must_use]
pub fn is_timed_msgtype(msgtype: u16) -> bool {
    msgtype == MSG_TIMED || msgtype == MSG_EXPIRED
}

/// Wrap `data` (of type `msgtype`) as a timed frame sent at `now_ms`.
#[must_use]
pub fn wrap(msgtype: u16, data: &[u8], now_ms: u64, ttl_ms: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(TIMED_HEADER_LEN + data.len());
    out.extend_from_slice(&msgtype.to_be_bytes());
    out.extend_from_slice(&(now_ms as u32).to_be_bytes());
    out.extend_from_slice(&ttl_ms.to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// A timed frame, unwrapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timed<'a> {
    /// The frame's own message type.
    pub msgtype: u16,
    /// The sender's clock when it sent the frame, truncated to 32 bits.
    pub sent_ms: u32,
    /// How long the frame is worth delivering after it was sent.
    pub ttl_ms: u32,
    /// The frame.
    pub data: &'a [u8],
}

/// Unwrap a [`MSG_TIMED`] payload.
#[must_use]
pub fn unwrap(payload: &[u8]) -> Option<Timed<'_>> {
    if payload.len() < TIMED_HEADER_LEN {
        return None;
    }
    Some(Timed {
        msgtype: u16::from_be_bytes([payload[0], payload[1]]),
        sent_ms: u32::from_be_bytes([payload[2], payload[3], payload[4], payload[5]]),
        ttl_ms: u32::from_be_bytes([payload[6], payload[7], payload[8], payload[9]]),
        data: &payload[TIMED_HEADER_LEN..],
    })
}

/// How long the baseline keeps a minimum before it may rise again: a path
/// whose delay grew for good is re-learned within two of these.
const BASELINE_WINDOW_MS: u64 = 30_000;

/// The receiver's estimate of a timed frame's age, per link.
///
/// The offset `recv - sent` of each frame is the one-way delay plus the
/// difference between the two clocks. Its minimum over a recent window is the
/// fastest the path has carried a frame; a frame's age is how far its offset
/// sits above that. Clock offset cancels; clock drift over the window does
/// not matter at media time scales.
#[derive(Debug, Clone, Default)]
pub struct AgeEstimator {
    current_min: Option<i64>,
    previous_min: Option<i64>,
    window_start_ms: u64,
}

impl AgeEstimator {
    /// A frame sent at `sent_ms` (sender clock) arrived at `now_ms`: its age
    /// beyond the fastest delivery seen, in milliseconds.
    pub fn age_ms(&mut self, sent_ms: u32, now_ms: u64) -> u64 {
        // Both clocks taken mod 2^32; the difference is meaningful as a
        // wrapping i32.
        let offset = i64::from((now_ms as u32).wrapping_sub(sent_ms) as i32);
        if now_ms.saturating_sub(self.window_start_ms) >= BASELINE_WINDOW_MS {
            self.previous_min = self.current_min;
            self.current_min = None;
            self.window_start_ms = now_ms;
        }
        self.current_min = Some(self.current_min.map_or(offset, |m| m.min(offset)));
        let baseline = match self.previous_min {
            Some(p) => p.min(self.current_min.unwrap_or(p)),
            None => self.current_min.unwrap_or(offset),
        };
        u64::try_from(offset - baseline).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timed_frame_round_trips() {
        let w = wrap(0x0001, b"frame", 1_234_567, 200);
        let t = unwrap(&w).unwrap();
        assert_eq!(
            t,
            Timed {
                msgtype: 1,
                sent_ms: 1_234_567,
                ttl_ms: 200,
                data: b"frame"
            }
        );
        assert!(unwrap(&w[..5]).is_none());
    }

    #[test]
    fn age_is_measured_above_the_fastest_delivery_whatever_the_clock_offset() {
        let mut est = AgeEstimator::default();
        // The receiver's clock runs 1_000_000 ms ahead; the path takes 50 ms.
        let skew = 1_000_000u64;
        assert_eq!(est.age_ms(10_000, 10_050 + skew), 0);
        assert_eq!(est.age_ms(10_100, 10_150 + skew), 0);
        // A frame that waited 300 ms behind a gap.
        assert_eq!(est.age_ms(10_200, 10_550 + skew), 300);
    }
}
