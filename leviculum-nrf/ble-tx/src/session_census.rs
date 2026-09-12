//! Per-connection heap bookkeeping for the BLE session tasks
//! (Codeberg #388).
//!
//! The heap census walks its owners on demand, but a BLE session's
//! heap — the packets queued on its private outbound queue, the packet
//! its pump is currently fragmenting, the packets sitting in the
//! node-facing channels — lives inside embassy `Channel`s and task
//! locals the census walker cannot reach. Before this module those
//! bytes landed in the census's `other=` term, which is precisely the
//! blindness the #388 instruction names: the per-connection state is
//! what grows with a second phone.
//!
//! So the owners *report*: the fan-out adds a packet's bytes to its
//! link's slot when it queues it, the pump subtracts them when the
//! packet has left (or died), and teardown resets the slot — the same
//! mirror pattern as the defragmenter's `DEFRAG_HELD`. The counters
//! are best-effort mirrors, not ledgers: a decrement lost to a
//! cancelled future is corrected by the reset at the slot's next
//! claim/teardown, and `sub` saturates at zero so a lost *increment*
//! can never underflow into a preposterous census figure.
//!
//! Everything here is plain atomics, so it is exercised on the host
//! like the rest of this crate.

use core::sync::atomic::{AtomicUsize, Ordering};

/// One owner's held-bytes mirror: a single saturating byte counter.
///
/// Used for the node-facing inbound/outbound channels, whose packets
/// belong to the BLE interface as a whole rather than to one link.
pub struct HeldBytes {
    bytes: AtomicUsize,
}

impl HeldBytes {
    /// An empty counter (const, for statics).
    pub const fn new() -> Self {
        Self {
            bytes: AtomicUsize::new(0),
        }
    }

    /// Record `bytes` entering the owner's custody.
    pub fn add(&self, bytes: usize) {
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Record `bytes` leaving custody. Saturates at zero: an unmatched
    /// decrement must not wrap into a giant census figure.
    pub fn sub(&self, bytes: usize) {
        let _ = self
            .bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                Some(held.saturating_sub(bytes))
            });
    }

    /// Current holding in bytes.
    pub fn get(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }
}

impl Default for HeldBytes {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-link held-bytes mirrors, indexed by the drain slot — the same
/// per-link identity the drain table and the outbound queues use.
pub struct SessionHeld<const N: usize> {
    slots: [HeldBytes; N],
}

impl<const N: usize> SessionHeld<N> {
    /// All slots empty (const, for statics).
    pub const fn new() -> Self {
        Self {
            slots: [const { HeldBytes::new() }; N],
        }
    }

    /// Record `bytes` queued for the link on `slot`. Out-of-range slots
    /// are ignored (the caller's slot came from the drain table, which
    /// shares this bound).
    pub fn add(&self, slot: usize, bytes: usize) {
        if let Some(held) = self.slots.get(slot) {
            held.add(bytes);
        }
    }

    /// Record `bytes` leaving the link on `slot` (sent or dropped).
    /// Saturating, like [`HeldBytes::sub`].
    pub fn sub(&self, slot: usize, bytes: usize) {
        if let Some(held) = self.slots.get(slot) {
            held.sub(bytes);
        }
    }

    /// Zero one slot — at claim (after draining a previous tenancy's
    /// leftovers) and at teardown, the two moments the slot's queue is
    /// known empty of live custody.
    pub fn reset(&self, slot: usize) {
        if let Some(held) = self.slots.get(slot) {
            held.bytes.store(0, Ordering::Relaxed);
        }
    }

    /// One slot's holding in bytes.
    pub fn get(&self, slot: usize) -> usize {
        self.slots.get(slot).map(HeldBytes::get).unwrap_or(0)
    }

    /// Sum over all slots.
    pub fn total(&self) -> usize {
        self.slots.iter().map(HeldBytes::get).sum()
    }
}

impl<const N: usize> Default for SessionHeld<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_sub_roundtrip_per_slot() {
        let held: SessionHeld<4> = SessionHeld::new();
        held.add(0, 500);
        held.add(1, 300);
        held.add(0, 200);
        assert_eq!(held.get(0), 700);
        assert_eq!(held.get(1), 300);
        assert_eq!(held.total(), 1000);
        held.sub(0, 500);
        assert_eq!(held.get(0), 200);
        assert_eq!(held.total(), 500);
    }

    #[test]
    fn sub_saturates_instead_of_wrapping() {
        // A pump that decrements a packet the previous tenancy queued
        // (its increment was wiped by the claim-time reset) must leave
        // zero, not usize::MAX-ish garbage in the census.
        let held: SessionHeld<4> = SessionHeld::new();
        held.add(2, 100);
        held.sub(2, 400);
        assert_eq!(held.get(2), 0);
        assert_eq!(held.total(), 0);
    }

    #[test]
    fn reset_clears_only_its_slot() {
        let held: SessionHeld<4> = SessionHeld::new();
        held.add(0, 64);
        held.add(3, 128);
        held.reset(3);
        assert_eq!(held.get(3), 0);
        assert_eq!(held.get(0), 64);
    }

    #[test]
    fn out_of_range_slot_is_ignored() {
        let held: SessionHeld<2> = SessionHeld::new();
        held.add(7, 1000);
        held.sub(7, 1000);
        held.reset(7);
        assert_eq!(held.get(7), 0);
        assert_eq!(held.total(), 0);
    }

    #[test]
    fn held_bytes_counter_saturates_too() {
        let held = HeldBytes::new();
        held.add(10);
        held.sub(25);
        assert_eq!(held.get(), 0);
        held.add(42);
        assert_eq!(held.get(), 42);
    }
}
