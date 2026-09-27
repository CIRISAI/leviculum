//! Two-bound admission for a firmware outbound queue (Codeberg #344).
//!
//! The reference answer to "what does the radio do when its outbound queue
//! fills" is not a retry mechanism. Mark Qvist's RNode firmware v1.86 bounds
//! its queue **twice** — `CONFIG_QUEUE_SIZE` bytes and
//! `CONFIG_QUEUE_MAX_LENGTH` packets, whichever binds first (`Boards.h:836-837`
//! for the Heltec T114, `:736-738` for the RAK4631) — and then drops silently
//! on overflow (`RNode_Firmware.ino:769-783` per frame, `:805-809` per byte,
//! neither with an `else`). There is no retry anywhere in that firmware.
//!
//! Of the two bounds the **byte bound is the real one**: 200 packets at our
//! MTU would be 100 KB, which no nRF52 has. The packet bound only keeps the
//! slot array — and, for us, the channel's `.bss` — finite. So a queue deep
//! enough that filling is rare, bounded in bytes so that a burst of large
//! packets cannot eat the heap, is the shape this type expresses.
//!
//! It lives in its own crate for the reason [`leviculum-sd-policy`] and
//! [`leviculum-ble-tx`] do: the firmware crate only builds for `thumbv7em`, so
//! anything tested only there is tested nowhere. It is deliberately **not** in
//! `leviculum-core` — that crate must cross-compile for `thumbv6m`
//! (`just m0-build-gate`), which has no atomic compare-and-swap, and the
//! counters here are shared between two Embassy tasks.
//!
//! [`leviculum-sd-policy`]: https://codeberg.org/Lew_Palm/leviculum
//! [`leviculum-ble-tx`]: https://codeberg.org/Lew_Palm/leviculum

#![cfg_attr(not(test), no_std)]

use core::sync::atomic::{AtomicUsize, Ordering};

/// Packet slots in the LoRa outbound queue.
///
/// Sixteen times the four slots this queue had until #344. Four slots against
/// a 723 ms SF10 frame is under a three-second backlog: any burst the stack
/// produces — a path response plus an announce plus a link proof — overran it,
/// and the overrun was a dropped packet. The reference carries 200
/// (`CONFIG_QUEUE_MAX_LENGTH`); we carry 64 because our slots are not free the
/// way its are. A slot is a `Vec<u8>` header, 12 bytes on `thumbv7em`, sitting
/// in `.bss` whether or not a packet is in it, and `.bss` is taken from the
/// stack margin under flip-link. 64 slots cost 768 B there against 48 B for 4.
/// The bound that actually binds under load is [`LORA_QUEUE_BYTES`] anyway.
pub const LORA_QUEUE_SLOTS: usize = 64;

/// Byte budget for the LoRa outbound queue: 6 KiB, the reference's
/// `CONFIG_QUEUE_SIZE` for both of our boards (`Boards.h:836` T114, `:737`
/// RAK4631).
///
/// The number is taken from the reference rather than invented because the
/// quantity it bounds is the same on both stacks — bytes of queued payload on
/// one SX1262 — and because it is the number that firmware has been shipping
/// on this hardware for years.
///
/// What it costs us: 6144 B is 6.25 % of the firmware's 96 KiB heap
/// (`leviculum-nrf/src/lib.rs`, `HEAP_SIZE`), plus at most one dequeued
/// in-flight packet (500 B at MTU) that the transmitter owns, so 6.76 % worst
/// case. The four-slot queue it replaces could hold 2000 B, 2.03 %.
///
/// What it admits: ~12 packets at the 500 B MTU, and many more below it —
/// which is the point of bounding memory rather than counting packets. The
/// caveat to watch on the air is that 6144 B is roughly 24 frames of 255 B,
/// about 17 s of airtime at SF10/BW125; the byte bound is a memory bound and
/// says nothing about how long a full queue takes to drain.
pub const LORA_QUEUE_BYTES: usize = 6 * 1024;

/// Which of the two bounds refused a packet.
///
/// Carried into the log line so a capture says *why* the board turned a
/// packet away: a byte-bound refusal at a low slot count means large packets
/// backed up, a slot-bound refusal at a low byte count means many small ones.
/// The two want different answers and used to look identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueBound {
    /// The packet count reached its maximum.
    Slots,
    /// The queued byte total would exceed the budget.
    Bytes,
}

impl QueueBound {
    /// Stable, greppable name for the structured log line.
    pub fn as_str(self) -> &'static str {
        match self {
            QueueBound::Slots => "slots",
            QueueBound::Bytes => "bytes",
        }
    }
}

/// The occupancy of one outbound queue, in slots and in bytes.
///
/// A producer calls [`reserve`](Self::reserve) *before* handing the packet to
/// the queue and a consumer calls [`release`](Self::release) the moment it
/// takes one out. Reserving first is what makes the counters safe with the
/// producer and the consumer in different tasks: the reservation exists before
/// the packet is visible to the consumer, and is released only after the
/// packet has left, so the counters are never below the queue's true contents
/// and can never wrap.
///
/// The counters describe **what is in the queue**, not what the system holds:
/// a consumer that has dequeued a packet and is still transmitting it owns
/// that memory without it being counted here. That is the honest reading, and
/// it is why the worst-case memory attributable to a queue is its byte budget
/// plus one in-flight packet.
pub struct QueueBudget {
    max_slots: usize,
    max_bytes: usize,
    slots: AtomicUsize,
    bytes: AtomicUsize,
}

impl QueueBudget {
    /// A queue bounded at `max_slots` packets and `max_bytes` bytes.
    pub const fn new(max_slots: usize, max_bytes: usize) -> Self {
        Self {
            max_slots,
            max_bytes,
            slots: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
        }
    }

    /// Claim room for one packet of `len` bytes, or say which bound refused.
    ///
    /// On `Ok` the caller **must** enqueue the packet or call
    /// [`release`](Self::release) — the reservation is already counted.
    ///
    /// A packet larger than the whole byte budget is refused rather than
    /// admitted-because-it-is-alone: admitting it would put the queue over its
    /// memory bound, which is the one thing the bound exists to prevent. At our
    /// 500-byte MTU against a 6 KiB budget the case cannot arise, but a future
    /// budget below the MTU would fail closed instead of silently not applying.
    pub fn reserve(&self, len: usize) -> Result<(), QueueBound> {
        let slots = self
            .slots
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |s| {
                if s < self.max_slots {
                    Some(s + 1)
                } else {
                    None
                }
            });
        if slots.is_err() {
            return Err(QueueBound::Slots);
        }
        let bytes = self
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |b| {
                let next = b.saturating_add(len);
                if next <= self.max_bytes {
                    Some(next)
                } else {
                    None
                }
            });
        if bytes.is_err() {
            // Hand the slot straight back: a refusal must leave the queue
            // exactly as it found it, or a stream of oversized packets would
            // strangle the queue one slot at a time.
            self.slots
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |s| {
                    Some(s.saturating_sub(1))
                })
                .ok();
            return Err(QueueBound::Bytes);
        }
        Ok(())
    }

    /// Give back the room one dequeued packet of `len` bytes held.
    ///
    /// Saturating on both counters. A release without a matching reserve is a
    /// bug in the caller, but the failure mode of an underflow here — a
    /// `usize` wrap to a byte total no packet can ever fit under — would wedge
    /// the queue shut permanently, so it fails toward the open queue.
    pub fn release(&self, len: usize) {
        self.slots
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |s| {
                Some(s.saturating_sub(1))
            })
            .ok();
        self.bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |b| {
                Some(b.saturating_sub(len))
            })
            .ok();
    }

    /// Bytes currently reserved by queued packets.
    pub fn queued_bytes(&self) -> usize {
        self.bytes.load(Ordering::Acquire)
    }

    /// Packets currently queued.
    pub fn queued_slots(&self) -> usize {
        self.slots.load(Ordering::Acquire)
    }

    /// The byte budget this queue was built with.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// The slot count this queue was built with.
    pub fn max_slots(&self) -> usize {
        self.max_slots
    }
}

/// How long an outbound frame may have waited when the airtime lock is the
/// thing holding it, before the interface throws it away instead of keying it
/// (Codeberg #433, option (a)).
///
/// # What the lock does to a queue
///
/// A duty-cycle lock does not thin traffic, it **ages** it. While
/// `AirtimeTracker::is_locked` is true the LoRa task holds the frame at the
/// head of the queue and re-checks; each dip of the ledger below the cap
/// admits exactly one frame, which re-pins it. Under sustained load the queue
/// therefore drains at the cap's rate with FIFO order intact, and the age of
/// whatever reaches the air grows to the standing backlog. Measured on the
/// Pocket V2 on 2026-09-27 (field test, #255): three relayed link requests
/// keyed 145.6 s, 137.7 s and 144.0 s after they were handed to the
/// interface, one frame leaving every 10-15 s.
///
/// # Where 20 s comes from
///
/// Two bounds, and the value is the round number between them.
///
/// **Upper bound — the relay entry the frame is trying to reach, 29 s.** A
/// forwarded link request is routable only until the relay's link-table entry
/// expires: `proof_timeout_ms = now + (hops + path_hops + 2) *
/// DEFAULT_PER_HOP_TIMEOUT`, 6 s per hop
/// (`leviculum-core/src/transport.rs:6503-6506`,
/// `leviculum-core/src/constants.rs:113`), which is 30 s in the measured field
/// topology (hops 1, path_hops 2) and starts at forward time — i.e. when the
/// frame is enqueued here. The measured return leg after key-up was 1.0 s
/// (key-up 14:24:24.009 UTC, proof back at this board 14:24:25.015), so a
/// frame keyed later than 29 s after enqueue produces a proof that arrives
/// after the entry it needs is gone. That is exactly the field failure, and it
/// is why 60 s — the figure the analysis in report 358 §3 floated — would not
/// have fixed it: with the cap in force the aired frames' age saturates just
/// under the cap, so a 60 s cap still keys frames at twice the entry's life.
///
/// **Lower bound — the short-term airtime window, 15 s.** The short-term
/// ledger is the sum of two 7.5 s histogram bins
/// (`leviculum-core/src/rnode.rs:1460`, `:1909-1910`), so a lock that engaged
/// on short-term airtime alone releases within 15 s as those bins roll off.
/// A cap at or below that would throw away frames the lock was about to
/// release anyway, turning a legal pause into a loss. 20 s leaves a frame the
/// whole roll-off plus 5 s.
///
/// **The cadences agree.** An initiator reissues a link request every
/// `ESTABLISHMENT_TIMEOUT_PER_HOP_MS × hops` — 18 s by our constant at 3
/// hops, 16 s observed at the phone — so a frame older than 20 s has already
/// been superseded by a fresher copy from the same sender, and the sender has
/// written the first attempt off. Announces are reissued on their own cadence
/// (300 s mobile, ~1 h stationary) and LXMF retries at its own layer, so
/// neither loses a message to this rule; both gain, because the airtime the
/// cap does allow now carries current traffic instead of fossils.
///
/// **What it cannot fix.** The arithmetic floor of the entry deadline is
/// (0 + 0 + 2) × 6 s = 18 s, below this cap. On a topology that short a frame
/// keyed at 19 s still outlives its entry. No cap can serve both that floor
/// and the 15 s short-term roll-off, so the rule is sized for the topology
/// that was measured; it makes no topology worse than the FIFO hold it
/// replaces, which aired at 145 s.
pub const HOLD_MAX_AGE_MS: u32 = 20_000;

/// What the age rule says about the frame the transmitter is holding.
///
/// Derived from the frame's age and the state of the airtime lock, and from
/// nothing else: the interface never looks at the packet. A link request, an
/// announce and a resource chunk of the same age get the same verdict, which
/// is what keeps this rule on the interface side of the isolation boundary
/// (CLAUDE.md, "Architecture: interface isolation").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldVerdict {
    /// The airtime lock is not engaged: key the frame, whatever its age.
    ///
    /// A frame that waited for CSMA, for an acquisition jitter draw or for a
    /// burst gap is not stale in this sense — those waits are milliseconds to
    /// seconds and end by themselves. Only the regulatory lock holds a frame
    /// for minutes, and only its hold is what this rule is about.
    Key,
    /// Locked, and the frame is still young enough to be worth its airtime.
    /// The caller holds it and re-checks on its next turn.
    Hold {
        /// How long the frame has waited, for the caller's diagnostics.
        age_ms: u32,
    },
    /// Locked, and the frame has waited longer than [`HOLD_MAX_AGE_MS`].
    /// The caller drops it, counts it, and takes the next one.
    DropStale {
        /// How long the frame waited before it was thrown away. Goes on the
        /// `LORA_QUEUE_DROP reason=stale` line, so a capture can show the
        /// distribution rather than just the count.
        age_ms: u32,
    },
}

/// Apply the age rule to one frame.
///
/// `enqueued_ms` and `now_ms` are the board's uptime in milliseconds,
/// truncated to `u32`. The difference is taken with `wrapping_sub`, which is
/// correct for every age shorter than the 49.7-day wrap and therefore for
/// every age a queued frame can have; the alternative, a saturating
/// subtraction, would make the frames enqueued in the 20 s before a wrap look
/// eternally fresh instead of costing one wrongly-dropped frame every seven
/// weeks. The truncation is deliberate: a `u32` stamp per queued frame is
/// 4 bytes of `.bss` against the 12 a `u64` would cost once alignment is
/// paid, and `.bss` is stack margin under flip-link.
pub fn hold_verdict(enqueued_ms: u32, now_ms: u32, locked: bool) -> HoldVerdict {
    if !locked {
        return HoldVerdict::Key;
    }
    let age_ms = now_ms.wrapping_sub(enqueued_ms);
    if age_ms > HOLD_MAX_AGE_MS {
        HoldVerdict::DropStale { age_ms }
    } else {
        HoldVerdict::Hold { age_ms }
    }
}

#[cfg(test)]
mod tests;
