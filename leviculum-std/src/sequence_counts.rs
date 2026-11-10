//! How many stored messages lie above a sequence cursor, in `O(log n)`
//! (order 1370, leviculum#478).
//!
//! A peer's unhandled count is the number of stored messages above its
//! sync cursor that may be offered to it. `--status` asks it for every
//! peer on every request and the rotation for the whole table every
//! 1344 s; over the store's walk that cost 4.7 ms a peer at 320 000
//! messages on the coder host, 94 ms a status request with 20 peers.
//!
//! [`SequenceCounts`] keeps every sequence the store holds, ascending, in
//! a `Vec<u64>` with a Fenwick tree over it: one slot a sequence, weight 1
//! while the entry is live and at most the counted size, 0 otherwise. A
//! count is one binary search and one prefix sum. Appends come in sequence
//! order, so an insert is a push; a removal zeroes its slot and leaves it,
//! and the table is rebuilt from the store's sequence index once dead
//! slots outnumber live ones, so the slots stay within twice the store.

#[cfg(test)]
std::thread_local! {
    /// Slots the counts touched: one per binary-search probe and one per
    /// Fenwick step. Pins the work, not the clock.
    static SLOT_VISITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[inline]
fn count_slot_visit() {
    #[cfg(test)]
    SLOT_VISITS.with(|visits| visits.set(visits.get() + 1));
}

#[cfg(test)]
pub(crate) fn slot_visits<T>(work: impl FnOnce() -> T) -> (T, u64) {
    SLOT_VISITS.with(|visits| visits.set(0));
    let result = work();
    (result, SLOT_VISITS.with(|visits| visits.get()))
}

/// What an insert needs from its store (module docs).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Inserted {
    Done,
    /// The sequence is below the newest slot and not one of the slots: the
    /// caller rebuilds from its sequence index.
    Rebuild,
}

/// Live, countable entries by sequence (module docs).
pub(crate) struct SequenceCounts {
    /// The largest size counted.
    largest_size: u64,
    /// Every slot's sequence, ascending.
    sequences: Vec<u64>,
    /// Whether the slot currently weighs 1.
    counted: Vec<bool>,
    /// Fenwick tree over the slots' weights, 1-based: `tree[0]` is unused.
    tree: Vec<u32>,
    /// Sum of all weights.
    total: u64,
    /// Slots whose entry left the store.
    dead: usize,
}

impl SequenceCounts {
    pub(crate) fn new(largest_size: u64) -> Self {
        Self {
            largest_size,
            sequences: Vec::new(),
            counted: Vec::new(),
            tree: vec![0],
            total: 0,
            dead: 0,
        }
    }

    /// The table over `entries`, `(sequence, size)` ascending by sequence,
    /// with room for `capacity` slots.
    pub(crate) fn rebuilt(
        largest_size: u64,
        capacity: usize,
        entries: impl Iterator<Item = (u64, u32)>,
    ) -> Self {
        let mut counts = Self::new(largest_size);
        counts.sequences.reserve_exact(capacity);
        counts.counted.reserve_exact(capacity);
        counts.tree.reserve_exact(capacity);
        for (sequence, size) in entries {
            counts.push(sequence, size);
        }
        counts
    }

    pub(crate) fn largest_size(&self) -> u64 {
        self.largest_size
    }

    fn prefix(&self, mut slots: usize) -> u64 {
        let mut sum = 0u64;
        while slots > 0 {
            count_slot_visit();
            sum += u64::from(self.tree[slots]);
            slots &= slots - 1;
        }
        sum
    }

    fn push(&mut self, sequence: u64, size: u32) {
        let weight = u32::from(u64::from(size) <= self.largest_size);
        let slot = self.tree.len();
        let below = slot - (slot & slot.wrapping_neg());
        let node = weight + (self.prefix(slot - 1) - self.prefix(below)) as u32;
        self.tree.push(node);
        self.sequences.push(sequence);
        self.counted.push(weight == 1);
        self.total += u64::from(weight);
    }

    fn lower(&mut self, index: usize) {
        let mut slot = index + 1;
        while slot < self.tree.len() {
            count_slot_visit();
            self.tree[slot] -= 1;
            slot += slot & slot.wrapping_neg();
        }
        self.counted[index] = false;
        self.total -= 1;
    }

    fn raise(&mut self, index: usize) {
        let mut slot = index + 1;
        while slot < self.tree.len() {
            count_slot_visit();
            self.tree[slot] += 1;
            slot += slot & slot.wrapping_neg();
        }
        self.counted[index] = true;
        self.total += 1;
    }

    fn find(&self, sequence: u64) -> Result<usize, usize> {
        self.sequences.binary_search_by(|probe| {
            count_slot_visit();
            probe.cmp(&sequence)
        })
    }

    /// One entry entered the store.
    pub(crate) fn insert(&mut self, sequence: u64, size: u32) -> Inserted {
        if self.sequences.last().is_none_or(|last| *last < sequence) {
            self.push(sequence, size);
            return Inserted::Done;
        }
        match self.find(sequence) {
            Ok(index) => {
                // A slot left dead by the same sequence's removal.
                if !self.counted[index] && u64::from(size) <= self.largest_size {
                    self.raise(index);
                }
                self.dead = self.dead.saturating_sub(1);
                Inserted::Done
            }
            Err(_) => Inserted::Rebuild,
        }
    }

    /// One entry left the store. Returns whether the caller should rebuild
    /// to reclaim dead slots.
    pub(crate) fn remove(&mut self, sequence: u64) -> bool {
        if let Ok(index) = self.find(sequence) {
            if self.counted[index] {
                self.lower(index);
            }
            self.dead += 1;
        }
        self.dead > 1024 && self.dead > self.sequences.len() / 2
    }

    /// Counted entries whose sequence is above `cursor`.
    pub(crate) fn above(&self, cursor: u64) -> u64 {
        let at_or_below = self.sequences.partition_point(|sequence| {
            count_slot_visit();
            *sequence <= cursor
        });
        self.total - self.prefix(at_or_below)
    }

    /// Heap bytes the table pins.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.heap_bytes_live_and_allocated().1
    }

    /// The three vectors' bytes by their length, and by their capacity:
    /// what the slots hold, and what the allocator handed out for them.
    pub(crate) fn heap_bytes_live_and_allocated(&self) -> (usize, usize) {
        let slot = std::mem::size_of::<u64>() + std::mem::size_of::<bool>();
        let fenwick = std::mem::size_of::<u32>();
        (
            self.sequences.len() * slot + self.tree.len() * fenwick,
            self.sequences.capacity() * std::mem::size_of::<u64>()
                + self.counted.capacity() * std::mem::size_of::<bool>()
                + self.tree.capacity() * fenwick,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn next_random(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// Random appends, removals, re-inserts of removed sequences and
    /// out-of-order inserts against a map counted by walking it.
    #[test]
    fn counts_answer_as_a_walk_of_the_live_entries() {
        let mut state = 0x9e37_79b9_7f4a_7c15;
        for case in 0..300 {
            let largest = 100 + next_random(&mut state) % 200;
            let mut counts = SequenceCounts::new(largest);
            let mut live: BTreeMap<u64, u32> = BTreeMap::new();
            let mut removed: Vec<(u64, u32)> = Vec::new();
            let mut next = 1u64;
            for _ in 0..(next_random(&mut state) % 400) {
                match next_random(&mut state) % 8 {
                    0..=3 => {
                        next += 1 + next_random(&mut state) % 3;
                        let size = (next_random(&mut state) % 400) as u32;
                        assert_eq!(counts.insert(next, size), Inserted::Done);
                        live.insert(next, size);
                    }
                    4 | 5 if !live.is_empty() => {
                        let at = (next_random(&mut state) % live.len() as u64) as usize;
                        let (&sequence, &size) = live.iter().nth(at).expect("an entry");
                        live.remove(&sequence);
                        removed.push((sequence, size));
                        if counts.remove(sequence) {
                            counts = SequenceCounts::rebuilt(
                                largest,
                                live.len(),
                                live.iter().map(|(sequence, size)| (*sequence, *size)),
                            );
                        }
                    }
                    6 if !removed.is_empty() => {
                        let (sequence, size) = removed.swap_remove(0);
                        if counts.insert(sequence, size) == Inserted::Rebuild {
                            live.insert(sequence, size);
                            counts = SequenceCounts::rebuilt(
                                largest,
                                live.len(),
                                live.iter().map(|(sequence, size)| (*sequence, *size)),
                            );
                        } else {
                            live.insert(sequence, size);
                        }
                    }
                    _ => {}
                }
                for cursor in [0, next / 3, next / 2, next.saturating_sub(2), next + 1] {
                    let walked = live
                        .iter()
                        .filter(|(sequence, size)| {
                            **sequence > cursor && u64::from(**size) <= largest
                        })
                        .count() as u64;
                    assert_eq!(counts.above(cursor), walked, "case {case}, cursor {cursor}");
                }
            }
        }
    }

    /// A count touches `O(log n)` slots: at 10 000 and 100 000 entries it
    /// visits at most twice `ceil(log2 n) + 1`, so ten times the entries cost
    /// at most eight more slots, where a walk visits all of them.
    #[test]
    fn a_count_visits_log_n_slots() {
        let mut per_size = Vec::new();
        for n in [10_000u64, 100_000] {
            let counts = SequenceCounts::rebuilt(
                u64::MAX,
                n as usize,
                (1..=n).map(|sequence| (sequence, 300)),
            );
            let (above, visits) = slot_visits(|| counts.above(n / 3));
            assert_eq!(above, n - n / 3);
            let log2_n = u64::from(n.next_power_of_two().trailing_zeros());
            assert!(visits <= 2 * (log2_n + 1), "{visits} visits at {n}");
            per_size.push(visits);
        }
        assert!(per_size[1] <= per_size[0] + 8, "{per_size:?}");
    }
}
