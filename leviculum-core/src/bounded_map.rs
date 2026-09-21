//! Capacity-bounded map with FIFO drop-oldest eviction (Codeberg #421).
//!
//! The host storage's tables were bounded by expiry alone, so every ceiling
//! was arrival rate times expiry window: on a field node the reverse table
//! held 73 901 entries in one 8 minute window, and doubling the neighbours'
//! traffic doubled it. This type is the ceiling.
//!
//! It is the `std`-side sibling of `embedded_storage::OrderedMap`, which has
//! done the same job on the board since `ab45458d8`, and it keeps that type's
//! two semantics deliberately:
//!
//! * **Ring buffer, not refusal.** A full table accepts the new entry and
//!   drops an old one. A node that stops learning because a table filled is
//!   worse than a node that forgot something recoverable.
//! * **Refresh on re-insert.** Re-inserting a key moves it to the back of the
//!   FIFO. Every table here is keyed by something that is re-announced,
//!   re-used or re-touched while it matters, so refresh turns plain FIFO into
//!   drop-least-recently-refreshed without any table needing to say so.
//!
//! The one thing it does NOT share with `OrderedMap` is how order is stored.
//! `OrderedMap` scans its (at most 32) slots to find the oldest, which is free
//! at board capacities and quadratic at host ones — a 200 000 entry reverse
//! table at 154 inserts per second would scan 31 million entries per second.
//! Here the insertion sequence is a second `BTreeMap` keyed by the sequence
//! number, so the oldest entry is `order.first_key_value()`: O(log n) per
//! insert, at a cost of one `(u64, K)` pair per live entry.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// How many of the oldest entries a policy-driven eviction inspects before it
/// gives up and drops the plain oldest.
///
/// Policies here express a *preference* (drop a terminal receipt before a
/// pending one, an unvalidated link before a live one), not a prohibition, so
/// the scan may legitimately find no preferred victim. Scanning the whole
/// table for one would make an insert O(n) exactly when the table is full,
/// which is the case the ceiling exists for. Sixty-four is far more than any
/// table needs in practice — a receipts table where none of the 64 oldest
/// entries has reached a terminal state is a table with 64 genuinely
/// outstanding proofs — and it bounds the work regardless.
const EVICTION_SCAN: usize = 64;

/// A value together with the insertion sequence that orders it.
struct Slot<V> {
    seq: u64,
    value: V,
}

/// Map held to `capacity` entries, evicting the oldest to make room.
///
/// `K: Ord + Copy` because the key is duplicated into the order index; every
/// key in this crate's tables is a 16-byte hash or a small tuple of one.
pub struct BoundedMap<K, V>
where
    K: Ord + Copy,
{
    map: BTreeMap<K, Slot<V>>,
    /// Insertion sequence to key. `first_key_value()` is the oldest entry.
    order: BTreeMap<u64, K>,
    next_seq: u64,
    capacity: usize,
}

impl<K, V> BoundedMap<K, V>
where
    K: Ord + Copy,
{
    /// A map that holds at most `capacity` entries.
    ///
    /// A `capacity` of 0 is raised to 1: a table that cannot hold the entry
    /// just written would make every read miss, and the operator who typed a
    /// zero meant "as small as possible", not "disable this table".
    pub fn new(capacity: usize) -> Self {
        Self {
            map: BTreeMap::new(),
            order: BTreeMap::new(),
            next_seq: 0,
            capacity: capacity.max(1),
        }
    }

    /// The configured ceiling, for the storage census.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key).map(|s| &s.value)
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.map.get_mut(key).map(|s| &mut s.value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.map.iter().map(|(k, s)| (k, &s.value))
    }

    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.map.keys()
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.map.values().map(|s| &s.value)
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let slot = self.map.remove(key)?;
        self.order.remove(&slot.seq);
        Some(slot.value)
    }

    /// Insert with refresh-on-re-insert and drop-oldest eviction.
    ///
    /// Returns the evicted `(key, value)` when the insert had to make room,
    /// so a caller that owes something to a dropped entry (a receipt owes its
    /// timeout) can still deliver it.
    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        self.insert_preferring(key, value, |_, _| false)
    }

    /// Insert, preferring to evict the oldest entry `prefer` accepts.
    ///
    /// The scan runs from oldest to newest over at most `EVICTION_SCAN`
    /// entries and takes the first `prefer` returns `true` for; if none does,
    /// the plain oldest goes. A preference is never a veto — the insert
    /// always succeeds, which is the whole point of a ring buffer.
    pub fn insert_preferring(
        &mut self,
        key: K,
        value: V,
        prefer: impl Fn(&K, &V) -> bool,
    ) -> Option<(K, V)> {
        let seq = self.next_seq;
        self.next_seq += 1;

        if let Some(slot) = self.map.get_mut(&key) {
            // Refresh: same key, new value, back of the FIFO.
            self.order.remove(&slot.seq);
            slot.seq = seq;
            slot.value = value;
            self.order.insert(seq, key);
            return None;
        }

        let evicted = if self.map.len() >= self.capacity {
            self.evict(&prefer)
        } else {
            None
        };

        self.map.insert(key, Slot { seq, value });
        self.order.insert(seq, key);
        evicted
    }

    /// Insert only when the key is absent, leaving an existing entry (and its
    /// FIFO position) untouched. Returns `true` when the entry was created.
    ///
    /// This is the "first request wins" shape two tables need; an update would
    /// change their meaning, so they must not get refresh-on-re-insert.
    pub fn insert_if_absent(&mut self, key: K, value: V) -> bool {
        if self.map.contains_key(&key) {
            return false;
        }
        self.insert(key, value);
        true
    }

    /// Drop the victim the policy picks. Only called when the map is full.
    fn evict(&mut self, prefer: &impl Fn(&K, &V) -> bool) -> Option<(K, V)> {
        let mut victim = None;
        for (seq, key) in self.order.iter().take(EVICTION_SCAN) {
            if victim.is_none() {
                // The plain oldest, kept as the fallback.
                victim = Some((*seq, *key));
            }
            if let Some(slot) = self.map.get(key) {
                if prefer(key, &slot.value) {
                    victim = Some((*seq, *key));
                    break;
                }
            }
        }
        let (seq, key) = victim?;
        self.order.remove(&seq);
        self.map.remove(&key).map(|slot| (key, slot.value))
    }

    /// Retain entries matching `keep`, dropping the rest.
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        let order = &mut self.order;
        self.map.retain(|k, slot| {
            if keep(k, &slot.value) {
                true
            } else {
                order.remove(&slot.seq);
                false
            }
        });
    }
}

impl<K, V> BoundedMap<K, V>
where
    K: Ord + Copy,
    V: Clone,
{
    /// Drop every entry `keep` rejects and return them.
    ///
    /// Used by the expiry sweeps that must report what they dropped
    /// (`expire_paths`, `expire_receipts`, `expire_link_entries`).
    pub fn drain_rejected(&mut self, mut keep: impl FnMut(&K, &V) -> bool) -> Vec<(K, V)> {
        let mut dropped = Vec::new();
        let order = &mut self.order;
        self.map.retain(|k, slot| {
            if keep(k, &slot.value) {
                true
            } else {
                dropped.push((*k, slot.value.clone()));
                order.remove(&slot.seq);
                false
            }
        });
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_map_accepts_the_new_entry_and_drops_the_oldest() {
        let mut m: BoundedMap<u32, u32> = BoundedMap::new(4);
        for i in 0..4 {
            assert!(m.insert(i, i).is_none());
        }
        assert_eq!(m.len(), 4);

        let evicted = m.insert(4, 4);
        assert_eq!(evicted, Some((0, 0)), "the oldest entry is handed back");
        assert_eq!(m.len(), 4, "the ceiling holds");
        assert_eq!(m.get(&4), Some(&4), "the new entry is in");
        assert_eq!(m.get(&0), None, "the oldest is out");
    }

    #[test]
    fn sustained_insert_pressure_never_grows_the_map() {
        let mut m: BoundedMap<u32, u32> = BoundedMap::new(8);
        for i in 0..10_000 {
            m.insert(i, i);
            assert!(m.len() <= 8);
        }
        assert_eq!(m.len(), 8);
        assert_eq!(m.get(&9_999), Some(&9_999));
    }

    #[test]
    fn re_inserting_a_key_refreshes_its_place_in_the_queue() {
        let mut m: BoundedMap<u32, u32> = BoundedMap::new(3);
        m.insert(1, 10);
        m.insert(2, 20);
        m.insert(3, 30);
        // Refresh 1: it must now outlive 2.
        m.insert(1, 11);
        assert_eq!(m.len(), 3, "a refresh is not a growth");

        let evicted = m.insert(4, 40);
        assert_eq!(evicted, Some((2, 20)));
        assert_eq!(m.get(&1), Some(&11), "the refreshed entry survived");
    }

    #[test]
    fn insert_if_absent_keeps_the_first_value_and_its_position() {
        let mut m: BoundedMap<u32, u32> = BoundedMap::new(2);
        assert!(m.insert_if_absent(1, 10));
        assert!(!m.insert_if_absent(1, 99));
        assert_eq!(m.get(&1), Some(&10));

        m.insert_if_absent(2, 20);
        m.insert_if_absent(3, 30);
        assert_eq!(m.get(&1), None, "1 kept its age and went first");
        assert_eq!(m.get(&2), Some(&20));
    }

    #[test]
    fn a_preference_picks_the_oldest_entry_it_accepts() {
        // Values >= 100 are "droppable"; 1 is the oldest but not droppable.
        let mut m: BoundedMap<u32, u32> = BoundedMap::new(3);
        m.insert(1, 1);
        m.insert(2, 100);
        m.insert(3, 101);

        let evicted = m.insert_preferring(4, 4, |_, v| *v >= 100);
        assert_eq!(evicted, Some((2, 100)), "oldest droppable, not oldest");
        assert_eq!(m.get(&1), Some(&1), "the protected entry stayed");
    }

    #[test]
    fn a_preference_no_entry_satisfies_still_makes_room() {
        let mut m: BoundedMap<u32, u32> = BoundedMap::new(3);
        m.insert(1, 1);
        m.insert(2, 2);
        m.insert(3, 3);

        let evicted = m.insert_preferring(4, 4, |_, _| false);
        assert_eq!(evicted, Some((1, 1)), "falls back to the plain oldest");
        assert_eq!(m.len(), 3);
        assert_eq!(m.get(&4), Some(&4), "the insert still succeeded");
    }

    #[test]
    fn removing_and_retaining_keep_the_order_index_in_step() {
        let mut m: BoundedMap<u32, u32> = BoundedMap::new(4);
        for i in 0..4 {
            m.insert(i, i);
        }
        assert_eq!(m.remove(&0), Some(0));
        m.retain(|k, _| *k != 1);
        assert_eq!(m.len(), 2);

        // 2 is now the oldest; filling up must evict it, not a ghost.
        m.insert(10, 10);
        m.insert(11, 11);
        let evicted = m.insert(12, 12);
        assert_eq!(evicted, Some((2, 2)));
    }

    #[test]
    fn a_zero_capacity_still_holds_one_entry() {
        let mut m: BoundedMap<u32, u32> = BoundedMap::new(0);
        m.insert(1, 1);
        assert_eq!(m.get(&1), Some(&1));
        m.insert(2, 2);
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(&2), Some(&2));
    }

    #[test]
    fn drain_rejected_hands_back_what_it_dropped() {
        let mut m: BoundedMap<u32, u32> = BoundedMap::new(8);
        for i in 0..6 {
            m.insert(i, i * 10);
        }
        let mut dropped = m.drain_rejected(|_, v| *v >= 30);
        dropped.sort_unstable();
        assert_eq!(dropped, alloc::vec![(0, 0), (1, 10), (2, 20)]);
        assert_eq!(m.len(), 3);
    }
}
