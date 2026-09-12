//! Heap-census estimators (Codeberg #388).
//!
//! The nRF firmware's 96 KiB heap is exhausted by BLE links plus the
//! propagation role, and the `[HEAP]` line only says *how much* is gone,
//! never *who holds it*. This module is the arithmetic half of the census:
//! walkers that estimate the heap bytes a collection currently pins,
//! computed at the allocation owners on demand — not a global allocator
//! hook, because `embedded-alloc`'s `LlffHeap` has no per-allocation
//! tagging to attribute a block to its owner.
//!
//! # Estimation model, stated so the numbers can be argued with
//!
//! * `Vec<T>` / `VecDeque<T>` spine: `capacity() * size_of::<T>()` —
//!   exact for `Vec`, exact for `VecDeque` up to its power-of-two
//!   rounding (the capacity reported *is* the allocated slot count).
//!   Content held behind the spine (e.g. `Vec<Vec<u8>>`) is the owner's
//!   job to add.
//! * `BTreeMap<K, V>` / `BTreeSet<K>`: alloc's B-tree (B = 6) allocates
//!   nodes of **11 entry slots** each, so a one-entry map still pins a
//!   full node — for a large `V` (a `Link` is several hundred bytes)
//!   that single node is kilobytes, which is exactly the kind of cost
//!   this census exists to surface. Model: `ceil(len / 7)` nodes
//!   (interior nodes average ~2/3 full), each `11·(K+V)` plus two words
//!   of header. Internal nodes' edge arrays (~1 node in 12) are ignored;
//!   the estimate is a floor, not an audit.
//!
//! Every walker here is O(len) at worst and allocation-free, so the
//! census can run from the firmware main loop at a coarse cadence
//! without disturbing the heap it measures.

extern crate alloc;

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;
use core::mem::size_of;

/// Max entries per B-tree node in `alloc`'s implementation (2B-1, B=6).
const BTREE_NODE_CAPACITY: usize = 11;

/// Assumed average entries per node: leaves after random insertion run
/// ~2/3 full; `7 ≈ 11 · 2/3`.
const BTREE_NODE_TYPICAL_FILL: usize = 7;

/// Heap bytes of a `Vec`'s spine (its one backing allocation). Contents
/// behind further pointers are the caller's to add.
pub fn vec_bytes<T>(v: &Vec<T>) -> usize {
    v.capacity() * size_of::<T>()
}

/// Heap bytes of a `VecDeque`'s ring buffer.
pub fn vec_deque_bytes<T>(v: &VecDeque<T>) -> usize {
    v.capacity() * size_of::<T>()
}

/// Estimated heap bytes of the B-tree node structure for `len` entries
/// of `key_size + val_size` bytes each. See the module docs for the
/// model; `btree_map_bytes`/`btree_set_bytes` are the typed front ends.
pub fn btree_bytes_model(len: usize, entry_size: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let nodes = len.div_ceil(BTREE_NODE_TYPICAL_FILL);
    nodes * (2 * size_of::<usize>() + BTREE_NODE_CAPACITY * entry_size)
}

/// Estimated heap bytes of a `BTreeMap`'s node structure (spine only).
pub fn btree_map_bytes<K, V>(m: &BTreeMap<K, V>) -> usize {
    btree_bytes_model(m.len(), size_of::<K>() + size_of::<V>())
}

/// Estimated heap bytes of a `BTreeSet`'s node structure.
pub fn btree_set_bytes<T>(s: &BTreeSet<T>) -> usize {
    btree_bytes_model(s.len(), size_of::<T>())
}

/// One node-level heap census (Codeberg #388): estimated bytes held per
/// subsystem of a `NodeCore`, as computed by `NodeCore::heap_census`.
/// All figures are heap bytes; `node_struct` is the one block the boxed
/// node itself occupies (`size_of::<NodeCore<..>>`, which inlines the
/// storage), everything else is what hangs off it.
#[derive(Debug, Default, Clone, Copy)]
pub struct NodeHeapCensus {
    /// `size_of` the node struct itself — the `Box<NodeCore>` block.
    pub node_struct: usize,
    /// Live entries in the link table.
    pub link_count: usize,
    /// Link table nodes plus per-link content (channel rings, cached
    /// proofs, remote identities) — excluding resources in flight.
    pub links: usize,
    /// Resource transfers in flight: incoming reassembly, outgoing
    /// staging, multi-segment plans, across all links.
    pub resources: usize,
    /// Event queue spine (`Vec<NodeEvent>` capacity). Event payloads are
    /// drained every dispatch and are transient by construction.
    pub events: usize,
    /// Request machinery: handlers, pending requests, resource
    /// correlations, link retry state and id aliases.
    pub requests: usize,
    /// Registered destinations (names, ratchet state).
    pub destinations: usize,
    /// Transport-owned dynamic state: pending actions, queued announces,
    /// interface bookkeeping maps.
    pub transport: usize,
    /// What the `Storage` impl reports via `Storage::heap_bytes` (for
    /// the firmware's `EmbeddedStorage`: the announce-cache and ratchet
    /// value `Vec`s — the maps themselves are heapless and inside
    /// `node_struct`).
    pub storage: usize,
}

impl NodeHeapCensus {
    /// Sum of every accounted field, `node_struct` included.
    pub fn total(&self) -> usize {
        self.node_struct
            + self.links
            + self.resources
            + self.events
            + self.requests
            + self.destinations
            + self.transport
            + self.storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn vec_spine_is_capacity_times_elem() {
        let mut v: Vec<u32> = Vec::with_capacity(10);
        v.push(1);
        assert_eq!(vec_bytes(&v), 10 * 4);
        let empty: Vec<u64> = Vec::new();
        assert_eq!(vec_bytes(&empty), 0);
    }

    #[test]
    fn vec_deque_spine_counts_allocated_slots() {
        let mut d: VecDeque<[u8; 16]> = VecDeque::with_capacity(4);
        d.push_back([0u8; 16]);
        assert!(vec_deque_bytes(&d) >= 4 * 16);
        let empty: VecDeque<u8> = VecDeque::new();
        assert_eq!(vec_deque_bytes(&empty), 0);
    }

    #[test]
    fn btree_empty_is_zero() {
        let m: BTreeMap<u32, u32> = BTreeMap::new();
        assert_eq!(btree_map_bytes(&m), 0);
        let s: BTreeSet<u64> = BTreeSet::new();
        assert_eq!(btree_set_bytes(&s), 0);
    }

    #[test]
    fn btree_single_entry_pins_a_full_node() {
        // The point of the model: one entry with a fat value still costs
        // a whole 11-slot node — never report `len * entry` for that.
        let mut m: BTreeMap<[u8; 16], [u8; 512]> = BTreeMap::new();
        m.insert([0u8; 16], [0u8; 512]);
        let est = btree_map_bytes(&m);
        assert!(
            est >= BTREE_NODE_CAPACITY * (16 + 512),
            "single-entry estimate {est} below one node"
        );
    }

    #[test]
    fn btree_estimate_grows_linearly_and_stays_above_len_times_entry() {
        let mut m: BTreeMap<u64, u64> = BTreeMap::new();
        for i in 0..100u64 {
            m.insert(i, i);
        }
        let est = btree_map_bytes(&m);
        // Floor: at least the raw entry bytes.
        assert!(est >= 100 * 16, "estimate {est} below raw entries");
        // Ceiling sanity: no more than one full node per 2 entries.
        assert!(est <= 50 * (2 * size_of::<usize>() + 11 * 16));
    }

    #[test]
    fn model_matches_hand_arithmetic() {
        // 14 entries of 24 bytes: ceil(14/7)=2 nodes, each 2 words + 11*24.
        let expected = 2 * (2 * size_of::<usize>() + 11 * 24);
        assert_eq!(btree_bytes_model(14, 24), expected);
        let _ = vec![0u8; 1]; // keep alloc linked in no_std test builds
    }
}
