//! In-memory Storage implementation backed by BTreeMaps with configurable caps.
//!
//! `MemoryStorage` is the production Storage implementation for embedded targets
//! and the default test storage for core tests. It is NOT `#[cfg(test)]`, it is
//! always available.
//!
//! # Every table has a ceiling (Codeberg #421)
//!
//! Until #421 the caps this module advertised were three: the packet dedup
//! generations, the known identities and the path-request tag ring. Every
//! other table was bounded by expiry alone, which means its real ceiling was
//! arrival rate times expiry window — a number the neighbours choose, not the
//! operator. A field node held 73 901 reverse entries in one 8 minute window;
//! doubling its traffic doubled the table.
//!
//! Now every collection is held to a [`TableCaps`] entry, and a full table
//! evicts rather than refuses: the new entry always lands. A node that stops
//! learning because a table filled would be a worse failure than forgetting
//! something the protocol re-derives from a timeout or a path request.

extern crate alloc;

use alloc::collections::BTreeSet;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::bounded_map::BoundedMap;
use crate::constants::{
    HASHLIST_MAXSIZE, MAX_PATH_REQUEST_TAGS, RATCHET_SIZE, RECEIPT_RETENTION_MS,
    TRUNCATED_HASHBYTES,
};
use crate::identity::Identity;
use crate::storage_census::CollectionCount;
use crate::storage_types::{
    AnnounceEntry, AnnounceRateEntry, LinkEntry, PacketReceipt, PathEntry, PathState,
    ReceiptStatus, ReverseEntry,
};
use crate::traits::Storage;

/// Default identity capacity for desktop/Linux.
/// 50k identities at ~144 bytes × 3x BTreeMap overhead = ~21 MB max.
const DEFAULT_IDENTITY_CAP: usize = 50_000;

/// Compact packet hash capacity for constrained devices.
const COMPACT_PACKET_HASH_CAP: usize = 10_000;

/// Compact identity capacity for constrained devices.
const COMPACT_IDENTITY_CAP: usize = 1_000;

/// Per-table ceilings (Codeberg #421).
///
/// Nine numbers cover twenty collections: tables keyed by the same population
/// share a knob, because two different ceilings on one population means the
/// tighter one silently truncates the other and no operator can see which.
///
/// # How the defaults were derived
///
/// Every figure below is `entries × modelled bytes`, where the modelled bytes
/// are the ones [`MemoryStorage::diagnostic_dump`] prints: key + `size_of` of
/// the value + any heap the value owns + the [`BoundedMap`] order index (a
/// `u64` sequence and a second copy of the key, 24 bytes for a 16-byte hash),
/// all times the 3× `BTreeMap` overhead factor this module has used since
/// #174. They are checkable, not chosen for looking round.
///
/// | table | bytes/entry | desktop | compact |
/// |---|---|---|---|
/// | `path_table` | 480 (4-blob window) | 32 768 → 15.7 MB | 8 192 → 3.9 MB |
/// | `path_states` | 123 | 32 768 → 4.0 MB | 8 192 → 1.0 MB |
/// | `path_requests` | 144 | 32 768 → 4.7 MB | 8 192 → 1.2 MB |
/// | `discovery_path_requests` | 168 | 32 768 → 5.5 MB | 8 192 → 1.4 MB |
/// | `reverse_table` | 192 | 200 000 → 38.4 MB | 16 384 → 3.1 MB |
/// | `link_table` | 384 | 8 192 → 3.1 MB | 1 024 → 0.4 MB |
/// | `announce_table` | 960 (200 B packet) | 16 384 → 15.7 MB | 2 048 → 2.0 MB |
/// | `announce_cache` | 792 (200 B packet) | 50 000 → 39.6 MB | 4 096 → 3.2 MB |
/// | `announce_rate_table` | 192 | 50 000 → 9.6 MB | 4 096 → 0.8 MB |
/// | `known_ratchets` | 240 | 50 000 → 12.0 MB | 4 096 → 1.0 MB |
/// | `known_dest_use` | 168 | 50 000 → 8.4 MB | 4 096 → 0.7 MB |
/// | `local_client_dest_map` | 168 | 4 096 → 0.7 MB | 1 024 → 0.2 MB |
/// | `local_client_known_dests` | 144 | 4 096 → 0.6 MB | 1 024 → 0.1 MB |
/// | `dest_ratchet_keys` | 384 | 4 096 → 1.6 MB | 1 024 → 0.4 MB |
/// | `receipts` | 384 | 1 024 → 0.4 MB | 1 024 → 0.4 MB |
/// | `known_identities` | 1 656 | 50 000 → 82.8 MB | 1 000 → 1.7 MB |
/// | packet dedup (both generations) | 96 | 1 000 000 → 96 MB | 10 000 → 1.0 MB |
///
/// Desktop totals about 339 MB, of which 179 MB is the two caps that already
/// existed (dedup and identities). Compact totals about 22 MB, which is what
/// makes it fit a Raspberry Pi Zero 2W: 512 MB shared with the GPU, no swap.
/// A path table whose blob windows are all full (64 blobs, the Python
/// `MAX_RANDOM_BLOBS`) adds 59 MB desktop / 15 MB compact on top; that is the
/// worst case, not the working set.
///
/// # Where the entry counts come from
///
/// * `reverse_table` 200 000 is 2.7× the 73 901 entries #421 measured on a
///   node forwarding ~154 packets per second through an 8 minute expiry
///   window. Reaching it takes sustained traffic at more than twice the
///   busiest node we have ever run.
/// * `path_table` 32 768 is 1.5× the 22 362 paths the largest public-mesh
///   node we run has held. Its expiry is seven days, so on any node up for
///   less than a week the size ceiling is the only one there is.
/// * `link_table` 8 192 is 9× the 892 links the same measurement saw.
/// * the destination-keyed tables take `known_identities`' existing 50 000,
///   because they are keyed by the same set of remote destinations.
/// * `receipts` 1 024 is Python's `Transport.MAX_RECEIPTS` (Transport.py:95),
///   the one cap the reference has and we did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableCaps {
    /// Packet dedup hashes across both generations; a generation rotates at
    /// half this.
    pub packet_hash_cap: usize,
    /// `known_identities`.
    pub identity_cap: usize,
    /// `path_table`, `path_states`, `path_requests`,
    /// `discovery_path_requests` — all keyed by a destination we have or want
    /// a route to.
    pub path_cap: usize,
    /// `reverse_table`.
    pub reverse_cap: usize,
    /// `link_table`.
    pub link_cap: usize,
    /// `announce_table`, the pending-rebroadcast queue.
    pub announce_cap: usize,
    /// `announce_cache`, `announce_rate_table`, `known_ratchets`,
    /// `known_dest_use` — all keyed by a remote destination we have heard
    /// announce.
    pub destination_cap: usize,
    /// `local_client_dest_map`, `local_client_known_dests`,
    /// `dest_ratchet_keys` — all driven by locally attached clients.
    pub local_dest_cap: usize,
    /// `receipts`.
    pub receipt_cap: usize,
}

impl TableCaps {
    /// Desktop/server profile. See the type documentation for the arithmetic.
    pub const fn desktop() -> Self {
        Self {
            packet_hash_cap: HASHLIST_MAXSIZE,
            identity_cap: DEFAULT_IDENTITY_CAP,
            path_cap: 32_768,
            reverse_cap: 200_000,
            link_cap: 8_192,
            announce_cap: 16_384,
            destination_cap: DEFAULT_IDENTITY_CAP,
            local_dest_cap: 4_096,
            receipt_cap: MAX_RECEIPTS,
        }
    }

    /// Constrained profile, sized to leave a Raspberry Pi Zero 2W usable.
    pub const fn compact() -> Self {
        Self {
            packet_hash_cap: COMPACT_PACKET_HASH_CAP,
            identity_cap: COMPACT_IDENTITY_CAP,
            path_cap: 8_192,
            reverse_cap: 16_384,
            link_cap: 1_024,
            announce_cap: 2_048,
            destination_cap: 4_096,
            local_dest_cap: 1_024,
            receipt_cap: MAX_RECEIPTS,
        }
    }
}

impl Default for TableCaps {
    fn default() -> Self {
        Self::desktop()
    }
}

/// Receipts kept at once, Python's `Transport.MAX_RECEIPTS`
/// (`reference/Reticulum/RNS/Transport.py:95`).
///
/// Not profile-dependent: it is a reference number, it is the same on both
/// sides of the mesh, and 1 024 outstanding receipts is already far more than
/// any node we run has in flight. We had only the 30 second
/// [`RECEIPT_RETENTION_MS`] window, which under load is no bound at all.
pub const MAX_RECEIPTS: usize = 1_024;

/// Bytes a [`BoundedMap`]'s FIFO order index costs per live entry: the `u64`
/// insertion sequence it is keyed by, plus the second copy of the key it maps
/// to. 24 bytes for a 16-byte destination hash.
///
/// The diagnostic dump adds this to every bounded row. A row that priced only
/// the entry would under-report by a fifth on the reverse table, and this dump
/// is read to attribute a resident set — a row that under-reports is worse
/// than no row, because it is believed.
const fn order_index_bytes(key_bytes: usize) -> usize {
    core::mem::size_of::<u64>() + key_bytes
}

/// [`order_index_bytes`] for the 16-byte destination hash every table but
/// `local_client_dest_map` is keyed by.
const HASH_ORDER_BYTES: usize = order_index_bytes(TRUNCATED_HASHBYTES);

/// In-memory storage with configurable per-collection capacity limits.
///
/// Uses BTreeMap/BTreeSet for all collections. Not persistent, all data is
/// lost when the process exits. For persistent storage, use `FileStorage`
/// in `leviculum-std`.
pub struct MemoryStorage {
    /// Ceilings every collection below is held to.
    caps: TableCaps,

    // Packet dedup
    /// Current generation of packet hashes
    packet_cache: BTreeSet<[u8; 32]>,
    /// Previous generation (rotated out when current exceeds half cap)
    packet_cache_prev: BTreeSet<[u8; 32]>,

    // Path table
    /// Routes to destinations.
    ///
    /// **Eviction: oldest first, refreshed on re-announce.** Dropping an
    /// entry costs one path request — recoverable, but not free, so the cap
    /// is set above the largest mesh we have measured. Plain FIFO would be
    /// wrong here on its own, because a stable path installed on day one
    /// would be evicted ahead of a churning one installed a minute ago;
    /// refresh-on-re-insert fixes that, since every announce for a live
    /// destination re-inserts its path and moves it to the back. The seven
    /// day expiry never fires on a node up for less than a week, so this
    /// ceiling is usually the only one acting.
    path_table: BoundedMap<[u8; TRUNCATED_HASHBYTES], PathEntry>,
    /// Per-path quality state.
    ///
    /// **Eviction: oldest first.** Bounded by `path_cap` because
    /// `clean_stale_path_metadata` already holds it to the path table's key
    /// set; a dropped entry falls back to `PathState::Unknown`, which is the
    /// state a path starts in.
    path_states: BoundedMap<[u8; TRUNCATED_HASHBYTES], PathState>,

    // Reverse table
    /// Where a reply to a forwarded packet has to leave by.
    ///
    /// **Eviction: oldest first.** Dropping an entry loses one reply, so the
    /// cap has to be large enough that ordinary forwarding never reaches it
    /// — 200 000 against a measured 73 901 (#421). Oldest-first is right
    /// despite that cost: the 8 minute expiry already declares old entries
    /// worthless, so the oldest live entry is the one closest to being
    /// dropped anyway. Entries are keyed per packet hash and never
    /// re-inserted, so FIFO here is plain arrival order.
    reverse_table: BoundedMap<[u8; TRUNCATED_HASHBYTES], ReverseEntry>,

    // Link table
    /// Links routed through this node.
    ///
    /// **Eviction: unvalidated first, then oldest.** Not plain FIFO, because
    /// dropping a live link's entry breaks that link, while an unvalidated
    /// entry is a link request whose proof has not arrived and may never —
    /// which is also the shape of a flood. So an overflow drops the oldest
    /// unvalidated entry it can find and only falls back to the oldest
    /// validated one when every candidate is live. Pinned by
    /// `link_table_prefers_unvalidated_entries_on_overflow`.
    link_table: BoundedMap<[u8; TRUNCATED_HASHBYTES], LinkEntry>,

    // Announce table
    /// Announces queued for rebroadcast.
    ///
    /// **Eviction: oldest first.** A dropped entry is a rebroadcast that
    /// does not happen; the announcing destination re-announces on its own
    /// cadence, and the neighbours that would have received it hear the
    /// original from somewhere else or on the next announce. This is a work
    /// queue, so oldest-first also drops the entry most likely to have
    /// missed its retransmit window already.
    announce_table: BoundedMap<[u8; TRUNCATED_HASHBYTES], AnnounceEntry>,
    /// Raw announce bytes, re-sent to answer a path request.
    ///
    /// **Eviction: oldest unretained first, then oldest.** Not plain FIFO: a
    /// retained entry (Codeberg #84, Python's `known_destinations[dest][4] ==
    /// -1`) is one an application pinned, and `clean_announce_cache` already
    /// refuses to reap it, so a size overflow must not do by the back door
    /// what the time sweep refuses to do. Refresh-on-re-insert makes the
    /// unretained order least-recently-announced rather than oldest-ever.
    /// Pinned by `announce_cache_evicts_unretained_entries_first`.
    announce_cache: BoundedMap<[u8; TRUNCATED_HASHBYTES], Vec<u8>>,
    /// Per-destination announce rate state.
    ///
    /// **Eviction: oldest first.** Dropping an entry forgets that a
    /// destination has been announcing too fast, which costs at most one
    /// extra rebroadcast before the state rebuilds; the entry is re-inserted
    /// on its next announce anyway.
    announce_rate_table: BoundedMap<[u8; TRUNCATED_HASHBYTES], AnnounceRateEntry>,

    // Receipts
    /// Sent packets awaiting proof.
    ///
    /// **Eviction: terminal receipts first, then oldest — and the dropped
    /// one still gets its timeout.** Not plain FIFO: a `Delivered`/`Failed`
    /// receipt is only being kept for the [`RECEIPT_RETENTION_MS`] grace
    /// window and its outcome already reached the application, so it is free
    /// to drop, while a `Sent` receipt still owes its sender an answer.
    /// When every candidate is still pending the oldest goes, and it is
    /// parked in `culled_receipts` so the next `expire_receipts` reports it
    /// as a timeout — which is exactly what Python does on the same overflow
    /// (`Transport.py:558-561` sets `timeout = -1` and calls
    /// `check_timeout()`). Pinned by
    /// `receipts_evict_terminal_first_and_cull_pending_with_a_timeout`.
    receipts: BoundedMap<[u8; TRUNCATED_HASHBYTES], PacketReceipt>,
    /// Pending receipts dropped by a size overflow, waiting to be reported as
    /// timeouts by the next `expire_receipts`. Bounded by `receipt_cap`: a
    /// caller that never sweeps must not grow this instead of the table.
    culled_receipts: VecDeque<PacketReceipt>,

    // Path requests
    /// When we last asked for a path to a destination.
    ///
    /// **Eviction: oldest first.** Dropping an entry lifts the rate limit on
    /// one destination's path requests — one extra request on the air, which
    /// is what the table exists to avoid but not a correctness problem.
    path_requests: BoundedMap<[u8; TRUNCATED_HASHBYTES], u64>,
    path_request_tags: VecDeque<[u8; 32]>,
    path_request_tag_set: BTreeSet<[u8; 32]>,

    // Known identities
    /// Public keys of destinations we have heard announce.
    ///
    /// **Eviction: oldest first, refreshed on re-announce.** This table was
    /// already capped, but it evicted "the first key", i.e. the numerically
    /// smallest destination hash — deterministic, unrelated to age, and it
    /// meant a destination whose hash starts with 0x00 could never stay
    /// known. FIFO with refresh drops the least recently re-announced
    /// instead. A dropped identity is re-learned from the next announce.
    known_identities: BoundedMap<[u8; TRUNCATED_HASHBYTES], Identity>,

    // Known ratchets (sender-side cache)
    /// **Eviction: oldest first, refreshed on re-announce.** A dropped
    /// ratchet means the next packet to that destination goes out under the
    /// long-term key, which is a privacy loss, not a delivery failure, and
    /// the next announce restores it.
    known_ratchets: BoundedMap<[u8; TRUNCATED_HASHBYTES], ([u8; RATCHET_SIZE], u64)>,

    // Local client destinations (per-interface tracking)
    /// Destinations each locally attached client has registered, keyed by
    /// `(interface index, destination hash)` so the pair carries one FIFO
    /// position — the previous map-of-sets had no order to evict by at all.
    ///
    /// **Eviction: oldest first.** Dropping an entry means a local client's
    /// destination stops being recognised as local until it re-registers.
    /// The cap exists so a looping local client cannot grow the daemon
    /// without bound; at 4 096 it is far above any real client's
    /// registration count.
    local_client_dest_map: BoundedMap<(usize, [u8; TRUNCATED_HASHBYTES]), ()>,

    // Local client known destinations (persist across disconnects)
    /// **Eviction: oldest first.** Same population and same argument as
    /// `local_client_dest_map`; an entry dropped early is re-added the next
    /// time the client is seen.
    local_client_known_dests: BoundedMap<[u8; TRUNCATED_HASHBYTES], u64>,

    // Discovery path requests
    /// Pending discovery path requests: dest_hash → (requesting_interface, timeout_ms)
    /// Removal: expire_discovery_path_requests() or remove_discovery_path_request()
    ///
    /// **Eviction: oldest first, and no refresh** — the table's own rule is
    /// that the first request wins (Python `Transport.py:2793-2794`), so a
    /// repeat request must not move the entry or change its interface.
    /// Remote peers drive this one, which is why it gets a ceiling at all.
    discovery_path_requests: BoundedMap<[u8; TRUNCATED_HASHBYTES], (usize, u64)>,

    // Sender-side ratchet keys (destination private keys)
    /// **Eviction: oldest first.** These are our own destinations' ratchet
    /// private keys, so the population is bounded by how many destinations
    /// this node creates; the cap is a backstop against a client that
    /// creates them in a loop.
    dest_ratchet_keys: BoundedMap<[u8; TRUNCATED_HASHBYTES], Vec<u8>>,

    // Known-destination cache lifecycle (Codeberg #84).
    // Mirrors Python's known_destinations[dest][4] use-state field: an entry is
    // either recency-touched (`Used`, Python >0) or pinned (`Retained`, Python
    // -1). Absence means "never used" (Python 0). Keyed by destination hash; the
    // authoritative "known" set is announce_cache. Retained entries survive
    // clean_announce_cache even without a path.
    //
    // **Eviction: oldest unretained first, then oldest** — the same argument
    // as `announce_cache`, whose lifecycle this table describes: evicting a
    // `Retained` marker would silently unpin a destination the application
    // asked to keep.
    known_dest_use: BoundedMap<[u8; TRUNCATED_HASHBYTES], KnownDestUse>,
}

/// Cache-lifecycle state for a known destination, mirroring the fifth field of
/// Python's `Identity.known_destinations` entry (Codeberg #84).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KnownDestUse {
    /// Recency touch: last-used monotonic timestamp in ms (Python `time.time()`, >0).
    Used(u64),
    /// Pinned against eviction (Python sentinel -1).
    Retained,
}

impl MemoryStorage {
    /// Create MemoryStorage with generous defaults (suitable for Linux/desktop)
    pub fn with_defaults() -> Self {
        Self::with_caps(TableCaps::desktop())
    }

    /// Create MemoryStorage with small caps (suitable for constrained devices)
    pub fn compact() -> Self {
        Self::with_caps(TableCaps::compact())
    }

    /// Create MemoryStorage with explicit per-table ceilings (Codeberg #421).
    ///
    /// The caps are fixed at construction: a `BoundedMap` is built around its
    /// ceiling, and a daemon that could change one at runtime would have to
    /// decide what to do with the entries already over it. The operator sets
    /// them in the config file, which is read before the node is built.
    pub fn with_caps(caps: TableCaps) -> Self {
        Self {
            caps,
            packet_cache: BTreeSet::new(),
            packet_cache_prev: BTreeSet::new(),
            path_table: BoundedMap::new(caps.path_cap),
            path_states: BoundedMap::new(caps.path_cap),
            reverse_table: BoundedMap::new(caps.reverse_cap),
            link_table: BoundedMap::new(caps.link_cap),
            announce_table: BoundedMap::new(caps.announce_cap),
            announce_cache: BoundedMap::new(caps.destination_cap),
            announce_rate_table: BoundedMap::new(caps.destination_cap),
            receipts: BoundedMap::new(caps.receipt_cap),
            culled_receipts: VecDeque::new(),
            path_requests: BoundedMap::new(caps.path_cap),
            path_request_tags: VecDeque::new(),
            path_request_tag_set: BTreeSet::new(),
            known_identities: BoundedMap::new(caps.identity_cap),
            known_ratchets: BoundedMap::new(caps.destination_cap),
            local_client_dest_map: BoundedMap::new(caps.local_dest_cap),
            local_client_known_dests: BoundedMap::new(caps.local_dest_cap),
            discovery_path_requests: BoundedMap::new(caps.path_cap),
            dest_ratchet_keys: BoundedMap::new(caps.local_dest_cap),
            known_dest_use: BoundedMap::new(caps.destination_cap),
        }
    }

    /// The ceilings this storage was built with.
    pub fn caps(&self) -> TableCaps {
        self.caps
    }

    /// Write a known-destination use marker, evicting an unpinned marker
    /// before a `Retained` one.
    ///
    /// Same argument as `announce_cache`, whose lifecycle this table
    /// describes: a size overflow that dropped a `Retained` marker would
    /// silently unpin a destination the application asked to keep, and the
    /// next `clean_announce_cache` would then reap its cached announce.
    fn set_known_dest_use(&mut self, dest: [u8; TRUNCATED_HASHBYTES], use_state: KnownDestUse) {
        self.known_dest_use
            .insert_preferring(dest, use_state, |_, v| !matches!(v, KnownDestUse::Retained));
    }

    // Test convenience methods
    /// Number of packet hashes in both generations
    pub fn packet_hash_count(&self) -> usize {
        self.packet_cache.len() + self.packet_cache_prev.len()
    }

    /// Number of path request tags stored
    pub fn path_request_tag_count(&self) -> usize {
        self.path_request_tags.len()
    }

    /// Number of link table entries
    pub fn link_entry_count(&self) -> usize {
        self.link_table.len()
    }

    /// Iterate over all link table entry values
    pub fn link_entry_values(&self) -> impl Iterator<Item = &LinkEntry> {
        self.link_table.values()
    }

    /// Clear all packet hashes (test convenience)
    pub fn clear_packet_hashes(&mut self) {
        self.packet_cache.clear();
        self.packet_cache_prev.clear();
    }

    /// Clear all state (test convenience)
    pub fn clear_all(&mut self) {
        self.packet_cache.clear();
        self.packet_cache_prev.clear();
        self.path_table.clear();
        self.path_states.clear();
        self.reverse_table.clear();
        self.link_table.clear();
        self.announce_table.clear();
        self.announce_cache.clear();
        self.announce_rate_table.clear();
        self.receipts.clear();
        self.culled_receipts.clear();
        self.path_requests.clear();
        self.path_request_tags.clear();
        self.path_request_tag_set.clear();
        self.known_identities.clear();
        self.known_ratchets.clear();
        self.local_client_dest_map.clear();
        self.local_client_known_dests.clear();
        self.discovery_path_requests.clear();
        self.dest_ratchet_keys.clear();
        self.known_dest_use.clear();
    }

    /// Number of entries in the announce rate table (test/stats convenience)
    pub fn announce_rate_count(&self) -> usize {
        self.announce_rate_table.len()
    }

    /// Iterate all packet hashes across both generations (for persistence on flush)
    pub fn packet_hash_iter(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.packet_cache
            .iter()
            .chain(self.packet_cache_prev.iter())
    }

    /// Iterate all known ratchets (for FileStorage disk persistence and expiry)
    pub fn known_ratchet_iter(
        &self,
    ) -> impl Iterator<Item = (&[u8; TRUNCATED_HASHBYTES], &([u8; RATCHET_SIZE], u64))> {
        self.known_ratchets.iter()
    }

    /// Iterate all known identities (for persistence on flush)
    pub fn known_identity_iter(
        &self,
    ) -> impl Iterator<Item = (&[u8; TRUNCATED_HASHBYTES], &Identity)> {
        self.known_identities.iter()
    }

    /// Rotate packet cache: current becomes prev, fresh empty set takes its place
    fn rotate_packet_cache(&mut self) {
        core::mem::swap(&mut self.packet_cache, &mut self.packet_cache_prev);
        self.packet_cache.clear();
    }

    /// Diagnostic dump for packet_cache and packet_cache_prev only.
    ///
    /// Returns (formatted_text, estimated_bytes). FileStorage overrides packet_cache
    /// with its own HashSet and calls only `diagnostic_dump_non_packet_cache()`.
    pub fn diagnostic_dump_packet_cache(&self) -> (String, u64) {
        use core::fmt::Write;
        let mut s = String::new();
        let mut total = 0u64;

        // packet_cache: BTreeSet<[u8; 32]>, 3x overhead
        let n = self.packet_cache.len();
        let raw = (n * 32) as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "packet_cache: {} entries, raw {} bytes, estimated {} bytes (BTreeSet 3x)",
            n, raw, est
        );

        // packet_cache_prev: BTreeSet<[u8; 32]>, 3x
        let n = self.packet_cache_prev.len();
        let raw = (n * 32) as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "packet_cache_prev: {} entries, raw {} bytes, estimated {} bytes (BTreeSet 3x)",
            n, raw, est
        );

        (s, total)
    }

    /// Diagnostic dump for all collections except packet_cache/packet_cache_prev.
    ///
    /// Used by FileStorage which manages its own packet_cache HashSets.
    pub fn diagnostic_dump_non_packet_cache(&self) -> (String, u64) {
        use core::fmt::Write;
        let mut s = String::new();
        let mut total = 0u64;

        // path_table: BTreeMap<[u8; 16], PathEntry>, 3x.
        // The blob window is priced by CAPACITY, not by length: capacity is
        // what was asked of the allocator and what the process holds, and
        // the two differ whenever a window has been appended to past its
        // cap.
        let n = self.path_table.len();
        let mut raw = 0u64;
        for entry in self.path_table.values() {
            raw += (HASH_ORDER_BYTES
                + TRUNCATED_HASHBYTES
                + core::mem::size_of::<PathEntry>()
                + entry.random_blobs.capacity() * crate::constants::RANDOM_HASHBYTES)
                as u64;
        }
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "path_table: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // path_states: BTreeMap<[u8; 16], PathState>, 3x
        let n = self.path_states.len();
        let raw = (n * (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + core::mem::size_of::<PathState>()))
            as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "path_states: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // reverse_table: BTreeMap<[u8; 16], ReverseEntry>, 3x
        let n = self.reverse_table.len();
        let raw = (n
            * (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + core::mem::size_of::<ReverseEntry>()))
            as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "reverse_table: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // link_table: BTreeMap<[u8; 16], LinkEntry>, 3x.
        // `peer_signing_key` is an Option<[u8; 32]> stored INLINE, so it
        // costs the same whether or not a proof ever arrived; the old model
        // added it conditionally and under-priced every unvalidated link.
        let n = self.link_table.len();
        let raw = (n * (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + core::mem::size_of::<LinkEntry>()))
            as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "link_table: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // announce_table: BTreeMap<[u8; 16], AnnounceEntry>, 3x. The
        // `raw_packet` heap is the second full copy of the announce beside
        // `announce_cache`, which is the whole reason this row is worth
        // reading.
        let n = self.announce_table.len();
        let mut raw = 0u64;
        for entry in self.announce_table.values() {
            raw += (HASH_ORDER_BYTES
                + TRUNCATED_HASHBYTES
                + core::mem::size_of::<AnnounceEntry>()
                + entry.raw_packet.capacity()) as u64;
        }
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "announce_table: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // announce_cache: BTreeMap<[u8; 16], Vec<u8>>, 3x
        let n = self.announce_cache.len();
        let mut raw = 0u64;
        for v in self.announce_cache.values() {
            raw += (HASH_ORDER_BYTES
                + TRUNCATED_HASHBYTES
                + core::mem::size_of::<Vec<u8>>()
                + v.capacity()) as u64;
        }
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "announce_cache: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // announce_rate_table: BTreeMap<[u8; 16], AnnounceRateEntry>, 3x
        let n = self.announce_rate_table.len();
        let raw = (n
            * (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + core::mem::size_of::<AnnounceRateEntry>()))
            as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "announce_rate_table: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // receipts: BTreeMap<[u8; 16], PacketReceipt>, 3x
        let n = self.receipts.len();
        let raw = (n
            * (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + core::mem::size_of::<PacketReceipt>()))
            as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "receipts: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // culled_receipts: VecDeque<PacketReceipt>, 1x. Pending receipts a
        // size overflow dropped, held only until the next expiry sweep
        // reports them as timeouts.
        let n = self.culled_receipts.len();
        let raw = (n * core::mem::size_of::<PacketReceipt>()) as u64;
        let est = raw;
        total += est;
        let _ = writeln!(
            s,
            "culled_receipts: {} entries, raw {} bytes, estimated {} bytes (VecDeque 1x)",
            n, raw, est
        );

        // path_requests: BTreeMap<[u8; 16], u64>, 3x
        let n = self.path_requests.len();
        let raw =
            (n * (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + core::mem::size_of::<u64>())) as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "path_requests: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // path_request_tags: VecDeque<[u8; 32]>, 1x
        let n = self.path_request_tags.len();
        let raw = (n * 32) as u64;
        let est = raw;
        total += est;
        let _ = writeln!(
            s,
            "path_request_tags: {} entries, raw {} bytes, estimated {} bytes (VecDeque 1x)",
            n, raw, est
        );

        // path_request_tag_set: BTreeSet<[u8; 32]>, 3x
        let n = self.path_request_tag_set.len();
        let raw = (n * 32) as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "path_request_tag_set: {} entries, raw {} bytes, estimated {} bytes (BTreeSet 3x)",
            n, raw, est
        );

        // known_identities: BTreeMap<[u8; 16], Identity>, 3x.
        //
        // `Identity` is priced by `size_of`, not by a number written down
        // here. It used to be 128 and the type is four times that -- an
        // expanded ed25519 `VerifyingKey` alone is 192 bytes -- so on the
        // miauhaus soak node, sitting on the 50 000-entry identity cap,
        // the dump under-reported its single largest table by ~58 MB and
        // the resident-set gap it was being used to explain was
        // correspondingly overstated (2026-09-21). Every key is stored
        // inline, so `size_of` is the whole cost and cannot drift again.
        //
        // That is now the rule for every row above and below, not an
        // exception made for one type: a literal byte count is a claim
        // about a struct that nothing rechecks, and this dump is read to
        // attribute a resident-set gap -- a row that under-reports is
        // worse than no row, because it is believed.
        // `diagnostic_model_tests` holds each row to it.
        let n = self.known_identities.len();
        let raw = (n * (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + core::mem::size_of::<Identity>()))
            as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "known_identities: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // known_ratchets: BTreeMap<[u8; 16], ([u8; 32], u64)>, 3x
        let n = self.known_ratchets.len();
        let raw = (n
            * (HASH_ORDER_BYTES
                + TRUNCATED_HASHBYTES
                + core::mem::size_of::<([u8; RATCHET_SIZE], u64)>())) as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "known_ratchets: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // local_client_dest_map: BoundedMap<(usize, [u8; 16]), ()>, 3x.
        // Keyed by the (interface, destination) pair since #421, so a
        // destination costs its key and its order-index slot and nothing
        // else; the map-of-sets this replaced also charged a BTreeSet header
        // per interface.
        let n = self.local_client_dest_map.len();
        let raw = (n
            * (core::mem::size_of::<(usize, [u8; TRUNCATED_HASHBYTES])>()
                + order_index_bytes(core::mem::size_of::<(usize, [u8; TRUNCATED_HASHBYTES])>())))
            as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "local_client_dest_map: {} dest entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // local_client_known_dests: BTreeMap<[u8; 16], u64>, 3x
        let n = self.local_client_known_dests.len();
        let raw =
            (n * (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + core::mem::size_of::<u64>())) as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "local_client_known_dests: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // discovery_path_requests: BTreeMap<[u8; 16], (usize, u64)>, 3x
        let n = self.discovery_path_requests.len();
        let raw = (n
            * (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + core::mem::size_of::<(usize, u64)>()))
            as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "discovery_path_requests: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // dest_ratchet_keys: BTreeMap<[u8; 16], Vec<u8>>, 3x
        let n = self.dest_ratchet_keys.len();
        let mut raw = 0u64;
        for v in self.dest_ratchet_keys.values() {
            raw += (HASH_ORDER_BYTES
                + TRUNCATED_HASHBYTES
                + core::mem::size_of::<Vec<u8>>()
                + v.capacity()) as u64;
        }
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "dest_ratchet_keys: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        // known_dest_use: BTreeMap<[u8; 16], KnownDestUse>, 3x
        let n = self.known_dest_use.len();
        let raw = (n
            * (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + core::mem::size_of::<KnownDestUse>()))
            as u64;
        let est = raw * 3;
        total += est;
        let _ = writeln!(
            s,
            "known_dest_use: {} entries, raw {} bytes, estimated {} bytes (BTreeMap 3x)",
            n, raw, est
        );

        (s, total)
    }
}

#[cfg(test)]
impl MemoryStorage {
    /// Test-only: count of known ratchets stored
    pub fn known_ratchet_count(&self) -> usize {
        self.known_ratchets.len()
    }

    /// Test-only: check if a destination hash is tracked for a local client interface
    pub fn has_local_client_dest(
        &self,
        iface_id: usize,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
    ) -> bool {
        self.local_client_dest_map
            .contains_key(&(iface_id, *dest_hash))
    }

    /// Test-only: check if a destination hash is in the known-dest set
    pub fn has_local_client_known_dest(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        self.local_client_known_dests.contains_key(dest_hash)
    }
}

impl Storage for MemoryStorage {
    // Packet Dedup
    fn has_packet_hash(&self, hash: &[u8; 32]) -> bool {
        self.packet_cache.contains(hash) || self.packet_cache_prev.contains(hash)
    }

    fn add_packet_hash(&mut self, hash: [u8; 32]) {
        self.packet_cache.insert(hash);
        // Two-generation rotation: when current exceeds half cap, rotate
        if self.packet_cache.len() > self.caps.packet_hash_cap / 2 {
            self.rotate_packet_cache();
        }
    }

    // Path Table
    fn get_path(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<&PathEntry> {
        self.path_table.get(dest_hash)
    }

    fn set_path(&mut self, dest_hash: [u8; TRUNCATED_HASHBYTES], entry: PathEntry) {
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
        self.path_table.retain(|hash, entry| {
            if entry.expires_ms < now_ms {
                expired.push(*hash);
                false
            } else {
                true
            }
        });
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
        // An overflow drops a half-open link request before it breaks a live
        // link; see the field's eviction note.
        self.link_table
            .insert_preferring(link_id, entry, |_, e| !e.validated);
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
        // A retained destination is pinned against the time sweep
        // (`clean_announce_cache`), so a size overflow must not unpin it by
        // the back door; see the field's eviction note.
        let Self {
            announce_cache,
            known_dest_use,
            ..
        } = self;
        announce_cache.insert_preferring(dest_hash, raw, |hash, _| {
            !matches!(known_dest_use.get(hash), Some(KnownDestUse::Retained))
        });
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
        // Terminal receipts are only serving out their retention grace, so
        // they go before a pending one; a pending one that has to go is
        // parked for the next sweep to report as a timeout, which is what
        // Python does at Transport.py:558-561.
        let culled = self
            .receipts
            .insert_preferring(hash, receipt, |_, r| r.status != ReceiptStatus::Sent);
        if let Some((_, dropped)) = culled {
            if dropped.status == ReceiptStatus::Sent {
                if self.culled_receipts.len() >= self.caps.receipt_cap {
                    self.culled_receipts.pop_front();
                }
                self.culled_receipts.push_back(dropped);
            }
        }
    }

    // Path Requests
    fn get_path_request_time(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<u64> {
        self.path_requests.get(dest_hash).copied()
    }

    fn set_path_request_time(&mut self, dest_hash: [u8; TRUNCATED_HASHBYTES], time_ms: u64) {
        self.path_requests.insert(dest_hash, time_ms);
    }

    fn expire_path_requests(&mut self, now_ms: u64, max_age_ms: u64) {
        self.path_requests
            .retain(|_, last_ms| now_ms.saturating_sub(*last_ms) < max_age_ms);
    }

    fn check_path_request_tag(&mut self, tag: &[u8; 32]) -> bool {
        if self.path_request_tag_set.contains(tag) {
            return true;
        }
        self.path_request_tags.push_back(*tag);
        self.path_request_tag_set.insert(*tag);
        while self.path_request_tags.len() > MAX_PATH_REQUEST_TAGS {
            if let Some(evicted) = self.path_request_tags.pop_front() {
                self.path_request_tag_set.remove(&evicted);
            }
        }
        false
    }

    // Known Identities
    fn get_identity(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<&Identity> {
        self.known_identities.get(dest_hash)
    }

    fn set_identity(&mut self, dest_hash: [u8; TRUNCATED_HASHBYTES], identity: Identity) {
        // Drop-oldest with refresh-on-re-announce. Before #421 this evicted
        // the numerically smallest destination hash, which is deterministic
        // but unrelated to age: a destination whose hash starts low could
        // never stay known on a full node.
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
        // Pending receipts a size overflow had to cull owe their sender a
        // timeout, and this sweep is what delivers it.
        let mut expired: Vec<PacketReceipt> = self.culled_receipts.drain(..).collect();
        self.receipts.retain(|_, receipt| {
            if receipt.status == ReceiptStatus::Sent && receipt.is_expired(now_ms) {
                // A still-pending packet whose proof never came: timed out.
                // Returned so the caller can emit a ReceiptTimeout event.
                expired.push(receipt.clone());
                false
            } else if receipt.status != ReceiptStatus::Sent
                && now_ms.saturating_sub(receipt.sent_at_ms)
                    > receipt.timeout_ms.saturating_add(RECEIPT_RETENTION_MS)
            {
                // A terminal (Delivered/Failed) receipt kept only for the
                // retention grace window is now reaped silently: the outcome
                // already reached the application, so it is not returned as a
                // timeout (Codeberg #275).
                false
            } else {
                true
            }
        });
        expired
    }

    fn expire_link_entries(
        &mut self,
        now_ms: u64,
        link_timeout_ms: u64,
    ) -> Vec<([u8; TRUNCATED_HASHBYTES], LinkEntry)> {
        let mut expired = Vec::new();
        self.link_table.retain(|hash, entry| {
            let is_expired = if entry.validated {
                now_ms.saturating_sub(entry.timestamp_ms) > link_timeout_ms
            } else {
                now_ms > entry.proof_timeout_ms
            };
            if is_expired {
                expired.push((*hash, entry.clone()));
                false
            } else {
                true
            }
        });
        expired
    }

    fn clean_stale_path_metadata(&mut self) {
        self.path_states
            .retain(|hash, _| self.path_table.contains_key(hash));
        self.announce_rate_table
            .retain(|hash, _| self.path_table.contains_key(hash));
    }

    fn clean_announce_cache(&mut self, local_destinations: &BTreeSet<[u8; TRUNCATED_HASHBYTES]>) {
        let known_dest_use = &self.known_dest_use;
        self.announce_cache.retain(|hash, _| {
            self.path_table.contains_key(hash)
                || local_destinations.contains(hash)
                || matches!(known_dest_use.get(hash), Some(KnownDestUse::Retained))
        });
        // Drop lifecycle state for destinations whose cached announce was
        // evicted (retained ones survive above, so this only reaps stale
        // Used timestamps), mirroring Python popping the whole entry.
        let announce_cache = &self.announce_cache;
        self.known_dest_use
            .retain(|hash, _| announce_cache.contains_key(hash));
    }

    fn announce_cache_keys(&self) -> Vec<[u8; TRUNCATED_HASHBYTES]> {
        self.announce_cache.keys().copied().collect()
    }

    fn retain_known_dest(&mut self, dest: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        // Python Identity._retain_destination_data: pin iff the destination is
        // known (has a cached announce), setting use-state to the -1 sentinel.
        if self.announce_cache.contains_key(dest) {
            self.set_known_dest_use(*dest, KnownDestUse::Retained);
            true
        } else {
            false
        }
    }

    fn unretain_known_dest(&mut self, dest: &[u8; TRUNCATED_HASHBYTES], now_ms: u64) -> bool {
        // Python Identity._unretain_destination_data: reset use-state to a
        // recency timestamp (lifting the pin) iff the destination is known.
        if self.announce_cache.contains_key(dest) {
            self.set_known_dest_use(*dest, KnownDestUse::Used(now_ms));
            true
        } else {
            false
        }
    }

    fn used_known_dest(&mut self, dest: &[u8; TRUNCATED_HASHBYTES], now_ms: u64) -> bool {
        // Python Identity._used_destination_data: touch recency only when the
        // destination is known AND not retained (use-state not < 0); a retained
        // entry is left pinned and the call reports False.
        if !self.announce_cache.contains_key(dest) {
            return false;
        }
        if matches!(self.known_dest_use.get(dest), Some(KnownDestUse::Retained)) {
            return false;
        }
        self.set_known_dest_use(*dest, KnownDestUse::Used(now_ms));
        true
    }

    fn is_known_dest_retained(&self, dest: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        matches!(self.known_dest_use.get(dest), Some(KnownDestUse::Retained))
    }

    fn known_dest_last_used(&self, dest: &[u8; TRUNCATED_HASHBYTES]) -> Option<u64> {
        match self.known_dest_use.get(dest) {
            Some(KnownDestUse::Used(ts)) => Some(*ts),
            _ => None,
        }
    }

    fn remove_link_entries_for_interface(
        &mut self,
        iface_index: usize,
    ) -> Vec<([u8; TRUNCATED_HASHBYTES], LinkEntry)> {
        let mut removed = Vec::new();
        self.link_table.retain(|hash, entry| {
            if entry.received_interface_index == iface_index
                || entry.next_hop_interface_index == iface_index
            {
                removed.push((*hash, entry.clone()));
                false
            } else {
                true
            }
        });
        removed
    }

    fn remove_paths_for_interface(&mut self, iface_index: usize) -> Vec<[u8; TRUNCATED_HASHBYTES]> {
        let mut removed = Vec::new();
        self.path_table.retain(|hash, entry| {
            if entry.interface_index == iface_index {
                removed.push(*hash);
                false
            } else {
                true
            }
        });
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

    // Diagnostics
    /// Every collection above, in field order, with the ceilings the code
    /// above actually enforces (Codeberg #174).
    ///
    /// Since Codeberg #421 every row carries a ceiling: the two dedup
    /// generations rotate at half `packet_hash_cap`, the path-request tag
    /// ring and its index are trimmed to `MAX_PATH_REQUEST_TAGS`, and every
    /// other collection is a [`BoundedMap`] reporting its own capacity. A
    /// `None` here would mean a table whose size the neighbours choose, which
    /// is the shape #421 was opened to remove; `every_collection_declares_a_ceiling`
    /// holds the list to that.
    ///
    /// Cost: `len()` throughout, O(1), except `local_client_dest_map` whose
    /// entries live in the inner sets and are summed over the local
    /// interfaces (a single-digit count on every node we run). The number
    /// reported for it is the destination total, not the interface count,
    /// because what it costs is destinations.
    fn collection_counts(&self) -> Vec<CollectionCount> {
        // A generation is rotated out once it passes half the cap, so half
        // the cap — not the cap — is the ceiling either generation is held
        // to. Reporting the full cap here would make a rotating cache look
        // half empty at the moment it rotates.
        let generation_cap = self.caps.packet_hash_cap / 2;
        vec![
            CollectionCount::bounded("packet_cache", self.packet_cache.len(), generation_cap),
            CollectionCount::bounded(
                "packet_cache_prev",
                self.packet_cache_prev.len(),
                generation_cap,
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
                "reverse_table",
                self.reverse_table.len(),
                self.reverse_table.capacity(),
            ),
            CollectionCount::bounded(
                "link_table",
                self.link_table.len(),
                self.link_table.capacity(),
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
            CollectionCount::bounded("receipts", self.receipts.len(), self.receipts.capacity()),
            CollectionCount::bounded(
                "culled_receipts",
                self.culled_receipts.len(),
                self.caps.receipt_cap,
            ),
            CollectionCount::bounded(
                "path_requests",
                self.path_requests.len(),
                self.path_requests.capacity(),
            ),
            CollectionCount::bounded(
                "path_request_tags",
                self.path_request_tags.len(),
                MAX_PATH_REQUEST_TAGS,
            ),
            CollectionCount::bounded(
                "path_request_tag_set",
                self.path_request_tag_set.len(),
                MAX_PATH_REQUEST_TAGS,
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
                "local_client_dest_map",
                self.local_client_dest_map.len(),
                self.local_client_dest_map.capacity(),
            ),
            CollectionCount::bounded(
                "local_client_known_dests",
                self.local_client_known_dests.len(),
                self.local_client_known_dests.capacity(),
            ),
            CollectionCount::bounded(
                "discovery_path_requests",
                self.discovery_path_requests.len(),
                self.discovery_path_requests.capacity(),
            ),
            CollectionCount::bounded(
                "dest_ratchet_keys",
                self.dest_ratchet_keys.len(),
                self.dest_ratchet_keys.capacity(),
            ),
            CollectionCount::bounded(
                "known_dest_use",
                self.known_dest_use.len(),
                self.known_dest_use.capacity(),
            ),
        ]
    }

    fn diagnostic_dump(&self) -> (String, u64) {
        let (mut s, mut total) = self.diagnostic_dump_packet_cache();
        let (s2, total2) = self.diagnostic_dump_non_packet_cache();
        s.push_str(&s2);
        total += total2;
        (s, total)
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

    // Local Client Destinations
    fn add_local_client_dest(
        &mut self,
        iface_id: usize,
        dest_hash: [u8; TRUNCATED_HASHBYTES],
    ) -> bool {
        self.local_client_dest_map
            .insert_if_absent((iface_id, dest_hash), ())
    }

    fn remove_local_client_dests(&mut self, iface_id: usize) {
        self.local_client_dest_map
            .retain(|(i, _), _| *i != iface_id);
    }

    // Local Client Known Destinations
    fn set_local_client_known_dest(
        &mut self,
        dest_hash: [u8; TRUNCATED_HASHBYTES],
        last_seen_ms: u64,
    ) {
        self.local_client_known_dests
            .insert(dest_hash, last_seen_ms);
    }

    fn local_client_known_dest_hashes(&self) -> Vec<[u8; TRUNCATED_HASHBYTES]> {
        self.local_client_known_dests.keys().copied().collect()
    }

    fn expire_local_client_known_dests(&mut self, now_ms: u64, expiry_ms: u64) -> usize {
        let before = self.local_client_known_dests.len();
        self.local_client_known_dests
            .retain(|_, last_seen| now_ms.saturating_sub(*last_seen) < expiry_ms);
        before - self.local_client_known_dests.len()
    }

    // Discovery Path Requests
    fn set_discovery_path_request(
        &mut self,
        dest_hash: [u8; TRUNCATED_HASHBYTES],
        requesting_interface: usize,
        timeout_ms: u64,
    ) {
        // Only store first request (`discovery_path_requests`, Transport.py:3016-3017)
        self.discovery_path_requests
            .insert_if_absent(dest_hash, (requesting_interface, timeout_ms));
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::DestinationHash;
    use alloc::vec;

    /// Codeberg #174: the census and the struct agree, in both directions.
    ///
    /// Rust has no reflection, so the binding is the source text of this very
    /// file: a collection field added without a counter fails here rather
    /// than quietly going unreported. That failure mode is not hypothetical —
    /// `known_dest_use` (Codeberg #84) was added to this struct and to
    /// nothing that reports it, and stayed invisible until a soak node's
    /// memory had to be attributed.
    #[test]
    fn collection_counts_names_every_collection_field() {
        let fields = crate::storage_census::collection_fields(
            include_str!("memory_storage.rs"),
            "MemoryStorage",
        )
        .expect("MemoryStorage is declared in this file");
        assert!(
            fields.len() > 10,
            "the parse found only {fields:?}, which cannot be this struct"
        );

        let storage = MemoryStorage::with_defaults();
        let counted: Vec<&str> = storage.collection_counts().iter().map(|c| c.name).collect();

        for field in &fields {
            assert!(
                counted.contains(&field.as_str()),
                "MemoryStorage.{field} is a collection that collection_counts() does not report"
            );
        }
        for name in &counted {
            assert!(
                fields.iter().any(|f| f == name),
                "collection_counts() reports {name}, which is not a collection field"
            );
        }
        assert_eq!(
            counted.len(),
            fields.len(),
            "one row per collection, no duplicates: {counted:?} vs {fields:?}"
        );
    }

    /// The text dump is the other surface that enumerates these collections
    /// by hand, and it is the one that already drifted. Bind it to the same
    /// list so it cannot drift again separately.
    #[test]
    fn diagnostic_dump_names_every_collection_field() {
        let storage = MemoryStorage::with_defaults();
        let (dump, _) = storage.diagnostic_dump();
        for row in storage.collection_counts() {
            assert!(
                dump.contains(row.name),
                "diagnostic_dump() does not mention {}",
                row.name
            );
        }
    }

    /// A count without its ceiling does not say how close to full a table is.
    /// Every collection reports the ceiling it is actually held to, and the
    /// two dedup generations report half the hash cap, because half is what
    /// either generation is held to across a rotation.
    #[test]
    fn collection_counts_carry_the_configured_ceilings() {
        let storage = MemoryStorage::with_caps(TableCaps {
            packet_hash_cap: 10,
            identity_cap: 7,
            ..TableCaps::desktop()
        });
        let cap = |name: &str| {
            storage
                .collection_counts()
                .into_iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("{name} must be reported"))
                .capacity
        };
        // Half the cap: that is the size at which a generation rotates.
        assert_eq!(cap("packet_cache"), Some(5));
        assert_eq!(cap("packet_cache_prev"), Some(5));
        assert_eq!(cap("known_identities"), Some(7));
        assert_eq!(cap("path_request_tags"), Some(MAX_PATH_REQUEST_TAGS));
        assert_eq!(cap("path_request_tag_set"), Some(MAX_PATH_REQUEST_TAGS));
        assert_eq!(cap("path_table"), Some(TableCaps::desktop().path_cap));
        assert_eq!(
            cap("announce_cache"),
            Some(TableCaps::desktop().destination_cap)
        );
    }

    /// The rotation is the event worth seeing, and a sum of the two
    /// generations hides it: the total barely moves across a rotation while
    /// one generation is freed whole. Reported separately, the step is
    /// visible.
    #[test]
    fn the_two_dedup_generations_are_counted_separately() {
        let mut storage = MemoryStorage::with_caps(TableCaps {
            packet_hash_cap: 10,
            ..TableCaps::desktop()
        });
        let count = |s: &MemoryStorage, name: &str| {
            s.collection_counts()
                .into_iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("{name} must be reported"))
                .entries
        };
        let add = |storage: &mut MemoryStorage, i: u8| {
            let mut hash = [0u8; 32];
            hash[0] = i;
            storage.add_packet_hash(hash);
        };

        for i in 0..5u8 {
            add(&mut storage, i);
        }
        assert_eq!(count(&storage, "packet_cache"), 5);
        assert_eq!(count(&storage, "packet_cache_prev"), 0);

        // The sixth hash crosses the threshold: the generation is handed to
        // prev whole and a fresh empty one takes its place.
        add(&mut storage, 5);
        assert_eq!(count(&storage, "packet_cache"), 0);
        assert_eq!(count(&storage, "packet_cache_prev"), 6);

        // Fill the new generation to the threshold again. Now the rotation
        // FREES the six hashes in prev, and this is the shape a sawtooth in
        // the resident set has: the sum falls by a whole generation at once.
        for i in 6..11u8 {
            add(&mut storage, i);
        }
        let sum_before = count(&storage, "packet_cache") + count(&storage, "packet_cache_prev");
        assert_eq!(sum_before, 11);
        add(&mut storage, 11);
        assert_eq!(count(&storage, "packet_cache"), 0);
        assert_eq!(count(&storage, "packet_cache_prev"), 6);
    }

    /// A map of sets costs what its inner sets hold, not what its outer map
    /// holds, so that is what the count states.
    #[test]
    fn local_client_dest_map_counts_destinations_not_interfaces() {
        let mut storage = MemoryStorage::with_defaults();
        for (iface, dest) in [(0usize, 1u8), (0, 2), (1, 3)] {
            storage.add_local_client_dest(iface, [dest; TRUNCATED_HASHBYTES]);
        }
        let row = storage
            .collection_counts()
            .into_iter()
            .find(|c| c.name == "local_client_dest_map")
            .expect("local_client_dest_map must be reported");
        assert_eq!(row.entries, 3);
    }

    #[test]
    fn test_packet_hash_dedup() {
        let mut s = MemoryStorage::with_defaults();
        let hash = [0x42u8; 32];
        assert!(!s.has_packet_hash(&hash));
        s.add_packet_hash(hash);
        assert!(s.has_packet_hash(&hash));
    }

    #[test]
    fn test_packet_hash_rotation() {
        let mut s = MemoryStorage::with_caps(TableCaps {
            packet_hash_cap: 10,
            ..TableCaps::desktop()
        });
        // Add 6 hashes (exceeds half of 10 = 5), triggers rotation
        for i in 0..6u8 {
            let mut hash = [0u8; 32];
            hash[0] = i;
            s.add_packet_hash(hash);
        }
        // First 5 hashes should be in prev, hash[5] in current
        let mut hash0 = [0u8; 32];
        hash0[0] = 0;
        assert!(s.has_packet_hash(&hash0)); // in prev

        let mut hash5 = [0u8; 32];
        hash5[0] = 5;
        assert!(s.has_packet_hash(&hash5)); // in current
    }

    #[test]
    fn test_path_operations() {
        let mut s = MemoryStorage::with_defaults();
        let hash = [0x01u8; TRUNCATED_HASHBYTES];
        assert!(s.get_path(&hash).is_none());
        assert_eq!(s.path_count(), 0);

        s.set_path(
            hash,
            PathEntry {
                hops: 2,
                expires_ms: 5000,
                interface_index: 0,
                random_blobs: Vec::new(),
                next_hop: None,
                via_peer: None,
            },
        );
        assert_eq!(s.path_count(), 1);
        assert_eq!(s.get_path(&hash).unwrap().hops, 2);
        assert!(s.has_path(&hash));

        let removed = s.remove_path(&hash);
        assert!(removed.is_some());
        assert_eq!(s.path_count(), 0);
    }

    #[test]
    fn test_path_expiry() {
        let mut s = MemoryStorage::with_defaults();
        let h1 = [0x01u8; TRUNCATED_HASHBYTES];
        let h2 = [0x02u8; TRUNCATED_HASHBYTES];

        s.set_path(
            h1,
            PathEntry {
                hops: 0,
                expires_ms: 1000,
                interface_index: 0,
                random_blobs: Vec::new(),
                next_hop: None,
                via_peer: None,
            },
        );
        s.set_path(
            h2,
            PathEntry {
                hops: 0,
                expires_ms: 5000,
                interface_index: 0,
                random_blobs: Vec::new(),
                next_hop: None,
                via_peer: None,
            },
        );
        assert_eq!(s.earliest_path_expiry(), Some(1000));

        let expired = s.expire_paths(2000);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0], h1);
        assert_eq!(s.path_count(), 1);
    }

    #[test]
    fn test_reverse_table() {
        let mut s = MemoryStorage::with_defaults();
        let hash = [0x01u8; TRUNCATED_HASHBYTES];

        s.set_reverse(
            hash,
            ReverseEntry {
                timestamp_ms: 1000,
                receiving_interface_index: 0,
                outbound_interface_index: 1,
            },
        );
        assert!(s.get_reverse(&hash).is_some());
        assert_eq!(s.get_reverse(&hash).unwrap().receiving_interface_index, 0);

        let count = s.expire_reverses(2000, 500);
        assert_eq!(count, 1);
        assert!(s.get_reverse(&hash).is_none());
    }

    #[test]
    fn test_receipt_operations() {
        let mut s = MemoryStorage::with_defaults();
        let hash = [0x01u8; TRUNCATED_HASHBYTES];
        let receipt = PacketReceipt::new([0x42u8; 32], DestinationHash::new(hash), 1000);

        s.set_receipt(hash, receipt);
        assert!(s.get_receipt(&hash).is_some());
        assert_eq!(s.earliest_receipt_deadline(), Some(1000 + 30_000));

        let expired = s.expire_receipts(50_000);
        assert_eq!(expired.len(), 1);
    }

    #[test]
    fn delivered_receipts_are_reaped_after_the_retention_window() {
        // Codeberg #275: a proved send used to leak its receipt forever. Store
        // several delivered receipts, advance the clock past the retention
        // window, and the table must be empty with no spurious timeout events.
        let mut s = MemoryStorage::with_defaults();
        let sent_at = 1_000u64;
        let mut hashes = Vec::new();
        for i in 0..8u8 {
            let hash = [i; TRUNCATED_HASHBYTES];
            let mut receipt = PacketReceipt::new([i; 32], DestinationHash::new(hash), sent_at);
            receipt.set_delivered();
            s.set_receipt(hash, receipt);
            hashes.push(hash);
        }

        // Still inside the window: nothing reaped yet.
        let inside = sent_at + crate::constants::DATA_RECEIPT_TIMEOUT_MS;
        assert!(s.expire_receipts(inside).is_empty());
        assert!(hashes.iter().all(|h| s.get_receipt(h).is_some()));

        // Past timeout + retention: all gone, and no timeout events emitted
        // (they were delivered, not timed out).
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
            hashes.iter().all(|h| s.get_receipt(h).is_none()),
            "every delivered receipt must be reaped"
        );
    }

    #[test]
    fn test_path_request_tag_dedup() {
        let mut s = MemoryStorage::with_defaults();
        let tag = [0x42u8; 32];
        assert!(!s.check_path_request_tag(&tag)); // first time: not duplicate
        assert!(s.check_path_request_tag(&tag)); // second time: duplicate
    }

    #[test]
    fn test_known_identities_cap() {
        let mut s = MemoryStorage::with_caps(TableCaps {
            identity_cap: 2,
            ..TableCaps::desktop()
        });
        let id1 = Identity::generate(&mut rand_core::OsRng);
        let id2 = Identity::generate(&mut rand_core::OsRng);
        let id3 = Identity::generate(&mut rand_core::OsRng);

        s.set_identity([0x01; TRUNCATED_HASHBYTES], id1);
        s.set_identity([0x02; TRUNCATED_HASHBYTES], id2);
        assert_eq!(s.known_identities.len(), 2);

        // Third should evict first
        s.set_identity([0x03; TRUNCATED_HASHBYTES], id3);
        assert_eq!(s.known_identities.len(), 2);
        assert!(s.get_identity(&[0x01; TRUNCATED_HASHBYTES]).is_none());
    }

    #[test]
    fn test_known_ratchet_store_get() {
        let mut s = MemoryStorage::with_defaults();
        let hash = [0xaa; TRUNCATED_HASHBYTES];
        let ratchet = [0xbb; RATCHET_SIZE];

        assert!(s.get_known_ratchet(&hash).is_none());
        assert_eq!(s.known_ratchet_count(), 0);

        s.remember_known_ratchet(hash, ratchet, 1000);
        assert_eq!(s.get_known_ratchet(&hash), Some(ratchet));
        assert_eq!(s.known_ratchet_count(), 1);

        // Update replaces
        let ratchet2 = [0xcc; RATCHET_SIZE];
        s.remember_known_ratchet(hash, ratchet2, 2000);
        assert_eq!(s.get_known_ratchet(&hash), Some(ratchet2));
        assert_eq!(s.known_ratchet_count(), 1);
    }

    #[test]
    fn test_known_ratchet_expire() {
        let mut s = MemoryStorage::with_defaults();
        let h1 = [0x01; TRUNCATED_HASHBYTES];
        let h2 = [0x02; TRUNCATED_HASHBYTES];
        let ratchet = [0xaa; RATCHET_SIZE];

        s.remember_known_ratchet(h1, ratchet, 1000);
        s.remember_known_ratchet(h2, ratchet, 5000);

        // Expire with threshold: h1 is 4000ms old, h2 is 0ms old
        let removed = s.expire_known_ratchets(5000, 3000);
        assert_eq!(removed, 1);
        assert!(s.get_known_ratchet(&h1).is_none());
        assert!(s.get_known_ratchet(&h2).is_some());
    }

    #[test]
    fn test_known_ratchet_iter() {
        let mut s = MemoryStorage::with_defaults();
        let h1 = [0x01; TRUNCATED_HASHBYTES];
        let h2 = [0x02; TRUNCATED_HASHBYTES];
        let r1 = [0xaa; RATCHET_SIZE];
        let r2 = [0xbb; RATCHET_SIZE];

        s.remember_known_ratchet(h1, r1, 1000);
        s.remember_known_ratchet(h2, r2, 2000);

        let entries: alloc::vec::Vec<_> = s.known_ratchet_iter().collect();
        assert_eq!(entries.len(), 2);
        // BTreeMap is sorted by key
        assert_eq!(entries[0].0, &h1);
        assert_eq!(entries[0].1, &(r1, 1000));
        assert_eq!(entries[1].0, &h2);
        assert_eq!(entries[1].1, &(r2, 2000));
    }

    #[test]
    fn test_local_client_dest_add_remove() {
        let mut s = MemoryStorage::with_defaults();
        let hash = [0xaa; TRUNCATED_HASHBYTES];

        assert!(!s.has_local_client_dest(0, &hash));
        assert!(s.add_local_client_dest(0, hash)); // first insert returns true
        assert!(!s.add_local_client_dest(0, hash)); // duplicate returns false
        assert!(s.has_local_client_dest(0, &hash));
        assert!(!s.has_local_client_dest(1, &hash)); // different iface

        s.remove_local_client_dests(0);
        assert!(!s.has_local_client_dest(0, &hash));
    }

    #[test]
    fn test_local_client_known_dest_expire() {
        let mut s = MemoryStorage::with_defaults();
        let h1 = [0x01; TRUNCATED_HASHBYTES];
        let h2 = [0x02; TRUNCATED_HASHBYTES];

        s.set_local_client_known_dest(h1, 1000);
        s.set_local_client_known_dest(h2, 5000);
        assert!(s.has_local_client_known_dest(&h1));
        assert_eq!(s.local_client_known_dest_hashes().len(), 2);

        let removed = s.expire_local_client_known_dests(5000, 3000);
        assert_eq!(removed, 1);
        assert!(!s.has_local_client_known_dest(&h1));
        assert!(s.has_local_client_known_dest(&h2));
    }

    #[test]
    fn test_dest_ratchet_keys_store_load() {
        let mut s = MemoryStorage::with_defaults();
        let hash = [0xaa; TRUNCATED_HASHBYTES];

        assert!(s.load_dest_ratchet_keys(&hash).is_none());

        let data = vec![1, 2, 3, 4, 5];
        s.store_dest_ratchet_keys(hash, data.clone());
        assert_eq!(s.load_dest_ratchet_keys(&hash), Some(data));
    }

    #[test]
    fn test_link_entry_expire() {
        let mut s = MemoryStorage::with_defaults();
        let h1 = [0x01u8; TRUNCATED_HASHBYTES];

        s.set_link_entry(
            h1,
            LinkEntry {
                timestamp_ms: 1000,
                next_hop_interface_index: 0,
                remaining_hops: 1,
                received_interface_index: 1,
                hops: 1,
                validated: true,
                proof_timeout_ms: 0,
                destination_hash: [0u8; TRUNCATED_HASHBYTES],
                peer_signing_key: None,
            },
        );

        // Not expired yet
        let expired = s.expire_link_entries(2000, 5000);
        assert!(expired.is_empty());

        // Expired
        let expired = s.expire_link_entries(10_000, 5000);
        assert_eq!(expired.len(), 1);
    }

    #[test]
    fn test_clean_stale_path_metadata() {
        let mut s = MemoryStorage::with_defaults();
        let h1 = [0x01u8; TRUNCATED_HASHBYTES];
        let h2 = [0x02u8; TRUNCATED_HASHBYTES];

        // h1 has a path, h2 does not
        s.set_path(
            h1,
            PathEntry {
                hops: 0,
                expires_ms: u64::MAX,
                interface_index: 0,
                random_blobs: Vec::new(),
                next_hop: None,
                via_peer: None,
            },
        );

        s.set_path_state(h1, PathState::Responsive);
        s.set_path_state(h2, PathState::Unresponsive); // stale
        s.set_announce_rate(
            h2,
            AnnounceRateEntry {
                last_ms: 0,
                rate_violations: 0,
                blocked_until_ms: 0,
            },
        ); // stale

        s.set_announce_cache(h1, vec![0xAA; 20]);
        s.set_announce_cache(h2, vec![0xBB; 20]);

        s.clean_stale_path_metadata();
        assert!(s.get_path_state(&h1).is_some());
        assert!(s.get_path_state(&h2).is_none());
        assert!(s.get_announce_rate(&h2).is_none());
        // clean_stale_path_metadata does NOT touch announce_cache
        assert!(s.get_announce_cache(&h1).is_some());
        assert!(s.get_announce_cache(&h2).is_some());

        // clean_announce_cache removes entries with no path AND not local
        let h3 = [0x03u8; TRUNCATED_HASHBYTES];
        s.set_announce_cache(h3, vec![0xCC; 20]);
        let mut local = BTreeSet::new();
        local.insert(h2); // h2 is "local" — no path but should survive
        s.clean_announce_cache(&local);
        assert!(s.get_announce_cache(&h1).is_some(), "has path → kept");
        assert!(s.get_announce_cache(&h2).is_some(), "local dest → kept");
        assert!(
            s.get_announce_cache(&h3).is_none(),
            "no path, not local → removed"
        );
    }

    #[test]
    fn test_default_identity_cap() {
        assert_eq!(DEFAULT_IDENTITY_CAP, 50_000);
    }

    // Known-destination cache lifecycle (Codeberg #84).
    #[test]
    fn test_retain_survives_clean_announce_cache() {
        let mut s = MemoryStorage::with_defaults();
        let pinned = [0x01u8; TRUNCATED_HASHBYTES];
        let plain = [0x02u8; TRUNCATED_HASHBYTES];
        s.set_announce_cache(pinned, vec![0xAA; 20]);
        s.set_announce_cache(plain, vec![0xBB; 20]);

        // Retain only takes effect for a known destination.
        assert!(s.retain_known_dest(&pinned), "known dest → retained");
        assert!(
            !s.retain_known_dest(&[0x09u8; TRUNCATED_HASHBYTES]),
            "unknown dest → not retained"
        );
        assert!(s.is_known_dest_retained(&pinned));

        // Neither has a path nor is local: only the pinned one survives.
        let empty = BTreeSet::new();
        s.clean_announce_cache(&empty);
        assert!(
            s.get_announce_cache(&pinned).is_some(),
            "retained → survives cache pressure"
        );
        assert!(
            s.get_announce_cache(&plain).is_none(),
            "non-retained, no path, not local → evicted (negative guard)"
        );
        // Use-state for the evicted entry is reaped; the pin persists.
        assert!(s.is_known_dest_retained(&pinned));

        // Unretain lifts the pin; the entry can then be evicted.
        assert!(s.unretain_known_dest(&pinned, 5_000));
        assert!(!s.is_known_dest_retained(&pinned));
        assert_eq!(s.known_dest_last_used(&pinned), Some(5_000));
        s.clean_announce_cache(&empty);
        assert!(
            s.get_announce_cache(&pinned).is_none(),
            "after unretain → evicted normally"
        );
    }

    #[test]
    fn test_used_touches_recency_and_skips_retained() {
        let mut s = MemoryStorage::with_defaults();
        let dest = [0x03u8; TRUNCATED_HASHBYTES];

        // Unknown destination: used reports false, no recency recorded.
        assert!(!s.used_known_dest(&dest, 1_000));
        assert_eq!(s.known_dest_last_used(&dest), None);

        // Known destination: used is a recency touch (Python >0).
        s.set_announce_cache(dest, vec![0xCC; 20]);
        assert!(s.used_known_dest(&dest, 1_000));
        assert_eq!(s.known_dest_last_used(&dest), Some(1_000));
        assert!(s.used_known_dest(&dest, 2_000));
        assert_eq!(s.known_dest_last_used(&dest), Some(2_000));

        // Once retained, used leaves the pin intact and reports false
        // (Python skips use-state < 0).
        assert!(s.retain_known_dest(&dest));
        assert!(!s.used_known_dest(&dest, 3_000));
        assert!(s.is_known_dest_retained(&dest));
        assert_eq!(
            s.known_dest_last_used(&dest),
            None,
            "retained entry exposes no recency timestamp"
        );
    }

    #[test]
    fn test_retained_survives_even_without_path_or_local() {
        // Explicit negative-vs-positive contrast in a single clean pass.
        let mut s = MemoryStorage::with_defaults();
        let a = [0x0Au8; TRUNCATED_HASHBYTES];
        let b = [0x0Bu8; TRUNCATED_HASHBYTES];
        let c = [0x0Cu8; TRUNCATED_HASHBYTES];
        s.set_announce_cache(a, vec![1; 8]);
        s.set_announce_cache(b, vec![2; 8]);
        s.set_announce_cache(c, vec![3; 8]);

        s.retain_known_dest(&b);
        s.used_known_dest(&c, 100); // touched but not pinned

        s.clean_announce_cache(&BTreeSet::new());
        assert!(s.get_announce_cache(&a).is_none(), "never used → evicted");
        assert!(s.get_announce_cache(&b).is_some(), "retained → kept");
        assert!(
            s.get_announce_cache(&c).is_none(),
            "recency-touched but not pinned → still evicted"
        );
    }

    // Announce table operations
    #[test]
    fn test_announce_table_operations() {
        let mut s = MemoryStorage::with_defaults();
        let h1 = [0x01u8; TRUNCATED_HASHBYTES];
        let h2 = [0x02u8; TRUNCATED_HASHBYTES];

        // Initially empty
        assert!(s.get_announce(&h1).is_none());
        assert!(s.announce_keys().is_empty());

        // Set and get
        s.set_announce(
            h1,
            AnnounceEntry {
                timestamp_ms: 1000,
                hops: 2,
                retries: 0,
                retransmit_at_ms: Some(2000),
                raw_packet: [0xAA; 10].to_vec(),
                receiving_interface_index: 0,
                target_interface: None,
                local_rebroadcasts: 0,
                block_rebroadcasts: false,
            },
        );
        assert_eq!(s.get_announce(&h1).unwrap().hops, 2);

        // Mutable access
        s.get_announce_mut(&h1).unwrap().retries = 3;
        assert_eq!(s.get_announce(&h1).unwrap().retries, 3);

        // Second entry and keys
        s.set_announce(
            h2,
            AnnounceEntry {
                timestamp_ms: 2000,
                hops: 1,
                retries: 0,
                retransmit_at_ms: None,
                raw_packet: [0xBB; 5].to_vec(),
                receiving_interface_index: 1,
                target_interface: None,
                local_rebroadcasts: 0,
                block_rebroadcasts: true,
            },
        );
        let keys = s.announce_keys();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&h1));
        assert!(keys.contains(&h2));

        // Remove
        let removed = s.remove_announce(&h1);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().hops, 2);
        assert!(s.get_announce(&h1).is_none());
        assert_eq!(s.announce_keys().len(), 1);

        // Announce cache
        assert!(s.get_announce_cache(&h2).is_none());
        s.set_announce_cache(h2, [0xCC; 20].to_vec());
        assert_eq!(s.get_announce_cache(&h2).unwrap().len(), 20);

        // Announce rate
        assert!(s.get_announce_rate(&h2).is_none());
        s.set_announce_rate(
            h2,
            AnnounceRateEntry {
                last_ms: 500,
                rate_violations: 2,
                blocked_until_ms: 3000,
            },
        );
        let rate = s.get_announce_rate(&h2).unwrap();
        assert_eq!(rate.rate_violations, 2);
        assert_eq!(rate.blocked_until_ms, 3000);
    }

    // earliest_link_deadline
    #[test]
    fn test_earliest_link_deadline() {
        let mut s = MemoryStorage::with_defaults();

        // Empty → None
        assert_eq!(s.earliest_link_deadline(5000), None);

        // Validated entry: deadline = timestamp_ms + link_timeout_ms
        let h1 = [0x01u8; TRUNCATED_HASHBYTES];
        s.set_link_entry(
            h1,
            LinkEntry {
                timestamp_ms: 1000,
                next_hop_interface_index: 0,
                remaining_hops: 1,
                received_interface_index: 1,
                hops: 1,
                validated: true,
                proof_timeout_ms: 0,
                destination_hash: [0u8; TRUNCATED_HASHBYTES],
                peer_signing_key: None,
            },
        );
        assert_eq!(s.earliest_link_deadline(5000), Some(6000));

        // Unvalidated entry with earlier deadline: uses proof_timeout_ms directly
        let h2 = [0x02u8; TRUNCATED_HASHBYTES];
        s.set_link_entry(
            h2,
            LinkEntry {
                timestamp_ms: 2000,
                next_hop_interface_index: 0,
                remaining_hops: 1,
                received_interface_index: 1,
                hops: 1,
                validated: false,
                proof_timeout_ms: 3000,
                destination_hash: [0u8; TRUNCATED_HASHBYTES],
                peer_signing_key: None,
            },
        );
        // min(6000, 3000) = 3000
        assert_eq!(s.earliest_link_deadline(5000), Some(3000));
    }

    // Unvalidated link entry expiry
    #[test]
    fn test_link_entry_expire_unvalidated() {
        let mut s = MemoryStorage::with_defaults();
        let h1 = [0x01u8; TRUNCATED_HASHBYTES];

        s.set_link_entry(
            h1,
            LinkEntry {
                timestamp_ms: 1000,
                next_hop_interface_index: 0,
                remaining_hops: 1,
                received_interface_index: 1,
                hops: 1,
                validated: false,
                proof_timeout_ms: 5000,
                destination_hash: [0u8; TRUNCATED_HASHBYTES],
                peer_signing_key: None,
            },
        );

        // now_ms=4000 < proof_timeout_ms=5000 → not expired
        let expired = s.expire_link_entries(4000, 999_999);
        assert!(expired.is_empty());

        // now_ms=6000 > proof_timeout_ms=5000 → expired
        let expired = s.expire_link_entries(6000, 999_999);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, h1);
    }

    // remove_link_entries_for_interface
    #[test]
    fn test_remove_link_entries_for_interface() {
        let mut s = MemoryStorage::with_defaults();
        let h1 = [0x01u8; TRUNCATED_HASHBYTES];
        let h2 = [0x02u8; TRUNCATED_HASHBYTES];
        let h3 = [0x03u8; TRUNCATED_HASHBYTES];

        let make_entry = |recv: usize, next: usize| LinkEntry {
            timestamp_ms: 1000,
            next_hop_interface_index: next,
            remaining_hops: 1,
            received_interface_index: recv,
            hops: 1,
            validated: true,
            proof_timeout_ms: 0,
            destination_hash: [0u8; TRUNCATED_HASHBYTES],
            peer_signing_key: None,
        };

        // h1: received on iface 0, forwarded on iface 1
        s.set_link_entry(h1, make_entry(0, 1));
        // h2: received on iface 1, forwarded on iface 2
        s.set_link_entry(h2, make_entry(1, 2));
        // h3: received on iface 2, forwarded on iface 3
        s.set_link_entry(h3, make_entry(2, 3));

        // Remove all entries involving iface 1 (h1 forwards on 1, h2 received on 1)
        let removed = s.remove_link_entries_for_interface(1);
        assert_eq!(removed.len(), 2);
        assert!(s.get_link_entry(&h1).is_none());
        assert!(s.get_link_entry(&h2).is_none());
        assert!(s.get_link_entry(&h3).is_some());
    }

    // remove_paths_for_interface
    #[test]
    fn test_remove_paths_for_interface() {
        let mut s = MemoryStorage::with_defaults();
        let h1 = [0x01u8; TRUNCATED_HASHBYTES];
        let h2 = [0x02u8; TRUNCATED_HASHBYTES];
        let h3 = [0x03u8; TRUNCATED_HASHBYTES];

        let make_path = |iface: usize| PathEntry {
            hops: 1,
            expires_ms: u64::MAX,
            interface_index: iface,
            random_blobs: Vec::new(),
            next_hop: None,
            via_peer: None,
        };

        s.set_path(h1, make_path(0));
        s.set_path(h2, make_path(1));
        s.set_path(h3, make_path(1));

        let removed = s.remove_paths_for_interface(1);
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&h2));
        assert!(removed.contains(&h3));
        assert!(s.get_path(&h1).is_some());
        assert!(s.get_path(&h2).is_none());
        assert!(s.get_path(&h3).is_none());
    }

    // path_request_time get/set
    #[test]
    fn test_path_request_time() {
        let mut s = MemoryStorage::with_defaults();
        let h1 = [0x01u8; TRUNCATED_HASHBYTES];

        // Initially none
        assert_eq!(s.get_path_request_time(&h1), None);

        // Set and get
        s.set_path_request_time(h1, 42000);
        assert_eq!(s.get_path_request_time(&h1), Some(42000));

        // Overwrite
        s.set_path_request_time(h1, 99000);
        assert_eq!(s.get_path_request_time(&h1), Some(99000));
    }

    #[test]
    fn test_diagnostic_dump_empty() {
        let s = MemoryStorage::with_defaults();
        let (dump, total) = s.diagnostic_dump();
        assert!(dump.contains("packet_cache: 0 entries"));
        assert!(dump.contains("known_identities: 0 entries"));
        assert!(dump.contains("known_ratchets: 0 entries"));
        assert_eq!(total, 0);
    }

    /// The identity row is priced from the type, not from a number
    /// somebody typed.
    ///
    /// The literal this replaced said 128 where `Identity` is four times
    /// that, and the error was invisible until the table filled: at the
    /// 50 000-entry cap the dump was ~58 MB short on its own largest
    /// row, and that shortfall was being read as memory the allocator
    /// had lost. A wrong estimate is worse than no estimate, because it
    /// is believed.
    #[test]
    fn identity_rows_are_priced_from_size_of() {
        use rand_core::OsRng;

        let mut s = MemoryStorage::with_defaults();
        s.set_identity([0x11; TRUNCATED_HASHBYTES], Identity::generate(&mut OsRng));

        let expected = HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + core::mem::size_of::<Identity>();
        let (dump, _) = s.diagnostic_dump();
        assert!(
            dump.contains(&alloc::format!(
                "known_identities: 1 entries, raw {expected} bytes"
            )),
            "identity row not priced at size_of::<Identity>() = {}: {dump}",
            core::mem::size_of::<Identity>(),
        );
    }

    #[test]
    fn test_diagnostic_dump_with_data() {
        let mut s = MemoryStorage::with_defaults();
        // Add some packet hashes
        s.add_packet_hash([0x01; 32]);
        s.add_packet_hash([0x02; 32]);
        // Add a path
        s.set_path(
            [0xAA; TRUNCATED_HASHBYTES],
            PathEntry {
                hops: 1,
                expires_ms: u64::MAX,
                interface_index: 0,
                random_blobs: Vec::new(),
                next_hop: None,
                via_peer: None,
            },
        );
        let (dump, total) = s.diagnostic_dump();
        assert!(dump.contains("packet_cache: 2 entries"));
        assert!(dump.contains("path_table: 1 entries"));
        assert!(total > 0);
    }
}

/// Item 5 of the 2026-09-21 hygiene batch: the diagnostic dump's model
/// of what an entry costs, pinned to the types it models.
///
/// The dump is read to attribute a resident-set gap, which means a row
/// that under-reports is worse than no row at all: it is believed, and
/// the gap it leaves gets attributed to something else. It had already
/// happened once — `Identity` was modelled at 128 bytes against a type
/// four times that, and on a node sitting at the 50 000-entry identity
/// cap the dump was short by tens of megabytes on its single largest
/// table.
///
/// Rust has no reflection, so the only way a row can track its type is
/// to be written in terms of `size_of`. These tests assert exactly
/// that, row by row: a literal byte count reintroduced here fails,
/// and a field added to any of the modelled structs moves the row with
/// it instead of silently widening the gap.
#[cfg(test)]
mod diagnostic_model_tests {
    use super::*;
    use crate::destination::DestinationHash;
    use alloc::vec;
    use core::mem::size_of;

    /// The `raw N bytes` figure the dump prints for one named row.
    fn raw_bytes_of(dump: &str, row: &str) -> u64 {
        let line = dump
            .lines()
            .find(|l| l.starts_with(&alloc::format!("{row}:")))
            .unwrap_or_else(|| panic!("the dump has no {row} row:\n{dump}"));
        let after = line
            .split("raw ")
            .nth(1)
            .unwrap_or_else(|| panic!("no `raw ` in {line}"));
        after
            .split(' ')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no number after `raw ` in {line}"))
    }

    fn hash(n: u8) -> [u8; TRUNCATED_HASHBYTES] {
        let mut h = [0u8; TRUNCATED_HASHBYTES];
        h[0] = n;
        h
    }

    /// Every fixed-size row is its key plus `size_of` of its value.
    #[test]
    fn each_row_prices_its_entry_by_size_of_the_type_it_stores() {
        let mut s = MemoryStorage::with_defaults();

        s.set_path(
            hash(1),
            PathEntry {
                hops: 1,
                expires_ms: 0,
                interface_index: 0,
                random_blobs: Vec::new(),
                next_hop: None,
                via_peer: None,
            },
        );
        s.set_path_state(hash(1), PathState::Unknown);
        s.set_reverse(
            hash(2),
            ReverseEntry {
                timestamp_ms: 0,
                receiving_interface_index: 0,
                outbound_interface_index: 0,
            },
        );
        s.set_link_entry(
            hash(3),
            LinkEntry {
                timestamp_ms: 0,
                next_hop_interface_index: 0,
                remaining_hops: 1,
                received_interface_index: 0,
                hops: 1,
                validated: false,
                proof_timeout_ms: 0,
                destination_hash: hash(3),
                peer_signing_key: None,
            },
        );
        s.set_announce_rate(
            hash(4),
            AnnounceRateEntry {
                last_ms: 0,
                rate_violations: 0,
                blocked_until_ms: 0,
            },
        );
        s.set_receipt(
            hash(5),
            PacketReceipt::new([0u8; 32], DestinationHash::new(hash(5)), 0),
        );
        s.set_path_request_time(hash(6), 0);
        s.remember_known_ratchet(hash(7), [0u8; RATCHET_SIZE], 0);
        s.set_local_client_known_dest(hash(8), 0);
        s.set_discovery_path_request(hash(9), 0, 0);

        let (dump, _) = s.diagnostic_dump_non_packet_cache();

        // An empty blob window still costs the Vec header inside PathEntry,
        // which is what size_of carries and a literal never did.
        assert_eq!(
            raw_bytes_of(&dump, "path_table"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<PathEntry>()) as u64,
        );
        assert_eq!(
            raw_bytes_of(&dump, "path_states"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<PathState>()) as u64,
        );
        assert_eq!(
            raw_bytes_of(&dump, "reverse_table"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<ReverseEntry>()) as u64,
        );
        // Including the signing key, which is stored inline whether or
        // not it is present — the old model added it conditionally and
        // so priced a link without a proof below what it occupies.
        assert_eq!(
            raw_bytes_of(&dump, "link_table"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<LinkEntry>()) as u64,
        );
        assert_eq!(
            raw_bytes_of(&dump, "announce_rate_table"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<AnnounceRateEntry>()) as u64,
        );
        assert_eq!(
            raw_bytes_of(&dump, "receipts"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<PacketReceipt>()) as u64,
        );
        assert_eq!(
            raw_bytes_of(&dump, "path_requests"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<u64>()) as u64,
        );
        assert_eq!(
            raw_bytes_of(&dump, "known_ratchets"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<([u8; RATCHET_SIZE], u64)>())
                as u64,
        );
        assert_eq!(
            raw_bytes_of(&dump, "local_client_known_dests"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<u64>()) as u64,
        );
        assert_eq!(
            raw_bytes_of(&dump, "discovery_path_requests"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<(usize, u64)>()) as u64,
        );
    }

    /// Heap-carrying rows add the allocation the entry actually holds,
    /// on top of the header `size_of` accounts for — and they measure
    /// `capacity`, because that is what was asked of the allocator.
    #[test]
    fn heap_carrying_rows_add_capacity_on_top_of_the_header() {
        let mut s = MemoryStorage::with_defaults();

        let mut blobs: Vec<[u8; crate::constants::RANDOM_HASHBYTES]> = Vec::with_capacity(8);
        blobs.push([0u8; crate::constants::RANDOM_HASHBYTES]);
        let blob_capacity = blobs.capacity();
        s.set_path(
            hash(1),
            PathEntry {
                hops: 1,
                expires_ms: 0,
                interface_index: 0,
                random_blobs: blobs,
                next_hop: None,
                via_peer: None,
            },
        );

        let mut raw_packet: Vec<u8> = Vec::with_capacity(200);
        raw_packet.extend_from_slice(&[7u8; 100]);
        let packet_capacity = raw_packet.capacity();
        s.set_announce(
            hash(2),
            AnnounceEntry {
                timestamp_ms: 0,
                hops: 1,
                retries: 0,
                retransmit_at_ms: None,
                raw_packet,
                receiving_interface_index: 0,
                target_interface: None,
                local_rebroadcasts: 0,
                block_rebroadcasts: false,
            },
        );

        s.set_announce_cache(hash(3), vec![9u8; 64]);
        s.store_dest_ratchet_keys(hash(4), vec![1u8; 32]);

        let (dump, _) = s.diagnostic_dump_non_packet_cache();

        assert_eq!(
            raw_bytes_of(&dump, "path_table"),
            (HASH_ORDER_BYTES
                + TRUNCATED_HASHBYTES
                + size_of::<PathEntry>()
                + blob_capacity * crate::constants::RANDOM_HASHBYTES) as u64,
            "the blob window costs what it reserved, not what it holds"
        );
        assert_eq!(
            raw_bytes_of(&dump, "announce_table"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<AnnounceEntry>() + packet_capacity)
                as u64,
            "the second copy of the announce is the point of this row"
        );
        assert_eq!(
            raw_bytes_of(&dump, "announce_cache"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<Vec<u8>>() + 64) as u64,
        );
        assert_eq!(
            raw_bytes_of(&dump, "dest_ratchet_keys"),
            (HASH_ORDER_BYTES + TRUNCATED_HASHBYTES + size_of::<Vec<u8>>() + 32) as u64,
        );
    }
}

/// Codeberg #421: every table has a ceiling, and reaching it evicts rather
/// than refuses.
///
/// The acceptance bar from the issue: under sustained insert pressure a table
/// stops growing, keeps accepting, keeps the newest entry and has dropped the
/// oldest. The three tables whose eviction order is not plain FIFO get a test
/// of their own for the order they do use.
#[cfg(test)]
mod cap_tests {
    use super::*;
    use crate::destination::DestinationHash;

    /// Small enough that filling past it is cheap, big enough that the
    /// 64-entry eviction scan is not the whole table.
    const TINY: usize = 100;

    fn tiny_caps() -> TableCaps {
        TableCaps {
            packet_hash_cap: TINY,
            identity_cap: TINY,
            path_cap: TINY,
            reverse_cap: TINY,
            link_cap: TINY,
            announce_cap: TINY,
            destination_cap: TINY,
            local_dest_cap: TINY,
            receipt_cap: TINY,
        }
    }

    fn hash16(n: u32) -> [u8; TRUNCATED_HASHBYTES] {
        let mut h = [0u8; TRUNCATED_HASHBYTES];
        h[..4].copy_from_slice(&n.to_be_bytes());
        h
    }

    fn reverse(ts: u64) -> ReverseEntry {
        ReverseEntry {
            timestamp_ms: ts,
            receiving_interface_index: 0,
            outbound_interface_index: 1,
        }
    }

    fn path(expires_ms: u64) -> PathEntry {
        PathEntry {
            hops: 1,
            expires_ms,
            interface_index: 0,
            random_blobs: Vec::new(),
            next_hop: None,
            via_peer: None,
        }
    }

    fn link(validated: bool) -> LinkEntry {
        LinkEntry {
            timestamp_ms: 0,
            next_hop_interface_index: 0,
            remaining_hops: 1,
            received_interface_index: 0,
            hops: 1,
            validated,
            proof_timeout_ms: 0,
            destination_hash: hash16(0),
            peer_signing_key: None,
        }
    }

    fn declared_cap(s: &MemoryStorage, name: &str) -> Option<usize> {
        s.collection_counts()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no census row named {name}"))
            .capacity
    }

    /// Not one collection may answer "no ceiling". A `None` here is the
    /// bounded-only-by-the-neighbours'-traffic shape #421 was opened for.
    #[test]
    fn every_collection_declares_a_ceiling() {
        let s = MemoryStorage::with_defaults();
        let uncapped: Vec<&str> = s
            .collection_counts()
            .into_iter()
            .filter(|c| c.capacity.is_none())
            .map(|c| c.name)
            .collect();
        assert!(
            uncapped.is_empty(),
            "collections with no ceiling: {uncapped:?}"
        );
    }

    /// The reverse table under sustained forwarding pressure: it stops
    /// growing, and an insert past the ceiling still lands. This is the
    /// table #421 measured at 73 901 entries in one expiry window.
    #[test]
    fn reverse_table_stops_growing_and_keeps_accepting() {
        let mut s = MemoryStorage::with_caps(tiny_caps());
        let pressure = 10_000u32;
        for i in 0..pressure {
            s.set_reverse(hash16(i), reverse(i as u64));
        }
        let cap = declared_cap(&s, "reverse_table").unwrap_or_else(|| {
            panic!(
                "reverse_table declares no ceiling; it grew to {} entries under {pressure} inserts",
                s.reverse_entries().len()
            )
        });
        assert_eq!(cap, TINY);
        assert_eq!(s.reverse_entries().len(), cap, "size holds at the ceiling");
        assert!(
            s.get_reverse(&hash16(pressure - 1)).is_some(),
            "the newest entry is present"
        );
        assert!(s.get_reverse(&hash16(0)).is_none(), "the oldest is gone");

        s.set_reverse(hash16(pressure), reverse(pressure as u64));
        assert!(
            s.get_reverse(&hash16(pressure)).is_some(),
            "an insert after the ceiling still succeeds"
        );
        assert_eq!(
            s.reverse_entries().len(),
            cap,
            "and does not grow the table"
        );
    }

    /// The path table's seven-day expiry never fires on a young node, so the
    /// ceiling is the only bound; a re-announced destination has to survive
    /// it, which is what refresh-on-re-insert buys.
    #[test]
    fn path_table_holds_at_its_cap_and_keeps_the_re_announced_path() {
        let mut s = MemoryStorage::with_caps(tiny_caps());
        let kept = hash16(0);
        s.set_path(kept, path(1));
        for i in 1..TINY as u32 {
            s.set_path(hash16(i), path(1));
        }
        // Re-announce the oldest destination: it goes to the back of the
        // queue, so the next ceiling-worth of inserts evicts the paths that
        // were never re-announced and leaves it alone.
        s.set_path(kept, path(2));
        let newest = TINY as u32 * 2 - 2;
        for i in TINY as u32..=newest {
            s.set_path(hash16(i), path(1));
        }

        assert_eq!(s.path_count(), TINY, "the table stopped growing");
        assert!(
            s.get_path(&kept).is_some(),
            "the re-announced path survived entries inserted after it"
        );
        assert!(
            s.get_path(&hash16(1)).is_none(),
            "a path never re-announced was evicted"
        );
        assert!(
            s.get_path(&hash16(newest)).is_some(),
            "the newest path is present"
        );
    }

    /// Not plain FIFO: a half-open link request goes before a live link.
    #[test]
    fn link_table_prefers_unvalidated_entries_on_overflow() {
        let mut s = MemoryStorage::with_caps(tiny_caps());
        // Oldest entry is a live link; everything after it is unvalidated.
        let live = hash16(0);
        s.set_link_entry(live, link(true));
        for i in 1..TINY as u32 {
            s.set_link_entry(hash16(i), link(false));
        }
        assert_eq!(s.link_entry_count(), TINY);

        s.set_link_entry(hash16(TINY as u32), link(false));
        assert_eq!(s.link_entry_count(), TINY, "the ceiling holds");
        assert!(
            s.get_link_entry(&live).is_some(),
            "the live link survived although it was the oldest entry"
        );
        assert!(
            s.get_link_entry(&hash16(1)).is_none(),
            "the oldest unvalidated entry went instead"
        );
    }

    /// When every candidate is a live link there is nothing better to drop,
    /// and the insert must still succeed — a preference is not a refusal.
    #[test]
    fn link_table_still_accepts_when_every_entry_is_validated() {
        let mut s = MemoryStorage::with_caps(tiny_caps());
        for i in 0..TINY as u32 {
            s.set_link_entry(hash16(i), link(true));
        }
        s.set_link_entry(hash16(TINY as u32), link(true));
        assert_eq!(s.link_entry_count(), TINY);
        assert!(s.get_link_entry(&hash16(TINY as u32)).is_some());
        assert!(
            s.get_link_entry(&hash16(0)).is_none(),
            "it fell back to the plain oldest"
        );
    }

    /// Not plain FIFO: a destination an application pinned survives an
    /// overflow, the way it already survives `clean_announce_cache`.
    #[test]
    fn announce_cache_evicts_unretained_entries_first() {
        let mut s = MemoryStorage::with_caps(tiny_caps());
        let pinned = hash16(0);
        s.set_announce_cache(pinned, vec![1u8; 8]);
        assert!(s.retain_known_dest(&pinned), "pin the oldest entry");
        for i in 1..TINY as u32 {
            s.set_announce_cache(hash16(i), vec![1u8; 8]);
        }

        s.set_announce_cache(hash16(TINY as u32), vec![1u8; 8]);
        assert_eq!(s.announce_cache_keys().len(), TINY);
        assert!(
            s.get_announce_cache(&pinned).is_some(),
            "the retained destination survived although it was the oldest"
        );
        assert!(
            s.is_known_dest_retained(&pinned),
            "and its pin survived with it"
        );
        assert!(
            s.get_announce_cache(&hash16(1)).is_none(),
            "the oldest unretained entry went instead"
        );
    }

    /// Not plain FIFO: a terminal receipt is only serving out its retention
    /// grace, so it goes first; when only pending receipts are left, the one
    /// that is dropped is still reported as a timeout.
    #[test]
    fn receipts_evict_terminal_first_and_cull_pending_with_a_timeout() {
        let mut s = MemoryStorage::with_caps(tiny_caps());

        let pending_oldest = hash16(0);
        s.set_receipt(
            pending_oldest,
            PacketReceipt::new([0u8; 32], DestinationHash::new(pending_oldest), 0),
        );
        // One terminal receipt behind it, then fill to the ceiling.
        let terminal = hash16(1);
        let mut delivered = PacketReceipt::new([1u8; 32], DestinationHash::new(terminal), 0);
        delivered.status = ReceiptStatus::Delivered;
        s.set_receipt(terminal, delivered);
        for i in 2..TINY as u32 {
            s.set_receipt(
                hash16(i),
                PacketReceipt::new([2u8; 32], DestinationHash::new(hash16(i)), 0),
            );
        }

        let overflow = hash16(TINY as u32);
        s.set_receipt(
            overflow,
            PacketReceipt::new([3u8; 32], DestinationHash::new(overflow), 0),
        );
        assert!(
            s.get_receipt(&terminal).is_none(),
            "the terminal receipt went first"
        );
        assert!(
            s.get_receipt(&pending_oldest).is_some(),
            "the older pending receipt was kept"
        );
        assert!(
            s.expire_receipts(0).is_empty(),
            "a terminal receipt owes nobody a timeout"
        );

        // Now every entry is pending: the next overflow has to cull one, and
        // it must come back as a timeout (Python Transport.py:558-561).
        let next = hash16(TINY as u32 + 1);
        s.set_receipt(
            next,
            PacketReceipt::new([4u8; 32], DestinationHash::new(next), 0),
        );
        let timed_out = s.expire_receipts(0);
        assert_eq!(timed_out.len(), 1, "the culled pending receipt is reported");
        assert_eq!(timed_out[0].truncated_hash, pending_oldest);
        assert!(
            s.get_receipt(&next).is_some(),
            "the new receipt still landed"
        );
    }

    /// A locally attached client that registers destinations in a loop must
    /// not be able to grow the daemon without bound.
    #[test]
    fn local_client_destinations_are_bounded_across_interfaces() {
        let mut s = MemoryStorage::with_caps(tiny_caps());
        for i in 0..(TINY as u32 * 3) {
            s.add_local_client_dest(1, hash16(i));
        }
        assert_eq!(declared_cap(&s, "local_client_dest_map"), Some(TINY));
        assert!(s.has_local_client_dest(1, &hash16(TINY as u32 * 3 - 1)));
        assert!(!s.has_local_client_dest(1, &hash16(0)));

        // A second interface shares the ceiling, and dropping one interface
        // must not touch the other's entries.
        s.add_local_client_dest(2, hash16(9_999));
        assert!(s.has_local_client_dest(2, &hash16(9_999)));
        s.remove_local_client_dests(1);
        assert!(s.has_local_client_dest(2, &hash16(9_999)));
        assert!(!s.has_local_client_dest(1, &hash16(TINY as u32 * 3 - 1)));
    }

    /// The compact profile is the one that has to fit a Raspberry Pi Zero
    /// 2W, so it may never be looser than the desktop one.
    #[test]
    fn the_compact_profile_is_never_larger_than_the_desktop_one() {
        let d = TableCaps::desktop();
        let c = TableCaps::compact();
        assert!(c.packet_hash_cap <= d.packet_hash_cap);
        assert!(c.identity_cap <= d.identity_cap);
        assert!(c.path_cap <= d.path_cap);
        assert!(c.reverse_cap <= d.reverse_cap);
        assert!(c.link_cap <= d.link_cap);
        assert!(c.announce_cap <= d.announce_cap);
        assert!(c.destination_cap <= d.destination_cap);
        assert!(c.local_dest_cap <= d.local_dest_cap);
        assert_eq!(
            c.receipt_cap, d.receipt_cap,
            "receipts take the reference's number on both profiles"
        );
    }

    /// The reverse-table default has to clear the busiest window #421
    /// measured, or the cap would drop replies in ordinary operation.
    #[test]
    fn the_reverse_default_clears_the_measured_field_load() {
        assert!(
            TableCaps::desktop().reverse_cap >= 73_901 * 2,
            "the desktop reverse cap must leave headroom over the 73 901 \
             entries measured in one 8 minute window"
        );
    }
}
