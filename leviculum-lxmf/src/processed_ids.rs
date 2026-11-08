//! The propagation node's processed-id duplicate cache, held in week
//! buckets of sorted 16-byte id prefixes (order 1370, leviculum#478).
//!
//! The reference keeps every transient id it accepted in up to six message
//! expiries, 180 days, in one dict, and walks all of it to prune
//! (`locally_processed_transient_ids`, `clean_transient_id_caches`,
//! `reference/LXMF/LXMF/LXMRouter.py:1018-1024`, the duplicate test at
//! `:2496`). At the field node's rate that is about 10 M ids. As a
//! `BTreeMap<TransientId, u64>` they cost 69 bytes an id, 696 MB resident at
//! 10 M, and the daily prune walked them for 165 ms on the coder host
//! (`lnpnd/src/walk_cost.rs`).
//!
//! **The representation.** One bucket per week of receive time. A bucket
//! is mostly *sealed*: a sorted `Vec<[u8; 16]>` and a fence table over the
//! ids' leading bits, so a lookup in a bucket reads one fence pair and
//! binary-searches about eight ids. What was remembered since the last
//! merge is *open*, a `BTreeSet`, merged into the sorted part when the
//! next prune finds the bucket's week over or the set
//! `OPEN_SEAL_LEN` (4096) long. Pruning drops whole buckets and walks no id.
//!
//! **Why a week.** A lookup searches every bucket, about 90 ns each at
//! 10 M ids on the coder host: day buckets cost 12.6 µs a lookup there,
//! week buckets 2.3 µs, the map 0.42 µs. A prune by whole buckets keeps an
//! id up to one bucket past the window, a week here; that only widens the
//! duplicate window, never narrows it.
//!
//! **Why exact, and why 16 bytes.** A duplicate the cache misses is a
//! message stored twice; a fresh message it wrongly calls a duplicate is a
//! message the sender is told was stored and that no recipient ever
//! receives. A Bloom filter makes the second kind at a configured rate, so
//! it is out. The cache keeps the first 16 bytes of each 32-byte
//! `SHA-256(lxmf_data)`: a fresh id then matches a stored one with
//! probability `n / 2^128`, about `3 · 10^-32` at 10 M ids, and making one
//! on purpose is a second preimage of a 128-bit truncation. Half the key is
//! half the RAM.
//!
//! **The window.** An id is answered for at least the window the caller
//! prunes with and at most one bucket longer
//! ([`crate::propagation_node::PROCESSED_ID_PRUNE_SLACK_SECS`], a day
//! before this order, when the map was walked once a day).
//!
//! **The count cap (order 1558, LV-1000).** The window alone bounds the
//! cache by time, not by size: at the soak's 2148 accepts a minute it
//! grows by about 50 MB a day for 180 days. [`ProcessedIds::set_cap`]
//! holds it to a count as well, oldest first: a week is then cut into
//! parts of an eighth of the cap each, and while the cache holds more than
//! the cap the oldest part goes whole, so the cache keeps between seven
//! eighths of the cap and the cap, and a lookup searches at most about
//! nine parts more than the weeks it spans. Without a cap (the default)
//! a week is one part and nothing changes.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use crate::propagation::TransientId;

/// Bytes of a transient id the cache keeps (module docs, "Why exact, and
/// why 16 bytes").
pub const PROCESSED_KEY_LEN: usize = 16;

/// The width of one bucket, in seconds (module docs, "Why a week").
pub const PROCESSED_BUCKET_SECS: u64 = 7 * 24 * 60 * 60;

type Key = [u8; PROCESSED_KEY_LEN];

/// About this many sealed ids share one fence slot: 128 bytes, two cache
/// lines, for half a byte of fence an id.
const IDS_PER_FENCE: usize = 8;

/// The fence table's widest index, so a bucket remembered all at once (a
/// restore) cannot ask for more than 64 MiB of fences.
const MAX_FENCE_BITS: u32 = 24;

#[cfg(test)]
std::thread_local! {
    /// Ids the cache compared or copied: one per binary-search probe and
    /// one per id a seal merges. Pins the work, not the clock.
    static ID_VISITS: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

#[inline]
fn count_id_visit() {
    #[cfg(test)]
    ID_VISITS.with(|visits| visits.set(visits.get() + 1));
}

fn key_of(transient_id: &TransientId) -> Key {
    let mut key = [0u8; PROCESSED_KEY_LEN];
    key.copy_from_slice(&transient_id[..PROCESSED_KEY_LEN]);
    key
}

/// The ids remembered for one bucket of receive time.
#[derive(Default)]
struct Day {
    /// Sorted, unique, disjoint from `open`.
    sealed: Vec<Key>,
    /// `fences[slot]` is the first index in `sealed` whose leading
    /// `fence_bits` bits are at least `slot`; `2^fence_bits + 1` entries
    /// while `sealed` holds any.
    fences: Vec<u32>,
    fence_bits: u32,
    /// Ids remembered since the day was last sealed.
    open: BTreeSet<Key>,
}

impl Day {
    fn slot(&self, key: &Key) -> usize {
        if self.fence_bits == 0 {
            return 0;
        }
        let mut lead = [0u8; 8];
        lead.copy_from_slice(&key[..8]);
        (u64::from_be_bytes(lead) >> (64 - self.fence_bits)) as usize
    }

    /// Where `key` is or would be in `sealed`, as `binary_search` answers.
    fn sealed_search(&self, key: &Key) -> Result<usize, usize> {
        if self.sealed.is_empty() {
            return Err(0);
        }
        let slot = self.slot(key);
        let start = self.fences[slot] as usize;
        let end = self.fences[slot + 1] as usize;
        self.sealed[start..end]
            .binary_search_by(|probe| {
                count_id_visit();
                probe.cmp(key)
            })
            .map(|at| start + at)
            .map_err(|at| start + at)
    }

    fn contains(&self, key: &Key) -> bool {
        self.sealed_search(key).is_ok() || self.open.contains(key)
    }

    fn insert(&mut self, key: Key) -> bool {
        self.sealed_search(&key).is_err() && self.open.insert(key)
    }

    fn remove(&mut self, key: &Key) -> bool {
        if self.open.remove(key) {
            return true;
        }
        match self.sealed_search(key) {
            Ok(at) => {
                self.sealed.remove(at);
                self.rebuild_fences();
                true
            }
            Err(_) => false,
        }
    }

    fn len(&self) -> usize {
        self.sealed.len() + self.open.len()
    }

    /// Merge `open` into `sealed`: both are sorted and disjoint, so one
    /// pass, into an allocation of exactly the merged length.
    fn seal(&mut self) {
        if self.open.is_empty() {
            return;
        }
        let open = core::mem::take(&mut self.open);
        let mut merged = Vec::with_capacity(self.sealed.len() + open.len());
        let mut sealed = self.sealed.iter().copied().peekable();
        for key in open {
            while let Some(next) = sealed.next_if(|next| *next < key) {
                count_id_visit();
                merged.push(next);
            }
            count_id_visit();
            merged.push(key);
        }
        for next in sealed {
            count_id_visit();
            merged.push(next);
        }
        self.sealed = merged;
        self.rebuild_fences();
    }

    fn rebuild_fences(&mut self) {
        let len = self.sealed.len();
        self.fence_bits = (len / IDS_PER_FENCE)
            .checked_ilog2()
            .unwrap_or(0)
            .min(MAX_FENCE_BITS);
        let slots = 1usize << self.fence_bits;
        let mut fences = Vec::with_capacity(slots + 1);
        for (at, key) in self.sealed.iter().enumerate() {
            let slot = self.slot(key);
            while fences.len() <= slot {
                fences.push(at as u32);
            }
        }
        fences.resize(slots + 1, len as u32);
        if len == 0 {
            fences.clear();
        }
        self.fences = fences;
    }

    fn heap_bytes(&self) -> usize {
        self.sealed.capacity() * PROCESSED_KEY_LEN
            + self.fences.capacity() * core::mem::size_of::<u32>()
            + leviculum_core::heap_census::btree_set_bytes(&self.open)
    }
}

/// An open set this large is merged into its bucket's sorted part at the
/// next prune even while the bucket's time is still running, so the
/// running bucket stays mostly sorted and the merge stays short.
const OPEN_SEAL_LEN: usize = 4096;

/// A capped cache is cut into about this many parts (module docs, "The
/// count cap"): dropping the oldest drops an eighth of the cap at most.
const PARTS_PER_CAP: usize = 8;

/// A bucket's parts are numbered in the low bits of its map key
/// ([`part_key`]): the key stays one integer, and a week holds up to 2^24
/// parts.
const PART_BITS: u32 = 24;

/// The map key of `part` of bucket `day`: bucket in the high bits, so keys
/// order by bucket, then part.
const fn part_key(day: u64, part: u64) -> u64 {
    (day << PART_BITS) | part
}

/// The bucket a map key belongs to.
const fn day_of(key: u64) -> u64 {
    key >> PART_BITS
}

/// The processed-id cache (module docs).
pub struct ProcessedIds {
    /// Keyed by bucket (`at_secs / bucket_secs`) and part within it
    /// ([`part_key`]).
    days: BTreeMap<u64, Day>,
    len: usize,
    bucket_secs: u64,
    /// The most ids held ([`Self::set_cap`]); `usize::MAX` holds every id
    /// the window keeps.
    cap: usize,
}

impl Default for ProcessedIds {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessedIds {
    /// Buckets of [`PROCESSED_BUCKET_SECS`].
    pub const fn new() -> Self {
        Self::with_bucket_secs(PROCESSED_BUCKET_SECS)
    }

    /// Buckets `bucket_secs` wide (at least one second): the width trades
    /// lookup time (one bucket search each) against how long past the
    /// window an id may stay.
    pub const fn with_bucket_secs(bucket_secs: u64) -> Self {
        Self {
            days: BTreeMap::new(),
            len: 0,
            bucket_secs: if bucket_secs == 0 { 1 } else { bucket_secs },
            cap: usize::MAX,
        }
    }

    /// Hold at most `cap` ids (at least one), oldest dropped first by
    /// whole parts (module docs, "The count cap"); a cache above the new
    /// cap drops its oldest parts now. Returns the ids dropped.
    pub fn set_cap(&mut self, cap: usize) -> usize {
        self.cap = cap.max(1);
        self.trim_to_cap()
    }

    /// The count the cache is held to, `usize::MAX` without a cap.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// The most ids one part takes before the next is opened.
    fn part_len(&self) -> usize {
        if self.cap == usize::MAX {
            usize::MAX
        } else {
            (self.cap / PARTS_PER_CAP).max(1)
        }
    }

    fn trim_to_cap(&mut self) -> usize {
        let mut dropped = 0;
        while self.len > self.cap {
            let Some((_, gone)) = self.days.pop_first() else {
                break;
            };
            dropped += gone.len();
            self.len -= gone.len();
        }
        dropped
    }

    /// Remember `transient_id` as processed at `at_secs`. An id already
    /// remembered in that bucket is not stored twice; one remembered in
    /// another is, and is answered for until the later bucket is pruned.
    ///
    /// With a cap, an id already held in any part of its bucket is not
    /// stored again, a full part opens the next, and the oldest parts go
    /// while the cache holds more than the cap.
    pub fn insert(&mut self, transient_id: &TransientId, at_secs: u64) {
        // A time past what a key can number shares the last bucket.
        let day = (at_secs / self.bucket_secs).min((u64::MAX >> PART_BITS) - 1);
        let key = key_of(transient_id);
        let last_of_day = part_key(day + 1, 0) - 1;
        let mut parts = self.days.range(part_key(day, 0)..=last_of_day);
        let at = match parts.next_back() {
            None => part_key(day, 0),
            Some((_, newest)) if newest.contains(&key) => return,
            Some(_) if parts.any(|(_, older)| older.contains(&key)) => return,
            Some((&last, newest)) if newest.len() >= self.part_len() => (last + 1).min(last_of_day),
            Some((&last, _)) => last,
        };
        if self.days.entry(at).or_default().insert(key) {
            self.len += 1;
        }
        self.trim_to_cap();
    }

    /// Whether `transient_id` was remembered and not yet pruned: one fence
    /// read and a binary search of about `IDS_PER_FENCE` (8) ids per bucket.
    pub fn contains(&self, transient_id: &TransientId) -> bool {
        let key = key_of(transient_id);
        self.days.values().any(|day| day.contains(&key))
    }

    /// Forget `transient_id` from every bucket that holds it.
    pub fn remove(&mut self, transient_id: &TransientId) {
        let key = key_of(transient_id);
        for day in self.days.values_mut() {
            if day.remove(&key) {
                self.len -= 1;
            }
        }
    }

    /// Drop every bucket whose last second is more than `window` before
    /// `now_secs`, so no id is forgotten younger than `window`, and merge
    /// the open set of every bucket that has ended or holds
    /// `OPEN_SEAL_LEN` (4096) open ids. Visits the buckets, and of the ids only
    /// those a merge copies. Returns the ids dropped.
    pub fn prune(&mut self, now_secs: u64, window: u64) -> usize {
        let mut dropped = 0;
        while let Some((&first, _)) = self.days.first_key_value() {
            let day = day_of(first);
            let last_second = day
                .saturating_mul(self.bucket_secs)
                .saturating_add(self.bucket_secs - 1);
            if last_second.saturating_add(window) >= now_secs {
                break;
            }
            if let Some((_, gone)) = self.days.pop_first() {
                dropped += gone.len();
            }
        }
        self.len -= dropped;
        let today = now_secs / self.bucket_secs;
        let part_len = self.part_len();
        for (&key, day) in self.days.iter_mut() {
            if day_of(key) < today || day.open.len() >= OPEN_SEAL_LEN || day.len() >= part_len {
                day.seal();
            }
        }
        dropped
    }

    /// Ids remembered, an id remembered in two buckets counted twice.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Heap bytes the cache pins (#388 census): the sorted ids and their
    /// fences exactly, the open sets and the bucket map by the census's
    /// B-tree model.
    pub fn heap_bytes(&self) -> usize {
        self.days.values().map(Day::heap_bytes).sum::<usize>()
            + leviculum_core::heap_census::btree_bytes_model(
                self.days.len(),
                core::mem::size_of::<u64>() + core::mem::size_of::<Day>(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap as StdMap;

    const DAY: u64 = 24 * 60 * 60;
    const BUCKET: u64 = PROCESSED_BUCKET_SECS;
    const WINDOW: u64 = 180 * DAY;

    fn next_random(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn random_id(state: &mut u64) -> TransientId {
        let mut transient_id = [0u8; 32];
        for chunk in transient_id.chunks_mut(8) {
            chunk.copy_from_slice(&next_random(state).to_le_bytes());
        }
        transient_id
    }

    /// The cache against the map it replaced, over random inserts at days
    /// in and out of order, removals, re-inserts on later days, and prunes
    /// at random times: every id the map holds by the reference's rule
    /// (`now - at <= window`) the cache answers for, a removed one it does
    /// not, and one whose day ended more than the window before the last
    /// prune it no longer does.
    #[test]
    fn the_cache_answers_as_the_map_it_replaced() {
        let mut state = 0x9e37_79b9_7f4a_7c15;
        for case in 0..60 {
            let mut cache = ProcessedIds::new();
            // id -> latest remember time, the old map's semantics.
            let mut oracle: StdMap<TransientId, u64> = StdMap::new();
            let mut ids: Vec<TransientId> = Vec::new();
            let mut now = 1_000 * BUCKET + next_random(&mut state) % BUCKET;
            let mut last_prune = 0;
            let operations = 50 + next_random(&mut state) % 400;
            for _ in 0..operations {
                match next_random(&mut state) % 10 {
                    0..=4 => {
                        let id = random_id(&mut state);
                        let at = now - next_random(&mut state) % (3 * DAY);
                        cache.insert(&id, at);
                        oracle.insert(id, at.max(oracle.get(&id).copied().unwrap_or(0)));
                        ids.push(id);
                    }
                    5 if !ids.is_empty() => {
                        let id = ids[(next_random(&mut state) % ids.len() as u64) as usize];
                        cache.insert(&id, now);
                        oracle.insert(id, now);
                    }
                    6 if !ids.is_empty() => {
                        let id = ids[(next_random(&mut state) % ids.len() as u64) as usize];
                        cache.remove(&id);
                        oracle.remove(&id);
                    }
                    7 => {
                        now += next_random(&mut state) % (40 * DAY);
                        cache.prune(now, WINDOW);
                        last_prune = now;
                    }
                    _ => {
                        now += next_random(&mut state) % (DAY / 2);
                    }
                }
                for id in &ids {
                    let answered = cache.contains(id);
                    let Some(at) = oracle.get(id).copied() else {
                        assert!(!answered, "case {case}: a removed id is still answered");
                        continue;
                    };
                    let age = now.saturating_sub(at);
                    if age <= WINDOW {
                        assert!(answered, "case {case}: an id {age} s old was forgotten");
                    }
                    let bucket_end = (at / BUCKET) * BUCKET + BUCKET - 1;
                    if last_prune > bucket_end + WINDOW {
                        assert!(!answered, "case {case}: a pruned bucket still answers");
                    }
                }
            }
        }
    }

    /// A sealed bucket answers for its ids with its fences built over
    /// several slots, and a removal from the sorted part rebuilds them.
    #[test]
    fn a_sealed_bucket_finds_and_removes_ids_through_its_fences() {
        let mut state = 0x2545_f491_4f6c_dd1d;
        let mut cache = ProcessedIds::new();
        let ids: Vec<TransientId> = (0..5_000).map(|_| random_id(&mut state)).collect();
        for id in &ids {
            cache.insert(id, 10 * BUCKET + 5);
        }
        cache.prune(11 * BUCKET, WINDOW);
        let day = cache.days.get(&part_key(10, 0)).expect("the bucket");
        assert!(day.open.is_empty(), "an ended bucket is sealed");
        assert_eq!(day.fence_bits, 9, "5000 ids over 512 slots");
        assert!(ids.iter().all(|id| cache.contains(id)));
        let absent: Vec<TransientId> = (0..5_000).map(|_| random_id(&mut state)).collect();
        assert!(absent.iter().all(|id| !cache.contains(id)));

        cache.remove(&ids[17]);
        assert!(!cache.contains(&ids[17]));
        assert!(ids.iter().skip(18).all(|id| cache.contains(id)));
        assert_eq!(cache.len(), 4_999);
    }

    /// Pruning drops whole buckets at the window plus at most a bucket,
    /// and counts what it dropped.
    #[test]
    fn a_bucket_is_dropped_once_its_last_second_is_past_the_window() {
        let mut cache = ProcessedIds::new();
        let early = [1u8; 32];
        let late = [2u8; 32];
        cache.insert(&early, 100 * BUCKET);
        cache.insert(&late, 100 * BUCKET + BUCKET - 1);
        // The late one is exactly the window old: kept, and so its bucket.
        assert_eq!(cache.prune(100 * BUCKET + BUCKET - 1 + WINDOW, WINDOW), 0);
        assert!(cache.contains(&early) && cache.contains(&late));
        assert_eq!(cache.prune(100 * BUCKET + BUCKET + WINDOW, WINDOW), 2);
        assert!(cache.is_empty() && !cache.contains(&early));
        assert_eq!(cache.heap_bytes(), 0);
    }

    /// Order 1558: a capped cache never holds more than its cap, keeps at
    /// least seven eighths of it once full, forgets the oldest ids first,
    /// and answers for every id it still holds; one held in an earlier
    /// part of its week is not stored twice.
    #[test]
    fn a_capped_cache_holds_its_cap_and_forgets_the_oldest_first() {
        let mut state = 0x5be0_cd19_137e_2179;
        let mut cache = ProcessedIds::new();
        assert_eq!(cache.set_cap(800), 0);
        let ids: Vec<TransientId> = (0..5_000).map(|_| random_id(&mut state)).collect();
        for (n, id) in ids.iter().enumerate() {
            // All inside one week: only the cap can drop anything.
            cache.insert(id, 10 * BUCKET + n as u64);
            assert!(
                cache.len() <= 800,
                "{} ids held over a cap of 800",
                cache.len()
            );
            if n >= 800 {
                assert!(cache.len() >= 700, "only {} ids held", cache.len());
            }
            if n % 997 == 0 {
                cache.prune(10 * BUCKET + n as u64, WINDOW);
            }
        }
        let held = cache.len();
        assert!(ids[ids.len() - held..].iter().all(|id| cache.contains(id)));
        assert!(ids[..ids.len() - held].iter().all(|id| !cache.contains(id)));
        // The newest part holds the last id; inserting it again adds none,
        // nor does an id from an older part of the same week.
        cache.insert(&ids[ids.len() - 1], 10 * BUCKET + 9_000);
        cache.insert(&ids[ids.len() - held], 10 * BUCKET + 9_001);
        assert_eq!(cache.len(), held);
        // A lower cap drops the oldest parts at once.
        let dropped = cache.set_cap(100);
        assert_eq!(cache.len(), held - dropped);
        assert!(cache.len() <= 100);
        assert!(cache.contains(&ids[ids.len() - 1]));
    }

    /// The cache keeps 16 bytes of the id: two ids that share them are one.
    #[test]
    fn ids_sharing_the_kept_prefix_are_one_entry() {
        let mut cache = ProcessedIds::new();
        let mut twin = [7u8; 32];
        cache.insert(&twin, BUCKET);
        twin[31] = 8;
        assert!(cache.contains(&twin));
        cache.insert(&twin, BUCKET);
        assert_eq!(cache.len(), 1);
    }

    fn visits<T>(work: impl FnOnce() -> T) -> (T, u64) {
        ID_VISITS.with(|visits| visits.set(0));
        let result = work();
        (result, ID_VISITS.with(|visits| visits.get()))
    }

    /// `ids` ids over the 180 days before `now`, every day sealed.
    fn spread(ids: usize, now: u64, state: &mut u64) -> ProcessedIds {
        let mut cache = ProcessedIds::new();
        for n in 0..ids {
            let at = now - WINDOW + (n as u64 * WINDOW) / ids as u64;
            cache.insert(&random_id(state), at);
        }
        cache.prune(now, WINDOW);
        cache
    }

    /// Order 1370: the work a lookup and a prune cost, at 18 000 and
    /// 180 000 ids over 180 days. A lookup probes about
    /// `log2(2 · IDS_PER_FENCE) + 1` ids a bucket whatever the bucket
    /// holds, so ten times the ids cost at most one probe more a bucket;
    /// the old map's descent grew with `log2 n`. The prune that ends a
    /// week merges that week's open ids and drops the oldest week, and
    /// visits no other id: at both sizes the visits are the week's own
    /// ids. At 1 000 ids a day the heap holds at most 17 bytes an id (16 of
    /// key, half a byte of fence, the bucket map), against 69 resident for
    /// the old map at 1 M and 10 M (`lnpnd/src/walk_cost.rs`).
    #[test]
    fn lookups_and_prunes_cost_what_a_bucket_holds_not_what_the_cache_holds() {
        let now = 200 * BUCKET;
        let mut state = 0x1319_8a2e_0370_7344;
        let mut per_size = Vec::new();
        for ids in [18_000usize, 180_000] {
            let mut cache = spread(ids, now, &mut state);
            let buckets = cache.days.len() as u64;
            let absent: Vec<TransientId> = (0..200).map(|_| random_id(&mut state)).collect();
            let (hits, probes) = visits(|| absent.iter().filter(|id| cache.contains(id)).count());
            assert_eq!(hits, 0);
            let probes_per_bucket = probes as f64 / (absent.len() as u64 * buckets) as f64;

            // A day's worth of new ids, below the merge length, then the
            // prune the week after: it merges that week and drops the
            // oldest, walking nothing else.
            let week: Vec<TransientId> = (0..ids / 180).map(|_| random_id(&mut state)).collect();
            assert!(week.len() < OPEN_SEAL_LEN);
            for id in &week {
                cache.insert(id, now + 10);
            }
            let held = cache.len();
            let (dropped, prune_visits) = visits(|| cache.prune(now + BUCKET, WINDOW));
            assert!(dropped > 0, "the oldest bucket goes");
            let sealed = cache
                .days
                .get(&part_key(now / BUCKET, 0))
                .map_or(0, Day::len) as u64;
            assert_eq!(sealed, week.len() as u64);
            assert_eq!(
                prune_visits, sealed,
                "{ids} ids: the prune visited {prune_visits}, the week it merged holds {sealed}"
            );
            let bytes_per_id = cache.heap_bytes() as f64 / cache.len() as f64;
            std::eprintln!(
                "PROCESSED_PIN ids={ids} held={held} buckets={buckets} \
                 probes_per_bucket={probes_per_bucket:.2} prune_visits={prune_visits} \
                 bytes_per_id={bytes_per_id:.2}"
            );
            if ids == 180_000 {
                assert!(bytes_per_id <= 17.0, "{bytes_per_id} bytes an id");
            }
            per_size.push(probes_per_bucket);
        }
        assert!(
            per_size[1] <= per_size[0] + 1.0,
            "ten times the ids cost more than a probe a bucket more: {per_size:?}"
        );
    }
}
