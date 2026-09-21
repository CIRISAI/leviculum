//! Heapless Storage implementation for embedded targets (nRF52840).
//!
//! Uses `heapless::FnvIndexMap` and `heapless::IndexSet` with compile-time
//! capacities instead of `BTreeMap`/`BTreeSet`. Eliminates allocator overhead
//! for map containers, only variable-size data fields (`Vec<u8>` in announce
//! cache and ratchet keys) still use the heap allocator.
//!
//! Local client (shared-instance) methods are no-ops, embedded nodes are the
//! daemon, not a client of a daemon.

extern crate alloc;

use alloc::collections::BTreeSet;
use alloc::vec;
use alloc::vec::Vec;

use heapless::FnvIndexMap;
use heapless::FnvIndexSet;

use crate::constants::{RATCHET_SIZE, RECEIPT_RETENTION_MS, TRUNCATED_HASHBYTES};

/// Bytes of a 32-byte dedup hash actually stored (#388): the packet
/// dedup rings key on the first 16 bytes. Dedup is a pure membership
/// question over uniformly distributed SHA-256 packet hashes, 2^128
/// distinctness is beyond any collision this mesh can produce, and
/// halving the key width returns 8 KiB of the firmware node box.
/// Applies ONLY to keys that are hashes end to end — a structured key
/// (see `path_request_tag_set`) is not truncatable.
const DEDUP_KEY_BYTES: usize = 16;

/// Entries one dedup generation is allowed before it rotates. Half the
/// `FnvIndexSet` capacity below, mirroring `MemoryStorage`: the live window
/// is 128-256 packets across the rotation, not 256.
const PACKET_CACHE_GENERATION_CAP: usize = 128;

/// The stored dedup key of a full 32-byte hash.
fn dedup_key(hash: &[u8; 32]) -> [u8; DEDUP_KEY_BYTES] {
    let mut key = [0u8; DEDUP_KEY_BYTES];
    key.copy_from_slice(&hash[..DEDUP_KEY_BYTES]);
    key
}
use crate::identity::Identity;
use crate::storage_census::CollectionCount;
use crate::storage_types::{
    AnnounceEntry, AnnounceRateEntry, LinkEntry, PacketReceipt, PathEntry, PathState,
    ReceiptStatus, ReverseEntry,
};
use crate::traits::Storage;

/// Storage implementation using heapless collections for embedded targets.
///
/// All map capacities are compile-time constants matching the embedded sizing
/// analysis: path_table=32, announce_table=8, link_table=8, etc.
///
/// When a collection is full, insert operations evict the oldest entry
/// rather than panicking. This matches the MemoryStorage overflow behavior./// the protocol handles missing entries gracefully via timeouts and retransmits.
///
/// **Size note**: heapless collections are inline, so this struct IS the
/// bulk of the firmware's boxed `NodeCore` (the `node_box=` figure in the
/// `[HEAP_CENSUS]` line, Codeberg #388). On Cortex-M4 it must be placed in
/// a `static` or `Box`, not on the stack.
///
/// # Byte budget (#388), per field
///
/// Approximate inline bytes per map, measured on the 64-bit host (the
/// heapless maps carry no pointers in their entries, so the 32-bit target
/// differs only in the few `usize`/`Vec` fields inside entry values —
/// the target total is a little smaller). Per-slot cost of an
/// `OrderedMap<K, V, N>` is roughly `K + 4 (seq) + V + 2 (hash) + 4
/// (index)` plus padding, times N, allocated whether the slots are used
/// or not:
///
/// | field                     | slots | ~bytes |
/// |---------------------------|-------|--------|
/// | packet_cache (x2)         | 2x256 | 11 300 |
/// | known_identities          |     8 |  4 400 |
/// | path_table                |    32 |  3 400 |
/// | announce_table            |    16 |  1 800 |
/// | announce_rate_table       |    32 |  1 500 |
/// | path_request_tag_set      |    32 |  1 300 |
/// | link_table                |     8 |    900 |
/// | receipts                  |     8 |    900 |
/// | announce_cache            |    16 |    800 |
/// | reverse_table             |    16 |    800 |
/// | known_ratchets            |     8 |    600 |
/// | rest (small maps)         |       |  1 000 |
///
/// The guard below (`embedded_storage_base_size_guard`, plus the 32-bit
/// compile-time assertion) holds the total; grow a capacity only with
/// the heap budget (`HEAP_BUDGET`, leviculum-nrf) re-run.
pub struct EmbeddedStorage {
    // Packet dedup (two-generation ring)
    /// Truncated SHA-256 hashes of packets seen recently, current
    /// generation. The stored key is the FIRST [`DEDUP_KEY_BYTES`] bytes
    /// of the full packet hash (#388): dedup only ever asks "seen this
    /// exact hash before?", and a 16-byte truncation keeps 2^128
    /// distinctness — an accidental collision (a fresh packet wrongly
    /// dropped as a duplicate) is orders of magnitude less likely than a
    /// bit error the protocol already survives. Full 32-byte keys cost
    /// 8 KiB more of the 96 KiB firmware heap across both generations.
    ///
    /// **Re-insert semantics:** insert is idempotent (set membership);
    /// position never changes for an already-present hash.
    ///
    /// **Eviction policy on overflow:** when this generation exceeds 128
    /// entries (half capacity), it is rotated into `packet_cache_prev`
    /// and a fresh empty cache replaces it. No per-entry FIFO eviction.
    /// See `rotate_packet_cache`.
    ///
    /// **Typical access pattern:** every received packet inserts;
    /// every received packet reads (dedup check across both generations).
    ///
    /// **Capacity:** 256 — deliberately NOT shrunk in #388: the window
    /// (128-256 packets across the rotation) is what stops a relayed
    /// duplicate from re-entering the mesh, and halving it changes
    /// semantics under burst load; the byte cost was taken out of the
    /// key width instead.
    packet_cache: FnvIndexSet<[u8; DEDUP_KEY_BYTES], 256>,

    /// Previous-generation packet dedup ring (see `packet_cache`).
    ///
    /// Holds hashes from the prior rotation. Read on every dedup check;
    /// not written to directly, gets the contents of `packet_cache` on
    /// rotation and is cleared when the next rotation promotes it again.
    ///
    /// **Capacity:** 256.
    packet_cache_prev: FnvIndexSet<[u8; DEDUP_KEY_BYTES], 256>,

    // Path table
    /// Routing entries for known destinations: hops, expiry, next-hop,
    /// receiving interface.
    ///
    /// **Re-insert semantics:** refresh-on-re-insert. A re-announce of
    /// the same destination must move the entry to the back of the FIFO
    /// so an active path is not evicted by unrelated newer entries.
    /// Implemented in `set_path` via `remove + insert`.
    ///
    /// **Eviction policy on overflow:** drop the entry that was inserted
    /// earliest (after any refreshes have moved their entries to the
    /// back). Time-based cleanup happens via `expire_paths`.
    ///
    /// **Typical access pattern:** inserter is the announce/path-proof
    /// handler in `transport.rs` (frequent under network churn);
    /// reader is every routing decision (very frequent).
    ///
    /// **Capacity:** 32.
    path_table: OrderedMap<[u8; TRUNCATED_HASHBYTES], PathEntry, 32>,

    /// Per-destination path quality state (`Unknown` / `Unresponsive` /
    /// `Responsive`), used to allow same-emission worse-hop announces
    /// for a path marked unresponsive.
    ///
    /// **Re-insert semantics:** refresh-on-re-insert. Tracks current
    /// state for an active path; should not be evicted just because the
    /// state changes during a churn burst.
    ///
    /// **Eviction policy on overflow:** drop oldest by insertion order.
    /// `clean_stale_path_metadata` removes entries whose key is no
    /// longer in `path_table`.
    ///
    /// **Typical access pattern:** inserter is the responsiveness
    /// tracker in `transport.rs` (frequent); reader is the
    /// path-acceptance gate (per-announce).
    ///
    /// **Capacity:** 32.
    path_states: OrderedMap<[u8; TRUNCATED_HASHBYTES], PathState, 32>,

    // Announce
    /// Cached announce metadata for rate-limiting and rebroadcast
    /// scheduling: timestamps, retry counts, raw packet bytes,
    /// retransmit deadlines.
    ///
    /// **Re-insert semantics:** refresh-on-re-insert. Re-announces
    /// update retransmit schedule; an entry waiting on retransmit must
    /// not be silently dropped by an unrelated newer announce.
    ///
    /// **Eviction policy on overflow:** drop oldest by insertion order.
    /// Explicit removal via `remove_announce` (e.g. on rate-limit
    /// blocking).
    ///
    /// **Typical access pattern:** inserter is announce reception and
    /// path-request response in `transport.rs` (frequent); reader is
    /// `get_announce_mut` for rate checking (frequent).
    ///
    /// **Capacity:** 16.
    announce_table: OrderedMap<[u8; TRUNCATED_HASHBYTES], AnnounceEntry, 16>,

    /// Raw cached announce packet bytes, for serving path-request
    /// responses without waiting for the next live announce.
    ///
    /// **Re-insert semantics:** refresh-on-re-insert. Re-cached on each
    /// new validated announce of the same destination.
    ///
    /// **Eviction policy on overflow:** drop oldest by insertion order.
    /// `clean_announce_cache` removes entries whose key is no longer in
    /// `path_table` and not a local destination.
    ///
    /// **Typical access pattern:** inserter is announce validation
    /// (frequent); reader is path-request response building.
    ///
    /// **Capacity:** 16.
    announce_cache: OrderedMap<[u8; TRUNCATED_HASHBYTES], Vec<u8>, 16>,

    /// Per-destination announce rate-tracking: last accepted timestamp,
    /// violation count, blocked-until deadline.
    ///
    /// **Re-insert semantics:** refresh-on-re-insert. Updated on every
    /// announce; the entries describing currently-rate-limited
    /// destinations must outlive the eviction pressure they themselves
    /// generate.
    ///
    /// **Eviction policy on overflow:** drop oldest by insertion order.
    /// Implicit cleanup via `clean_stale_path_metadata` when the
    /// matching `path_table` entry is removed.
    ///
    /// **Typical access pattern:** inserter is the rate-check on every
    /// inbound announce (constant); reader is the rate gate.
    ///
    /// **Capacity:** 32.
    announce_rate_table: OrderedMap<[u8; TRUNCATED_HASHBYTES], AnnounceRateEntry, 32>,

    // Routing
    /// Active links routed through this transport node: timestamp,
    /// next-hop interface, validation state, proof deadline,
    /// destination identifier.
    ///
    /// **Re-insert semantics:** refresh-on-re-insert. An active link
    /// must outlive a burst of new (and likely shorter-lived) link
    /// setups.
    ///
    /// **Eviction policy on overflow:** drop oldest by insertion order.
    /// Time-based cleanup via `expire_link_entries` (proof timeout for
    /// unvalidated, link timeout for validated).
    ///
    /// **Typical access pattern:** inserter is link setup, proof, and
    /// validation paths in `transport.rs` (sometimes); reader is
    /// link-routed data forwarding (frequent).
    ///
    /// **Capacity:** 8.
    link_table: OrderedMap<[u8; TRUNCATED_HASHBYTES], LinkEntry, 8>,

    /// Reverse-routing entries for proof responses: which interface a
    /// forwarded packet arrived on and which interface it went out on.
    ///
    /// **Re-insert semantics:** refresh-on-re-insert. A re-forwarded
    /// destination should refresh its position; old entries are
    /// explicitly removed by `expire_reverses` and on proof receipt.
    ///
    /// **Eviction policy on overflow:** drop oldest by insertion order.
    /// `expire_reverses(now, timeout)` cleans by age;
    /// `remove_reverse_entries_for_interface` cleans on interface down.
    ///
    /// **Typical access pattern:** inserter is data-packet forwarding
    /// (sometimes); reader is proof handling (which then removes the
    /// entry).
    ///
    /// **Capacity:** 16.
    reverse_table: OrderedMap<[u8; TRUNCATED_HASHBYTES], ReverseEntry, 16>,

    /// Outstanding discovery requests: "if announces for this
    /// destination arrive, send PATH_RESPONSE to this interface."
    ///
    /// **Re-insert semantics:** FIFO-by-insertion. The setter
    /// (`set_discovery_path_request`) has an explicit
    /// `if !contains_key` guard, only the first request per key is
    /// recorded, mirroring Python behavior. Subsequent requests for
    /// the same destination are dropped, NOT used to refresh the
    /// position.
    ///
    /// **Eviction policy on overflow:** drop oldest by insertion order.
    /// Explicit removal via `remove_discovery_path_request` after
    /// delivery; time-based cleanup via `expire_discovery_path_requests`.
    ///
    /// **Typical access pattern:** inserter is path-request reception
    /// for unknown destinations; reader is the announce-handler when
    /// it learns the destination (sends a PATH_RESPONSE back to the
    /// requesting interface).
    ///
    /// **Capacity:** 4.
    discovery_path_requests: OrderedMap<[u8; TRUNCATED_HASHBYTES], (usize, u64), 4>,

    // Path requests
    /// Last-sent timestamp for outbound path requests, used as the
    /// rate-limit gate (one request per destination per
    /// `PATH_REQUEST_MIN_INTERVAL_MS`).
    ///
    /// **Re-insert semantics:** refresh-on-re-insert. Surviving the
    /// eviction race is the whole point of the table, if the rate-
    /// limit timestamp gets evicted, the next request is treated as
    /// fresh and the rate limit is bypassed.
    ///
    /// **Eviction policy on overflow:** drop oldest by insertion order.
    /// No explicit cleanup; entries age out implicitly through the
    /// rate-limit check.
    ///
    /// **Typical access pattern:** inserter is path-request emission
    /// (frequent); reader is the rate-limit gate before the next
    /// emission.
    ///
    /// **Capacity:** 8.
    path_requests: OrderedMap<[u8; TRUNCATED_HASHBYTES], u64, 8>,

    /// Inbound path-request dedup tags (hash of dest_hash + source
    /// tag), so a relayed request isn't acted on twice.
    ///
    /// **Re-insert semantics:** N/A (set, no values). A re-checked tag
    /// short-circuits without insertion; only fresh tags are inserted.
    ///
    /// **Eviction policy on overflow:** drop oldest tag by insertion
    /// order. Eviction happens inline in `check_path_request_tag`
    /// (not `map_set`), and uses the same insertion-order rebuild as
    /// `map_set` to defeat the heapless `swap_remove` reordering.
    ///
    /// **Typical access pattern:** inserter / reader is the path-
    /// request dedup check (very frequent under flooding).
    ///
    /// **Capacity:** 32. Deliberately NOT truncated like the packet
    /// rings (#388): this key is STRUCTURED, not a hash — the caller
    /// builds it as `dest_hash(16) ‖ request_tag(16)`
    /// (`transport.rs`, grep `dedup_key`), so keeping only the first
    /// [`DEDUP_KEY_BYTES`] bytes collapses every request for one
    /// destination into a single key and the second request is wrongly
    /// deduplicated (caught by
    /// `mvr_relay_pr_from_next_hop::peer_up_pull_recovers_…`, which
    /// re-requests the same destination on its cadence).
    path_request_tag_set: OrderedSet<[u8; 32], 32>,

    // Identity / security
    /// Cached remote `Identity` (Ed25519 + X25519 public keys), keyed
    /// by destination hash, for signature verification.
    ///
    /// **Re-insert semantics:** refresh-on-re-insert. Re-insert is
    /// rare in normal operation (identities are quasi-permanent), but
    /// when it does occur (e.g. identity refresh on a new announce)
    /// the entry is treated as "still alive" and bumped to the back.
    ///
    /// **Eviction policy on overflow:** drop oldest by insertion
    /// order. No explicit cleanup; identities persist until evicted.
    ///
    /// **Typical access pattern:** inserter is announce processing
    /// (rare); reader is signature verification on receive (frequent
    /// for the active set, but goes through other code paths).
    ///
    /// **Capacity:** 8, down from 16 in #388: an `Identity` is ~512
    /// inline bytes (dalek verifying keys precompute their Edwards
    /// point), so this table was the second-largest field, sized for a
    /// host. A board meshes with a handful of peers; an evicted
    /// identity is re-learned from the peer's next announce, exactly
    /// like an evicted path.
    ///
    /// **Who may depend on these slots (#388 pass 3):** NOT the
    /// propagation role's sync peers — up to 16 of them would cycle
    /// this table twice over, so each peer keeps its own 64-byte key
    /// copy in the peer store (`leviculum-lxmf::peering::Peer::
    /// public_keys`, captured at announce time) and recalls that copy
    /// first, this cache second. What the 8 slots serve is the
    /// delivery side: the two phones' delivery identities plus the few
    /// transport neighbours whose announces are being verified at any
    /// one time — a working set of well under 8, and every miss heals
    /// on the sender's next announce (mobile phones announce every
    /// 300 s; only the role's peers announce as rarely as stock lxmd's
    /// six hours, and they no longer live here).
    known_identities: OrderedMap<[u8; TRUNCATED_HASHBYTES], Identity, 8>,

    /// Cached remote ratchet public keys, used for batch decryption of
    /// packets sent under the sender's current ratchet.
    ///
    /// **Re-insert semantics:** refresh-on-re-insert. Refreshed on
    /// every announce containing the ratchet, exactly the
    /// `path_table` pattern, and an active sender's ratchet must
    /// outlive an unrelated burst.
    ///
    /// **Eviction policy on overflow:** drop oldest by insertion
    /// order. Time-based cleanup via `expire_known_ratchets`.
    ///
    /// **Typical access pattern:** inserter is announce reception
    /// (frequent); reader is decryption (frequent for active
    /// destinations).
    ///
    /// **Capacity:** 8.
    known_ratchets: OrderedMap<[u8; TRUNCATED_HASHBYTES], ([u8; RATCHET_SIZE], u64), 8>,

    /// Sender-side ratchet private keys (serialized) for our local
    /// destinations, persisted across rotations so old messages remain
    /// decipherable until the rotation window expires.
    ///
    /// **Re-insert semantics:** refresh-on-re-insert. Re-stored on
    /// each ratchet rotation; rotation is the signal that the entry
    /// is still hot.
    ///
    /// **Eviction policy on overflow:** drop oldest by insertion
    /// order. No explicit cleanup; entries persist until destination
    /// is removed or eviction strikes.
    ///
    /// **Typical access pattern:** inserter is ratchet rotation in
    /// `node/mod.rs` (sometimes); reader is loading at startup or on
    /// re-key.
    ///
    /// **Capacity:** 4.
    dest_ratchet_keys: OrderedMap<[u8; TRUNCATED_HASHBYTES], Vec<u8>, 4>,

    // Receipts
    /// Pending-delivery `PacketReceipt`s: what we sent, when, and
    /// whether the proof has been received.
    ///
    /// **Re-insert semantics:** refresh-on-re-insert. In-flight
    /// receipts must outlive newer ones submitted just after them.
    ///
    /// **Insert paths:**
    /// - `transport.rs:940`: `create_receipt`: fresh-key (truncated
    ///   hash of the just-sent packet).
    /// - `transport.rs:960`: `create_receipt_with_timeout`: fresh-key
    ///   (same shape as `create_receipt`).
    /// - `transport.rs:980`: `mark_receipt_delivered`: re-insert
    ///   (read existing receipt, clone, mutate `status` to
    ///   `Delivered`, write back under same hash). This is the only
    ///   re-insert path; it benefits from the refresh because an
    ///   application reading the delivery status shortly after must
    ///   not race a churn-eviction.
    ///
    /// **Eviction policy on overflow:** drop oldest by insertion
    /// order. Time-based cleanup via `expire_receipts` for receipts
    /// whose `Sent` status has timed out.
    ///
    /// **Typical access pattern:** inserter is `create_receipt` /
    /// `mark_receipt_delivered`; reader is application status polling
    /// and proof handling.
    ///
    /// **Capacity:** 8.
    receipts: OrderedMap<[u8; TRUNCATED_HASHBYTES], PacketReceipt, 8>,
}

// #388 instruction step 1: the bound on the node box's dominant part,
// enforced where the type lives. On the 32-bit target this struct is the
// bulk of the one `Box<NodeCore>` block the `[HEAP_CENSUS]` line reports
// as `node_box=`; the same bound is host-tested (with a 64-bit figure) in
// `embedded_storage_base_size_guard` below.
#[cfg(target_pointer_width = "32")]
const _: () = assert!(
    core::mem::size_of::<EmbeddedStorage>() <= 31_000,
    "EmbeddedStorage outgrew its #388 heap budget on the 32-bit target"
);

impl EmbeddedStorage {
    /// Create a new EmbeddedStorage with all collections empty.
    pub fn new() -> Self {
        Self {
            packet_cache: FnvIndexSet::new(),
            packet_cache_prev: FnvIndexSet::new(),
            path_table: OrderedMap::new(),
            path_states: OrderedMap::new(),
            announce_table: OrderedMap::new(),
            announce_cache: OrderedMap::new(),
            announce_rate_table: OrderedMap::new(),
            link_table: OrderedMap::new(),
            reverse_table: OrderedMap::new(),
            discovery_path_requests: OrderedMap::new(),
            path_requests: OrderedMap::new(),
            path_request_tag_set: OrderedSet::new(),
            known_identities: OrderedMap::new(),
            known_ratchets: OrderedMap::new(),
            dest_ratchet_keys: OrderedMap::new(),
            receipts: OrderedMap::new(),
        }
    }

    /// Rotate packet cache: current becomes prev, prev is cleared.
    fn rotate_packet_cache(&mut self) {
        core::mem::swap(&mut self.packet_cache, &mut self.packet_cache_prev);
        self.packet_cache.clear();
    }
}

impl Default for EmbeddedStorage {
    fn default() -> Self {
        Self::new()
    }
}

/// Capacity-bounded map with FIFO drop-oldest eviction, allocation-free and
/// stack-safe, with a minimal base-size footprint.
///
/// FIFO order is encoded as a monotonic `u32` insertion sequence stamped on
/// each entry (alongside its value) instead of a companion key-index. heapless
/// `remove` is `swap_remove` and perturbs the map's own iteration order, so the
/// order cannot be read back from the map directly; the per-entry sequence
/// records it without duplicating the (16- or 32-byte) keys. Eviction is an
/// O(N) scan (N<=32) for the entry with the greatest age. Every mutation
/// touches only inline storage: no heap `Vec`, no large stack frame.
///
/// This is the #65 fix. The original `map_set` rebuilt the entire map through
/// two transient heap `Vec`s on every overflow insert; under churn the maps sit
/// at capacity, so that fired per insert and OOMed the near-full 64 KiB heap.
/// Batch 12 made eviction allocation-free with a companion `heapless::Vec<K,N>`
/// order index, but full-key duplication grew `size_of::<EmbeddedStorage>()` by
/// +4.3 KB, eating the idle-heap headroom the win was meant to create. The
/// per-entry `u32` sequence keeps the allocation-free, stack-safe eviction
/// while costing only 4 bytes per slot.
///
/// **Sequence wraparound:** the `u32` counter wraps after 2^32 inserts. Age is
/// computed with wrapping subtraction, so FIFO order stays exact as long as no
/// live entry is older than 2^32 inserts (4 billion). Past that single extreme
/// the evicted entry is approximately-oldest rather than strictly-oldest, which
/// the storage contract permits (missing entries recover via timeouts and
/// retransmits).
struct OrderedMap<K, V, const N: usize>
where
    K: Eq + core::hash::Hash + Copy,
{
    /// Each value carries its insertion sequence: `(seq, value)`.
    map: FnvIndexMap<K, (u32, V), N>,
    /// Monotonic insertion counter; wraps (see type docs).
    next_seq: u32,
}

impl<K, V, const N: usize> OrderedMap<K, V, N>
where
    K: Eq + core::hash::Hash + Copy,
{
    fn new() -> Self {
        Self {
            map: FnvIndexMap::new(),
            next_seq: 0,
        }
    }

    /// Take the current sequence and advance the counter.
    fn bump_seq(&mut self) -> u32 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        seq
    }

    /// Key of the oldest entry (greatest age under wrapping arithmetic), or
    /// `None` if empty. O(N) scan.
    fn oldest_key(&self) -> Option<K> {
        let next = self.next_seq;
        self.map
            .iter()
            .max_by_key(|(_, (seq, _))| next.wrapping_sub(*seq))
            .map(|(k, _)| *k)
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    /// The compile-time ceiling, for the storage census.
    fn capacity(&self) -> usize {
        N
    }

    fn contains_key(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key).map(|(_, v)| v)
    }

    fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.map.get_mut(key).map(|(_, v)| v)
    }

    fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.map.iter().map(|(k, (_, v))| (k, v))
    }

    fn keys(&self) -> impl Iterator<Item = &K> {
        self.map.keys()
    }

    fn values(&self) -> impl Iterator<Item = &V> {
        self.map.values().map(|(_, v)| v)
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        self.map.remove(key).map(|(_, v)| v)
    }

    /// Insert with FIFO eviction and refresh-on-re-insert semantics:
    /// - existing key: update the value in place and stamp a fresh (newest)
    ///   sequence, so an actively-refreshed entry is not evicted by unrelated
    ///   newer inserts;
    /// - full + new key: drop the oldest entry, then insert as newest;
    /// - otherwise: insert as newest.
    fn insert(&mut self, key: K, value: V) {
        let seq = self.bump_seq();
        if let Some(slot) = self.map.get_mut(&key) {
            // In-place value update + refresh to newest.
            *slot = (seq, value);
            return;
        }
        if self.map.len() >= N {
            if let Some(oldest) = self.oldest_key() {
                self.map.remove(&oldest);
            }
        }
        let _ = self.map.insert(key, (seq, value));
    }

    /// Retain entries matching `keep`. Allocation-free.
    fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        self.map.retain(|k, (_, v)| keep(k, v));
    }

    /// Like `retain`, but returns the removed `(K, V)` pairs. The heap `Vec`
    /// here is the caller-visible return value, not eviction scratch; the
    /// reject-key list lives in inline (stack) storage.
    fn retain_collect(&mut self, mut keep: impl FnMut(&K, &V) -> bool) -> Vec<(K, V)> {
        let mut drop_keys: heapless::Vec<K, N> = heapless::Vec::new();
        for (k, (_, v)) in self.map.iter() {
            if !keep(k, v) {
                let _ = drop_keys.push(*k);
            }
        }
        let mut removed = Vec::new();
        for k in &drop_keys {
            if let Some((_, v)) = self.map.remove(k) {
                removed.push((*k, v));
            }
        }
        removed
    }
}

/// Capacity-bounded set with FIFO drop-oldest eviction, allocation-free.
/// Same per-entry `u32` sequence scheme as [`OrderedMap`] (the value side of
/// the backing map *is* the sequence), for the path-request dedup tag set.
struct OrderedSet<K, const N: usize>
where
    K: Eq + core::hash::Hash + Copy,
{
    /// Maps each key to its insertion sequence.
    set: FnvIndexMap<K, u32, N>,
    next_seq: u32,
}

impl<K, const N: usize> OrderedSet<K, N>
where
    K: Eq + core::hash::Hash + Copy,
{
    fn new() -> Self {
        Self {
            set: FnvIndexMap::new(),
            next_seq: 0,
        }
    }

    fn contains(&self, key: &K) -> bool {
        self.set.contains_key(key)
    }

    fn len(&self) -> usize {
        self.set.len()
    }

    /// The compile-time ceiling, for the storage census.
    fn capacity(&self) -> usize {
        N
    }

    /// Key of the oldest entry (greatest age under wrapping arithmetic).
    fn oldest_key(&self) -> Option<K> {
        let next = self.next_seq;
        self.set
            .iter()
            .max_by_key(|(_, seq)| next.wrapping_sub(**seq))
            .map(|(k, _)| *k)
    }

    /// Insert a key, evicting the oldest first if full. Returns true if the key
    /// was newly inserted (was not already present). Tags are never refreshed,
    /// so a present key leaves the set (and its sequence) untouched.
    fn insert(&mut self, key: K) -> bool {
        if self.set.contains_key(&key) {
            return false;
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        if self.set.len() >= N {
            if let Some(oldest) = self.oldest_key() {
                self.set.remove(&oldest);
            }
        }
        let _ = self.set.insert(key, seq);
        true
    }
}

impl Storage for EmbeddedStorage {
    /// Every collection this storage holds, in field order, each with the
    /// compile-time ceiling it is held to (Codeberg #174).
    ///
    /// Unlike `MemoryStorage`, nothing here is unbounded: every map is
    /// heapless with a const-generic capacity, so every row carries one. The
    /// two dedup generations are the one place where the ceiling reported is
    /// not the collection's own capacity: a generation rotates once it passes
    /// half the set capacity, so half is what it is held to, and the live
    /// window across the rotation is that half to the full set. All counts
    /// are O(1).
    fn collection_counts(&self) -> Vec<CollectionCount> {
        vec![
            CollectionCount::bounded(
                "packet_cache",
                self.packet_cache.len(),
                PACKET_CACHE_GENERATION_CAP,
            ),
            CollectionCount::bounded(
                "packet_cache_prev",
                self.packet_cache_prev.len(),
                PACKET_CACHE_GENERATION_CAP,
            ),
            CollectionCount::bounded(
                "path_table",
                self.path_table.len(),
                self.path_table.capacity(),
            ),
            CollectionCount::bounded(
                "path_states",
                self.path_states.len(),
                self.path_states.capacity(),
            ),
            CollectionCount::bounded(
                "announce_table",
                self.announce_table.len(),
                self.announce_table.capacity(),
            ),
            CollectionCount::bounded(
                "announce_cache",
                self.announce_cache.len(),
                self.announce_cache.capacity(),
            ),
            CollectionCount::bounded(
                "announce_rate_table",
                self.announce_rate_table.len(),
                self.announce_rate_table.capacity(),
            ),
            CollectionCount::bounded(
                "link_table",
                self.link_table.len(),
                self.link_table.capacity(),
            ),
            CollectionCount::bounded(
                "reverse_table",
                self.reverse_table.len(),
                self.reverse_table.capacity(),
            ),
            CollectionCount::bounded(
                "discovery_path_requests",
                self.discovery_path_requests.len(),
                self.discovery_path_requests.capacity(),
            ),
            CollectionCount::bounded(
                "path_requests",
                self.path_requests.len(),
                self.path_requests.capacity(),
            ),
            CollectionCount::bounded(
                "path_request_tag_set",
                self.path_request_tag_set.len(),
                self.path_request_tag_set.capacity(),
            ),
            CollectionCount::bounded(
                "known_identities",
                self.known_identities.len(),
                self.known_identities.capacity(),
            ),
            CollectionCount::bounded(
                "known_ratchets",
                self.known_ratchets.len(),
                self.known_ratchets.capacity(),
            ),
            CollectionCount::bounded(
                "dest_ratchet_keys",
                self.dest_ratchet_keys.len(),
                self.dest_ratchet_keys.capacity(),
            ),
            CollectionCount::bounded("receipts", self.receipts.len(), self.receipts.capacity()),
        ]
    }

    // Packet Dedup
    fn has_packet_hash(&self, hash: &[u8; 32]) -> bool {
        let key = dedup_key(hash);
        self.packet_cache.contains(&key) || self.packet_cache_prev.contains(&key)
    }

    fn add_packet_hash(&mut self, hash: [u8; 32]) {
        let _ = self.packet_cache.insert(dedup_key(&hash));
        // Two-generation rotation: when current exceeds half capacity, rotate
        if self.packet_cache.len() > PACKET_CACHE_GENERATION_CAP {
            self.rotate_packet_cache();
        }
    }

    // Path Table
    fn get_path(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<&PathEntry> {
        self.path_table.get(dest_hash)
    }

    fn set_path(&mut self, dest_hash: [u8; TRUNCATED_HASHBYTES], entry: PathEntry) {
        // OrderedMap::insert refreshes the FIFO position of a re-announced
        // destination, so an active path is not evicted by unrelated newer
        // entries even while still in use.
        self.path_table.insert(dest_hash, entry);
    }

    fn remove_path(&mut self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<PathEntry> {
        self.path_table.remove(dest_hash)
    }

    fn path_count(&self) -> usize {
        self.path_table.len()
    }

    fn expire_paths(&mut self, now_ms: u64) -> Vec<[u8; TRUNCATED_HASHBYTES]> {
        let mut expired = Vec::new();
        let keys: heapless::Vec<[u8; TRUNCATED_HASHBYTES], 32> = self
            .path_table
            .iter()
            .filter(|(_, entry)| entry.expires_ms < now_ms)
            .map(|(k, _)| *k)
            .collect();
        for key in &keys {
            self.path_table.remove(key);
            expired.push(*key);
        }
        expired
    }

    fn earliest_path_expiry(&self) -> Option<u64> {
        self.path_table.values().map(|e| e.expires_ms).min()
    }

    fn path_entries(&self) -> Vec<([u8; TRUNCATED_HASHBYTES], PathEntry)> {
        self.path_table
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    }

    fn announce_rate_entries(&self) -> Vec<([u8; TRUNCATED_HASHBYTES], AnnounceRateEntry)> {
        self.announce_rate_table
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect()
    }

    // Path State
    fn get_path_state(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<PathState> {
        self.path_states.get(dest_hash).copied()
    }

    fn set_path_state(&mut self, dest_hash: [u8; TRUNCATED_HASHBYTES], state: PathState) {
        self.path_states.insert(dest_hash, state);
    }

    // Reverse Table
    fn get_reverse(&self, hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<&ReverseEntry> {
        self.reverse_table.get(hash)
    }

    fn set_reverse(&mut self, hash: [u8; TRUNCATED_HASHBYTES], entry: ReverseEntry) {
        self.reverse_table.insert(hash, entry);
    }

    fn remove_reverse(&mut self, hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<ReverseEntry> {
        self.reverse_table.remove(hash)
    }

    fn reverse_entries(&self) -> Vec<([u8; TRUNCATED_HASHBYTES], ReverseEntry)> {
        self.reverse_table.iter().map(|(k, v)| (*k, *v)).collect()
    }

    // Link Table
    fn get_link_entry(&self, link_id: &[u8; TRUNCATED_HASHBYTES]) -> Option<&LinkEntry> {
        self.link_table.get(link_id)
    }

    fn get_link_entry_mut(
        &mut self,
        link_id: &[u8; TRUNCATED_HASHBYTES],
    ) -> Option<&mut LinkEntry> {
        self.link_table.get_mut(link_id)
    }

    fn set_link_entry(&mut self, link_id: [u8; TRUNCATED_HASHBYTES], entry: LinkEntry) {
        self.link_table.insert(link_id, entry);
    }

    fn link_entries(&self) -> Vec<([u8; TRUNCATED_HASHBYTES], LinkEntry)> {
        self.link_table
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    }

    // Announce Table
    fn get_announce(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<&AnnounceEntry> {
        self.announce_table.get(dest_hash)
    }

    fn get_announce_mut(
        &mut self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
    ) -> Option<&mut AnnounceEntry> {
        self.announce_table.get_mut(dest_hash)
    }

    fn set_announce(&mut self, dest_hash: [u8; TRUNCATED_HASHBYTES], entry: AnnounceEntry) {
        self.announce_table.insert(dest_hash, entry);
    }

    fn remove_announce(&mut self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<AnnounceEntry> {
        self.announce_table.remove(dest_hash)
    }

    fn announce_keys(&self) -> Vec<[u8; TRUNCATED_HASHBYTES]> {
        self.announce_table.keys().copied().collect()
    }

    // Announce Cache
    fn get_announce_cache(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<&Vec<u8>> {
        self.announce_cache.get(dest_hash)
    }

    fn set_announce_cache(&mut self, dest_hash: [u8; TRUNCATED_HASHBYTES], raw: Vec<u8>) {
        self.announce_cache.insert(dest_hash, raw);
    }

    fn announce_cache_keys(&self) -> Vec<[u8; TRUNCATED_HASHBYTES]> {
        self.announce_cache.keys().copied().collect()
    }

    // Known-destination cache lifecycle (Codeberg #84). The shared-instance
    // retain RPC is a std-daemon feature; embedded nodes keep the wire-contract
    // no-ops (no retained-pin bookkeeping in the flash-backed cache).
    fn retain_known_dest(&mut self, _dest: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        false
    }
    fn unretain_known_dest(&mut self, _dest: &[u8; TRUNCATED_HASHBYTES], _now_ms: u64) -> bool {
        false
    }
    fn used_known_dest(&mut self, _dest: &[u8; TRUNCATED_HASHBYTES], _now_ms: u64) -> bool {
        false
    }
    fn is_known_dest_retained(&self, _dest: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        false
    }
    fn known_dest_last_used(&self, _dest: &[u8; TRUNCATED_HASHBYTES]) -> Option<u64> {
        None
    }

    // Announce Rate
    fn get_announce_rate(
        &self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
    ) -> Option<&AnnounceRateEntry> {
        self.announce_rate_table.get(dest_hash)
    }

    fn set_announce_rate(
        &mut self,
        dest_hash: [u8; TRUNCATED_HASHBYTES],
        entry: AnnounceRateEntry,
    ) {
        self.announce_rate_table.insert(dest_hash, entry);
    }

    // Receipts
    fn get_receipt(&self, hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<&PacketReceipt> {
        self.receipts.get(hash)
    }

    fn set_receipt(&mut self, hash: [u8; TRUNCATED_HASHBYTES], receipt: PacketReceipt) {
        self.receipts.insert(hash, receipt);
    }

    // Path Requests
    fn get_path_request_time(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<u64> {
        self.path_requests.get(dest_hash).copied()
    }

    fn set_path_request_time(&mut self, dest_hash: [u8; TRUNCATED_HASHBYTES], time_ms: u64) {
        self.path_requests.insert(dest_hash, time_ms);
    }

    fn check_path_request_tag(&mut self, tag: &[u8; 32]) -> bool {
        if self.path_request_tag_set.contains(tag) {
            return true;
        }
        // New tag: OrderedSet::insert evicts the oldest by insertion order if
        // full, allocation-free (no rebuild, no transient heap Vec).
        self.path_request_tag_set.insert(*tag);
        false
    }

    // Known Identities
    fn get_identity(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<&Identity> {
        self.known_identities.get(dest_hash)
    }

    fn set_identity(&mut self, dest_hash: [u8; TRUNCATED_HASHBYTES], identity: Identity) {
        self.known_identities.insert(dest_hash, identity);
    }

    // Cleanup
    fn expire_reverses(&mut self, now_ms: u64, timeout_ms: u64) -> usize {
        let before = self.reverse_table.len();
        self.reverse_table
            .retain(|_, entry| now_ms.saturating_sub(entry.timestamp_ms) <= timeout_ms);
        before - self.reverse_table.len()
    }

    fn remove_reverse_entries_for_interface(&mut self, iface_index: usize) {
        self.reverse_table.retain(|_, e| {
            e.receiving_interface_index != iface_index && e.outbound_interface_index != iface_index
        });
    }

    fn expire_receipts(&mut self, now_ms: u64) -> Vec<PacketReceipt> {
        // Two removal reasons, only the first is a timeout the caller reports:
        //  - Sent and past its timeout: proof never came, return it.
        //  - Terminal (Delivered/Failed) and past the retention grace window:
        //    the outcome already reached the application, reap it silently so
        //    the receipt map stays bounded the same way MemoryStorage now does
        //    (Codeberg #275). retain_collect returns everything it drops, so
        //    the terminal ones are filtered back out below.
        self.receipts
            .retain_collect(|_, receipt| {
                let timed_out = receipt.status == ReceiptStatus::Sent && receipt.is_expired(now_ms);
                let terminal_expired = receipt.status != ReceiptStatus::Sent
                    && now_ms.saturating_sub(receipt.sent_at_ms)
                        > receipt.timeout_ms.saturating_add(RECEIPT_RETENTION_MS);
                !(timed_out || terminal_expired)
            })
            .into_iter()
            .filter(|(_, r)| r.status == ReceiptStatus::Sent)
            .map(|(_, r)| r)
            .collect()
    }

    fn expire_link_entries(
        &mut self,
        now_ms: u64,
        link_timeout_ms: u64,
    ) -> Vec<([u8; TRUNCATED_HASHBYTES], LinkEntry)> {
        self.link_table.retain_collect(|_, entry| {
            let is_expired = if entry.validated {
                now_ms.saturating_sub(entry.timestamp_ms) > link_timeout_ms
            } else {
                now_ms > entry.proof_timeout_ms
            };
            !is_expired
        })
    }

    fn clean_stale_path_metadata(&mut self) {
        // Collect keys of path_states not in path_table
        let stale_states: heapless::Vec<[u8; TRUNCATED_HASHBYTES], 32> = self
            .path_states
            .keys()
            .filter(|k| !self.path_table.contains_key(*k))
            .copied()
            .collect();
        for key in &stale_states {
            self.path_states.remove(key);
        }

        let stale_rates: heapless::Vec<[u8; TRUNCATED_HASHBYTES], 32> = self
            .announce_rate_table
            .keys()
            .filter(|k| !self.path_table.contains_key(*k))
            .copied()
            .collect();
        for key in &stale_rates {
            self.announce_rate_table.remove(key);
        }
    }

    fn clean_announce_cache(&mut self, local_destinations: &BTreeSet<[u8; TRUNCATED_HASHBYTES]>) {
        let stale: heapless::Vec<[u8; TRUNCATED_HASHBYTES], 16> = self
            .announce_cache
            .keys()
            .filter(|k| !self.path_table.contains_key(*k) && !local_destinations.contains(*k))
            .copied()
            .collect();
        for key in &stale {
            self.announce_cache.remove(key);
        }
    }

    fn remove_link_entries_for_interface(
        &mut self,
        iface_index: usize,
    ) -> Vec<([u8; TRUNCATED_HASHBYTES], LinkEntry)> {
        self.link_table.retain_collect(|_, entry| {
            entry.received_interface_index != iface_index
                && entry.next_hop_interface_index != iface_index
        })
    }

    fn remove_paths_for_interface(&mut self, iface_index: usize) -> Vec<[u8; TRUNCATED_HASHBYTES]> {
        let mut removed = Vec::new();
        let keys: heapless::Vec<[u8; TRUNCATED_HASHBYTES], 32> = self
            .path_table
            .iter()
            .filter(|(_, entry)| entry.interface_index == iface_index)
            .map(|(k, _)| *k)
            .collect();
        for key in &keys {
            self.path_table.remove(key);
            removed.push(*key);
        }
        removed
    }

    // Deadlines
    fn earliest_receipt_deadline(&self) -> Option<u64> {
        self.receipts
            .values()
            .filter(|r| r.status == ReceiptStatus::Sent)
            .map(|r| r.sent_at_ms.saturating_add(r.timeout_ms))
            .min()
    }

    fn earliest_link_deadline(&self, link_timeout_ms: u64) -> Option<u64> {
        self.link_table
            .values()
            .map(|entry| {
                if entry.validated {
                    entry.timestamp_ms.saturating_add(link_timeout_ms)
                } else {
                    entry.proof_timeout_ms
                }
            })
            .min()
    }

    // Known Ratchets
    fn get_known_ratchet(
        &self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
    ) -> Option<[u8; RATCHET_SIZE]> {
        self.known_ratchets.get(dest_hash).map(|(r, _)| *r)
    }

    fn remember_known_ratchet(
        &mut self,
        dest_hash: [u8; TRUNCATED_HASHBYTES],
        ratchet: [u8; RATCHET_SIZE],
        received_at_ms: u64,
    ) {
        self.known_ratchets
            .insert(dest_hash, (ratchet, received_at_ms));
    }

    fn expire_known_ratchets(&mut self, now_ms: u64, expiry_ms: u64) -> usize {
        let before = self.known_ratchets.len();
        self.known_ratchets
            .retain(|_, (_, received_at)| now_ms.saturating_sub(*received_at) < expiry_ms);
        before - self.known_ratchets.len()
    }

    // Local Client Destinations (no-op on embedded)
    fn add_local_client_dest(
        &mut self,
        _iface_id: usize,
        _dest_hash: [u8; TRUNCATED_HASHBYTES],
    ) -> bool {
        false
    }

    fn remove_local_client_dests(&mut self, _iface_id: usize) {}

    // Local Client Known Destinations (no-op on embedded)
    fn set_local_client_known_dest(
        &mut self,
        _dest_hash: [u8; TRUNCATED_HASHBYTES],
        _last_seen_ms: u64,
    ) {
    }

    fn local_client_known_dest_hashes(&self) -> Vec<[u8; TRUNCATED_HASHBYTES]> {
        Vec::new()
    }

    fn expire_local_client_known_dests(&mut self, _now_ms: u64, _expiry_ms: u64) -> usize {
        0
    }

    // Discovery Path Requests
    fn set_discovery_path_request(
        &mut self,
        dest_hash: [u8; TRUNCATED_HASHBYTES],
        requesting_interface: usize,
        timeout_ms: u64,
    ) {
        // Only store first request (Python behavior)
        if !self.discovery_path_requests.contains_key(&dest_hash) {
            self.discovery_path_requests
                .insert(dest_hash, (requesting_interface, timeout_ms));
        }
    }

    fn get_discovery_path_request(
        &self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
    ) -> Option<(usize, u64)> {
        self.discovery_path_requests.get(dest_hash).copied()
    }

    fn remove_discovery_path_request(&mut self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) {
        self.discovery_path_requests.remove(dest_hash);
    }

    fn expire_discovery_path_requests(&mut self, now_ms: u64) -> usize {
        let before = self.discovery_path_requests.len();
        self.discovery_path_requests
            .retain(|_, (_, timeout)| *timeout > now_ms);
        before - self.discovery_path_requests.len()
    }

    fn discovery_path_request_dest_hashes(&self) -> Vec<[u8; TRUNCATED_HASHBYTES]> {
        self.discovery_path_requests.keys().copied().collect()
    }

    // Sender-Side Ratchet Keys
    fn store_dest_ratchet_keys(
        &mut self,
        dest_hash: [u8; TRUNCATED_HASHBYTES],
        serialized: Vec<u8>,
    ) {
        self.dest_ratchet_keys.insert(dest_hash, serialized);
    }

    fn load_dest_ratchet_keys(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<Vec<u8>> {
        self.dest_ratchet_keys.get(dest_hash).cloned()
    }

    // Heap census (#388). The heapless maps are inline in the node's
    // one boxed block and counted by `node_struct` there; what this
    // storage pins on the HEAP is exactly its variable-size values —
    // the raw announce bytes and the serialized ratchet keys.
    fn heap_bytes(&self) -> usize {
        self.announce_cache
            .iter()
            .map(|(_, raw)| raw.capacity())
            .sum::<usize>()
            + self
                .dest_ratchet_keys
                .iter()
                .map(|(_, keys)| keys.capacity())
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::DestinationHash;

    /// Codeberg #174, the embedded half: the census and the struct agree.
    /// Same binding as `MemoryStorage` — the source text of this file is the
    /// only thing that can catch a seventeenth collection added without a
    /// counter.
    #[test]
    fn collection_counts_names_every_collection_field() {
        let fields = crate::storage_census::collection_fields(
            include_str!("embedded_storage.rs"),
            "EmbeddedStorage",
        )
        .expect("EmbeddedStorage is declared in this file");
        assert!(
            fields.len() > 10,
            "the parse found only {fields:?}, which cannot be this struct"
        );

        let storage = EmbeddedStorage::new();
        let counted: Vec<&str> = storage.collection_counts().iter().map(|c| c.name).collect();

        for field in &fields {
            assert!(
                counted.contains(&field.as_str()),
                "EmbeddedStorage.{field} is a collection that collection_counts() does not report"
            );
        }
        for name in &counted {
            assert!(
                fields.iter().any(|f| f == name),
                "collection_counts() reports {name}, which is not a collection field"
            );
        }
        assert_eq!(counted.len(), fields.len(), "one row per collection");
    }

    /// Every heapless map is bounded by construction, so every row carries a
    /// ceiling — and the dedup generations carry the rotation threshold, not
    /// the raw set capacity, which is the number that decides when they are
    /// freed.
    #[test]
    fn every_embedded_collection_reports_its_ceiling() {
        let storage = EmbeddedStorage::new();
        for row in storage.collection_counts() {
            assert!(
                row.capacity.is_some(),
                "{} is heapless and must report its capacity",
                row.name
            );
            assert_eq!(row.entries, 0, "{} starts empty", row.name);
        }
        let cap = |name: &str| {
            storage
                .collection_counts()
                .into_iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("{name} must be reported"))
                .capacity
        };
        assert_eq!(cap("packet_cache"), Some(PACKET_CACHE_GENERATION_CAP));
        assert_eq!(cap("path_table"), Some(32));
        assert_eq!(cap("link_table"), Some(8));
    }

    #[test]
    fn embedded_storage_base_size_guard() {
        // Base footprint guard (Codeberg #65). Allocation-free eviction must
        // not reinflate the struct the way Batch 12's full-key order index did
        // (41280 -> 45616, +4.3 KB, which ate the idle-heap headroom). The
        // per-entry u32 sequence keeps it near the original 41280 B; the
        // residual is the irreducible cost of an inline insertion sequence
        // (per-slot alignment padding, not the 4-byte counter itself). Tighten
        // or relax this bound deliberately, never by accident.
        //
        // Deliberate relaxation 2026-09-08 (Codeberg #365): `PathEntry`
        // gained `via_peer: Option<[u8; 16]>` — 24 B padded per slot,
        // 768 B over the 32-slot path table — so the peer-loss cull can
        // attribute an entry whose announce identity differs from its
        // link identity (Columba). 43_500 -> 44_300; measured 43_696 B.
        //
        // Deliberate tightening 2026-09-12 (Codeberg #388): packet-ring
        // dedup keys truncated to 16 B (−8 192; the tag set stays at 32 B,
        // its key is structured — see the field) and known_identities
        // halved to 8 slots (−4 352). 44_300 -> 31_500; measured 31_120 B
        // on the 64-bit host. The 32-bit target has its own compile-time
        // assertion beside the struct.
        let size = core::mem::size_of::<EmbeddedStorage>();
        assert!(
            size <= 31_500,
            "EmbeddedStorage grew to {size} B; every slot here is boxed-NodeCore \
             heap on the board (#388) — grow a capacity only with the HEAP_BUDGET \
             arithmetic re-run"
        );
    }

    #[test]
    fn fnv_remove_insert_moves_to_back() {
        // Invariants relied on by set_path.
        //
        // Heapless FnvIndexMap:
        // insert(existing_key, …) updates in place WITHOUT moving the
        //     entry, the FIFO position is unchanged.
        // remove(&K) uses swap_remove: the last entry is moved to K's
        //     old slot, then the tail is truncated. This means keys().next()
        //     after a front-removal is NOT the key that was second in
        //     insertion order, it's whatever was last.
        // insert(new_key) appends at the back.
        //
        // Consequence for the fix: remove(K) + insert(K) guarantees K ends
        // up at the BACK (last to be evicted by FIFO), even though the
        // intermediate state rearranges other keys due to swap_remove.
        use heapless::FnvIndexMap;
        let mut m: FnvIndexMap<u32, u32, 4> = FnvIndexMap::new();
        m.insert(1, 10).unwrap();
        m.insert(2, 20).unwrap();
        m.insert(3, 30).unwrap();
        assert_eq!(*m.keys().next().unwrap(), 1);

        // In-place update keeps position at the front.
        m.insert(1, 11).unwrap();
        assert_eq!(
            *m.keys().next().unwrap(),
            1,
            "in-place insert keeps position"
        );

        // remove(1) + insert(1), 1 must end up at the back.
        m.remove(&1);
        m.insert(1, 12).unwrap();
        assert_eq!(
            *m.keys().last().unwrap(),
            1,
            "remove+insert lands the key at the back"
        );
        // Per-heapless swap_remove: the entry that was last BEFORE the
        // remove (3) now sits at the front. Documented, not intended.        // for the fix, only the "back" invariant matters.
        assert_eq!(
            *m.keys().next().unwrap(),
            3,
            "swap_remove leaves former-last at the front"
        );
    }

    #[test]
    fn test_set_path_refreshes_fifo_position() {
        // set_path must move re-announced destinations to the back of the
        // FIFO so that active paths are not evicted by unrelated later entries.
        let mut s = EmbeddedStorage::new();
        let entry = PathEntry {
            hops: 1,
            expires_ms: 10_000,
            interface_index: 0,
            random_blobs: Vec::new(),
            next_hop: None,
            via_peer: None,
        };

        // Fill to capacity (32) with unique keys.
        for i in 0u8..32 {
            let mut h = [0u8; TRUNCATED_HASHBYTES];
            h[0] = i;
            s.set_path(h, entry.clone());
        }

        // Re-announce the oldest (key 0). Without the fix, this is a
        // no-op on insertion order. With the fix, it moves to the back.
        let mut oldest = [0u8; TRUNCATED_HASHBYTES];
        oldest[0] = 0;
        s.set_path(oldest, entry.clone());

        // Add one unrelated new entry, which must evict the current front.
        let mut new_key = [0u8; TRUNCATED_HASHBYTES];
        new_key[0] = 100;
        s.set_path(new_key, entry.clone());

        // The re-announced entry must survive.
        assert!(
            s.get_path(&oldest).is_some(),
            "re-announced path must survive unrelated evictions"
        );
    }

    #[test]
    fn test_embedded_storage_path_roundtrip() {
        let mut s = EmbeddedStorage::new();
        let hash = [0x01u8; TRUNCATED_HASHBYTES];
        let entry = PathEntry {
            hops: 2,
            expires_ms: 10000,
            interface_index: 0,
            random_blobs: Vec::new(),
            next_hop: None,
            via_peer: None,
        };

        assert!(s.get_path(&hash).is_none());
        s.set_path(hash, entry.clone());
        assert_eq!(s.get_path(&hash).unwrap().hops, 2);
        assert_eq!(s.path_count(), 1);
    }

    #[test]
    fn test_embedded_storage_path_eviction() {
        let mut s = EmbeddedStorage::new();

        // Fill path_table to capacity (32)
        for i in 0u8..32 {
            let mut hash = [0u8; TRUNCATED_HASHBYTES];
            hash[0] = i;
            s.set_path(
                hash,
                PathEntry {
                    hops: 1,
                    expires_ms: 10000 + i as u64,
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                    via_peer: None,
                },
            );
        }
        assert_eq!(s.path_count(), 32);

        // Insert one more, should evict the first
        let mut new_hash = [0xFFu8; TRUNCATED_HASHBYTES];
        new_hash[0] = 0xFF;
        s.set_path(
            new_hash,
            PathEntry {
                hops: 3,
                expires_ms: 99999,
                interface_index: 1,
                random_blobs: Vec::new(),
                next_hop: None,
                via_peer: None,
            },
        );
        assert_eq!(s.path_count(), 32);
        // New entry is present
        assert!(s.get_path(&new_hash).is_some());
        // First entry was evicted
        let first = [0u8; TRUNCATED_HASHBYTES];
        assert!(s.get_path(&first).is_none());
    }

    #[test]
    fn test_embedded_storage_packet_dedup() {
        let mut s = EmbeddedStorage::new();
        let hash = [0x42u8; 32];

        assert!(!s.has_packet_hash(&hash));
        s.add_packet_hash(hash);
        assert!(s.has_packet_hash(&hash));
    }

    #[test]
    fn test_embedded_storage_receipt_roundtrip() {
        let mut s = EmbeddedStorage::new();
        let hash = [0x01u8; TRUNCATED_HASHBYTES];
        let receipt = PacketReceipt::new([0x42u8; 32], DestinationHash::new(hash), 1000);

        s.set_receipt(hash, receipt);
        assert!(s.get_receipt(&hash).is_some());
        assert_eq!(s.earliest_receipt_deadline(), Some(1000 + 30_000));
    }

    #[test]
    fn test_embedded_storage_announce_roundtrip() {
        let mut s = EmbeddedStorage::new();
        let hash = [0x01u8; TRUNCATED_HASHBYTES];
        let entry = AnnounceEntry {
            timestamp_ms: 1000,
            hops: 1,
            retries: 0,
            retransmit_at_ms: None,
            raw_packet: alloc::vec![0xAA; 100],
            receiving_interface_index: 0,
            target_interface: None,
            local_rebroadcasts: 0,
            block_rebroadcasts: false,
        };

        s.set_announce(hash, entry);
        assert!(s.get_announce(&hash).is_some());
        assert_eq!(s.announce_keys().len(), 1);

        s.remove_announce(&hash);
        assert!(s.get_announce(&hash).is_none());
    }

    #[test]
    fn test_embedded_storage_local_client_noop() {
        let mut s = EmbeddedStorage::new();
        let hash = [0x42u8; TRUNCATED_HASHBYTES];
        assert!(!s.add_local_client_dest(0, hash));
        s.remove_local_client_dests(0);
        s.set_local_client_known_dest(hash, 1000);
        assert!(s.local_client_known_dest_hashes().is_empty());
    }
    // Overflow correctness + semantic-intent coverage for every storage map
    // and the explicit-FIFO set.
    use rand_core::OsRng;

    fn key_th(i: usize) -> [u8; TRUNCATED_HASHBYTES] {
        let mut h = [0u8; TRUNCATED_HASHBYTES];
        h[..4].copy_from_slice(&(i as u32).to_le_bytes());
        h
    }
    fn key32(i: usize) -> [u8; 32] {
        let mut h = [0u8; 32];
        h[..4].copy_from_slice(&(i as u32).to_le_bytes());
        h
    }

    fn mk_path(seed: u8) -> PathEntry {
        PathEntry {
            hops: 1,
            expires_ms: 10_000 + seed as u64,
            interface_index: 0,
            random_blobs: Vec::new(),
            next_hop: None,
            via_peer: None,
        }
    }
    fn mk_announce(seed: u8) -> AnnounceEntry {
        AnnounceEntry {
            timestamp_ms: 1000 + seed as u64,
            hops: 1,
            retries: 0,
            retransmit_at_ms: None,
            raw_packet: alloc::vec![seed; 8],
            receiving_interface_index: 0,
            target_interface: None,
            local_rebroadcasts: 0,
            block_rebroadcasts: false,
        }
    }
    fn mk_rate(seed: u8) -> AnnounceRateEntry {
        AnnounceRateEntry {
            last_ms: seed as u64,
            rate_violations: 0,
            blocked_until_ms: 0,
        }
    }
    fn mk_link(seed: u8) -> LinkEntry {
        LinkEntry {
            timestamp_ms: seed as u64,
            next_hop_interface_index: 0,
            remaining_hops: 1,
            received_interface_index: 0,
            hops: 1,
            validated: false,
            proof_timeout_ms: 99_999,
            destination_hash: [0u8; TRUNCATED_HASHBYTES],
            peer_signing_key: None,
        }
    }
    fn mk_reverse(seed: u8) -> ReverseEntry {
        ReverseEntry {
            timestamp_ms: seed as u64,
            receiving_interface_index: 0,
            outbound_interface_index: 0,
        }
    }
    fn mk_receipt(key: [u8; TRUNCATED_HASHBYTES]) -> PacketReceipt {
        let mut packet_hash = [0u8; 32];
        packet_hash[..TRUNCATED_HASHBYTES].copy_from_slice(&key);
        PacketReceipt::new(packet_hash, DestinationHash::new(key), 1000)
    }

    // Map 1: path_table
    #[test]
    fn level1_path_table_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 32;
        for i in 0..(cap * 3) {
            s.set_path(key_th(i), mk_path(i as u8));
        }
        assert_eq!(s.path_count(), cap);
        for i in (cap * 2)..(cap * 3) {
            assert!(s.get_path(&key_th(i)).is_some(), "newest key {} missing", i);
        }
    }

    #[test]
    fn level2_path_table_refresh_on_reinsert() {
        let mut s = EmbeddedStorage::new();
        let cap = 32;
        for i in 0..cap {
            s.set_path(key_th(i), mk_path(i as u8));
        }
        s.set_path(key_th(0), mk_path(99));
        s.set_path(key_th(cap), mk_path(0));
        // The refresh-on-re-insert contract is "a re-inserted key survives
        // the next eviction". We can't assert which OTHER key was evicted
        // because the setter's `remove + insert` triggers a `swap_remove`
        // side effect that perturbs internal positions.
        assert!(
            s.get_path(&key_th(0)).is_some(),
            "refreshed key must survive eviction"
        );
        assert_eq!(s.path_count(), cap, "exactly one entry was evicted");
    }

    // Map 2: path_states
    #[test]
    fn level1_path_states_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 32;
        for i in 0..(cap * 3) {
            s.set_path_state(key_th(i), PathState::Unresponsive);
        }
        for i in (cap * 2)..(cap * 3) {
            assert!(s.get_path_state(&key_th(i)).is_some(), "newest {}", i);
        }
        // Count: at most cap retrievable from the inserted range.
        let live: usize = (0..(cap * 3))
            .filter(|i| s.get_path_state(&key_th(*i)).is_some())
            .count();
        assert_eq!(live, cap);
    }

    #[test]
    fn level2_path_states_refresh_on_reinsert() {
        let mut s = EmbeddedStorage::new();
        let cap = 32;
        for i in 0..cap {
            s.set_path_state(key_th(i), PathState::Unresponsive);
        }
        s.set_path_state(key_th(0), PathState::Responsive);
        s.set_path_state(key_th(cap), PathState::Unknown);
        assert!(
            s.get_path_state(&key_th(0)).is_some(),
            "refreshed key survives"
        );
        assert_eq!(
            (0..=cap)
                .filter(|i| s.get_path_state(&key_th(*i)).is_some())
                .count(),
            cap,
            "exactly one entry was evicted"
        );
    }

    // Map 3: announce_table
    #[test]
    fn level1_announce_table_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 16;
        for i in 0..(cap * 3) {
            s.set_announce(key_th(i), mk_announce(i as u8));
        }
        for i in (cap * 2)..(cap * 3) {
            assert!(s.get_announce(&key_th(i)).is_some(), "newest {}", i);
        }
        let live: usize = (0..(cap * 3))
            .filter(|i| s.get_announce(&key_th(*i)).is_some())
            .count();
        assert_eq!(live, cap);
    }

    #[test]
    fn level2_announce_table_refresh_on_reinsert() {
        let mut s = EmbeddedStorage::new();
        let cap = 16;
        for i in 0..cap {
            s.set_announce(key_th(i), mk_announce(i as u8));
        }
        s.set_announce(key_th(0), mk_announce(99));
        s.set_announce(key_th(cap), mk_announce(0));
        assert!(s.get_announce(&key_th(0)).is_some(), "refreshed survives");
    }

    // Map 4: announce_cache
    #[test]
    fn level1_announce_cache_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 16;
        for i in 0..(cap * 3) {
            s.set_announce_cache(key_th(i), alloc::vec![i as u8; 4]);
        }
        for i in (cap * 2)..(cap * 3) {
            assert!(s.get_announce_cache(&key_th(i)).is_some(), "newest {}", i);
        }
        let live: usize = (0..(cap * 3))
            .filter(|i| s.get_announce_cache(&key_th(*i)).is_some())
            .count();
        assert_eq!(live, cap);
    }

    #[test]
    fn level2_announce_cache_refresh_on_reinsert() {
        let mut s = EmbeddedStorage::new();
        let cap = 16;
        for i in 0..cap {
            s.set_announce_cache(key_th(i), alloc::vec![i as u8; 4]);
        }
        s.set_announce_cache(key_th(0), alloc::vec![99u8; 4]);
        s.set_announce_cache(key_th(cap), alloc::vec![0u8; 4]);
        assert!(
            s.get_announce_cache(&key_th(0)).is_some(),
            "refreshed survives"
        );
    }

    /// Codeberg #417 §2: how long a BOARD can recall what a destination
    /// last said about itself. `NodeCore::recall_app_data` reads this map,
    /// and here it is RAM-only, 16 entries deep (the two tests above pin the
    /// drop-oldest bound) and swept of every destination the path table no
    /// longer holds. So the honest bound the propagation role may rely on is
    /// "while a path to that node lives" — which covers a node syncing to us
    /// over a link, because that node has a path by construction, and does
    /// not cover a reboot in between: nothing here survives one.
    #[test]
    fn a_boards_recall_source_dies_with_the_path() {
        let mut s = EmbeddedStorage::new();
        s.set_announce_cache(key_th(1), alloc::vec![7u8; 4]);
        s.set_announce_cache(key_th(2), alloc::vec![8u8; 4]);
        s.set_path(key_th(1), mk_path(0));

        s.clean_announce_cache(&BTreeSet::new());

        assert!(
            s.get_announce_cache(&key_th(1)).is_some(),
            "a destination with a path stays recallable"
        );
        assert!(
            s.get_announce_cache(&key_th(2)).is_none(),
            "a destination with no path and no local claim is swept"
        );
    }

    // Map 5: announce_rate_table
    #[test]
    fn level1_announce_rate_table_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 32;
        for i in 0..(cap * 3) {
            s.set_announce_rate(key_th(i), mk_rate(i as u8));
        }
        for i in (cap * 2)..(cap * 3) {
            assert!(s.get_announce_rate(&key_th(i)).is_some(), "newest {}", i);
        }
        let live: usize = (0..(cap * 3))
            .filter(|i| s.get_announce_rate(&key_th(*i)).is_some())
            .count();
        assert_eq!(live, cap);
    }

    #[test]
    fn level2_announce_rate_table_refresh_on_reinsert() {
        let mut s = EmbeddedStorage::new();
        let cap = 32;
        for i in 0..cap {
            s.set_announce_rate(key_th(i), mk_rate(i as u8));
        }
        s.set_announce_rate(key_th(0), mk_rate(99));
        s.set_announce_rate(key_th(cap), mk_rate(0));
        assert!(
            s.get_announce_rate(&key_th(0)).is_some(),
            "refreshed survives"
        );
    }

    // Map 6: link_table
    #[test]
    fn level1_link_table_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 8;
        for i in 0..(cap * 3) {
            s.set_link_entry(key_th(i), mk_link(i as u8));
        }
        for i in (cap * 2)..(cap * 3) {
            assert!(s.get_link_entry(&key_th(i)).is_some(), "newest {}", i);
        }
        let live: usize = (0..(cap * 3))
            .filter(|i| s.get_link_entry(&key_th(*i)).is_some())
            .count();
        assert_eq!(live, cap);
    }

    #[test]
    fn level2_link_table_refresh_on_reinsert() {
        let mut s = EmbeddedStorage::new();
        let cap = 8;
        for i in 0..cap {
            s.set_link_entry(key_th(i), mk_link(i as u8));
        }
        s.set_link_entry(key_th(0), mk_link(99));
        s.set_link_entry(key_th(cap), mk_link(0));
        assert!(s.get_link_entry(&key_th(0)).is_some(), "refreshed survives");
    }

    // Map 7: reverse_table
    #[test]
    fn level1_reverse_table_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 16;
        for i in 0..(cap * 3) {
            s.set_reverse(key_th(i), mk_reverse(i as u8));
        }
        for i in (cap * 2)..(cap * 3) {
            assert!(s.get_reverse(&key_th(i)).is_some(), "newest {}", i);
        }
        let live: usize = (0..(cap * 3))
            .filter(|i| s.get_reverse(&key_th(*i)).is_some())
            .count();
        assert_eq!(live, cap);
    }

    #[test]
    fn level2_reverse_table_refresh_on_reinsert() {
        let mut s = EmbeddedStorage::new();
        let cap = 16;
        for i in 0..cap {
            s.set_reverse(key_th(i), mk_reverse(i as u8));
        }
        s.set_reverse(key_th(0), mk_reverse(99));
        s.set_reverse(key_th(cap), mk_reverse(0));
        assert!(s.get_reverse(&key_th(0)).is_some(), "refreshed survives");
    }

    // Map 8: discovery_path_requests (FIFO-by-insertion + guard)
    #[test]
    fn level1_discovery_path_requests_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 4;
        for i in 0..(cap * 3) {
            s.set_discovery_path_request(key_th(i), i, 1000 + i as u64);
        }
        for i in (cap * 2)..(cap * 3) {
            assert!(
                s.get_discovery_path_request(&key_th(i)).is_some(),
                "newest {}",
                i
            );
        }
        let live: usize = (0..(cap * 3))
            .filter(|i| s.get_discovery_path_request(&key_th(*i)).is_some())
            .count();
        assert_eq!(live, cap);
    }

    #[test]
    fn level2_discovery_path_requests_first_request_wins() {
        let mut s = EmbeddedStorage::new();
        let cap = 4;
        for i in 0..cap {
            s.set_discovery_path_request(key_th(i), 10 + i, 1000 + i as u64);
        }
        // Re-insert with different value: guard prevents update.
        s.set_discovery_path_request(key_th(0), 999, 9999);
        let stored = s.get_discovery_path_request(&key_th(0)).unwrap();
        assert_eq!(
            stored,
            (10, 1000),
            "first-request guard must keep original value"
        );
        // New unrelated key triggers FIFO eviction of oldest.
        s.set_discovery_path_request(key_th(cap), 99, 99);
        assert!(
            s.get_discovery_path_request(&key_th(0)).is_none(),
            "FIFO-by-insertion: oldest evicted (no refresh on guarded re-insert)"
        );
        assert!(
            s.get_discovery_path_request(&key_th(cap)).is_some(),
            "newest present"
        );
    }

    // Map 9: path_requests
    #[test]
    fn level1_path_requests_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 8;
        for i in 0..(cap * 3) {
            s.set_path_request_time(key_th(i), 1000 + i as u64);
        }
        for i in (cap * 2)..(cap * 3) {
            assert!(
                s.get_path_request_time(&key_th(i)).is_some(),
                "newest {}",
                i
            );
        }
        let live: usize = (0..(cap * 3))
            .filter(|i| s.get_path_request_time(&key_th(*i)).is_some())
            .count();
        assert_eq!(live, cap);
    }

    #[test]
    fn level2_path_requests_refresh_on_reinsert() {
        let mut s = EmbeddedStorage::new();
        let cap = 8;
        for i in 0..cap {
            s.set_path_request_time(key_th(i), 1000 + i as u64);
        }
        s.set_path_request_time(key_th(0), 99_999);
        s.set_path_request_time(key_th(cap), 0);
        assert!(
            s.get_path_request_time(&key_th(0)).is_some(),
            "refreshed survives"
        );
    }

    // Map 10: known_identities
    #[test]
    fn level1_known_identities_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 8;
        for i in 0..(cap * 3) {
            s.set_identity(key_th(i), Identity::generate(&mut OsRng));
        }
        for i in (cap * 2)..(cap * 3) {
            assert!(s.get_identity(&key_th(i)).is_some(), "newest {}", i);
        }
        let live: usize = (0..(cap * 3))
            .filter(|i| s.get_identity(&key_th(*i)).is_some())
            .count();
        assert_eq!(live, cap);
    }

    #[test]
    fn level2_known_identities_refresh_on_reinsert() {
        let mut s = EmbeddedStorage::new();
        let cap = 8;
        for i in 0..cap {
            s.set_identity(key_th(i), Identity::generate(&mut OsRng));
        }
        s.set_identity(key_th(0), Identity::generate(&mut OsRng));
        s.set_identity(key_th(cap), Identity::generate(&mut OsRng));
        assert!(s.get_identity(&key_th(0)).is_some(), "refreshed survives");
    }

    // Map 11: known_ratchets
    #[test]
    fn level1_known_ratchets_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 8;
        for i in 0..(cap * 3) {
            s.remember_known_ratchet(key_th(i), [i as u8; RATCHET_SIZE], 1000 + i as u64);
        }
        for i in (cap * 2)..(cap * 3) {
            assert!(s.get_known_ratchet(&key_th(i)).is_some(), "newest {}", i);
        }
        let live: usize = (0..(cap * 3))
            .filter(|i| s.get_known_ratchet(&key_th(*i)).is_some())
            .count();
        assert_eq!(live, cap);
    }

    #[test]
    fn level2_known_ratchets_refresh_on_reinsert() {
        let mut s = EmbeddedStorage::new();
        let cap = 8;
        for i in 0..cap {
            s.remember_known_ratchet(key_th(i), [i as u8; RATCHET_SIZE], 1000 + i as u64);
        }
        s.remember_known_ratchet(key_th(0), [0xAA; RATCHET_SIZE], 9999);
        s.remember_known_ratchet(key_th(cap), [0; RATCHET_SIZE], 0);
        assert!(
            s.get_known_ratchet(&key_th(0)).is_some(),
            "refreshed survives"
        );
    }

    // Map 12: dest_ratchet_keys
    #[test]
    fn level1_dest_ratchet_keys_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 4;
        for i in 0..(cap * 3) {
            s.store_dest_ratchet_keys(key_th(i), alloc::vec![i as u8; 8]);
        }
        for i in (cap * 2)..(cap * 3) {
            assert!(
                s.load_dest_ratchet_keys(&key_th(i)).is_some(),
                "newest {}",
                i
            );
        }
        let live: usize = (0..(cap * 3))
            .filter(|i| s.load_dest_ratchet_keys(&key_th(*i)).is_some())
            .count();
        assert_eq!(live, cap);
    }

    #[test]
    fn level2_dest_ratchet_keys_refresh_on_reinsert() {
        let mut s = EmbeddedStorage::new();
        let cap = 4;
        for i in 0..cap {
            s.store_dest_ratchet_keys(key_th(i), alloc::vec![i as u8; 8]);
        }
        s.store_dest_ratchet_keys(key_th(0), alloc::vec![99u8; 8]);
        s.store_dest_ratchet_keys(key_th(cap), alloc::vec![0u8; 8]);
        assert!(
            s.load_dest_ratchet_keys(&key_th(0)).is_some(),
            "refreshed survives"
        );
    }

    // Map 13: receipts
    #[test]
    fn level1_receipts_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 8;
        for i in 0..(cap * 3) {
            let k = key_th(i);
            s.set_receipt(k, mk_receipt(k));
        }
        for i in (cap * 2)..(cap * 3) {
            assert!(s.get_receipt(&key_th(i)).is_some(), "newest {}", i);
        }
        let live: usize = (0..(cap * 3))
            .filter(|i| s.get_receipt(&key_th(*i)).is_some())
            .count();
        assert_eq!(live, cap);
    }

    #[test]
    fn delivered_receipts_are_reaped_after_the_retention_window() {
        // The same contract MemoryStorage is tested against (Codeberg #275):
        // delivered receipts are reaped after the retention window and never
        // surface as timeouts. Both storages must agree here.
        let mut s = EmbeddedStorage::new();
        let n = 8;
        for i in 0..n {
            let k = key_th(i);
            let mut receipt = mk_receipt(k);
            receipt.set_delivered();
            s.set_receipt(k, receipt);
        }

        let sent_at = 1000u64;
        let inside = sent_at + crate::constants::DATA_RECEIPT_TIMEOUT_MS;
        assert!(s.expire_receipts(inside).is_empty());
        assert!((0..n).all(|i| s.get_receipt(&key_th(i)).is_some()));

        let after = sent_at
            + crate::constants::DATA_RECEIPT_TIMEOUT_MS
            + crate::constants::RECEIPT_RETENTION_MS
            + 1;
        let timed_out = s.expire_receipts(after);
        assert!(
            timed_out.is_empty(),
            "delivered receipts must not surface as timeouts"
        );
        assert!(
            (0..n).all(|i| s.get_receipt(&key_th(i)).is_none()),
            "every delivered receipt must be reaped"
        );
    }

    #[test]
    fn level2_receipts_refresh_on_reinsert() {
        let mut s = EmbeddedStorage::new();
        let cap = 8;
        for i in 0..cap {
            let k = key_th(i);
            s.set_receipt(k, mk_receipt(k));
        }
        // Re-insert key 0 (mirrors mark_receipt_delivered's get→clone→set path).
        let k0 = key_th(0);
        s.set_receipt(k0, mk_receipt(k0));
        let kcap = key_th(cap);
        s.set_receipt(kcap, mk_receipt(kcap));
        assert!(s.get_receipt(&k0).is_some(), "refreshed survives");
    }

    // Set 14: path_request_tag_set (own FIFO logic)
    #[test]
    fn level1_path_request_tag_set_overflow_correctness() {
        let mut s = EmbeddedStorage::new();
        let cap = 32;
        // First-time inserts return false ("not seen before").
        for i in 0..(cap * 3) {
            let seen = s.check_path_request_tag(&key32(i));
            assert!(!seen, "tag {} reported seen on first insert", i);
        }
        // The most-recently inserted `cap` tags must still be reported seen.
        for i in (cap * 2)..(cap * 3) {
            assert!(
                s.check_path_request_tag(&key32(i)),
                "newest tag {} should still be in the set",
                i
            );
        }
    }

    #[test]
    fn level2_path_request_tag_set_fifo_eviction() {
        let mut s = EmbeddedStorage::new();
        let cap = 32;
        for i in 0..cap {
            assert!(!s.check_path_request_tag(&key32(i)));
        }
        // All 32 tags now in set; second check returns true.
        assert!(
            s.check_path_request_tag(&key32(0)),
            "tag 0 seen second time"
        );
        // Insert a 33rd new tag. Built-in FIFO logic must evict oldest.
        assert!(
            !s.check_path_request_tag(&key32(cap)),
            "33rd tag is fresh (and triggers eviction)"
        );
        assert!(
            s.check_path_request_tag(&key32(cap)),
            "33rd tag now reported seen"
        );
        // Oldest tag was evicted: first call now returns false (not seen).
        assert!(
            !s.check_path_request_tag(&key32(0)),
            "oldest tag must be evicted by FIFO insert"
        );
    }
}
