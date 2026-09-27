//! The two bounds, each shown to bind on its own.
//!
//! Every test drives the numbers the firmware actually ships
//! ([`LORA_QUEUE_SLOTS`] / [`LORA_QUEUE_BYTES`]), so a change to either
//! constant is a change to what these assert.

use super::*;

/// MTU-sized packet, the shape that makes the byte bound bind first.
const MTU: usize = 500;

#[test]
fn byte_bound_binds_far_below_the_slot_count() {
    let q = QueueBudget::new(LORA_QUEUE_SLOTS, LORA_QUEUE_BYTES);
    // 12 × 500 = 6000 B fits, 13 × 500 = 6500 B does not.
    let fits = LORA_QUEUE_BYTES / MTU;
    for _ in 0..fits {
        assert_eq!(q.reserve(MTU), Ok(()));
    }
    assert_eq!(q.reserve(MTU), Err(QueueBound::Bytes));
    // The point of the assertion: the queue is nowhere near full by slot
    // count, so nothing but the byte bound can have refused this.
    assert_eq!(q.queued_slots(), fits);
    assert!(
        q.queued_slots() * 4 < LORA_QUEUE_SLOTS,
        "slot count {} is not far below {}",
        q.queued_slots(),
        LORA_QUEUE_SLOTS
    );
    assert_eq!(q.queued_bytes(), fits * MTU);
}

#[test]
fn a_refused_packet_leaves_the_queue_exactly_as_it_found_it() {
    let q = QueueBudget::new(LORA_QUEUE_SLOTS, LORA_QUEUE_BYTES);
    for _ in 0..(LORA_QUEUE_BYTES / MTU) {
        assert_eq!(q.reserve(MTU), Ok(()));
    }
    let (slots, bytes) = (q.queued_slots(), q.queued_bytes());
    // Ten refusals in a row must not consume ten slots: the byte-bound path
    // hands the speculatively taken slot back.
    for _ in 0..10 {
        assert_eq!(q.reserve(MTU), Err(QueueBound::Bytes));
    }
    assert_eq!((q.queued_slots(), q.queued_bytes()), (slots, bytes));
}

#[test]
fn slot_bound_binds_on_tiny_packets() {
    let q = QueueBudget::new(LORA_QUEUE_SLOTS, LORA_QUEUE_BYTES);
    // A 20-byte packet: 64 of them are 1280 B, a fifth of the byte budget.
    for _ in 0..LORA_QUEUE_SLOTS {
        assert_eq!(q.reserve(20), Ok(()));
    }
    assert_eq!(q.reserve(20), Err(QueueBound::Slots));
    assert!(
        q.queued_bytes() * 4 < LORA_QUEUE_BYTES,
        "byte total {} is not far below {}",
        q.queued_bytes(),
        LORA_QUEUE_BYTES
    );
}

#[test]
fn dequeuing_frees_both_budgets() {
    let q = QueueBudget::new(LORA_QUEUE_SLOTS, LORA_QUEUE_BYTES);
    let fits = LORA_QUEUE_BYTES / MTU;
    for _ in 0..fits {
        assert_eq!(q.reserve(MTU), Ok(()));
    }
    assert_eq!(q.reserve(MTU), Err(QueueBound::Bytes));
    q.release(MTU);
    assert_eq!(q.queued_slots(), fits - 1);
    assert_eq!(q.queued_bytes(), (fits - 1) * MTU);
    assert_eq!(q.reserve(MTU), Ok(()));

    // And the same for the slot bound, with tiny packets.
    let q = QueueBudget::new(LORA_QUEUE_SLOTS, LORA_QUEUE_BYTES);
    for _ in 0..LORA_QUEUE_SLOTS {
        assert_eq!(q.reserve(1), Ok(()));
    }
    assert_eq!(q.reserve(1), Err(QueueBound::Slots));
    q.release(1);
    assert_eq!(q.reserve(1), Ok(()));
}

#[test]
fn draining_the_whole_queue_returns_it_to_empty() {
    let q = QueueBudget::new(LORA_QUEUE_SLOTS, LORA_QUEUE_BYTES);
    let lens = [1usize, 255, 500, 12, 300];
    for _ in 0..4 {
        for len in lens {
            assert_eq!(q.reserve(len), Ok(()));
        }
    }
    for _ in 0..4 {
        for len in lens {
            q.release(len);
        }
    }
    assert_eq!(q.queued_slots(), 0);
    assert_eq!(q.queued_bytes(), 0);
    // An empty queue admits a full MTU packet again.
    assert_eq!(q.reserve(MTU), Ok(()));
}

/// Control: ordinary traffic, well under both bounds, is admitted exactly as
/// it was before the bounds existed.
///
/// Without this a `reserve` that refuses everything would pass every test
/// above — they all assert refusals — while silencing the radio completely.
#[test]
fn control_ordinary_traffic_is_admitted_unchanged() {
    let q = QueueBudget::new(LORA_QUEUE_SLOTS, LORA_QUEUE_BYTES);
    // A realistic burst: an announce, a path response, a link proof, a data
    // packet. Four packets, ~600 B — the shape that overran the old 4-slot
    // queue and must sail through the new one.
    for (i, len) in [184usize, 60, 32, 300].into_iter().enumerate() {
        assert_eq!(q.reserve(len), Ok(()), "packet {i} refused");
    }
    assert_eq!(q.queued_slots(), 4);
    assert_eq!(q.queued_bytes(), 576);
    assert!(q.queued_bytes() < q.max_bytes());
    assert!(q.queued_slots() < q.max_slots());
}

/// Control: a queue that never refuses is not what was built. The old
/// behaviour — four slots, no byte bound — must be reachable, and refuse.
#[test]
fn control_the_old_four_slot_queue_still_behaves_like_four_slots() {
    let q = QueueBudget::new(4, usize::MAX);
    for _ in 0..4 {
        assert_eq!(q.reserve(MTU), Ok(()));
    }
    assert_eq!(q.reserve(MTU), Err(QueueBound::Slots));
}

#[test]
fn a_packet_larger_than_the_whole_budget_is_refused() {
    let q = QueueBudget::new(LORA_QUEUE_SLOTS, LORA_QUEUE_BYTES);
    assert_eq!(q.reserve(LORA_QUEUE_BYTES + 1), Err(QueueBound::Bytes));
    assert_eq!(q.queued_slots(), 0);
    assert_eq!(q.queued_bytes(), 0);
    // Exactly the budget still fits: the bound is inclusive.
    assert_eq!(q.reserve(LORA_QUEUE_BYTES), Ok(()));
}

/// A release the queue never saw must not wrap the counters shut.
#[test]
fn release_without_reserve_saturates_at_empty() {
    let q = QueueBudget::new(LORA_QUEUE_SLOTS, LORA_QUEUE_BYTES);
    q.release(MTU);
    assert_eq!(q.queued_slots(), 0);
    assert_eq!(q.queued_bytes(), 0);
    assert_eq!(q.reserve(MTU), Ok(()));
}

#[test]
fn bound_names_are_stable() {
    assert_eq!(QueueBound::Slots.as_str(), "slots");
    assert_eq!(QueueBound::Bytes.as_str(), "bytes");
}

/// The shipped numbers, asserted so a change to either is a deliberate one.
#[test]
fn shipped_bounds_are_the_reference_shape() {
    assert_eq!(LORA_QUEUE_BYTES, 6144, "CONFIG_QUEUE_SIZE for T114/RAK4631");
    assert_eq!(LORA_QUEUE_SLOTS, 64);
    // The byte bound must be the one that binds under large packets, which is
    // only true while the slot count exceeds budget/MTU. Both operands are
    // compile-time constants, so this is a const block: it fails the build
    // rather than one test run (same move as c746bf8).
    const { assert!(LORA_QUEUE_SLOTS > LORA_QUEUE_BYTES / 500) };
}

// ---------------------------------------------------------------------------
// The age rule behind an engaged airtime lock (Codeberg #433).
// ---------------------------------------------------------------------------

/// The dequeue side of the LoRa task's hold, with the I/O taken out: a FIFO of
/// enqueue stamps, a clock the test moves by hand, and the airtime lock as a
/// bool. One `turn` is one iteration of the task's TX branch — it consults
/// [`hold_verdict`] for the frame at the head and does what the firmware does
/// with the answer.
///
/// The firmware's own atomic and log line sit on top of `stale_drops` and
/// `aired`; what is modelled here is the decision and the order, which is the
/// part that has to hold on a host.
struct HoldLoop {
    queue: std::vec::Vec<u32>,
    now_ms: u32,
    locked: bool,
    /// Ages, in order, of the frames the rule threw away.
    stale_drops: std::vec::Vec<u32>,
    /// Ages, in order, of the frames that reached the radio.
    aired: std::vec::Vec<u32>,
    /// When each of those frames was keyed, same order as `aired`.
    aired_at: std::vec::Vec<u32>,
}

impl HoldLoop {
    fn new() -> Self {
        Self {
            queue: std::vec::Vec::new(),
            now_ms: 0,
            locked: false,
            stale_drops: std::vec::Vec::new(),
            aired: std::vec::Vec::new(),
            aired_at: std::vec::Vec::new(),
        }
    }

    fn enqueue(&mut self) {
        self.queue.push(self.now_ms);
    }

    fn advance(&mut self, ms: u32) {
        self.now_ms = self.now_ms.wrapping_add(ms);
    }

    /// One turn of the transmit branch. `false` when the queue is empty.
    fn turn(&mut self) -> bool {
        if self.queue.is_empty() {
            return false;
        }
        let enqueued = self.queue[0];
        match hold_verdict(enqueued, self.now_ms, self.locked) {
            HoldVerdict::Key => {
                self.queue.remove(0);
                self.aired.push(self.now_ms.wrapping_sub(enqueued));
                self.aired_at.push(self.now_ms);
            }
            HoldVerdict::Hold { .. } => {}
            HoldVerdict::DropStale { age_ms } => {
                self.queue.remove(0);
                self.stale_drops.push(age_ms);
            }
        }
        true
    }
}

/// The rule itself: held while locked, dropped once the clock passes the age,
/// and the drop is the thing the counter sees.
#[test]
fn a_frame_held_past_the_age_under_the_lock_is_dropped_and_counted() {
    let mut h = HoldLoop::new();
    h.enqueue();
    h.locked = true;
    // A whole minute of holding, in ten-second turns: nothing airs, and
    // nothing is dropped until the age is passed.
    h.advance(10_000);
    h.turn();
    assert_eq!(h.stale_drops, [], "10 s is inside the age");
    assert_eq!(h.aired, [], "the lock is engaged, so nothing may be keyed");
    assert_eq!(h.queue.len(), 1, "the frame is still held");
    h.advance(10_001);
    h.turn();
    assert_eq!(
        h.stale_drops,
        [20_001],
        "past HOLD_MAX_AGE_MS the frame must be dropped, with its age"
    );
    assert_eq!(h.aired, [], "a dropped frame does not reach the radio");
    assert!(
        h.queue.is_empty(),
        "the drop must free the head of the queue"
    );
}

/// The other half: a frame younger than the age is kept, and keyed the moment
/// the lock lets go. The hold is a hold, not a slow drop.
#[test]
fn a_frame_below_the_age_is_keyed_when_the_lock_releases() {
    let mut h = HoldLoop::new();
    h.enqueue();
    h.locked = true;
    h.advance(HOLD_MAX_AGE_MS - 1);
    h.turn();
    assert_eq!(h.stale_drops, []);
    assert_eq!(h.aired, []);
    h.locked = false;
    h.turn();
    assert_eq!(
        h.aired,
        [HOLD_MAX_AGE_MS - 1],
        "it must be keyed, not dropped"
    );
    assert_eq!(h.stale_drops, []);
}

/// The rule bites only under the lock. A frame that waited for CSMA, for an
/// acquisition jitter draw or behind a burst can be arbitrarily old and is
/// still keyed: those waits end by themselves, and dropping on them would
/// make the interface lossy where nothing is wrong.
#[test]
fn an_old_frame_is_keyed_when_the_lock_is_not_engaged() {
    let mut h = HoldLoop::new();
    h.enqueue();
    h.advance(10 * HOLD_MAX_AGE_MS);
    assert_eq!(
        hold_verdict(0, h.now_ms, false),
        HoldVerdict::Key,
        "unlocked, age is not a reason to drop"
    );
    h.turn();
    assert_eq!(h.aired, [10 * HOLD_MAX_AGE_MS]);
    assert_eq!(h.stale_drops, []);
}

/// The boundary, both sides of it: equal to the age is still a hold, one
/// millisecond past it is a drop.
#[test]
fn the_age_boundary_is_inclusive_on_the_hold_side() {
    assert_eq!(
        hold_verdict(0, HOLD_MAX_AGE_MS, true),
        HoldVerdict::Hold {
            age_ms: HOLD_MAX_AGE_MS
        }
    );
    assert_eq!(
        hold_verdict(0, HOLD_MAX_AGE_MS + 1, true),
        HoldVerdict::DropStale {
            age_ms: HOLD_MAX_AGE_MS + 1
        }
    );
}

/// A frame enqueued just before the `u32` millisecond stamp wraps (49.7 days
/// of uptime) still has its true age. `wrapping_sub` is what makes that true;
/// a saturating subtraction would read zero and hold the frame forever.
#[test]
fn the_age_survives_the_uptime_stamp_wrapping() {
    let enqueued = u32::MAX - 5_000;
    let now = 16_000u32; // 21 s later, across the wrap
    assert_eq!(
        hold_verdict(enqueued, now, true),
        HoldVerdict::DropStale { age_ms: 21_001 }
    );
}

/// The field failure, in the regime it was measured in (#255, 2026-09-27): a
/// standing 132 s backlog, the lock pinned at the cap and dipping once every
/// 12 s to admit one frame, and the task taking a hold turn every 200 ms in
/// between (the order of the post-TX receive window — the field capture shows
/// 860 `holding` lines against ~85 frames aired in 17 minutes, ten holds per
/// key-up). The rule has to bound the age of what reaches the air, because
/// that age is what the relay's link-table entry is racing.
///
/// The claim is about the steady state, and the phase of the first dip is left
/// free: a dip that falls on the very first turn after the rule starts looking
/// keys one fossil, because the rule deliberately does not drop while the lock
/// is off. So the loop runs every phase, and what is asserted is that one dip
/// interval is enough to bound everything after it.
#[test]
fn the_rule_bounds_the_age_of_everything_that_airs() {
    /// One hold turn per post-TX receive window.
    const TURN_MS: u32 = 200;
    /// Turns between two budget dips: 12 s, the measured drain spacing.
    const TURNS_PER_DIP: u32 = 12_000 / TURN_MS;

    for phase in 0..TURNS_PER_DIP {
        let mut h = HoldLoop::new();
        // Twelve frames enqueued 12 s apart: the oldest is 132 s old by the
        // time the last one arrives, which is the backlog the Pocket carried.
        for _ in 0..12 {
            h.enqueue();
            h.advance(12_000);
        }
        let backlog = h.queue.len();
        let regime_start = h.now_ms;

        // Ten minutes of the pinned regime. The lock is engaged on every turn
        // but the dip, and the phone keeps handing over one frame per 12 s.
        let turns = 600_000 / TURN_MS;
        for turn in 0..turns {
            if turn % TURNS_PER_DIP == TURNS_PER_DIP / 2 {
                h.enqueue();
            }
            h.locked = turn % TURNS_PER_DIP != phase;
            h.turn();
            h.advance(TURN_MS);
        }

        assert!(
            !h.aired.is_empty(),
            "phase {phase}: the rule must not starve the radio"
        );
        // One dip interval in, nothing older than the cap may still be
        // reaching the air: that is the property the relay entry needs.
        let settled = regime_start + 12_000;
        for (age, at) in h.aired.iter().zip(&h.aired_at) {
            if *at < settled {
                continue;
            }
            assert!(
                *age <= HOLD_MAX_AGE_MS,
                "phase {phase}: a frame keyed at {age} ms, {} ms into the \
                 regime, is older than the cap, so its proof cannot reach the \
                 relay entry that expires at 30 s",
                at - regime_start
            );
        }
        // The fossils went, every one of them either counted as a stale drop
        // or keyed by the one dip the rule lets through — never silently lost.
        assert!(
            h.stale_drops.len() >= backlog - 1,
            "phase {phase}: only {} of the {backlog}-frame backlog was counted \
             as stale",
            h.stale_drops.len()
        );
        // And once the backlog is gone the regime loses nothing more: the
        // phone's 12 s cadence and the cap's 12 s drain balance, so all 50
        // later frames air.
        assert_eq!(
            h.aired.len() + h.stale_drops.len() + h.queue.len(),
            backlog + 50,
            "phase {phase}: every frame is keyed, counted stale, or queued"
        );
        assert!(
            h.aired.len() >= 49,
            "phase {phase}: only {} of the 50 frames the balanced regime \
             handed over aired",
            h.aired.len()
        );
    }
}

/// The derivation, asserted rather than only written down: the cap has to sit
/// above the short-term airtime window (a lock that engaged on short-term
/// airtime alone releases within 2 × 7500 ms as the bins roll off, and a cap
/// below that would drop frames the lock was about to release) and below the
/// measured field topology's relay-entry deadline minus the measured return
/// leg, (1 + 2 + 2) × 6 s − 1.0 s.
#[test]
fn the_age_sits_between_the_two_bounds_it_is_derived_from() {
    const SHORT_TERM_WINDOW_MS: u32 = 2 * 7_500;
    const FIELD_ENTRY_DEADLINE_MS: u32 = (1 + 2 + 2) * 6_000;
    const MEASURED_RETURN_LEG_MS: u32 = 1_000;
    // Const blocks: both operands are compile-time constants, so these fail
    // the build rather than one test run (the move c746bf8 made, and 358's).
    // A const `assert!` takes no formatted message, hence the plain strings.
    const {
        assert!(
            HOLD_MAX_AGE_MS > SHORT_TERM_WINDOW_MS,
            "a short-term lock releases within its own window; a cap at or \
             below it would drop frames the lock was about to release"
        )
    };
    const {
        assert!(
            HOLD_MAX_AGE_MS <= FIELD_ENTRY_DEADLINE_MS - MEASURED_RETURN_LEG_MS,
            "a frame keyed later than that produces a proof with no relay \
             entry left to reach"
        )
    };
    // The value shipped, so a change to it is a deliberate one.
    assert_eq!(HOLD_MAX_AGE_MS, 20_000);
}
