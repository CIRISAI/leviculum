//! Which frames the LoRa outbound queue holds, so a byte-identical copy of
//! one of them is not keyed a second time (1169, Refs #24).
//!
//! The core rebuilds whatever a peer asks for with no memory of what still
//! waits in an interface queue. On 2026-10-31 (`lora_lnode_rncp_bidir_seq`,
//! leviculum 47b8100f) the answering T114 took three path requests for one
//! destination while its radio sat in a receive window, answered each one as
//! the reference does (`Transport.path_request`, Transport.py:2945-3000, one
//! announce-table entry per non-duplicate request), and queued three
//! identical 183-byte path responses. They went on the air back to back,
//! 2.7 s each at SF10, and every copy restarted the requester's pre-TX
//! jitter in front of the link request it had queued: 17.3 s from host to
//! air, and rncp gave up before the proof came back. Only the queue knows
//! what is queued and not yet keyed, so the memory lives here, the shape
//! lnsd's RNode send queue has had since 979e82f1
//! (`refuse_queued_duplicate`, `leviculum-std/src/interfaces/rnode.rs`).
//!
//! Bytes equal is the whole rule: two identical frames are the same frame on
//! the air, whatever packet type they carry, and the copy already queued
//! delivers it. A frame that has left the queue for the transmitter no
//! longer suppresses a later copy: a frame on the air can still be lost, and
//! a retry of it has to go out.
//!
//! The firmware's queue is an Embassy channel, which cannot be searched, so
//! this is a side table of 64-bit FNV-1a digests, one per queued frame,
//! filled by the single producer (`LoRaInterface::try_send`) and emptied by
//! the single consumer (the LoRa task) at each dequeue. Two `AtomicU32`
//! words a slot, because `thumbv7em` has no 64-bit atomics: the low word
//! doubles as the occupancy mark (forced non-zero), the producer writes the
//! high word first, and the consumer only ever clears the low word. A
//! digest collision would refuse a frame that differs from a queued one; at
//! 64 bits and at most [`LORA_QUEUE_SLOTS`] queued frames that is one in
//! 2^58 per frame.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::LORA_QUEUE_SLOTS;

/// 64-bit FNV-1a over the frame, split into the slot's two words, the low
/// one never zero.
fn digest(frame: &[u8]) -> (u32, u32) {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in frame {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let lo = (h as u32).max(1);
    (lo, (h >> 32) as u32)
}

/// The digests of the frames the LoRa outbound queue holds right now.
pub struct QueuedCopies {
    lo: [AtomicU32; LORA_QUEUE_SLOTS],
    hi: [AtomicU32; LORA_QUEUE_SLOTS],
}

impl Default for QueuedCopies {
    fn default() -> Self {
        Self::new()
    }
}

impl QueuedCopies {
    /// An empty table, sized to the queue's slot bound: the budget never
    /// lets more frames into the channel than there are slots here.
    pub const fn new() -> Self {
        Self {
            lo: [const { AtomicU32::new(0) }; LORA_QUEUE_SLOTS],
            hi: [const { AtomicU32::new(0) }; LORA_QUEUE_SLOTS],
        }
    }

    fn find(&self, (lo, hi): (u32, u32)) -> Option<usize> {
        (0..LORA_QUEUE_SLOTS).find(|&i| {
            self.lo[i].load(Ordering::Acquire) == lo && self.hi[i].load(Ordering::Relaxed) == hi
        })
    }

    /// Whether a frame with exactly these bytes is queued and not yet
    /// handed to the transmitter.
    pub fn is_queued(&self, frame: &[u8]) -> bool {
        self.find(digest(frame)).is_some()
    }

    /// Record a frame the queue has just accepted. Producer side only.
    ///
    /// A full table records nothing: the frame is queued untracked, so a
    /// later copy of it is keyed too. Unreachable while every dequeue calls
    /// [`dequeued`](Self::dequeued), and the failure it degrades to is the
    /// behaviour before this table existed, never a refused frame.
    pub fn queued(&self, frame: &[u8]) {
        let (lo, hi) = digest(frame);
        if let Some(i) = (0..LORA_QUEUE_SLOTS).find(|&i| self.lo[i].load(Ordering::Acquire) == 0) {
            self.hi[i].store(hi, Ordering::Relaxed);
            self.lo[i].store(lo, Ordering::Release);
        }
    }

    /// Forget one frame that has left the queue. Consumer side, and the
    /// producer's own hand-back when the channel refuses a frame it
    /// recorded. A frame that was never recorded changes nothing.
    pub fn dequeued(&self, frame: &[u8]) {
        if let Some(i) = self.find(digest(frame)) {
            self.lo[i].store(0, Ordering::Release);
        }
    }

    /// Frames currently recorded.
    pub fn len(&self) -> usize {
        self.lo
            .iter()
            .filter(|w| w.load(Ordering::Acquire) != 0)
            .count()
    }

    /// Whether no frame is recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
