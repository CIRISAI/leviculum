//! Node-to-node peering for the propagation role (Codeberg #384, part 2).
//!
//! This module is the `no_std + alloc` protocol half of peering: the peer
//! table, the `/offer` wire codec, the inbound gate, and the pure decisions
//! of an outbound sync round (what to offer, how to move the cursor, how to
//! react to the peer's answer). It performs no I/O, owns no links and no
//! clocks; the host engine (lnpnd today, the board's adapter in part 3)
//! feeds it announces, requests and transfer conclusions and dispatches
//! what it returns. The design it implements is
//! `docs/src/concepts/propagation-node-on-a-board.md` §5; the reference is
//! `reference/LXMF` at pin 795fdaa.
//!
//! # What replaces the reference's per-peer sets
//!
//! The reference tracks distribution per message as two lists of peer
//! hashes (`propagation_entries[4]` and `[5]`,
//! `reference/LXMF/LXMF/LXMRouter.py:2518`), filled by
//! `flush_peer_distribution_queue` (`:2472`) — 16 bytes per peer per
//! message, which §2 of the concept paper priced out of the board's RAM.
//! Here each peer instead holds one **cursor** into the store's append
//! order: everything at or below it has been offered and concluded (or was
//! deliberately skipped, see [`build_offer`]); everything above it is
//! offered next round. A cursor that no longer matches any live record —
//! after a board page reclaim or a host store reset — simply orders below
//! everything live, and the next offer is a bounded full re-offer the peer
//! answers with "want none" for what it already holds.
//!
//! What a Python peer observes from the cursor design: offers that may
//! include ids it already holds, including messages it itself sent us (a
//! cursor cannot encode the reference's `from_peer` exclusion,
//! `flush_peer_distribution_queue`, `LXMRouter.py:2484`). That is
//! wire-legal and self-limiting: `offer_request` answers out of its own
//! store membership (`LXMRouter.py:2318`) and declines them; the cost is
//! offer-list bytes, never message bodies.

use alloc::{collections::BTreeMap, vec::Vec};

use leviculum_core::identity::Identity;
use leviculum_core::traits::Storage;

use crate::{
    constants::DESTINATION_LENGTH,
    msgpack,
    propagation::{PeerError, PropagationError, PropagationNodeAnnounce, TransientId},
    propagation_store::StoredMessage,
    storage::StorageError,
};

/// Request path a peer offers batches on
/// (`OFFER_REQUEST_PATH`, `reference/LXMF/LXMF/LXMPeer.py:14`).
pub const OFFER_REQUEST_PATH: &str = "/offer";

/// Reference peer-table cap and our host default
/// (`MAX_PEERS`, `reference/LXMF/LXMF/LXMRouter.py:43`). The board build
/// configures 16 instead: §5 budgets `80·N + 6144 ≤ 8192` bytes of peering
/// RAM against the worst observed free heap, N ≤ 25, with margin.
pub const MAX_PEERS_DEFAULT: usize = 20;

/// Autopeer on propagation announces by default
/// (`AUTOPEER`, `reference/LXMF/LXMF/LXMRouter.py:44`).
pub const AUTOPEER_DEFAULT: bool = true;

/// Hop depth inside which an announce creates a peer
/// (`AUTOPEER_MAXDEPTH`, `reference/LXMF/LXMF/LXMRouter.py:45`).
pub const AUTOPEER_MAXDEPTH_DEFAULT: u8 = 4;

/// Highest remote peering cost we will mine a key for
/// (`MAX_PEERING_COST`, `reference/LXMF/LXMF/LXMRouter.py:51`; enforced in
/// `peer`, `:2005-2010`).
pub const REMOTE_PEERING_COST_MAX_DEFAULT: u8 = 26;

/// Concurrent inbound sync resources before `/offer` answers throttled
/// (`MAX_INBOUND_SYNCS`, `reference/LXMF/LXMF/LXMRouter.py:58`; applied at
/// `:2280-2284`).
pub const MAX_INBOUND_SYNCS_DEFAULT: usize = 3;

/// How long a peer that sent invalid stamps is throttled, and how long a
/// throttled peer defers its next sync
/// (`PN_STAMP_THROTTLE`, `reference/LXMF/LXMF/LXMRouter.py:63`; applied at
/// `:2445-2450` on the node side and `LXMPeer.py:421-424` on the peer
/// side).
pub const PN_STAMP_THROTTLE_SECS: u64 = 180;

/// A peer unreachable this long is dropped from the table
/// (`MAX_UNREACHABLE`, `reference/LXMF/LXMF/LXMPeer.py:39`; culled in
/// `sync_peers`, `LXMRouter.py:2136-2140`; static peers exempt).
pub const MAX_UNREACHABLE_SECS: u64 = 14 * 24 * 60 * 60;

/// Backoff added per link-establishment attempt, cleared when the link
/// comes up (`SYNC_BACKOFF_STEP`, `reference/LXMF/LXMF/LXMPeer.py:45`;
/// added at `:321-322`, cleared at `:330` and `:541`).
pub const SYNC_BACKOFF_STEP_SECS: u64 = 12 * 60;

/// Cadence of the sync scheduler: the reference runs `sync_peers` every
/// `JOB_PEERSYNC_INTERVAL × PROCESSING_INTERVAL` = 6 × 4 s
/// (`reference/LXMF/LXMF/LXMRouter.py:877`, `:31`, applied at `:908-910`).
pub const SYNC_INTERVAL_SECS: u64 = 24;

/// RAM ceiling on one encoded `/offer` request body, §5's 6 144 B budget:
/// the bound that keeps the board's per-sync RAM inside its slice. Each id
/// encodes to 34 B (bin8 header + 32), so this admits ~180 ids per round;
/// what does not fit is offered next round, the cursor never jumps past
/// it.
///
/// This is a ceiling, never the budget on its own: an offer also has to
/// fit the link it is handed to, which on LoRa is an order of magnitude
/// smaller. Callers size a round with [`offer_budget_for_mdu`] and pass
/// the result to [`build_offer`].
pub const OFFER_BYTES_LIMIT: usize = 6144;

/// What `NodeCore::send_request` wraps a request body in before it meets
/// the link MDU: `fixarray(3)` 1 B, the float64 timestamp 9 B, and the
/// bin8 path hash 18 B (`leviculum-core/src/node/mod.rs`, "Build msgpack:
/// fixarray(3) + float64(timestamp) + bin(path_hash) + data_or_nil"). The
/// check that refuses an oversized request is `packed.len() > link.mdu()`
/// right below it, and that refusal is what silently stopped every sync
/// round whose store held more than about ten offerable records.
pub const REQUEST_ENVELOPE_BYTES: usize = 28;

/// The encoded `/offer` body before the first id: `fixarray(2)` 1 B plus
/// the bin8 peering key 34 B ([`PeerOffer::encode`]).
pub const OFFER_KEY_BYTES: usize = 35;

/// One offered id on the wire: bin8 header 2 B plus the 32-byte id.
pub const OFFER_ID_BYTES: usize = 34;

/// The smallest budget that can still carry one id. A caller that only
/// needs to know *whether* anything is offerable — before a link exists,
/// so before an MDU is known — plans with this and re-plans against the
/// link once it is up.
pub const OFFER_PROBE_BUDGET: usize = OFFER_KEY_BYTES + 1 + OFFER_ID_BYTES;

/// Bytes [`PeerOffer::encode`] produces for `id_count` ids: the key, the
/// msgpack array header for the id list (1 B up to 15 entries, 3 B up to
/// 65 535 — `rmp`'s `write_array_len`), and the ids.
pub fn offer_encoded_len(id_count: usize) -> usize {
    let header = if id_count <= 15 {
        1
    } else if id_count <= 65_535 {
        3
    } else {
        5
    };
    OFFER_KEY_BYTES + header + id_count * OFFER_ID_BYTES
}

/// The `/offer` body budget one request on a link with this MDU can
/// carry: what the link takes, less the request envelope around the body,
/// and never above the §5 RAM ceiling.
///
/// The peer's answer is bounded by the same number without needing its
/// own: the wanted list is a subset of the ids we named, it carries no
/// peering key, and the response envelope (`fixarray(2)` + bin8 request
/// id, 19 B) is smaller than the request's. An offer that fits the link
/// therefore cannot provoke an answer that does not.
pub fn offer_budget_for_mdu(link_mdu: usize) -> usize {
    link_mdu
        .saturating_sub(REQUEST_ENVELOPE_BYTES)
        .min(OFFER_BYTES_LIMIT)
}

/// Per-message overhead and initial size the reference budgets when
/// packing an offer against the peer's limits
/// (`reference/LXMF/LXMF/LXMPeer.py:359-360`).
pub const OFFER_PER_MESSAGE_OVERHEAD: u64 = 16;
pub const OFFER_BASE_SIZE: u64 = 24;

/// What one message ACTUALLY costs in the shipped sync resource, at most:
/// the wire form is `msgpack([timestamp, [lxmf_bytes, ...]])`
/// (`reference/LXMF/LXMF/LXMPeer.py:466`), so each message adds one
/// msgpack bin header — 2 B (bin8) below 256 bytes, 3 B (bin16) up to
/// 64 KiB, and no message can reach bin32 because the per-message
/// transfer limit drops it first (`LXMPeer.py:370`).
pub const SYNC_FRAMING_PER_MESSAGE_MAX: u64 = 3;

/// What the batch structure ACTUALLY costs on the wire, at most: the
/// outer 2-element fixarray (1 B), the float64 timestamp (9 B), and the
/// message list's array header (≤ 3 B up to 65 535 entries).
pub const SYNC_FRAMING_BASE_MAX: u64 = 13;

// The receivable-sync-limit proof (#388 pass 3, item 3): the packing
// loop counts `OFFER_BASE_SIZE` up front and `OFFER_PER_MESSAGE_OVERHEAD`
// per message and stops strictly below the announced limit
// (`next_size >= limit` skips, `LXMPeer.py:376`). Because each accounted
// overhead dominates its actual wire framing (asserted here), the shipped
// resource is strictly smaller than the accounted sum, hence strictly
// below the announced limit — a receiver whose resource cap is at least
// the announced limit can always take the batch. Pinned empirically by
// `sync_batch_wire_size_never_exceeds_announced_limit`.
const _: () = {
    assert!(SYNC_FRAMING_PER_MESSAGE_MAX <= OFFER_PER_MESSAGE_OVERHEAD);
    assert!(SYNC_FRAMING_BASE_MAX <= OFFER_BASE_SIZE);
};

/// Peering configuration, reference key names
/// (`reference/LXMF/LXMF/Utilities/lxmd.py:128-233`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeeringConfig {
    /// `max_peers` — table cap (`LXMRouter.py:43`; board 16 per §5).
    pub max_peers: usize,
    /// `autopeer` (`LXMRouter.py:44`).
    pub autopeer: bool,
    /// `autopeer_maxdepth` (`LXMRouter.py:45`).
    pub autopeer_maxdepth: u8,
    /// `peering_cost` — what WE require of an inbound `/offer` key.
    /// Default 0 (Lead decision); the reference clamps only its
    /// propagation cost, never the peering cost (`LXMRouter.py:137`).
    pub peering_cost: u8,
    /// `remote_peering_cost_max` — above this we refuse to peer
    /// (`LXMRouter.py:2005-2010`).
    pub remote_peering_cost_max: u8,
    /// `max_inbound_syncs` (`LXMRouter.py:58`).
    pub max_inbound_syncs: usize,
    /// `from_static_only` — only static peers may `/offer`
    /// (`LXMRouter.py:2292-2295`).
    pub from_static_only: bool,
    /// `static_peers` — always peered, never culled, never declined by
    /// the cap (`Handlers.py:68-78`; cull exemption `LXMRouter.py:2140`).
    pub static_peers: Vec<[u8; DESTINATION_LENGTH]>,
}

impl Default for PeeringConfig {
    fn default() -> Self {
        Self {
            max_peers: MAX_PEERS_DEFAULT,
            autopeer: AUTOPEER_DEFAULT,
            autopeer_maxdepth: AUTOPEER_MAXDEPTH_DEFAULT,
            peering_cost: 0,
            remote_peering_cost_max: REMOTE_PEERING_COST_MAX_DEFAULT,
            max_inbound_syncs: MAX_INBOUND_SYNCS_DEFAULT,
            from_static_only: false,
            static_peers: Vec::new(),
        }
    }
}

/// Transport phase of one peer's sync, the reference's state ladder
/// (`reference/LXMF/LXMF/LXMPeer.py:17-22`). Held on the peer so the
/// engine drives one round at a time; never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncPhase {
    #[default]
    Idle,
    /// A peering key is being mined on a worker; no sync until it lands.
    KeyMining,
    LinkEstablishing,
    RequestSent,
    ResourceTransferring,
}

/// One peer: the wire-visible facts plus the minimum liveness state, §5's
/// 80-byte record. Everything the reference additionally persists per peer
/// (`LXMPeer.to_bytes`, `reference/LXMF/LXMF/LXMPeer.py:138-175`) is
/// statistics or the per-message sets the cursor replaces.
#[derive(Debug, Clone, PartialEq)]
pub struct Peer {
    /// The peer's `lxmf.propagation` destination hash (16 B).
    pub destination_hash: [u8; DESTINATION_LENGTH],
    /// The peer's identity hash, once recalled — the first half of the
    /// peering-key material (`key_material`,
    /// `reference/LXMF/LXMF/LXMPeer.py:258`).
    pub identity_hash: Option<[u8; DESTINATION_LENGTH]>,
    /// The peer's public keys (X25519 ‖ Ed25519, the announce's key
    /// field), captured when the peer's announce is processed and kept
    /// with the peer (#388 pass 3). The general `known_identities`
    /// cache is a small rolling table on the board (8 slots against up
    /// to 16 peers), so a peer's keys must not depend on still being in
    /// it when its sync round comes — recall goes through
    /// [`Peer::recall_identity`], this field first, the cache second.
    /// 64 bytes per peer, 1 KiB at a full board table.
    pub public_keys: Option<[u8; 64]>,
    /// Mined peering key and its value (32 + 2 B). Mined at the peer's
    /// announced cost, reused as long as its value still satisfies that
    /// cost (`peering_key_ready`, `reference/LXMF/LXMF/LXMPeer.py:227-236`).
    pub peering_key: Option<([u8; 32], u16)>,
    /// Announce field 3, kilobytes (4 B).
    pub transfer_limit_kb: u64,
    /// Announce field 4, kilobytes (4 B).
    pub sync_limit_kb: u64,
    /// Announce field 5: stamp cost, flexibility, peering cost (3 B).
    pub stamp_cost: u8,
    pub stamp_cost_flexibility: u8,
    pub peering_cost: u8,
    /// The peer's announce timebase; only a newer announce updates the
    /// record (`peer`, `reference/LXMF/LXMF/LXMRouter.py:2016`).
    pub peering_timebase: u64,
    /// Liveness (9 B with the backoff and state below).
    pub last_heard: u64,
    pub next_sync_attempt: u64,
    pub sync_backoff_secs: u64,
    /// Cursor into the store's append order (6 B on the board:
    /// `page_sequence(u32) << 16 | offset(u16)`): everything at or below
    /// it has been offered and concluded.
    pub cursor: u64,
    /// From the static list: never culled, never declined by the cap.
    pub is_static: bool,
    /// Runtime transport phase, never persisted.
    pub state: SyncPhase,
}

impl Peer {
    fn from_announce(
        destination_hash: [u8; DESTINATION_LENGTH],
        announce: &PropagationNodeAnnounce,
        is_static: bool,
        now: u64,
    ) -> Self {
        Self {
            destination_hash,
            identity_hash: None,
            public_keys: None,
            peering_key: None,
            transfer_limit_kb: announce.transfer_limit_kb,
            sync_limit_kb: announce.sync_limit_kb,
            stamp_cost: announce.stamp_cost.min(u8::MAX as u64) as u8,
            stamp_cost_flexibility: announce.stamp_cost_flexibility.min(u8::MAX as u64) as u8,
            peering_cost: announce.peering_cost.min(u8::MAX as u64) as u8,
            peering_timebase: announce.timebase,
            last_heard: now,
            next_sync_attempt: 0,
            sync_backoff_secs: 0,
            cursor: 0,
            is_static,
            state: SyncPhase::Idle,
        }
    }

    /// The sync link to this peer came up.
    ///
    /// The round that opened the link booked a deferral before dialling
    /// — `sync_backoff_secs += SYNC_BACKOFF_STEP_SECS`, then
    /// `next_sync_attempt = now + sync_backoff_secs`
    /// (`reference/LXMF/LXMF/LXMPeer.py:321-322`). The moment the peer
    /// answers, that booking has been disproved: the peer is reachable.
    /// The reference clears the accumulator here (`:330`, `:541`) but
    /// leaves the deferral it already wrote in place, so a round that
    /// fails AFTER the link came up — a lost `/offer` response, an error
    /// answer — still holds the peer for a full `SYNC_BACKOFF_STEP_SECS`
    /// before it is due again.
    ///
    /// **Deviation from the reference, deliberate:** we clear the
    /// booking too. Wire format untouched (this is our own scheduler),
    /// semantics untouched (a Python peer sees at most an earlier
    /// re-sync from us, which `sync_peers` handles by construction), and
    /// Priority 1 improves: recovery from one lost frame mid-round drops
    /// from at least `SYNC_BACKOFF_STEP_SECS` to the next sync pass.
    /// Measured on the rig 2026-09-17 (`lora_pn_board_sync`, the run at
    /// 04:37Z): two consecutive rounds each lost one LoRa frame — the
    /// first the peer's `/offer` response into the offering board's own
    /// announce transmission, the second the first half of the `/offer`
    /// request — and the message then sat undelivered for twelve minutes
    /// with a reachable peer one hop away.
    ///
    /// The unreachable-peer case the backoff exists for is untouched: a
    /// peer whose link never comes up never reaches this call, so its
    /// accumulator keeps growing 12, 24, 36 minutes. So is an explicit
    /// throttle — `ResponseAction::Backoff` writes `next_sync_attempt`
    /// at response time, after this.
    pub fn note_link_established(&mut self) {
        self.sync_backoff_secs = 0;
        self.next_sync_attempt = 0;
    }

    /// The minimum stamp value this peer accepts:
    /// `max(0, cost − flexibility)`, the filter the offering side applies
    /// (`reference/LXMF/LXMF/LXMPeer.py:331`, `:340`).
    pub fn min_accepted_cost(&self) -> u8 {
        self.stamp_cost.saturating_sub(self.stamp_cost_flexibility)
    }

    /// Whether the held key still satisfies the peer's announced cost; a
    /// devalued key is discarded so the caller re-mines
    /// (`peering_key_ready`, `reference/LXMF/LXMF/LXMPeer.py:227-236`).
    ///
    /// **Deviation from the reference, deliberate:** `peering_key_ready`
    /// short-circuits false on a *falsy* cost (`LXMPeer.py:228`), so a
    /// stock peer never syncs toward a node announcing peering cost 0 —
    /// its own sync postpones forever on "peering key not generated". We
    /// treat cost 0 as "any key ready" (mine a free one once), which is
    /// what the validator side accepts (`validate_peering_key` with
    /// target 0 accepts any key, `LXStamper.py:79-82`). Wire format
    /// untouched; it makes cost-0 peers reachable rather than silently
    /// unreachable.
    pub fn peering_key_ready(&mut self) -> bool {
        match self.peering_key {
            Some((_, value)) if value as u32 >= self.peering_cost as u32 => true,
            Some(_) => {
                self.peering_key = None;
                false
            }
            None => false,
        }
    }

    /// Remember the peer's identity with the peer itself (#388 pass 3):
    /// public keys and identity hash, persisted via [`PeerRecord`]. Called
    /// when the identity is at hand (announce processing, a cache hit
    /// during recall). Returns whether anything changed, so the caller
    /// knows to persist.
    pub fn capture_identity(&mut self, identity: &Identity) -> bool {
        let keys = identity.public_key_bytes();
        let changed =
            self.public_keys != Some(keys) || self.identity_hash != Some(*identity.hash());
        self.public_keys = Some(keys);
        self.identity_hash = Some(*identity.hash());
        changed
    }

    /// Recall the peer's identity: the keys kept with the peer first, the
    /// node's `known_identities` cache second (#388 pass 3). The cache is
    /// a rolling 8-slot table on the board while the role peers with up
    /// to 16 nodes — sixteen announces cycle it twice over, so by the
    /// time a peer's sync round comes its cache entry may be long gone;
    /// the copy captured at announce time is what keeps the sync able to
    /// derive the peering-key material and open the link.
    pub fn recall_identity<S: Storage>(&self, storage: &S) -> Option<Identity> {
        if let Some(keys) = &self.public_keys {
            if let Ok(identity) = Identity::from_public_key_bytes(keys) {
                return Some(identity);
            }
        }
        storage.get_identity(&self.destination_hash).cloned()
    }
}

/// The persistable subset of a [`Peer`], what [`PeerStore`] carries.
///
/// What the trait demands of the board's record log (part 3): upsert is
/// append-a-new-record-then-purge-the-old under the same key (the
/// destination hash, zero-padded to the log's 32-byte key), load is one
/// tagged scan, and nothing is ever rewritten in place — no fixed page,
/// which is the §2 endurance rule. At ~90 B of body a full table of 16 is
/// a negligible tenant of the 64 KiB region.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerRecord {
    pub destination_hash: [u8; DESTINATION_LENGTH],
    pub identity_hash: Option<[u8; DESTINATION_LENGTH]>,
    /// The peer's public keys (see [`Peer::public_keys`]); persisted so a
    /// reboot does not have to wait for the peer's next announce (stock
    /// lxmd announces its propagation destination every six hours).
    pub public_keys: Option<[u8; 64]>,
    pub peering_key: Option<([u8; 32], u16)>,
    pub transfer_limit_kb: u64,
    pub sync_limit_kb: u64,
    pub stamp_cost: u8,
    pub stamp_cost_flexibility: u8,
    pub peering_cost: u8,
    pub peering_timebase: u64,
    pub last_heard: u64,
    pub cursor: u64,
    pub is_static: bool,
}

impl PeerRecord {
    pub fn of(peer: &Peer) -> Self {
        Self {
            destination_hash: peer.destination_hash,
            identity_hash: peer.identity_hash,
            public_keys: peer.public_keys,
            peering_key: peer.peering_key,
            transfer_limit_kb: peer.transfer_limit_kb,
            sync_limit_kb: peer.sync_limit_kb,
            stamp_cost: peer.stamp_cost,
            stamp_cost_flexibility: peer.stamp_cost_flexibility,
            peering_cost: peer.peering_cost,
            peering_timebase: peer.peering_timebase,
            last_heard: peer.last_heard,
            cursor: peer.cursor,
            is_static: peer.is_static,
        }
    }

    /// Rehydrate; transport phase and backoff start fresh, as the
    /// reference's own restore does for link state.
    pub fn into_peer(self) -> Peer {
        Peer {
            destination_hash: self.destination_hash,
            identity_hash: self.identity_hash,
            public_keys: self.public_keys,
            peering_key: self.peering_key,
            transfer_limit_kb: self.transfer_limit_kb,
            sync_limit_kb: self.sync_limit_kb,
            stamp_cost: self.stamp_cost,
            stamp_cost_flexibility: self.stamp_cost_flexibility,
            peering_cost: self.peering_cost,
            peering_timebase: self.peering_timebase,
            last_heard: self.last_heard,
            next_sync_attempt: 0,
            sync_backoff_secs: 0,
            cursor: self.cursor,
            is_static: self.is_static,
            state: SyncPhase::Idle,
        }
    }
}

/// Persistence boundary for the peer table. The host implementation is a
/// file (`leviculum-std`'s `FilePeerStore`), the board's is the record
/// log in part 3 — see [`PeerRecord`] for what that demands of it.
pub trait PeerStore {
    /// Upsert one record by destination hash.
    fn save(&mut self, record: &PeerRecord) -> Result<(), StorageError>;
    /// Remove one record; absent is not an error.
    fn remove(&mut self, destination_hash: &[u8; DESTINATION_LENGTH]) -> Result<(), StorageError>;
    /// Load every record.
    fn load_all(&self) -> Result<Vec<PeerRecord>, StorageError>;
}

/// In-memory [`PeerStore`] for tests and transient use.
#[derive(Debug, Default, Clone)]
pub struct MemoryPeerStore {
    records: BTreeMap<[u8; DESTINATION_LENGTH], PeerRecord>,
}

impl PeerStore for MemoryPeerStore {
    fn save(&mut self, record: &PeerRecord) -> Result<(), StorageError> {
        self.records.insert(record.destination_hash, record.clone());
        Ok(())
    }

    fn remove(&mut self, destination_hash: &[u8; DESTINATION_LENGTH]) -> Result<(), StorageError> {
        self.records.remove(destination_hash);
        Ok(())
    }

    fn load_all(&self) -> Result<Vec<PeerRecord>, StorageError> {
        Ok(self.records.values().cloned().collect())
    }
}

/// What one announce (or cull pass) did to the table, for the host's
/// `PN_PEER` log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerChange {
    Added,
    Updated,
    Dropped(DropReason),
    /// Not added, and why — the deterministic full-table policy is
    /// [`DeclineReason::TableFull`].
    Declined(DeclineReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Announce field 2 went false — the peer left the role
    /// (`Handlers.py:98-99`).
    Disabled,
    /// Moved outside `autopeer_maxdepth` (`Handlers.py:93-96`).
    OutOfDepth,
    /// Raised its peering cost beyond our maximum
    /// (`LXMRouter.py:2005-2008`).
    CostRaised,
    /// Unheard for [`MAX_UNREACHABLE_SECS`] (`LXMRouter.py:2136-2140`).
    Unreachable,
    /// The peer answered `/offer` with `ERROR_NO_ACCESS`
    /// (`LXMPeer.py:416-419`).
    NoAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclineReason {
    /// **The documented cap policy: first heard wins.** A full table
    /// declines new candidates, exactly the reference's behaviour
    /// (`LXMRouter.py:2032`); slots free deterministically when a peer is
    /// culled after [`MAX_UNREACHABLE_SECS`], unpeers, or leaves the
    /// role. No announce-time eviction: an eviction contest on every
    /// announce would let any newcomer churn the table, and the
    /// reference's acceptance-rate rotation needs the statistics we
    /// deliberately do not keep.
    TableFull,
    /// Its peering cost exceeds `remote_peering_cost_max`
    /// (`LXMRouter.py:2010`).
    CostTooHigh,
    /// Autopeering disabled and not a static peer.
    AutopeerOff,
    /// Beyond `autopeer_maxdepth` (`Handlers.py:83`).
    TooDeep,
    /// Announce field 2 false: not (or no longer) a propagation node.
    Disabled,
    /// Already in the table, so the candidate path had nothing to do. Only
    /// [`PeerTable::handle_inbound_sync`] reports this: the reference skips
    /// its whole recall-and-peer block for a remote it already peers with
    /// (`LXMRouter.py:2355-2357`), and the announce path has its own,
    /// refreshing answer for a known peer.
    AlreadyPeered,
    /// The announce arrived as a PATH_RESPONSE, so it is not evidence the
    /// destination announced itself (`Handlers.py:80`). Neither peers nor
    /// unpeers; the exception is a static peer never yet heard.
    PathResponse,
    /// A sync landed from a destination we can recall no propagation
    /// announce for — a client, not a node (`LXMRouter.py:2356`).
    NotANode,
}

/// The peer table: cap, static list, announce ingestion, culling.
#[derive(Debug)]
pub struct PeerTable {
    config: PeeringConfig,
    peers: BTreeMap<[u8; DESTINATION_LENGTH], Peer>,
}

impl PeerTable {
    pub fn new(config: PeeringConfig) -> Self {
        Self {
            config,
            peers: BTreeMap::new(),
        }
    }

    pub fn config(&self) -> &PeeringConfig {
        &self.config
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Estimated heap bytes the table pins (#388 census): the peer map's
    /// node structure. A [`Peer`] is inline — no heap behind it.
    pub fn heap_bytes(&self) -> usize {
        leviculum_core::heap_census::btree_map_bytes(&self.peers)
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    pub fn get(&self, destination_hash: &[u8; DESTINATION_LENGTH]) -> Option<&Peer> {
        self.peers.get(destination_hash)
    }

    pub fn get_mut(&mut self, destination_hash: &[u8; DESTINATION_LENGTH]) -> Option<&mut Peer> {
        self.peers.get_mut(destination_hash)
    }

    /// Iterate peers in destination-hash order — the deterministic order
    /// every scheduler decision below uses.
    pub fn iter(&self) -> impl Iterator<Item = &Peer> {
        self.peers.values()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Peer> {
        self.peers.values_mut()
    }

    /// Restore persisted peers (engine start-up).
    pub fn restore(&mut self, records: Vec<PeerRecord>) {
        for record in records {
            let mut peer = record.into_peer();
            peer.is_static =
                self.config.static_peers.contains(&peer.destination_hash) || peer.is_static;
            self.peers.insert(peer.destination_hash, peer);
        }
    }

    /// The highest stamp value any peer requires of a message before it
    /// will take it in an offer. When this is above zero the accept path
    /// computes true stamp values even at our own cost 0 (§5's
    /// consequence of `reference/LXMF/LXMF/LXMPeer.py:340`).
    pub fn max_peer_min_cost(&self) -> u8 {
        self.peers
            .values()
            .map(Peer::min_accepted_cost)
            .max()
            .unwrap_or(0)
    }

    /// Ingest one propagation announce, the handler's decision tree
    /// (`LXMFPropagationAnnounceHandler.received_announce`,
    /// `reference/LXMF/LXMF/Handlers.py:56-99`, and `peer` / `unpeer`,
    /// `LXMRouter.py:2004-2058`). `hops` is the transport's current hop
    /// count to the announcer, `None` when no path is known.
    ///
    /// `is_path_response` is the announce's own
    /// [`ReceivedAnnounce::is_path_response`](leviculum_core::ReceivedAnnounce::is_path_response):
    /// the announce came back as the answer to a path request, so it says
    /// only that somebody — possibly this node itself, for a destination it
    /// merely wanted to reach — asked where the destination is. The
    /// reference gates its whole autopeer arm on it (`Handlers.py:80`), and
    /// so neither peers nor unpeers here. It is a required argument rather
    /// than a caller-side guard because a caller that does not know it has
    /// to ask is exactly how this was wrong in both hosts (Codeberg #417):
    /// the transport reports a path response as an `AnnounceReceived` like
    /// any other, so a role that never asks peers every propagation node
    /// whose path anyone looked up.
    pub fn handle_announce(
        &mut self,
        destination_hash: [u8; DESTINATION_LENGTH],
        announce: &PropagationNodeAnnounce,
        hops: Option<u8>,
        now: u64,
        is_path_response: bool,
    ) -> PeerChange {
        if is_path_response && !self.peers_on_path_response(&destination_hash) {
            return PeerChange::Declined(DeclineReason::PathResponse);
        }
        let is_static = self.config.static_peers.contains(&destination_hash);
        let known = self.peers.contains_key(&destination_hash);

        // Peering cost beyond our maximum: refuse, and break an existing
        // peering (`LXMRouter.py:2005-2010`).
        if announce.peering_cost > self.config.remote_peering_cost_max as u64 {
            return if known {
                self.peers.remove(&destination_hash);
                PeerChange::Dropped(DropReason::CostRaised)
            } else {
                PeerChange::Declined(DeclineReason::CostTooHigh)
            };
        }

        if !is_static {
            // Field 2 false: every router unpeers on the next announce
            // (`Handlers.py:98-99`).
            if !announce.enabled {
                return if known {
                    self.unpeer(&destination_hash, announce.timebase)
                        .map_or(PeerChange::Declined(DeclineReason::Disabled), |()| {
                            PeerChange::Dropped(DropReason::Disabled)
                        })
                } else {
                    PeerChange::Declined(DeclineReason::Disabled)
                };
            }
            if !self.config.autopeer {
                return PeerChange::Declined(DeclineReason::AutopeerOff);
            }
            // Outside the depth: no new peering, and an existing one
            // breaks (`Handlers.py:83-96`).
            if hops.unwrap_or(u8::MAX) > self.config.autopeer_maxdepth {
                return if known {
                    self.unpeer(&destination_hash, announce.timebase)
                        .map_or(PeerChange::Declined(DeclineReason::TooDeep), |()| {
                            PeerChange::Dropped(DropReason::OutOfDepth)
                        })
                } else {
                    PeerChange::Declined(DeclineReason::TooDeep)
                };
            }
        }

        self.admit(destination_hash, announce, is_static, now)
    }

    /// [`Self::handle_inbound_sync`] for the caller that holds only what it
    /// could RECALL about the sender — the raw `app_data` of that
    /// destination's last announce, or nothing at all.
    ///
    /// This is the shape both hosts actually have at the moment a sync
    /// concludes: `NodeCore::recall_app_data` answers with bytes or `None`,
    /// and what those bytes mean is this crate's question, not the caller's.
    /// Keeping the decode here is what stops the daemon and the board from
    /// each inventing their own reading of "nothing recallable" — the
    /// reference has exactly one, a failed guard on `recall_app_data`
    /// returning `None` (`reference/LXMF/LXMF/LXMRouter.py:2356`), and
    /// [`DeclineReason::NotANode`] is its name here.
    pub fn handle_inbound_sync_recalled(
        &mut self,
        destination_hash: [u8; DESTINATION_LENGTH],
        recalled_app_data: Option<&[u8]>,
        hops: Option<u8>,
        now: u64,
    ) -> PeerChange {
        let Some(app_data) = recalled_app_data else {
            return PeerChange::Declined(DeclineReason::NotANode);
        };
        let Ok(announce) = PropagationNodeAnnounce::decode(app_data) else {
            return PeerChange::Declined(DeclineReason::NotANode);
        };
        self.handle_inbound_sync(destination_hash, &announce, hops, now)
    }

    /// Peer off a RECALLED announce because an inbound sync just landed
    /// (`propagation_resource_concluded`,
    /// `reference/LXMF/LXMF/LXMRouter.py:2350-2375`).
    ///
    /// The announce data here did not arrive with this event: it is what the
    /// remote last said about itself, recalled from the known-destination
    /// table, possibly hours old. That is the whole point — a propagation node
    /// announces every six hours, so a neighbour that announced before we took
    /// the role is invisible to [`Self::handle_announce`] until the next one,
    /// while its sync proves it is there now.
    ///
    /// **Why this is not just `handle_announce`:** that function also DROPS —
    /// a disabled flag, a raised cost or a depth violation breaks an existing
    /// peering. Acting on stale data that way would let a recalled announce
    /// unpeer a node whose live announce has since said otherwise, and the
    /// reference never unpeers on this path: it reaches `peer()` through three
    /// positive gates and has no `else` at all. So this refuses where
    /// `handle_announce` would drop, and where it would add, it adds through
    /// the very same private `admit` — which is what makes a peer discovered
    /// by sync indistinguishable from one discovered by announce.
    pub fn handle_inbound_sync(
        &mut self,
        destination_hash: [u8; DESTINATION_LENGTH],
        announce: &PropagationNodeAnnounce,
        hops: Option<u8>,
        now: u64,
    ) -> PeerChange {
        if self.peers.contains_key(&destination_hash) {
            return PeerChange::Declined(DeclineReason::AlreadyPeered);
        }
        if announce.peering_cost > self.config.remote_peering_cost_max as u64 {
            return PeerChange::Declined(DeclineReason::CostTooHigh);
        }
        let is_static = self.config.static_peers.contains(&destination_hash);
        if !is_static {
            // `pn_config[2]`, `self.autopeer`, `hops_to <= autopeer_maxdepth`
            // (`LXMRouter.py:2364`), in the reference's own order.
            if !announce.enabled {
                return PeerChange::Declined(DeclineReason::Disabled);
            }
            if !self.config.autopeer {
                return PeerChange::Declined(DeclineReason::AutopeerOff);
            }
            if hops.unwrap_or(u8::MAX) > self.config.autopeer_maxdepth {
                return PeerChange::Declined(DeclineReason::TooDeep);
            }
        }
        self.admit(destination_hash, announce, is_static, now)
    }

    /// The one case the reference acts on a path response: a STATIC peer it
    /// has never heard from (`not is_path_response or static_peer.last_heard
    /// == 0`, `reference/LXMF/LXMF/Handlers.py:68-70`). A static peering is
    /// configured rather than discovered, so the first path response is
    /// allowed to fill in the announce facts the operator could not
    /// configure; once the peer has been heard, a path response adds nothing.
    fn peers_on_path_response(&self, destination_hash: &[u8; DESTINATION_LENGTH]) -> bool {
        self.config.static_peers.contains(destination_hash)
            && self
                .peers
                .get(destination_hash)
                .is_none_or(|peer| peer.last_heard == 0)
    }

    /// Insert or refresh the peer record itself, once some path has decided
    /// the peering is allowed. The single place a [`Peer`] is built from
    /// announce data, so every route into the table produces the same record.
    fn admit(
        &mut self,
        destination_hash: [u8; DESTINATION_LENGTH],
        announce: &PropagationNodeAnnounce,
        is_static: bool,
        now: u64,
    ) -> PeerChange {
        if let Some(peer) = self.peers.get_mut(&destination_hash) {
            // Only a newer timebase updates (`LXMRouter.py:2016`).
            if announce.timebase > peer.peering_timebase {
                peer.peering_timebase = announce.timebase;
                peer.last_heard = now;
                peer.sync_backoff_secs = 0;
                peer.next_sync_attempt = 0;
                peer.transfer_limit_kb = announce.transfer_limit_kb;
                peer.sync_limit_kb = announce.sync_limit_kb;
                peer.stamp_cost = announce.stamp_cost.min(u8::MAX as u64) as u8;
                peer.stamp_cost_flexibility =
                    announce.stamp_cost_flexibility.min(u8::MAX as u64) as u8;
                peer.peering_cost = announce.peering_cost.min(u8::MAX as u64) as u8;
            } else {
                peer.last_heard = now;
            }
            return PeerChange::Updated;
        }

        // The cap: full-table candidates are declined, first heard wins
        // (`LXMRouter.py:2032`; the policy statement is on
        // [`DeclineReason::TableFull`]). Static peers bypass it.
        if !is_static && self.peers.len() >= self.config.max_peers {
            return PeerChange::Declined(DeclineReason::TableFull);
        }

        self.peers.insert(
            destination_hash,
            Peer::from_announce(destination_hash, announce, is_static, now),
        );
        PeerChange::Added
    }

    /// Break one peering (`unpeer`, `LXMRouter.py:2049-2058`): removed
    /// only when `timestamp` is not older than the peering timebase.
    pub fn unpeer(
        &mut self,
        destination_hash: &[u8; DESTINATION_LENGTH],
        timestamp: u64,
    ) -> Option<()> {
        let peer = self.peers.get(destination_hash)?;
        if timestamp >= peer.peering_timebase {
            self.peers.remove(destination_hash);
            Some(())
        } else {
            None
        }
    }

    /// Remove one peering unconditionally (the `ERROR_NO_ACCESS` answer,
    /// `LXMPeer.py:416-419`).
    pub fn remove(&mut self, destination_hash: &[u8; DESTINATION_LENGTH]) -> bool {
        self.peers.remove(destination_hash).is_some()
    }

    /// Cull peers unheard beyond [`MAX_UNREACHABLE_SECS`]; static peers
    /// are exempt (`sync_peers`, `LXMRouter.py:2136-2140`). Returns the
    /// dropped hashes.
    pub fn cull(&mut self, now: u64) -> Vec<[u8; DESTINATION_LENGTH]> {
        let dropped: Vec<[u8; DESTINATION_LENGTH]> = self
            .peers
            .values()
            .filter(|peer| {
                !peer.is_static && now > peer.last_heard.saturating_add(MAX_UNREACHABLE_SECS)
            })
            .map(|peer| peer.destination_hash)
            .collect();
        for hash in &dropped {
            self.peers.remove(hash);
        }
        dropped
    }

    /// Pick the next peer due for an outbound sync: the first, in
    /// destination-hash order, that is idle, past its backoff, and has
    /// records above its cursor. Deterministic round-robin replaces the
    /// reference's random pick from the fastest peers
    /// (`sync_peers`, `LXMRouter.py:2148-2176`) — a selection policy,
    /// nothing on the wire; determinism is what the test discipline
    /// wants and the board's single-sync-at-a-time constraint (§5) makes
    /// the pool size 1 anyway. `last` rotates fairness: the search
    /// starts strictly after it.
    pub fn next_due(
        &self,
        now: u64,
        newest_sequence: u64,
        last: Option<[u8; DESTINATION_LENGTH]>,
    ) -> Option<[u8; DESTINATION_LENGTH]> {
        let due = |peer: &Peer| {
            peer.state == SyncPhase::Idle
                && now >= peer.next_sync_attempt
                && peer.cursor < newest_sequence
        };
        let mut wrapped: Vec<&Peer> = self.peers.values().collect();
        if let Some(pivot) = last {
            wrapped.rotate_left(self.peers.keys().position(|key| *key > pivot).unwrap_or(0));
        }
        wrapped
            .iter()
            .find(|peer| due(peer))
            .map(|peer| peer.destination_hash)
    }
}

/// Peering-key material: the peer's identity hash followed by ours —
/// `key_material = self.identity.hash + self.router.identity.hash` on the
/// mining side (`reference/LXMF/LXMF/LXMPeer.py:258`), matching
/// `peering_id = self.identity.hash + remote_identity.hash` on the
/// validating side (`LXMRouter.py:2300`): in both, the *validating* node's
/// identity hash comes first.
pub fn peering_key_material(
    validator_identity_hash: &[u8; DESTINATION_LENGTH],
    offerer_identity_hash: &[u8; DESTINATION_LENGTH],
) -> [u8; 2 * DESTINATION_LENGTH] {
    let mut material = [0u8; 2 * DESTINATION_LENGTH];
    material[..DESTINATION_LENGTH].copy_from_slice(validator_identity_hash);
    material[DESTINATION_LENGTH..].copy_from_slice(offerer_identity_hash);
    material
}

/// The `/offer` request body: `[peering_key, [transient_id, …]]`
/// (`reference/LXMF/LXMF/LXMPeer.py:385`; parsed at `LXMRouter.py:2298`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerOffer {
    pub peering_key: [u8; 32],
    pub transient_ids: Vec<TransientId>,
}

impl PeerOffer {
    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::new();
        msgpack::array(&mut output, 2);
        msgpack::bin(&mut output, &self.peering_key);
        msgpack::array(&mut output, self.transient_ids.len());
        for id in &self.transient_ids {
            msgpack::bin(&mut output, id);
        }
        output
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PropagationError> {
        let mut position = 0;
        if msgpack::array_len(bytes, &mut position)? < 2 {
            return Err(PropagationError::InvalidLength);
        }
        let key = msgpack::read_bin(bytes, &mut position)?;
        let peering_key: [u8; 32] = key
            .try_into()
            .map_err(|_| PropagationError::InvalidLength)?;
        let count = msgpack::array_len(bytes, &mut position)?;
        if count > bytes.len().saturating_sub(position) {
            return Err(PropagationError::InvalidLength);
        }
        let mut transient_ids = Vec::with_capacity(count);
        for _ in 0..count {
            let id: TransientId = msgpack::read_bin(bytes, &mut position)?
                .try_into()
                .map_err(|_| PropagationError::InvalidLength)?;
            transient_ids.push(id);
        }
        Ok(Self {
            peering_key,
            transient_ids,
        })
    }
}

/// The `/offer` response: `True` (want all), `False` (want none), the
/// wanted sublist, or an error code
/// (`offer_request`, `reference/LXMF/LXMF/LXMRouter.py:2320-2329`; read
/// back in `offer_response`, `LXMPeer.py:400-452`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferResponse {
    WantNone,
    WantAll,
    Wanted(Vec<TransientId>),
    Error(PeerError),
}

impl OfferResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::new();
        match self {
            Self::WantNone => msgpack::bool(&mut output, false),
            Self::WantAll => msgpack::bool(&mut output, true),
            Self::Wanted(ids) => {
                msgpack::array(&mut output, ids.len());
                for id in ids {
                    msgpack::bin(&mut output, id);
                }
            }
            Self::Error(error) => msgpack::uint(&mut output, error.code() as u64),
        }
        output
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PropagationError> {
        let mut position = 0;
        match msgpack::peek_kind(bytes, position)? {
            msgpack::Kind::False => Ok(Self::WantNone),
            msgpack::Kind::True => Ok(Self::WantAll),
            msgpack::Kind::Array => {
                let count = msgpack::array_len(bytes, &mut position)?;
                if count > bytes.len().saturating_sub(position) {
                    return Err(PropagationError::InvalidLength);
                }
                let mut ids = Vec::with_capacity(count);
                for _ in 0..count {
                    let id: TransientId = msgpack::read_bin(bytes, &mut position)?
                        .try_into()
                        .map_err(|_| PropagationError::InvalidLength)?;
                    ids.push(id);
                }
                Ok(Self::Wanted(ids))
            }
            _ => Ok(Self::Error(PeerError::try_from(msgpack::read_uint(
                bytes,
                &mut position,
            )?)?)),
        }
    }
}

/// The sync resource body: `msgpack([timestamp, [lxmf_data ‖ stamp, …]])`
/// (`reference/LXMF/LXMF/LXMPeer.py:466`) — the multi-message form of the
/// client upload envelope, admitted only on a link whose peering key
/// validated (`LXMRouter.py:2381-2389`).
#[derive(Debug, Clone, PartialEq)]
pub struct PeerSyncEnvelope {
    pub timestamp: f64,
    pub messages: Vec<Vec<u8>>,
}

impl PeerSyncEnvelope {
    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::new();
        msgpack::array(&mut output, 2);
        msgpack::f64(&mut output, self.timestamp);
        msgpack::array(&mut output, self.messages.len());
        for message in &self.messages {
            msgpack::bin(&mut output, message);
        }
        output
    }

    /// Decode either form — one message or many; the caller enforces the
    /// peering-key gate on the multi-message case, exactly where the
    /// reference enforces it (`LXMRouter.py:2381-2389`).
    pub fn decode(bytes: &[u8]) -> Result<Self, PropagationError> {
        let mut position = 0;
        if msgpack::array_len(bytes, &mut position)? != 2 {
            return Err(PropagationError::InvalidLength);
        }
        let timestamp = msgpack::read_number_f64(bytes, &mut position)?;
        let count = msgpack::array_len(bytes, &mut position)?;
        if count > bytes.len().saturating_sub(position) {
            return Err(PropagationError::InvalidLength);
        }
        let mut messages = Vec::with_capacity(count);
        for _ in 0..count {
            messages.push(msgpack::read_bin(bytes, &mut position)?.to_vec());
        }
        // Trailing bytes tolerated for the same reason
        // `PropagationUpload::decode` tolerates them: the reference
        // unpacks with umsgpack, which ignores what follows the value.
        Ok(Self {
            timestamp,
            messages,
        })
    }
}

/// Inbound `/offer` gate state: which peers are throttled and until when
/// (`throttled_peers`, `reference/LXMF/LXMF/LXMRouter.py:2286-2291`,
/// `:2445-2450`; expired entries cleaned in `clean_throttled_peers`,
/// `:1136`).
#[derive(Debug, Default)]
pub struct InboundGate {
    throttled: BTreeMap<[u8; DESTINATION_LENGTH], u64>,
}

impl InboundGate {
    /// Every gate the reference applies before looking at the offer
    /// itself (`offer_request`, `LXMRouter.py:2266-2295`), in its order:
    /// sequential stamp validation, concurrent inbound syncs, the
    /// per-peer throttle, the static-only policy. Static peers bypass
    /// the first two (`bypass_sequential`, `:2273`).
    #[allow(clippy::too_many_arguments)]
    pub fn admit(
        &mut self,
        config: &PeeringConfig,
        remote_hash: &[u8; DESTINATION_LENGTH],
        now: u64,
        validating_stamps: bool,
        inbound_syncs: usize,
    ) -> Result<(), PeerError> {
        let is_static = config.static_peers.contains(remote_hash);
        if !is_static && validating_stamps {
            return Err(PeerError::Throttled);
        }
        if !is_static && config.max_inbound_syncs > 0 && inbound_syncs >= config.max_inbound_syncs {
            return Err(PeerError::Throttled);
        }
        if let Some(until) = self.throttled.get(remote_hash) {
            if now < *until {
                return Err(PeerError::Throttled);
            }
            self.throttled.remove(remote_hash);
        }
        if config.from_static_only && !is_static {
            return Err(PeerError::NoAccess);
        }
        Ok(())
    }

    /// Throttle a peer that shipped invalid stamps
    /// (`LXMRouter.py:2445-2450`).
    pub fn throttle(&mut self, remote_hash: [u8; DESTINATION_LENGTH], now: u64) {
        self.throttled
            .insert(remote_hash, now + PN_STAMP_THROTTLE_SECS);
    }

    pub fn is_throttled(&self, remote_hash: &[u8; DESTINATION_LENGTH], now: u64) -> bool {
        self.throttled
            .get(remote_hash)
            .is_some_and(|until| now < *until)
    }
}

/// Answer the membership half of an `/offer`: wanted = ids not in the
/// store (`offer_request`, `LXMRouter.py:2318-2329`). Store membership is
/// the only filter, as in the reference — a drained id is re-accepted and
/// re-deduplicated at ingest.
pub fn answer_offer(
    transient_ids: &[TransientId],
    contains: impl Fn(&TransientId) -> bool,
) -> OfferResponse {
    let wanted: Vec<TransientId> = transient_ids
        .iter()
        .filter(|id| !contains(id))
        .copied()
        .collect();
    if wanted.is_empty() {
        OfferResponse::WantNone
    } else if wanted.len() == transient_ids.len() {
        OfferResponse::WantAll
    } else {
        OfferResponse::Wanted(wanted)
    }
}

/// One planned outbound offer: the ids to send and where the cursor lands
/// when the round concludes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferPlan {
    pub ids: Vec<TransientId>,
    /// The cursor value a concluded (or "want none") round advances to:
    /// the highest sequence this plan looked at and either offered or
    /// deliberately skipped forever.
    pub cursor_target: u64,
    /// Skipped for exceeding the peer's per-message transfer limit —
    /// never retried, the reference marks these handled immediately
    /// (`LXMPeer.py:370-373`).
    pub skipped_oversize: usize,
    /// Skipped for a stamp value below the peer's minimum — never
    /// retried (`LXMPeer.py:340`, dropped at `:354-356`).
    pub skipped_low_value: usize,
}

/// Build one offer from the store scan: every entry above the peer's
/// cursor, in append order, filtered exactly as the offering reference
/// filters (`sync`, `reference/LXMF/LXMF/LXMPeer.py:331-379`) and bounded
/// by `offer_budget` bytes of encoded body.
///
/// `offer_budget` is the caller's, because only the caller knows the link
/// the offer will be handed to: [`offer_budget_for_mdu`] turns that
/// link's MDU into this number. Passing [`OFFER_BYTES_LIMIT`] blind is
/// what made a round with more than about ten offerable records die at
/// `send_request` with `PayloadTooLarge`, silently and for good — the
/// store only grows between purges, so the next round refused the same
/// way.
///
/// The append-order walk is what makes the cursor sound: the plan stops —
/// without advancing `cursor_target` further — at the first entry the
/// peer's sync limit or the offer byte bound excludes, so nothing above
/// the target was withheld for a resumable reason. Entries the plan
/// *does* step past are the permanently-skipped classes (oversize, low
/// stamp value), which the reference also never retries. The reference
/// instead offers weight-sorted and keeps scanning past a sync-limit hit
/// (`:375-376`); ours stops there — a selection-order deviation with no
/// wire effect, required for the cursor to replace the per-peer sets.
///
/// `entries` must be sorted ascending by `sequence`. Returns `None` when
/// nothing above the cursor is offerable and the cursor cannot advance.
pub fn build_offer(
    peer: &Peer,
    entries: &[StoredMessage],
    offer_budget: usize,
) -> Option<OfferPlan> {
    let mut plan = OfferPlan {
        ids: Vec::new(),
        cursor_target: peer.cursor,
        skipped_oversize: 0,
        skipped_low_value: 0,
    };
    let mut cumulative = OFFER_BASE_SIZE;
    let min_value = peer.min_accepted_cost();

    for entry in entries {
        if entry.sequence <= peer.cursor {
            continue;
        }
        if entry.stamp_value < min_value {
            plan.skipped_low_value += 1;
            plan.cursor_target = entry.sequence;
            continue;
        }
        let transfer_size = entry.size as u64 + OFFER_PER_MESSAGE_OVERHEAD;
        if transfer_size > peer.transfer_limit_kb.saturating_mul(1000) {
            plan.skipped_oversize += 1;
            plan.cursor_target = entry.sequence;
            continue;
        }
        if cumulative + transfer_size >= peer.sync_limit_kb.saturating_mul(1000) {
            break;
        }
        if offer_encoded_len(plan.ids.len() + 1) > offer_budget {
            break;
        }
        cumulative += transfer_size;
        plan.ids.push(entry.transient_id);
        plan.cursor_target = entry.sequence;
    }

    if plan.ids.is_empty() && plan.cursor_target == peer.cursor {
        None
    } else {
        Some(plan)
    }
}

/// What the engine must do with an `/offer` response
/// (`offer_response`, `reference/LXMF/LXMF/LXMPeer.py:400-452`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseAction {
    /// Send these bodies as one resource; ids the peer declined are
    /// handled by the cursor advancing on conclusion, which is the
    /// reference's own semantics rolled into one step (declined at
    /// response time `:443-448`, sent at conclusion `:500-502`).
    SendMessages(Vec<TransientId>),
    /// Nothing wanted: advance the cursor to the plan's target, tear the
    /// link down (`:473-480`).
    Concluded,
    /// Defer the next sync attempt this many seconds and tear down
    /// (`ERROR_THROTTLED`, `:421-424`).
    Backoff(u64),
    /// Break the peering (`ERROR_NO_ACCESS`, `:416-419`).
    Unpeer,
    /// Discard the held peering key and retry next round — the peer no
    /// longer accepts it (`ERROR_INVALID_KEY`; the reference reaches its
    /// generic teardown for this code, ours also drops the key so the
    /// next round re-mines at the announced cost).
    RemineKey,
    /// Identify-and-retry territory (`ERROR_NO_IDENTITY`, `:408-414`);
    /// tear down and retry next round.
    Retry,
}

/// Map a decoded response to its action.
pub fn response_action(response: &OfferResponse, plan: &OfferPlan) -> ResponseAction {
    match response {
        OfferResponse::WantNone => ResponseAction::Concluded,
        OfferResponse::WantAll => ResponseAction::SendMessages(plan.ids.clone()),
        OfferResponse::Wanted(ids) => {
            // Only ids we actually offered may be requested back; an id
            // we never named is ignored rather than served.
            let wanted: Vec<TransientId> = plan
                .ids
                .iter()
                .filter(|id| ids.contains(id))
                .copied()
                .collect();
            if wanted.is_empty() {
                ResponseAction::Concluded
            } else {
                ResponseAction::SendMessages(wanted)
            }
        }
        OfferResponse::Error(PeerError::Throttled) => {
            ResponseAction::Backoff(PN_STAMP_THROTTLE_SECS)
        }
        OfferResponse::Error(PeerError::NoAccess) => ResponseAction::Unpeer,
        OfferResponse::Error(PeerError::InvalidKey) => ResponseAction::RemineKey,
        OfferResponse::Error(_) => ResponseAction::Retry,
    }
}

#[cfg(test)]
#[path = "peering_tests.rs"]
mod tests;
