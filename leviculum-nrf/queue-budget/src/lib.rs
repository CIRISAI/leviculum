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
/// # Where 18 s comes from
///
/// It is the **shortest deadline the transport gives anything that crosses a
/// relay**, and it sits between two bounds that a cap must respect.
///
/// **The value — the shortest relay entry, 18 s.** A forwarded link request is
/// routable only until the relay's link-table entry expires:
/// `proof_timeout_ms = now + (hops + path_hops + 2) * DEFAULT_PER_HOP_TIMEOUT`,
/// 6 s per hop (`leviculum-core/src/transport.rs:6791-6794`,
/// `leviculum-core/src/constants.rs:113`), starting at forward time — i.e.
/// when the frame is enqueued here. A request a relay heard over the air has
/// `hops >= 1`, so the shortest entry any relay grants is the one-hop request
/// to a destination it reaches directly, `(1 + 0 + 2) × 6 s = 18 s`. A frame
/// older than that is dead on arrival there, and no relay grants a shorter
/// entry; keying it spends the one air slot the lock allows on traffic nobody
/// can use. (Until #433's lnsd half this was 20 s, sized for
/// the measured field topology alone; 18 s is the figure that holds for every
/// topology, and it still satisfies both bounds below.)
///
/// **Upper bound — the measured field topology, 29 s.** The field entry was
/// `(1 + 2 + 2) × 6 s = 30 s`, and the measured return leg after key-up was
/// 1.0 s (key-up 14:24:24.009 UTC, proof back at this board 14:24:25.015), so
/// a frame keyed later than 29 s after enqueue produces a proof that arrives
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
/// release anyway, turning a legal pause into a loss. 18 s leaves a frame the
/// whole roll-off plus 3 s.
///
/// **The cadences agree.** An initiator reissues a link request every
/// `ESTABLISHMENT_TIMEOUT_PER_HOP_MS × hops` — 18 s by our constant at 3
/// hops, 16 s observed at the phone — so a frame older than 18 s has already
/// been superseded by a fresher copy from the same sender, and the sender has
/// written the first attempt off. Announces are reissued on their own cadence
/// (300 s mobile, ~1 h stationary) and LXMF retries at its own layer, so
/// neither loses a message to this rule; both gain, because the airtime the
/// cap does allow now carries current traffic instead of fossils.
///
/// **What it cannot fix.** A frame keyed at 17.9 s in the shortest topology
/// still needs its return leg, so its proof can arrive just after the 18 s
/// entry is gone. A cap that also covered the return leg would sit at ~17 s,
/// 2 s above the short-term roll-off; the margin to that bound is worth more
/// than the last second, and the rule makes no topology worse than the FIFO
/// hold it replaces, which aired at 145 s.
///
/// # Both stacks
///
/// The firmware applies the rule against its own `AirtimeTracker::is_locked`
/// ([`hold_verdict`]); `lnsd`'s RNode interface cannot see the modem's lock
/// and applies it against the holds it observes on its CMD_READY gate
/// ([`DutyHolds`]). One constant, so the two stacks drop at the same age.
pub const HOLD_MAX_AGE_MS: u32 = 18_000;

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
/// subtraction, would make the frames enqueued in the 18 s before a wrap look
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

/// The duty holds a host-side queue has observed, reduced to what the age
/// rule needs: whether a frame's wait overlapped one, and by how much.
///
/// A host driving an RNode cannot read the modem's airtime lock. What it sees
/// is its consequence: the CMD_READY gate stays closed with frames queued,
/// because a modem that may not key cannot drain. Every handover closes that
/// gate briefly, so the caller says which closures count — `min_hold_ms`, the
/// longest closure ordinary airtime can explain — and only a closure at least
/// that long is a duty hold here.
///
/// Only the most recent qualifying hold is kept, and that is enough: a frame
/// overlapped SOME qualifying hold iff it overlapped the one that ended last,
/// because every earlier one ended before it. So the tracker is two stamps,
/// whatever the queue depth.
///
/// Milliseconds as `u32` on the caller's own clock, differences taken
/// wrappingly, the same convention as [`hold_verdict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DutyHolds {
    min_hold_ms: u32,
    /// Start of the closure in progress, if the gate is holding now.
    current: Option<u32>,
    /// `(start, end)` of the most recent closure that qualified.
    last: Option<(u32, u32)>,
}

impl DutyHolds {
    /// A tracker that counts a closure as a duty hold once it has lasted
    /// `min_hold_ms`.
    pub const fn new(min_hold_ms: u32) -> Self {
        Self {
            min_hold_ms,
            current: None,
            last: None,
        }
    }

    /// Feed the gate's state. Called on every turn of the caller's loop,
    /// before it dequeues, so a hold that ended this turn is on record when
    /// the frames it held are judged.
    pub fn observe(&mut self, holding: bool, now_ms: u32) {
        match (holding, self.current) {
            (true, None) => self.current = Some(now_ms),
            (false, Some(start)) => {
                self.current = None;
                if now_ms.wrapping_sub(start) >= self.min_hold_ms {
                    self.last = Some((start, now_ms));
                }
            }
            _ => {}
        }
    }

    /// Whether a qualifying hold is in progress at `now_ms`: the gate is
    /// closed and has been for at least `min_hold_ms`.
    ///
    /// This is the boolean the host hands the core as the interface's
    /// `duty_hold` state (leviculum#493): while it reads true the transport
    /// relays no announce and no path response on the interface, and the
    /// moment it reads false again the interface advertises routes as
    /// before. The qualification threshold is the same one the age rule
    /// uses, so the state the core refuses announces on and the holds that
    /// age frames into stale drops are one notion, not two.
    pub fn holding(&self, now_ms: u32) -> bool {
        matches!(self.current, Some(start) if now_ms.wrapping_sub(start) >= self.min_hold_ms)
    }

    /// How long a frame enqueued at `enqueued_ms` has waited behind a duty
    /// hold by `now_ms`: the overlap of its wait with the most recent
    /// qualifying hold, a hold still in progress included once it qualifies.
    /// Zero when its wait overlapped none.
    pub fn held_ms(&self, enqueued_ms: u32, now_ms: u32) -> u32 {
        let hold = match self.current {
            Some(start) if now_ms.wrapping_sub(start) >= self.min_hold_ms => Some((start, now_ms)),
            _ => self.last,
        };
        let Some((start, end)) = hold else {
            return 0;
        };
        // Everything as an age before `now`, where the wrap cannot bite.
        let waited = now_ms.wrapping_sub(enqueued_ms);
        let since_start = now_ms.wrapping_sub(start);
        let since_end = now_ms.wrapping_sub(end);
        waited.min(since_start).saturating_sub(since_end)
    }

    /// [`hold_verdict`] for a frame on a host-side queue: locked iff its wait
    /// overlapped a duty hold. Returns the verdict and the overlap, which the
    /// caller names on its drop line.
    pub fn verdict(&self, enqueued_ms: u32, now_ms: u32) -> (HoldVerdict, u32) {
        let held_ms = self.held_ms(enqueued_ms, now_ms);
        (hold_verdict(enqueued_ms, now_ms, held_ms > 0), held_ms)
    }
}

/// The edges of the firmware's own duty hold (leviculum#493): when the LoRa
/// task opens one, when it lifts it, and what the lift reports.
///
/// The task owns the radio and reads the airtime lock directly, so it needs
/// no [`DutyHolds`] inference; what it has to get right is the EDGE. The hold
/// opens on the first turn the gate holds or sweeps a frame and on no later
/// one, and it lifts exactly once: on a [`HoldVerdict::Key`], or on a turn
/// with nothing to send and the lock disengaged ([`Self::lifts_when_idle`]).
/// Each edge is what the firmware turns into the core's `duty_hold` mirror
/// and the USB `TYPE_DUTY_HOLD` frame, so a second open would re-signal a
/// hold the host already knows and a lift that never came would leave the
/// core refusing announces on a budget that has returned.
///
/// No rig cell sees the lift: a fresh ledger frees nothing for an hour
/// (`periculum/hardware/lora_duty_lock_lawful_default.toml`, "WHY THE LIFT
/// IS NOT HERE"), which is why the edge lives here and not inline in the
/// task (leviculum#413).
///
/// Milliseconds as `u64` uptime and the stale-drop counter as the `u32` the
/// firmware's atomic holds; the counter difference is taken wrappingly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HoldEdge {
    /// When the open hold began, and the stale-drop total at that moment.
    started: Option<(u64, u32)>,
}

/// What a lift reports on the firmware's `[LORA] hold lifted` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldLift {
    /// How long the board held, from the opening turn to this one.
    pub held_for_ms: u64,
    /// How many queued frames the hold aged into stale drops meanwhile.
    pub dropped_stale: u32,
}

impl HoldEdge {
    /// No hold open.
    pub const fn new() -> Self {
        Self { started: None }
    }

    /// Whether a hold is open.
    pub const fn is_open(&self) -> bool {
        self.started.is_some()
    }

    /// The gate held or swept a frame at `now_ms`. True iff this opened the
    /// hold, which is the one turn the caller raises its flag and signals.
    pub fn open(&mut self, now_ms: u64, stale_total: u32) -> bool {
        if self.started.is_some() {
            return false;
        }
        self.started = Some((now_ms, stale_total));
        true
    }

    /// Close the hold if one is open. `Some` iff this was the lift, carrying
    /// what the lift line states; `None` on every turn no hold was open.
    pub fn lift(&mut self, now_ms: u64, stale_total: u32) -> Option<HoldLift> {
        let (since_ms, stale_at_start) = self.started.take()?;
        Some(HoldLift {
            held_for_ms: now_ms.saturating_sub(since_ms),
            dropped_stale: stale_total.wrapping_sub(stale_at_start),
        })
    }

    /// Whether an idle turn lifts the hold: one is open, no frame is
    /// pending, and the lock has disengaged. A turn with a frame pending
    /// leaves the lift to that frame's [`HoldVerdict::Key`], so the lift is
    /// stated before the frame that proves it.
    pub const fn lifts_when_idle(&self, frame_pending: bool, locked: bool) -> bool {
        self.is_open() && !frame_pending && !locked
    }
}

/// The share of the lawful long-term airtime budget a propagation node's
/// OWN originated traffic may use, in permille: half (Lew, 2026-10-07,
/// leviculum#433, #384).
///
/// The field relay of 2026-09-27 was a propagation node. Its own sync
/// traffic filled the 10 % budget, the duty hold followed, and the link
/// setups of everyone behind it died in the hold. A relay that is also a
/// propagation node therefore stops starting work of its own once the
/// rolling hour has spent half the cap, so forwarding always has the other
/// half. The split is by origin, decided where the traffic is originated:
/// the LoRa queue still treats every frame alike.
///
/// A constant, not a persisted setting: the propagation-node record
/// (`leviculum_core::pn_config_store`) has no room for a third field without
/// a format bump, which the module docs of that record name.
pub const OWN_TRAFFIC_SHARE_PERMILLE: u16 = 500;

/// The long-term airtime cap in milliseconds of the rolling hour for a
/// `CMD_LT_ALOCK` value, or 0 for "unlimited".
///
/// The same mapping as `leviculum_core::rnode::alock_u16_to_fraction`
/// (`at / 10000`, 0 and anything at or above 100 % unlimited), in integer
/// milliseconds so the firmware can publish it in an atomic and nobody pays
/// a float: 100 (1 %) is 36 000 ms, 1000 (10 %) is 360 000 ms.
pub const fn lt_cap_ms(lt_alock: u16) -> u32 {
    if lt_alock == 0 || lt_alock >= 10_000 {
        0
    } else {
        lt_alock as u32 * 360
    }
}

/// Whether a node may originate traffic of its own: true while the rolling
/// hour's used airtime is below `share_permille` of the long-term cap.
///
/// `lt_cap_ms` 0 is an unlimited ledger (the tracker's own convention for
/// a limit of 0), against which nothing is refused. Exactly at the share is
/// refused: "below" is the rule, so a node that has spent precisely half
/// leaves the rest to forwarding.
pub fn own_traffic_allowed(lt_used_ms: u32, lt_cap_ms: u32, share_permille: u16) -> bool {
    if lt_cap_ms == 0 {
        return true;
    }
    (lt_used_ms as u64) * 1000 < (lt_cap_ms as u64) * (share_permille as u64)
}

/// What [`OwnShare::check`] decided for one originating site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnVerdict {
    /// Below the share: originate.
    Go,
    /// At or above the share: do not originate, retry on the caller's own
    /// cadence.
    Yield {
        /// True on the first refusal after a [`Go`](Self::Go) (or since
        /// boot), so the caller states the yield once per edge and not once
        /// per tick.
        edge: bool,
    },
}

/// The own-traffic share as one decision with memory of its edge, shared by
/// every site where a propagation node originates traffic.
///
/// One tracker for all sites, so a sync round refused and an offer answer
/// refused a second later are the same yield and produce one line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OwnShare {
    yielding: bool,
}

impl OwnShare {
    /// A tracker that has not yielded yet.
    pub const fn new() -> Self {
        Self { yielding: false }
    }

    /// Apply [`own_traffic_allowed`] and report whether a refusal is the
    /// edge into yielding.
    pub fn check(&mut self, lt_used_ms: u32, lt_cap_ms: u32, share_permille: u16) -> OwnVerdict {
        if own_traffic_allowed(lt_used_ms, lt_cap_ms, share_permille) {
            self.yielding = false;
            OwnVerdict::Go
        } else {
            let edge = !self.yielding;
            self.yielding = true;
            OwnVerdict::Yield { edge }
        }
    }

    /// Whether the last check refused.
    pub fn yielding(&self) -> bool {
        self.yielding
    }
}

/// What the LoRa interface does with a packet the core hands it.
///
/// Three outcomes, in the order they are decided: a carrier switched off by
/// the operator (`lnflash --set-media lora=off`) swallows the packet however
/// full the queue is, a lit carrier with a full queue refuses it with the
/// bound that bound, and otherwise the packet is queued with its room already
/// reserved. Was inline in the firmware's `LoRaInterface::try_send`, moved
/// here so a host test holds each arm (leviculum#413): no rig cell switches
/// the LoRa carrier off under traffic or fills the queue to its bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outbound {
    /// The carrier is off: answer `Ok` and drop the packet. `Ok` and not
    /// `BufferFull`, because a full buffer promises the packet another
    /// chance and this one never gets it, so the driver must not re-queue
    /// it. Nothing is reserved.
    Swallow,
    /// The queue is full: answer `BufferFull`, saying which bound refused.
    /// Nothing is reserved.
    Refuse(QueueBound),
    /// Room is reserved: the caller must enqueue the packet, or hand the
    /// room back through [`QueueBudget::channel_refused`].
    Queue,
}

/// Decide what happens to an outbound packet of `len` bytes. On
/// [`Outbound::Queue`] the room is already reserved in `budget`.
///
/// The carrier is read before the queue, so a dark carrier answers the same
/// whatever the queue holds. The same order as the BLE outbound path
/// (`leviculum_ble_tx::saturation::outbound_verdict`).
pub fn outbound_verdict(carrier_on: bool, budget: &QueueBudget, len: usize) -> Outbound {
    if !carrier_on {
        return Outbound::Swallow;
    }
    match budget.reserve(len) {
        Ok(()) => Outbound::Queue,
        Err(bound) => Outbound::Refuse(bound),
    }
}

impl QueueBudget {
    /// The channel refused a packet the budget had already reserved room for:
    /// hand the room back, so the caller answers `BufferFull` with the queue
    /// exactly as it found it.
    ///
    /// Unreachable while the budget's slot bound is the channel's own
    /// capacity, which is the firmware's arrangement. Handed back rather than
    /// asserted because the failure it guards against, a reservation leaked
    /// on every refusal, closes the queue for good one packet at a time.
    pub fn channel_refused(&self, len: usize) {
        self.release(len);
    }
}

/// What the transmit path does after one frame of a packet was keyed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameStep {
    /// The frame left the air: charge its airtime to the duty ledger, note
    /// the end for the spacing knob, and key the next frame if there is one.
    Charge,
    /// The radio refused the frame: charge nothing and key no further frame
    /// of this packet.
    Abort,
}

/// One packet's frames on their way to the air: one frame, or the two of a
/// split packet, keyed back to back.
///
/// Only a frame that left the air is charged, mirroring the RNode firmware's
/// `add_airtime()` after each `transmit()`. A frame the radio refused stops
/// the packet: the receiver's split reassembler expects the two halves back to
/// back, so a second half keyed after a failed first would be airtime spent on
/// a frame nobody can use. The packet counts as sent only if every frame went
/// out. Was inline in the firmware's `transmit_all_frames`, moved here so a
/// host test holds each arm (leviculum#413): no rig cell makes the radio
/// refuse a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketTx {
    frames: usize,
    sent: usize,
    aborted: bool,
}

impl PacketTx {
    /// A packet of `frames` frames, none keyed yet.
    pub const fn new(frames: usize) -> Self {
        Self {
            frames,
            sent: 0,
            aborted: false,
        }
    }

    /// The radio's answer for the next frame: `true` if it reported the frame
    /// on the air.
    pub fn frame_keyed(&mut self, on_air: bool) -> FrameStep {
        if on_air && !self.aborted {
            self.sent += 1;
            FrameStep::Charge
        } else {
            self.aborted = true;
            FrameStep::Abort
        }
    }

    /// Whether every frame of the packet went out: what the transmit counter
    /// and the display flash count.
    pub const fn sent(&self) -> bool {
        !self.aborted && self.sent == self.frames
    }
}

#[cfg(test)]
mod tests;
