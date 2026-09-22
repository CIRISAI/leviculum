//! The propagation-node role: announce, upload accept, mailbox drain
//! (Codeberg #384, part 1).
//!
//! This is the *node* half of the exchange whose client half lives in
//! [`crate::propagation`] and [`crate::propagation_client`]: the state
//! machine a host (and later a board) runs to be somebody else's mailbox. It
//! performs no I/O and owns no links; the caller feeds it decoded transport
//! inputs and dispatches what it returns. Storage goes through
//! [`PropagationStore`], so the same role runs on a directory of files today
//! and the boards' record log in part 3.
//!
//! Node↔node peering — `/offer`, peering keys, outbound sync — lives in
//! [`crate::peering`] (part 2; the design, with numbers, is in
//! `docs/src/concepts/propagation-node-on-a-board.md` §5). This module
//! contributes the shared accept path: [`PropagationNode::accept_stamped`]
//! ingests one stamped message from a peer-sync resource through the same
//! validation, dedup and eviction as a client upload, and the caller
//! enforces the reference's peering-key gate on the multi-message form
//! (`reference/LXMF/LXMF/LXMRouter.py:2381-2389`). The raw-packet upload
//! keeps part 1's shape: a multi-message *packet* is answered with a torn
//! link — peers sync with a Resource (`LXMPeer.py:466-468`), never a
//! packet, so only a nonconforming sender can observe the difference.
//!
//! Every wire fact carries its `file:line` into the reference at the point
//! it is implemented. The reference pin is 795fdaa (LXMF 1.1.0).

use alloc::vec::Vec;

use leviculum_core::constants::RESOURCE_HASHMAP_LEN;
use leviculum_core::resource::RESOURCE_RANDOM_HASH_SIZE;

use crate::{
    constants::{DESTINATION_LENGTH, STAMP_SIZE},
    msgpack,
    propagation::{
        MessageGetRequest, MessageGetResponse, MessageListResponse, PropagationError,
        PropagationNodeAnnounce, PropagationSignal, PropagationUpload, TransferLimit, TransientId,
    },
    propagation_store::{PropagationStore, StoredMessage},
    storage::StorageError,
};

/// Announce metadata key carrying the node's display name
/// (`PN_META_NAME`, `reference/LXMF/LXMF/LXMF.py:133`).
pub const PN_META_NAME: u64 = 0x01;

/// Messages expire after 30 days
/// (`MESSAGE_EXPIRY`, `reference/LXMF/LXMF/LXMRouter.py:38`).
pub const MESSAGE_EXPIRY_SECS: u64 = 30 * 24 * 60 * 60;

/// How long a processed transient ID is remembered for duplicate detection:
/// six times the message expiry, as the reference prunes its own cache
/// (`clean_transient_id_caches`, `reference/LXMF/LXMF/LXMRouter.py:1011`).
pub const PROCESSED_ID_EXPIRY_SECS: u64 = 6 * MESSAGE_EXPIRY_SECS;

/// Bytes the request/response envelope adds to a response body on its way
/// to the wire: msgpack `fixarray(2)` (one byte) plus the 16-byte request
/// id as msgpack `bin8` (two header bytes plus its payload). Both the
/// single-packet path and the Resource path pack exactly this frame
/// (`write_fixarray_header`, `leviculum-core/src/node/mod.rs:1637`).
pub const RESPONSE_FRAME_BYTES: usize = 1 + 2 + 16;

/// Bookkeeping heap each part of an outgoing Resource costs beyond its
/// payload: the `Vec<Vec<u8>>` spine slot (24 B), the part's hashmap entry
/// and its collision-guard copy (`RESOURCE_HASHMAP_LEN` each) and the
/// `sent_mask` bool, rounded up. Second-order next to the payload copies
/// below, but it scales with part count and a bound that omits it is not
/// a bound.
pub const PART_BOOKKEEPING_BYTES: usize = 24 + 2 * RESOURCE_HASHMAP_LEN + 8;

/// Peak heap live *at one instant* while a node answers one `/get` fetch
/// whose accounted size is capped at `cap_bytes`, sent over a link whose
/// resource SDU is `resource_sdu` bytes.
///
/// This is the term the board's role budget was missing (#384): the serve
/// cap bounds the *wire* response, but the path from stored bodies to an
/// advertised Resource materialises that response seven more times, and
/// six of those copies are live simultaneously. On a 96 KiB heap the
/// difference between "8 KB fits" and "8 KB costs 80 KB" is the whole
/// question.
///
/// # Why the encoded response is bounded by the cap
///
/// The serve loop accounts 24 B up front and `body + 16` per served
/// message, where `body` is the *stamped* length, and stops strictly below
/// the cap (`cumulative_size`, `leviculum-lxmf/src/propagation_node.rs:583`).
/// What ships is the unstamped body, `STAMP_SIZE` (32 B) shorter, inside
/// `msgpack [bin, ...]`: at most 5 B of array header and 5 B of `bin`
/// header per message. Each accounted term therefore dominates its wire
/// term — 16 ≥ 5 per message, 24 ≥ 5 for the array — so the encoded
/// response is strictly shorter than the accounted sum, and `cap_bytes`
/// bounds it. Same dominance argument the inbound sync limit already
/// rests on, in the other direction.
///
/// # The copies, in the order they appear
///
/// With `R = cap_bytes` and `F = R + RESPONSE_FRAME_BYTES`:
///
/// 1. **`response`, ≤ 2R** — `MessageGetResponse::encode` extends a
///    `Vec::new()`, so its block is the next power of two at or above the
///    length. It is owned by the caller and live for the whole path below.
///    (`encode`, `leviculum-lxmf/src/propagation.rs:483`.)
/// 2. **`packed`, ≤ 2F** — `NodeCore::send_response` frames the response
///    *before* it compares the result with the link MDU, so an oversized
///    response pays one full framed copy that is then thrown away
///    (`send_response`, `leviculum-core/src/node/mod.rs:1636`). It is not
///    live at the peak, but it is the FIRST allocation of that size on the
///    path and therefore the first one that can fail.
/// 3. **`wrapped`, ≤ 2F** — `send_response_resource` frames it again, and
///    holds it until the call returns
///    (`send_response_resource`, `leviculum-core/src/node/mod.rs:1838`).
/// 4. **`combined`, ≤ 2F** — the Resource constructor's own copy, live to
///    the end of the constructor
///    (`new_with_flags`, `leviculum-core/src/resource/outgoing.rs:394`).
/// 5. **`data_to_encrypt`, F** — `combined.clone()` when nothing
///    compresses, an exact-capacity block.
/// 6. **`plaintext`, F + 4** — `with_capacity(4 + len)`, the wire random
///    prepended.
/// 7. **`encrypted`, `Link::encrypted_size(plaintext)`** — IV, padding to
///    the next 16, HMAC.
/// 8. **`parts`, ≈ `encrypted`** — `encrypted` re-split into one owned
///    block per part, plus [`PART_BOOKKEEPING_BYTES`] each. `hash_input`
///    and `proof_input` (each ≈ F) are built and dropped one at a time
///    just before this, so `parts` is what stands at the peak.
///
/// Terms 1 and 3-8 are live together at the end of the constructor: that
/// instant is the peak, and it is what this function sums. Term 2 is
/// counted too — it is the same size and it is strictly earlier, so a
/// heap that cannot serve the peak cannot serve term 2 either, and a
/// bound that skipped it would be describing a path the code does not
/// take.
///
/// # What is NOT in here
///
/// The allocator's own per-block header. `embedded-alloc`'s `LlffHeap`
/// carries one per live block, so a board pays more than this sum, most
/// visibly on the part blocks. This is a lower bound on the board's cost
/// and an exact bound on requested bytes.
pub const fn serve_peak_bytes(cap_bytes: usize, resource_sdu: usize) -> usize {
    let framed = cap_bytes + RESPONSE_FRAME_BYTES;
    // Term 1: the encoded response, grown by extension.
    let response = 2 * cap_bytes;
    // Term 2: the framed copy send_response builds and discards.
    let packed = 2 * framed;
    // Term 3 and 4: framed again, then copied into the constructor.
    let wrapped = 2 * framed;
    let combined = 2 * framed;
    // Term 5 and 6.
    let data_to_encrypt = framed;
    let plaintext = RESOURCE_RANDOM_HASH_SIZE + framed;
    // Term 7: IV + padding to the next 16 + HMAC, as Link::encrypted_size.
    let encrypted = 16 + (plaintext / 16 + 1) * 16 + 32;
    // Term 8: the same bytes again, one block per part.
    let sdu = if resource_sdu == 0 { 1 } else { resource_sdu };
    let part_count = encrypted.div_ceil(sdu);
    let parts = encrypted + part_count * PART_BOOKKEEPING_BYTES;
    response + packed + wrapped + combined + data_to_encrypt + plaintext + encrypted + parts
}

/// The largest serve cap whose [`serve_peak_bytes`] still fits
/// `budget_bytes` — the inverse a heap plan asks for: "given this much
/// free heap, how big may the cap be?". Zero when the budget does not
/// afford even a one-byte response.
///
/// Binary search rather than a closed form because
/// [`serve_peak_bytes`] rounds twice (the encryption padding and the part
/// count); it is monotone in `cap_bytes`, which is all a search needs.
pub const fn serve_cap_for_peak(budget_bytes: usize, resource_sdu: usize) -> usize {
    let mut low = 0usize;
    let mut high = budget_bytes;
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if serve_peak_bytes(mid, resource_sdu) <= budget_bytes {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low
}

/// Configuration of the role. The numeric defaults are the concept paper's
/// §2 recommendation ("What we announce",
/// `docs/src/concepts/propagation-node-on-a-board.md`): field 3 = 4 is the
/// largest whole kilobyte whose worst case still fits the one flash page a
/// board record may not straddle, and field 4 = 32 is about a hundred median
/// field messages — well under a lap of the 64 KiB region. The host uses the
/// same numbers so a peer sees one node class, not two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropagationNodeConfig {
    /// Announce field 3: per-transfer limit in kilobytes of 1000 bytes
    /// (the offering peer reads it that way,
    /// `propagation_transfer_limit`, `reference/LXMF/LXMF/LXMPeer.py:370`).
    pub transfer_limit_kb: u64,
    /// Announce field 4: per-sync limit in kilobytes, enforced by us against
    /// inbound resources (`propagation_resource_advertised`,
    /// `reference/LXMF/LXMF/LXMRouter.py:2220-2224`).
    pub sync_limit_kb: u64,
    /// Announce field 5\[0\]: the propagation stamp cost clients mine to.
    /// Default 0: accepted without work, user-settable. The reference clamps
    /// its own to ≥13 (`PROPAGATION_COST_MIN`,
    /// `reference/LXMF/LXMF/LXMRouter.py:52`, applied at `:137`); announcing
    /// below that is wire-legal and honoured — a peer's accepted cost is
    /// `max(0, cost − flexibility)` — and is this project's chosen policy
    /// (concept paper §1, "We would be the first to advertise cheap").
    pub stamp_cost: u8,
    /// Announce field 5\[1\]: how far below our cost a stamp may fall and
    /// still be accepted (`PROPAGATION_COST_FLEX`,
    /// `reference/LXMF/LXMF/LXMRouter.py:53`, default 3).
    pub stamp_cost_flexibility: u8,
    /// Announce field 5\[2\]: the peering cost another node mines to `/offer`
    /// us batches. Default 0 under the same policy as `stamp_cost`; unlike
    /// the propagation cost, the reference applies **no lower clamp** to its
    /// peering cost (`LXMRouter.py:137` clamps `propagation_cost` only), so
    /// 0 here conflicts with nothing in the reference.
    pub peering_cost: u8,
    /// Announce metadata: the node's display name, UTF-8.
    pub name: Option<Vec<u8>>,
    /// Message expiry. [`MESSAGE_EXPIRY_SECS`] unless a test shortens it.
    pub message_expiry_secs: u64,
}

impl Default for PropagationNodeConfig {
    fn default() -> Self {
        Self {
            transfer_limit_kb: 4,
            sync_limit_kb: 32,
            stamp_cost: 0,
            stamp_cost_flexibility: 3,
            peering_cost: 0,
            name: None,
            message_expiry_secs: MESSAGE_EXPIRY_SECS,
        }
    }
}

/// One message removed by expiry or displacement, for the host's `PN_EVICT`
/// log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Eviction {
    pub transient_id: TransientId,
    pub size: u32,
    pub age_secs: u64,
    pub reason: EvictionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictionReason {
    /// Older than the message expiry
    /// (`clean_message_store`, `reference/LXMF/LXMF/LXMRouter.py:1156-1160`).
    Expired,
    /// Culled by `age × size` weight to make room for a new message
    /// (`get_weight`, `reference/LXMF/LXMF/LXMRouter.py:1056-1067`;
    /// cull loop `:1188-1218`).
    Displaced,
}

/// What one upload envelope led to. The caller owes the wire actions named
/// on each variant; the role has already done the storage side.
#[derive(Debug, Clone, PartialEq)]
pub enum UploadOutcome {
    /// Stored (or already held). **Prove the packet now** — and only now:
    /// the reference proves after storing (`packet.prove`,
    /// `reference/LXMF/LXMF/LXMRouter.py:2255`), which is what makes a power
    /// cut mid-upload a client retry instead of a lost message. A duplicate
    /// is proven too: validation gates the proof, not storage newness
    /// (`:2252-2255` proves whenever every stamp validated;
    /// `lxmf_propagation` `:2496` merely declines to store again).
    Accepted {
        transient_id: TransientId,
        destination_hash: [u8; DESTINATION_LENGTH],
        size: u32,
        stamp_value: u8,
        duplicate: bool,
        /// Messages displaced to make room, oldest-heaviest first.
        evicted: Vec<Eviction>,
    },
    /// The stamp does not satisfy `max(0, cost − flexibility)`
    /// (`reference/LXMF/LXMF/LXMRouter.py:2242`). Send `reject` as a raw
    /// link packet and tear the link down (`:2257-2260`). Do not prove.
    InvalidStamp { reject: Vec<u8> },
    /// More than one message in the transfer: the `/offer` peer-sync form,
    /// which requires a validated peering key we do not implement until
    /// part 2. Tear the link down, as the reference does for the keyless
    /// case (`reference/LXMF/LXMF/LXMRouter.py:2382-2385`). Do not prove.
    PeerSyncForm,
    /// Undecodable envelope. Drop silently — the reference logs and ignores
    /// (`reference/LXMF/LXMF/LXMRouter.py:2262-2264`). Do not prove.
    Malformed(PropagationError),
    /// The store could not hold the message even after eviction. Do not
    /// prove: an unproven upload is retried by the client, an accepted-and-
    /// dropped one is silently lost (concept paper §1, "acceptance is proven
    /// and retention is not").
    StoreFailed(StorageError),
}

/// Why a `/get` could not be answered. Either way the caller responds with
/// msgpack nil, which is how the reference answers a request its handler
/// could not process (`reference/LXMF/LXMF/LXMRouter.py:1558-1560`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GetError {
    /// The request bytes did not decode.
    Request(PropagationError),
    /// The store failed mid-request.
    Store(StorageError),
}

impl From<PropagationError> for GetError {
    fn from(error: PropagationError) -> Self {
        Self::Request(error)
    }
}

impl From<StorageError> for GetError {
    fn from(error: StorageError) -> Self {
        Self::Store(error)
    }
}

impl core::fmt::Display for GetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Request(error) => write!(f, "get request: {error}"),
            Self::Store(error) => write!(f, "get store: {error}"),
        }
    }
}

impl core::error::Error for GetError {}

/// A `/get` answered. `response` is the exact msgpack the request response
/// must carry.
#[derive(Debug, Clone, PartialEq)]
pub enum GetOutcome {
    /// The list form (`wants` and `haves` both absent,
    /// `reference/LXMF/LXMF/LXMRouter.py:1491-1504`).
    List { response: Vec<u8>, count: usize },
    /// The fetch/acknowledge form: `purged` were deleted on the client's
    /// explicit confirmation, `served` go out in `response` with their
    /// stamps stripped.
    Fetch {
        response: Vec<u8>,
        served: Vec<TransientId>,
        served_bytes: u64,
        purged: Vec<TransientId>,
    },
}

/// The propagation-node role over one [`PropagationStore`].
pub struct PropagationNode<S> {
    store: S,
    config: PropagationNodeConfig,
    /// Recently processed transient IDs and when, for duplicate detection
    /// beyond the store's own lifetime (a drained-and-purged message must
    /// not be re-accepted from a stale retry,
    /// `locally_processed_transient_ids`,
    /// `reference/LXMF/LXMF/LXMRouter.py:2496,2499`).
    ///
    /// **Deviation:** the reference persists this set across restarts; ours
    /// is RAM-only and starts empty. Wire format is untouched; semantically
    /// a client that re-uploads an already-drained message after our restart
    /// sees it accepted again, which its own `has_message` filter already
    /// tolerates (`message_list_response`,
    /// `reference/LXMF/LXMF/LXMRouter.py:1581`). What it buys is Priority 1
    /// on the board: no per-message state outside the log — a rewritten
    /// metadata page is the 18.6-day endurance failure the concept paper's
    /// §2 forbids — and the host keeps the same shape so both run one code
    /// path.
    processed: alloc::collections::BTreeMap<TransientId, u64>,
    /// Compute true stamp values even when our own accepted cost is 0.
    /// Set by the engine while any known peer requires a stamp value above
    /// 0 of the messages it takes (`docs/src/concepts/
    /// propagation-node-on-a-board.md` §5: the offering side drops ids
    /// whose stored value is below the peer's minimum,
    /// `reference/LXMF/LXMF/LXMPeer.py:340`, and the record tag is written
    /// once at accept, so the decision is per-message at accept time).
    compute_stamp_value: bool,
    /// The calendar estimate this node last held before its calendar jumped
    /// to real time, or 0 on a node whose calendar never was birth-anchored
    /// (every host with a platform clock). Records stamped at or below it
    /// were written under the old calendar; see [`Self::tick`].
    calendar_jump_floor: u64,
}

impl<S: PropagationStore> PropagationNode<S> {
    /// Estimated heap bytes the role itself pins (#388 census): the
    /// processed-id duplicate cache and the announced name. The store
    /// adapter accounts for its own queued writes.
    pub fn heap_bytes(&self) -> usize {
        leviculum_core::heap_census::btree_map_bytes(&self.processed)
            + self.config.name.as_ref().map_or(0, |name| name.capacity())
    }

    pub fn new(store: S, config: PropagationNodeConfig) -> Self {
        Self {
            store,
            config,
            processed: alloc::collections::BTreeMap::new(),
            compute_stamp_value: false,
            calendar_jump_floor: 0,
        }
    }

    /// The node's calendar just jumped from its birth anchor to real time;
    /// `before` is the estimate the old calendar gave at the moment of the
    /// jump (Codeberg #247). Every stored record stamped at or below it was
    /// written under the old calendar, and [`Self::tick`] must not read the
    /// jump as elapsed time.
    ///
    /// Told rather than inferred: since the birth anchor is the build
    /// timestamp, a birth-era stamp is a perfectly plausible-looking value
    /// and no fixed date can separate the two epochs any more. The owner of
    /// the node — the firmware's propagation role, which seeds the clock —
    /// is the one place that knows the boundary.
    pub fn note_calendar_jump(&mut self, before: u64) {
        self.calendar_jump_floor = self.calendar_jump_floor.max(before);
    }

    /// See the field: while set, the accept path asks `validate` for the
    /// true stamp value even at accepted cost 0 (validation at cost 0
    /// cannot fail, so this only fills the stored value).
    pub fn set_compute_stamp_value(&mut self, compute: bool) {
        self.compute_stamp_value = compute;
    }

    pub fn compute_stamp_value(&self) -> bool {
        self.compute_stamp_value
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// Mutable store access, for seeding, migration, and tests. The role's
    /// own bookkeeping holds no shadow copy of the store, so mutating it
    /// directly cannot desynchronise anything.
    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    pub fn config(&self) -> &PropagationNodeConfig {
        &self.config
    }

    /// The minimum stamp value an upload must prove:
    /// `max(0, cost − flexibility)`
    /// (`reference/LXMF/LXMF/LXMRouter.py:2242`).
    pub fn min_accepted_cost(&self) -> u8 {
        self.config
            .stamp_cost
            .saturating_sub(self.config.stamp_cost_flexibility)
    }

    /// The seven-field announce app-data, exactly as the reference builds it
    /// (`get_propagation_node_app_data`,
    /// `reference/LXMF/LXMF/LXMRouter.py:324-336`): field 0 legacy `False`,
    /// field 1 the current timebase, field 2 `True` (the role is live and
    /// serves strangers), fields 3 and 4 the limits, field 5
    /// `[stamp_cost, flexibility, peering_cost]`, field 6 the metadata map
    /// with the name under [`PN_META_NAME`].
    pub fn announce_app_data(&self, now_secs: u64) -> Vec<u8> {
        let mut metadata = Vec::new();
        if let Some(name) = &self.config.name {
            let mut raw = Vec::with_capacity(name.len() + 2);
            msgpack::bin(&mut raw, name);
            metadata.push((PN_META_NAME, raw));
        }
        let announce = PropagationNodeAnnounce {
            legacy_support: false,
            timebase: now_secs,
            enabled: true,
            transfer_limit_kb: self.config.transfer_limit_kb,
            sync_limit_kb: self.config.sync_limit_kb,
            stamp_cost: self.config.stamp_cost as u64,
            stamp_cost_flexibility: self.config.stamp_cost_flexibility as u64,
            peering_cost: self.config.peering_cost as u64,
            metadata,
        };
        // The encoder fails only on malformed raw metadata, and ours is a
        // single msgpack bin built two lines up.
        announce.encode().unwrap_or_default()
    }

    /// Whether an advertised inbound resource may transfer at all: refused
    /// before it moves when larger than the announced per-sync limit
    /// (`propagation_resource_advertised`,
    /// `reference/LXMF/LXMF/LXMRouter.py:2220-2224`; a kilobyte is 1000
    /// bytes there too).
    pub fn accepts_resource_of(&self, data_size: u64) -> bool {
        data_size <= self.config.sync_limit_kb * 1000
    }

    /// Accept one client upload envelope (`[timestamp, [lxmf_data ‖ stamp]]`,
    /// `propagation_packet`, `reference/LXMF/LXMF/LXMRouter.py:2234-2255`),
    /// from either the raw-link-packet or the single-message resource path.
    ///
    /// `validate` is called only when [`Self::min_accepted_cost`] is above
    /// zero (or [`Self::set_compute_stamp_value`] demands true values for
    /// peering), with the transient ID and the 32-byte stamp; it returns the
    /// stamp's value if valid at that cost (`validate_pn_stamp`,
    /// `reference/LXMF/LXMF/LXStamper.py:84-96`, over the
    /// [`crate::constants::WORKBLOCK_EXPAND_ROUNDS_PN`]-round workblock) or
    /// `None`. At cost 0 the work is skipped entirely and the stored stamp
    /// value is 0 — the reference expands the workblock even then to record
    /// the true value (`LXStamper.py:95`); ours is a deviation that touches
    /// no wire byte and only the local value used when offering to peers
    /// (`reference/LXMF/LXMF/LXMPeer.py:340`), which part 2's design accounts
    /// for (concept paper §5).
    pub fn handle_upload(
        &mut self,
        envelope: &[u8],
        now_secs: u64,
        validate: impl FnOnce(&TransientId, &[u8; STAMP_SIZE]) -> Option<u16>,
    ) -> UploadOutcome {
        let upload = match PropagationUpload::decode(envelope) {
            Ok(upload) => upload,
            Err(PropagationError::MultipleMessages) => return UploadOutcome::PeerSyncForm,
            Err(error) => return UploadOutcome::Malformed(error),
        };
        self.ingest_split(
            upload.unstamped_lxmf(),
            upload.propagation_stamp(),
            *upload.transient_id(),
            now_secs,
            validate,
        )
    }

    /// Accept one stamped message body (`lxmf_data ‖ stamp`) from a peer
    /// sync resource. Same acceptance path as a client upload — the
    /// reference funnels both through `lxmf_propagation`
    /// (`reference/LXMF/LXMF/LXMRouter.py:2430-2436` for the sync side,
    /// `:2245-2250` for the client side) — with the same length guard as
    /// `validate_pn_stamp` (`reference/LXMF/LXMF/LXStamper.py:87`).
    pub fn accept_stamped(
        &mut self,
        stamped: &[u8],
        now_secs: u64,
        validate: impl FnOnce(&TransientId, &[u8; STAMP_SIZE]) -> Option<u16>,
    ) -> UploadOutcome {
        if stamped.len() <= crate::constants::LXMF_OVERHEAD + STAMP_SIZE {
            return UploadOutcome::Malformed(PropagationError::InvalidLength);
        }
        let (unstamped, stamp) = stamped.split_at(stamped.len() - STAMP_SIZE);
        let mut propagation_stamp = [0u8; STAMP_SIZE];
        propagation_stamp.copy_from_slice(stamp);
        let transient_id = leviculum_core::crypto::full_hash(unstamped);
        self.ingest_split(
            unstamped,
            &propagation_stamp,
            transient_id,
            now_secs,
            validate,
        )
    }

    fn ingest_split(
        &mut self,
        unstamped: &[u8],
        stamp: &[u8; STAMP_SIZE],
        transient_id: TransientId,
        now_secs: u64,
        validate: impl FnOnce(&TransientId, &[u8; STAMP_SIZE]) -> Option<u16>,
    ) -> UploadOutcome {
        let min_cost = self.min_accepted_cost();
        let stamp_value = if min_cost == 0 && !self.compute_stamp_value {
            0
        } else {
            match validate(&transient_id, stamp) {
                Some(value) => value.min(u8::MAX as u16) as u8,
                None => {
                    return UploadOutcome::InvalidStamp {
                        reject: PropagationSignal::InvalidStamp.encode(),
                    }
                }
            }
        };

        let mut destination_hash = [0u8; DESTINATION_LENGTH];
        destination_hash.copy_from_slice(&unstamped[..DESTINATION_LENGTH]);

        let mut body = Vec::with_capacity(unstamped.len() + stamp.len());
        body.extend_from_slice(unstamped);
        body.extend_from_slice(stamp);
        let size = body.len() as u32;

        let duplicate = self.processed.contains_key(&transient_id)
            || self.store.contains(&transient_id).unwrap_or(false);
        if duplicate {
            return UploadOutcome::Accepted {
                transient_id,
                destination_hash,
                size,
                stamp_value,
                duplicate: true,
                evicted: Vec::new(),
            };
        }

        let mut evicted = Vec::new();
        let mut result = self
            .store
            .append(&transient_id, now_secs, stamp_value, &body);
        if result == Err(StorageError::Full) {
            evicted = self.make_room(body.len() as u64, now_secs);
            result = self
                .store
                .append(&transient_id, now_secs, stamp_value, &body);
        }
        if let Err(error) = result {
            return UploadOutcome::StoreFailed(error);
        }
        self.processed.insert(transient_id, now_secs);

        UploadOutcome::Accepted {
            transient_id,
            destination_hash,
            size,
            stamp_value,
            duplicate: false,
            evicted,
        }
    }

    /// Un-remember one transient ID from the duplicate cache.
    ///
    /// For a caller whose store defers durability past the
    /// [`PropagationStore::append`] boundary (the board's flush queue,
    /// `leviculum-pn-store`): when the deferred write fails, the entry
    /// this role recorded at accept time would otherwise turn the
    /// client's retry into a proven-but-unstored duplicate — the exact
    /// lie "persist before you prove" exists to prevent.
    pub fn forget_processed(&mut self, transient_id: &TransientId) {
        self.processed.remove(transient_id);
    }

    /// Answer one `/get` request for the client whose delivery destination
    /// hash is `remote_delivery_hash` (derived by the caller from the
    /// link-identified identity, as the reference derives it,
    /// `message_get_request`, `reference/LXMF/LXMF/LXMRouter.py:1487`).
    ///
    /// A decode error maps to the reference's behaviour of answering a
    /// request it cannot parse with `None`
    /// (`reference/LXMF/LXMF/LXMRouter.py:1558-1560`); the caller sends
    /// msgpack nil.
    pub fn handle_get(
        &mut self,
        request: &[u8],
        remote_delivery_hash: &[u8; DESTINATION_LENGTH],
        now_secs: u64,
    ) -> Result<GetOutcome, GetError> {
        let request = MessageGetRequest::decode(request)?;

        // Both fields absent: the list form
        // (reference/LXMF/LXMF/LXMRouter.py:1489-1504).
        if request.wants.is_none() && request.haves.is_none() {
            let mut available: Vec<(TransientId, u32)> = Vec::new();
            self.store.for_each(&mut |meta: &StoredMessage| {
                if &meta.destination_hash == remote_delivery_hash {
                    available.push((meta.transient_id, meta.size));
                }
            })?;
            // Smallest first (:1500), so a limited later fetch drains the
            // most messages.
            available.sort_by_key(|(_, size)| *size);
            let ids: Vec<TransientId> = available.into_iter().map(|(id, _)| id).collect();
            let count = ids.len();
            let response = MessageListResponse::TransientIds(ids).encode()?;
            return Ok(GetOutcome::List { response, count });
        }

        // One directory pass for the whole request; membership below is
        // asked of this snapshot rather than of the store per ID.
        let mut owned: alloc::collections::BTreeSet<TransientId> =
            alloc::collections::BTreeSet::new();
        self.store.for_each(&mut |meta: &StoredMessage| {
            if &meta.destination_hash == remote_delivery_hash {
                owned.insert(meta.transient_id);
            }
        })?;

        // Deletion happens here and only here: on the client's explicit
        // `haves` confirmation, which it sends only after taking local
        // delivery (`message_get_response` builds `haves` from what it
        // ingested and requests `[None, haves]`,
        // reference/LXMF/LXMF/LXMRouter.py:1622-1638; the purge loop is
        // :1508-1519). A crash mid-transfer therefore deletes nothing.
        let mut purged = Vec::new();
        if let Some(haves) = &request.haves {
            for transient_id in haves {
                if owned.contains(transient_id) && self.store.purge(transient_id).unwrap_or(false) {
                    purged.push(*transient_id);
                }
            }
        }

        // The client's transfer limit arrives in kilobytes of 1000 bytes as
        // element 2 (:1526-1530); our own announced per-sync limit caps the
        // response as well. The second cap is ours, not the reference's —
        // wire-compatible under the deviation rule: a response shorter than
        // the request is the protocol's normal shape (skipped IDs are
        // re-requested on the next sync, :1547 skips them the same way), and
        // bounding one response resource at what we advertise as one sync
        // keeps a slow link's transfer inside the budget the announce names.
        let client_limit = request.transfer_limit_kb.map(|limit| match limit {
            TransferLimit::Integer(kb) => (kb as f64) * 1000.0,
            TransferLimit::Float(kb) => kb * 1000.0,
        });
        let our_limit = (self.config.sync_limit_kb * 1000) as f64;
        let limit = client_limit.map_or(our_limit, |client| client.min(our_limit));

        // Overheads exactly as the reference budgets them (:1532-1533).
        let per_message_overhead = 16.0;
        let mut cumulative_size = 24.0;

        let mut served = Vec::new();
        let mut bodies = Vec::new();
        let mut served_bytes = 0u64;
        if let Some(wants) = &request.wants {
            for transient_id in wants {
                if !owned.contains(transient_id) || purged.contains(transient_id) {
                    // Gone or never ours to give: skipped silently, the
                    // response is simply shorter (:1535 membership test; the
                    // concept paper's §1 records that absence has no error).
                    continue;
                }
                let Some(body) = self.store.read_body(transient_id)? else {
                    continue;
                };
                let lxm_size = body.len() as f64;
                let next_size = cumulative_size + lxm_size + per_message_overhead;
                if next_size > limit {
                    // Too big for this round; the reference keeps scanning
                    // rather than stopping (:1547 `pass`).
                    continue;
                }
                cumulative_size = next_size;
                // Serve the message without its propagation stamp
                // (:1549 strips `STAMP_SIZE` from the tail).
                let unstamped = body[..body.len() - STAMP_SIZE].to_vec();
                served_bytes += unstamped.len() as u64;
                served.push(*transient_id);
                bodies.push(unstamped);
            }
        }

        // Keep the duplicate guard alive for what was just confirmed
        // drained, so a client retry of the original upload is not
        // re-stored.
        for transient_id in &purged {
            self.processed.insert(*transient_id, now_secs);
        }

        let response = MessageGetResponse::Messages(bodies).encode()?;
        Ok(GetOutcome::Fetch {
            response,
            served,
            served_bytes,
            purged,
        })
    }

    /// Periodic maintenance: purge expired messages
    /// (`clean_message_store`, `reference/LXMF/LXMF/LXMRouter.py:1156-1160`)
    /// and prune the processed-ID cache
    /// (`clean_transient_id_caches`, `:1011`).
    ///
    /// Expiry is epoch-guarded for the board's clockless bring-up (#384
    /// part 3, instruction item 6): a message stored while the node's
    /// calendar was still on its birth anchor carries a stamp from that
    /// epoch, and the first real time seed would otherwise make every such
    /// record "30 days old" in one jump and mass-expire a store the node
    /// just proved it accepted. A record stamped at or below the boundary
    /// [`Self::note_calendar_jump`] recorded is therefore never expired by
    /// age — its space is still reclaimed by the store's own displacement
    /// (host) or page reclaim (board), so the guard costs retention policy,
    /// never capacity.
    ///
    /// The boundary is told, not inferred from a fixed date: since #247 the
    /// birth anchor is the build timestamp, which no value test can tell
    /// apart from real time.
    pub fn tick(&mut self, now_secs: u64) -> Vec<Eviction> {
        let expiry = self.config.message_expiry_secs;
        let floor = self.calendar_jump_floor;
        let mut expired = Vec::new();
        let _ = self.store.for_each(&mut |meta: &StoredMessage| {
            if floor > 0 && meta.received_at <= floor && now_secs > floor {
                return;
            }
            if now_secs.saturating_sub(meta.received_at) > expiry {
                expired.push(Eviction {
                    transient_id: meta.transient_id,
                    size: meta.size,
                    age_secs: now_secs.saturating_sub(meta.received_at),
                    reason: EvictionReason::Expired,
                });
            }
        });
        expired.retain(|eviction| self.store.purge(&eviction.transient_id).unwrap_or(false));

        self.processed
            .retain(|_, received| now_secs.saturating_sub(*received) <= PROCESSED_ID_EXPIRY_SECS);
        expired
    }

    /// Cull by the reference's weight until at least `needed` bytes are
    /// free: `age_weight × size` with
    /// `age_weight = max(1, age / 4 days)` (`get_weight`,
    /// `reference/LXMF/LXMF/LXMRouter.py:1056-1067`; the prioritised-list
    /// factor is omitted — this node has no prioritised list). The trigger
    /// differs from the reference by necessity: it culls when a *configured*
    /// limit is exceeded on a periodic job (`:1183-1218`), our store has a
    /// hard capacity and culls when an append reports [`StorageError::Full`].
    fn make_room(&mut self, needed: u64, now_secs: u64) -> Vec<Eviction> {
        let mut weighted: Vec<(f64, Eviction)> = Vec::new();
        let _ = self.store.for_each(&mut |meta: &StoredMessage| {
            let age_secs = now_secs.saturating_sub(meta.received_at);
            let age_weight = (age_secs as f64 / (4.0 * 24.0 * 60.0 * 60.0)).max(1.0);
            weighted.push((
                age_weight * meta.size as f64,
                Eviction {
                    transient_id: meta.transient_id,
                    size: meta.size,
                    age_secs,
                    reason: EvictionReason::Displaced,
                },
            ));
        });
        weighted.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(core::cmp::Ordering::Equal)
        });

        let mut evicted = Vec::new();
        for (_, eviction) in weighted {
            if self.store.free_space() >= needed {
                break;
            }
            if self.store.purge(&eviction.transient_id).unwrap_or(false) {
                evicted.push(eviction);
            }
        }
        evicted
    }
}

#[cfg(test)]
#[path = "propagation_node_tests.rs"]
mod tests;
