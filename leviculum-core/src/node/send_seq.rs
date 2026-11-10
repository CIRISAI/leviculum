//! A Channel send's sequence, and the delivery proof that names it (+ciris).
//!
//! [`NodeCore::send_on_link_seq`] returns the sequence the envelope went out
//! under; [`NodeCore::link_seq_stats`] reads the link's newest sent sequence
//! and the oldest still unproved (the front of the Channel's send-ordered tx
//! ring). Together they let a caller prove that one message, and every
//! message sent before it, was delivered, while later traffic keeps the ring
//! busy: [`LinkSeqStats::delivered_through`]. Kept apart from
//! [`super::LinkStats`] so no line an upstream citation points at moves.

use rand_core::CryptoRngCore;

use crate::link::LinkId;
use crate::traits::{Clock, Storage};

use super::{send, NodeCore};

/// One link's Channel sequence watermarks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkSeqStats {
    pub(crate) oldest_unacked_sequence: Option<u16>,
    pub(crate) last_sent_sequence: u16,
}

impl<R: CryptoRngCore, C: Clock, S: Storage> NodeCore<R, C, S> {
    /// [`send_on_link`](Self::send_on_link), returning the Channel sequence
    /// the envelope went out under. The sequence is read while the send
    /// holds `&mut self`, so no other send can come between.
    pub fn send_on_link_seq(
        &mut self,
        link_id: &LinkId,
        data: &[u8],
    ) -> Result<(u16, crate::transport::TickOutput), send::SendError> {
        self.send_on_link_ttl_seq(link_id, data, None)
    }

    /// [`Self::send_on_link_ttl_seq`] without the sequence.
    pub(super) fn send_on_link_ttl(
        &mut self,
        link_id: &LinkId,
        data: &[u8],
        ttl_ms: Option<u32>,
    ) -> Result<crate::transport::TickOutput, send::SendError> {
        self.send_on_link_ttl_seq(link_id, data, ttl_ms)
            .map(|(_, out)| out)
    }

    /// `link_id`'s Channel sequence watermarks, or `None` when the link is
    /// gone or has no Channel yet (nothing sent on it).
    pub fn link_seq_stats(&self, link_id: &LinkId) -> Option<LinkSeqStats> {
        let link_id = &self.resolve_link_id(link_id);
        let ch = self.links.get(link_id)?.channel()?;
        Some(LinkSeqStats {
            oldest_unacked_sequence: ch.oldest_unacked_sequence(),
            last_sent_sequence: ch.last_sent_sequence(),
        })
    }
}

impl LinkSeqStats {
    /// The sequence of the oldest Channel envelope still awaiting its
    /// delivery proof, or `None` when every envelope sent has been proved.
    #[must_use]
    pub fn oldest_unacked_sequence(&self) -> Option<u16> {
        self.oldest_unacked_sequence
    }

    /// The sequence of the newest Channel envelope sent. Read after a send it
    /// is at or after that send's own sequence (another sender may have sent
    /// since), so waiting for [`Self::delivered_through`] it proves the send
    /// delivered, at worst along with a few later envelopes. For the exact
    /// sequence use [`NodeCore::send_on_link_seq`]. A Channel that has sent
    /// nothing reads 0xFFFF with nothing outstanding, which
    /// [`Self::delivered_through`] counts as delivered: nothing to prove.
    #[must_use]
    pub fn last_sent_sequence(&self) -> u16 {
        self.last_sent_sequence
    }

    /// Whether the envelope sent as `sequence` on this link has been proved
    /// delivered, along with every envelope sent before it: nothing is
    /// outstanding, or the oldest outstanding envelope was sent after
    /// `sequence`. Sequences wrap at 2^16; "after" is serial order (RFC 1982),
    /// exact while the ring holds no more than the window (at most
    /// `CHANNEL_WINDOW_MAX_FAST`, far below 2^15).
    ///
    /// For a frame sent with a deadline
    /// ([`NodeCore::send_on_link_timed`]), the proof may be for the expired
    /// marker sent in its place: use an untimed send when the proof must
    /// mean that the frame itself arrived.
    #[must_use]
    pub fn delivered_through(&self, sequence: u16) -> bool {
        match self.oldest_unacked_sequence {
            None => true,
            Some(oldest) => serial_after(oldest, sequence),
        }
    }
}

/// `a` comes after `b` in 16-bit serial order.
pub(crate) fn serial_after(a: u16, b: u16) -> bool {
    let d = a.wrapping_sub(b);
    d != 0 && d < 0x8000
}
