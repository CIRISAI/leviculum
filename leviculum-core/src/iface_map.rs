//! Small maps and sets keyed by interface index (order 1634, #474).
//!
//! The transport keeps about thirty tables keyed by an interface index: its
//! mode, its MTU, its announce cap, its rate windows, its held announces. As
//! `BTreeMap<usize, V>` each one was its own B-tree monomorph, 1 to 3 KB of
//! flash per value type, and on the RAK4631 image they added up to about
//! 21 KB of the 90 KB the B-tree code cost (1634 report). The tables never
//! hold more than one entry per live interface: four or five on a board, a
//! few dozen on a host, a few hundred on a hub whose TCP server gives every
//! client its own interface. A B-tree buys nothing at that size.
//!
//! [`IfaceMap`] keeps its entries in one `Vec<(usize, V)>` sorted by key.
//! Lookups are a binary search, O(log n); inserts and removes move at most
//! a few hundred entries; iteration is ascending by key exactly as the
//! B-tree's was. The struct is one `Vec`, the same three words a `BTreeMap`
//! is, so the node box the boards' heap budget pins does not grow (two
//! parallel vectors would have shared the search code across value types,
//! but cost 12 B more per table and broke `NODE_BOX_BYTES`). The API mirrors
//! the subset of `BTreeMap` the transport uses, so a call site reads the
//! same.
//!
//! [`IfaceSet`] is the same for `BTreeSet<usize>`.

use alloc::vec::Vec;
use core::iter::Map;
use core::mem::size_of;
use core::slice;

/// Borrowed entry as a `BTreeMap` iterator yields it.
type Pair<'a, V> = (&'a usize, &'a V);
/// Mutably borrowed entry as a `BTreeMap` iterator yields it.
type PairMut<'a, V> = (&'a usize, &'a mut V);

/// Iterator over `(&key, &value)` in ascending key order.
pub type Iter<'a, V> = Map<slice::Iter<'a, (usize, V)>, fn(&'a (usize, V)) -> Pair<'a, V>>;
/// Iterator over `(&key, &mut value)` in ascending key order.
pub type IterMut<'a, V> =
    Map<slice::IterMut<'a, (usize, V)>, fn(&'a mut (usize, V)) -> PairMut<'a, V>>;
/// Iterator over the keys in ascending order.
pub type Keys<'a, V> = Map<slice::Iter<'a, (usize, V)>, fn(&'a (usize, V)) -> &'a usize>;
/// Iterator over the values in ascending key order.
pub type Values<'a, V> = Map<slice::Iter<'a, (usize, V)>, fn(&'a (usize, V)) -> &'a V>;
/// Iterator over the values in ascending key order, mutable.
pub type ValuesMut<'a, V> =
    Map<slice::IterMut<'a, (usize, V)>, fn(&'a mut (usize, V)) -> &'a mut V>;

fn pair<V>(e: &(usize, V)) -> Pair<'_, V> {
    (&e.0, &e.1)
}

fn pair_mut<V>(e: &mut (usize, V)) -> PairMut<'_, V> {
    let (k, v) = e;
    (k, v)
}

fn key<V>(e: &(usize, V)) -> &usize {
    &e.0
}

fn value<V>(e: &(usize, V)) -> &V {
    &e.1
}

fn value_mut<V>(e: &mut (usize, V)) -> &mut V {
    &mut e.1
}

/// A map from interface index to `V`, sorted by index. See the module docs.
#[derive(Clone, PartialEq, Eq)]
pub struct IfaceMap<V> {
    entries: Vec<(usize, V)>,
}

impl<V> Default for IfaceMap<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: core::fmt::Debug> core::fmt::Debug for IfaceMap<V> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<V> IfaceMap<V> {
    /// An empty map; allocates nothing.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn search(&self, key: usize) -> Result<usize, usize> {
        self.entries.binary_search_by(|e| e.0.cmp(&key))
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the map holds no entry.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The value for `key`.
    pub fn get(&self, key: &usize) -> Option<&V> {
        match self.search(*key) {
            Ok(i) => self.entries.get(i).map(value),
            Err(_) => None,
        }
    }

    /// The value for `key`, mutably.
    pub fn get_mut(&mut self, key: &usize) -> Option<&mut V> {
        match self.search(*key) {
            Ok(i) => self.entries.get_mut(i).map(value_mut),
            Err(_) => None,
        }
    }

    /// True when `key` has an entry.
    pub fn contains_key(&self, key: &usize) -> bool {
        self.search(*key).is_ok()
    }

    /// Insert `value` under `key`; returns the value it replaced.
    pub fn insert(&mut self, key: usize, value: V) -> Option<V> {
        match self.search(key) {
            Ok(i) => self
                .entries
                .get_mut(i)
                .map(|e| core::mem::replace(&mut e.1, value)),
            Err(i) => {
                self.entries.insert(i, (key, value));
                None
            }
        }
    }

    /// Remove the entry for `key`; returns its value.
    pub fn remove(&mut self, key: &usize) -> Option<V> {
        match self.search(*key) {
            Ok(i) => Some(self.entries.remove(i).1),
            Err(_) => None,
        }
    }

    /// Drop every entry; keeps the allocation.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Keep only the entries for which `keep` returns true, in key order.
    pub fn retain<F: FnMut(&usize, &mut V) -> bool>(&mut self, mut keep: F) {
        self.entries.retain_mut(|e| keep(&e.0, &mut e.1));
    }

    /// Entries in ascending key order.
    pub fn iter(&self) -> Iter<'_, V> {
        self.entries
            .iter()
            .map(pair as fn(&(usize, V)) -> Pair<'_, V>)
    }

    /// Entries in ascending key order, values mutable.
    pub fn iter_mut(&mut self) -> IterMut<'_, V> {
        self.entries
            .iter_mut()
            .map(pair_mut as fn(&mut (usize, V)) -> PairMut<'_, V>)
    }

    /// Keys in ascending order.
    pub fn keys(&self) -> Keys<'_, V> {
        self.entries.iter().map(key as fn(&(usize, V)) -> &usize)
    }

    /// Values in ascending key order.
    pub fn values(&self) -> Values<'_, V> {
        self.entries.iter().map(value as fn(&(usize, V)) -> &V)
    }

    /// Values in ascending key order, mutable.
    pub fn values_mut(&mut self) -> ValuesMut<'_, V> {
        self.entries
            .iter_mut()
            .map(value_mut as fn(&mut (usize, V)) -> &mut V)
    }

    /// The smallest key above `key`, whether or not `key` itself has an
    /// entry: a key cursor that survives inserts and removes between steps.
    pub fn next_key_after(&self, key: usize) -> Option<usize> {
        let i = match self.search(key) {
            Ok(i) => i + 1,
            Err(i) => i,
        };
        self.entries.get(i).map(|e| e.0)
    }

    /// The entry for `key`, for in-place insert-or-update.
    pub fn entry(&mut self, key: usize) -> Entry<'_, V> {
        let pos = self.search(key);
        Entry {
            map: self,
            key,
            pos,
        }
    }

    /// Heap bytes the entry vector holds (its capacity, not its length),
    /// for the #388 heap census. The values' own heap is the caller's.
    pub fn heap_bytes(&self) -> usize {
        self.entries.capacity() * size_of::<(usize, V)>()
    }
}

/// `map[&key]`, as on a `BTreeMap`: panics when `key` has no entry. Tests
/// only; product code says `get`.
#[cfg(test)]
impl<V> core::ops::Index<&usize> for IfaceMap<V> {
    type Output = V;
    fn index(&self, key: &usize) -> &V {
        match self.get(key) {
            Some(v) => v,
            None => panic!("IfaceMap: no entry for interface {key}"),
        }
    }
}

impl<'a, V> IntoIterator for &'a IfaceMap<V> {
    type Item = Pair<'a, V>;
    type IntoIter = Iter<'a, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<V> FromIterator<(usize, V)> for IfaceMap<V> {
    fn from_iter<I: IntoIterator<Item = (usize, V)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

/// A vacant or occupied slot of an [`IfaceMap`], from [`IfaceMap::entry`].
pub struct Entry<'a, V> {
    map: &'a mut IfaceMap<V>,
    key: usize,
    pos: Result<usize, usize>,
}

impl<'a, V> Entry<'a, V> {
    /// The value, inserting `make()` first when the slot is vacant.
    pub fn or_insert_with<F: FnOnce() -> V>(self, make: F) -> &'a mut V {
        let i = match self.pos {
            Ok(i) => i,
            Err(i) => {
                self.map.entries.insert(i, (self.key, make()));
                i
            }
        };
        // In bounds: the search found `i`, or the insert above put it there.
        &mut self.map.entries[i].1
    }

    /// The value, inserting `value` first when the slot is vacant.
    pub fn or_insert(self, value: V) -> &'a mut V {
        self.or_insert_with(|| value)
    }

    /// The value, inserting `V::default()` first when the slot is vacant.
    pub fn or_default(self) -> &'a mut V
    where
        V: Default,
    {
        self.or_insert_with(V::default)
    }

    /// Apply `f` to the value when the slot is occupied.
    pub fn and_modify<F: FnOnce(&mut V)>(self, f: F) -> Self {
        if let Ok(i) = self.pos {
            if let Some(e) = self.map.entries.get_mut(i) {
                f(&mut e.1);
            }
        }
        self
    }
}

/// A set of interface indices, sorted. See the module docs.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct IfaceSet {
    keys: Vec<usize>,
}

impl core::fmt::Debug for IfaceSet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_set().entries(self.keys.iter()).finish()
    }
}

impl IfaceSet {
    /// An empty set; allocates nothing.
    pub const fn new() -> Self {
        Self { keys: Vec::new() }
    }

    /// Number of members.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// True when the set has no member.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// True when `key` is a member.
    pub fn contains(&self, key: &usize) -> bool {
        self.keys.binary_search(key).is_ok()
    }

    /// Add `key`; true when it was not a member before.
    pub fn insert(&mut self, key: usize) -> bool {
        match self.keys.binary_search(&key) {
            Ok(_) => false,
            Err(i) => {
                self.keys.insert(i, key);
                true
            }
        }
    }

    /// Remove `key`; true when it was a member.
    pub fn remove(&mut self, key: &usize) -> bool {
        match self.keys.binary_search(key) {
            Ok(i) => {
                self.keys.remove(i);
                true
            }
            Err(_) => false,
        }
    }

    /// Drop every member; keeps the allocation.
    pub fn clear(&mut self) {
        self.keys.clear();
    }

    /// Keep only the members for which `keep` returns true.
    pub fn retain<F: FnMut(&usize) -> bool>(&mut self, keep: F) {
        self.keys.retain(keep);
    }

    /// Members in ascending order.
    pub fn iter(&self) -> slice::Iter<'_, usize> {
        self.keys.iter()
    }

    /// Heap bytes the member vector holds (its capacity), for the #388
    /// heap census.
    pub fn heap_bytes(&self) -> usize {
        self.keys.capacity() * size_of::<usize>()
    }
}

impl<'a> IntoIterator for &'a IfaceSet {
    type Item = &'a usize;
    type IntoIter = slice::Iter<'a, usize>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl FromIterator<usize> for IfaceSet {
    fn from_iter<I: IntoIterator<Item = usize>>(iter: I) -> Self {
        let mut set = Self::new();
        for k in iter {
            set.insert(k);
        }
        set
    }
}

impl Extend<usize> for IfaceSet {
    fn extend<I: IntoIterator<Item = usize>>(&mut self, iter: I) {
        for k in iter {
            self.insert(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::{BTreeMap, BTreeSet};
    use alloc::vec;

    /// A deterministic op sequence (xorshift) run against both the B-tree
    /// and the sorted-vector map: every observable answer must agree, which
    /// is what "behaviour unchanged" means for the transport's tables.
    #[test]
    fn map_agrees_with_btreemap_over_random_ops() {
        let mut ours: IfaceMap<u32> = IfaceMap::new();
        let mut reference: BTreeMap<usize, u32> = BTreeMap::new();
        let mut x: u32 = 0x2545_f491;
        for step in 0..20_000u32 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let key = (x % 40) as usize;
            match x % 7 {
                0 | 1 => assert_eq!(ours.insert(key, step), reference.insert(key, step)),
                2 => assert_eq!(ours.remove(&key), reference.remove(&key)),
                3 => assert_eq!(ours.get(&key), reference.get(&key)),
                4 => {
                    *ours.entry(key).or_default() += 1;
                    *reference.entry(key).or_default() += 1;
                }
                5 => {
                    if let Some(v) = ours.get_mut(&key) {
                        *v ^= 1;
                    }
                    if let Some(v) = reference.get_mut(&key) {
                        *v ^= 1;
                    }
                }
                _ => {
                    let cut = x % 3;
                    ours.retain(|k, v| (*k as u32 + *v) % 3 != cut);
                    reference.retain(|k, v| (*k as u32 + *v) % 3 != cut);
                }
            }
            assert_eq!(ours.len(), reference.len());
            assert_eq!(ours.contains_key(&key), reference.contains_key(&key));
        }
        let a: Vec<(usize, u32)> = ours.iter().map(|(k, v)| (*k, *v)).collect();
        let b: Vec<(usize, u32)> = reference.iter().map(|(k, v)| (*k, *v)).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn map_iterates_ascending_whatever_the_insert_order() {
        let mut m: IfaceMap<&str> = IfaceMap::new();
        for k in [7usize, 2, 9, 0, 4] {
            m.insert(k, "x");
        }
        assert_eq!(m.keys().copied().collect::<Vec<_>>(), vec![0, 2, 4, 7, 9]);
        assert_eq!(m.iter().next_back().map(|(k, _)| *k), Some(9));
    }

    #[test]
    fn next_key_after_steps_over_present_and_absent_keys() {
        let m: IfaceMap<()> = [(2usize, ()), (5, ()), (9, ())].into_iter().collect();
        assert_eq!(m.next_key_after(0), Some(2));
        assert_eq!(m.next_key_after(2), Some(5));
        assert_eq!(m.next_key_after(3), Some(5));
        assert_eq!(m.next_key_after(9), None);
    }

    #[test]
    fn entry_or_insert_with_runs_only_when_vacant() {
        let mut m: IfaceMap<u8> = IfaceMap::new();
        *m.entry(3).or_insert_with(|| 10) += 1;
        *m.entry(3).or_insert_with(|| panic!("occupied")) += 1;
        assert_eq!(m.get(&3), Some(&12));
        m.entry(3).and_modify(|v| *v = 0).or_insert(99);
        m.entry(5).and_modify(|v| *v = 0).or_insert(99);
        assert_eq!(m.get(&3), Some(&0));
        assert_eq!(m.get(&5), Some(&99));
    }

    #[test]
    fn set_agrees_with_btreeset_over_random_ops() {
        let mut ours = IfaceSet::new();
        let mut reference: BTreeSet<usize> = BTreeSet::new();
        let mut x: u32 = 0x9e37_79b9;
        for _ in 0..20_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let key = (x % 40) as usize;
            match x % 4 {
                0 | 1 => assert_eq!(ours.insert(key), reference.insert(key)),
                2 => assert_eq!(ours.remove(&key), reference.remove(&key)),
                _ => assert_eq!(ours.contains(&key), reference.contains(&key)),
            }
            assert_eq!(ours.len(), reference.len());
        }
        assert!(ours.iter().eq(reference.iter()));
    }

    #[test]
    fn heap_bytes_follow_capacity_and_empty_is_zero() {
        let mut m: IfaceMap<u64> = IfaceMap::new();
        assert_eq!(m.heap_bytes(), 0);
        m.insert(1, 1);
        assert!(m.heap_bytes() >= size_of::<(usize, u64)>());
        let s = IfaceSet::new();
        assert_eq!(s.heap_bytes(), 0);
    }
}
