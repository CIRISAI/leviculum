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
use leviculum_core::resource::{ResourceSource, SourceError, RESOURCE_RANDOM_HASH_SIZE};

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
/// single-packet path and the Resource path pack exactly this frame, and
/// since #384 both compute its length before building it rather than
/// after (`bin_len`, `leviculum-core/src/node/mod.rs:1642`).
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
/// advertised Resource also holds working copies, and what a heap plan
/// has to fund is all of them at once, not the response.
///
/// # Why the encoded response is bounded by the cap
///
/// The serve loop accounts 24 B up front and `body + 16` per served
/// message, where `body` is the *stamped* length, and stops strictly below
/// the cap (`plan_fetch`, `leviculum-lxmf/src/propagation_node.rs`).
/// What ships is the unstamped body, `STAMP_SIZE` (32 B) shorter, inside
/// `msgpack [bin, ...]`: at most 5 B of array header and 5 B of `bin`
/// header per message. Each accounted term therefore dominates its wire
/// term — 16 ≥ 5 per message, 24 ≥ 5 for the array — so the encoded
/// response is strictly shorter than the accounted sum, and `cap_bytes`
/// bounds it. Same dominance argument the inbound sync limit already
/// rests on, in the other direction.
///
/// # The copies, in the order they appear (B2)
///
/// With `R = cap_bytes` and `F = R + RESPONSE_FRAME_BYTES`:
///
/// 1. **one stored body, ≤ R** — the record the source is reading
///    through. `PropagationStore::read_body` hands out a whole record and
///    the serve holds exactly one at a time
///    ([`FetchSource`]). Bounded by the cap because a message whose
///    accounted size does not fit the cap is never planned, so every
///    served body is `≤ R − 40`.
/// 2. **one part's scratch, `resource_sdu`** — the buffer the resource
///    builder reads the source into and encrypts out of
///    (`new_response_from_source`,
///    `leviculum-core/src/resource/outgoing.rs`).
/// 3. **`parts`, the transfer** — the encrypted stream, one owned block
///    per part, plus [`PART_BOOKKEEPING_BYTES`] each. This IS the
///    resource; it is live until the receiver proves it, and nothing can
///    remove it short of refusing to serve.
///
/// All three are live together while the last part is being cut: that
/// instant is the peak, and it is what this function sums.
///
/// # What the path no longer spends (#384, B2)
///
/// Four whole-response copies the earlier model had to carry are gone,
/// and the numbers here moved with them:
///
/// * `response`, ≤ 2R — `MessageGetResponse::encode` grew a `Vec` to hold
///   every served body before the resource path had copied anything. The
///   role now returns a [`FetchPlan`] — ids and lengths — and writes the
///   msgpack framing around records read at part-cut time
///   ([`PropagationNode::fetch_source`]).
/// * `wrapped`, F — the `[request_id, response]` frame as a second
///   buffer. It is 19 bytes of prefix on the source now
///   (`PrefixSource`, `leviculum-core/src/resource/source.rs`).
/// * `plaintext`, F + 4 — the wire random prepended to the payload. The
///   token encryption is streaming, so the plaintext exists one AES block
///   at a time (`TokenEncryptor`, `leviculum-core/src/crypto/token.rs`).
/// * `encrypted` ≈ F — the joined ciphertext, whose only reader was the
///   part slicing and, afterwards, its own length. The parts ARE the
///   ciphertext; `OutgoingResource` keeps the length.
///
/// The single-packet path is not modelled separately because it cannot
/// exceed this: a response that fits one data packet is under the link
/// MDU (`response_fits_packet`, `leviculum-core/src/node/mod.rs`), a few
/// hundred bytes, and is materialised precisely because that is cheaper
/// than streaming it.
///
/// # What is NOT in here
///
/// The allocator's own per-block header, as before — `embedded-alloc`'s
/// `LlffHeap` carries one per live block, so a board pays more than this
/// sum, most visibly on the part blocks. And the cached advertisement
/// packet, whose hashmap segment is at most `HASHMAP_MAX_LEN × 4` bytes;
/// it was outside the pre-B2 model too, so the before/after comparison
/// is like for like. This is a lower bound on the board's cost and an
/// exact bound on requested bytes.
pub const fn serve_peak_bytes(cap_bytes: usize, resource_sdu: usize) -> usize {
    let framed = cap_bytes + RESPONSE_FRAME_BYTES;
    let sdu = if resource_sdu == 0 { 1 } else { resource_sdu };
    // Term 1: the one stored body the source holds while it streams it.
    let body = cap_bytes;
    // Term 2: the builder's read/encrypt scratch, one part wide and never
    // wider than the payload (`new_response_from_source`,
    // `leviculum-core/src/resource/outgoing.rs`).
    let scratch = if sdu < framed { sdu } else { framed };
    // Term 3: the transfer itself. The token is IV + PKCS7 padding to the
    // next whole block + HMAC over the wire random and the framed
    // response (`token_len`, `leviculum-core/src/crypto/token.rs`).
    let plaintext = RESOURCE_RANDOM_HASH_SIZE + framed;
    let transfer = 16 + (plaintext / 16 + 1) * 16 + 32;
    let part_count = transfer.div_ceil(sdu);
    let parts = transfer + part_count * PART_BOOKKEEPING_BYTES;
    body + scratch + parts
}

/// The pre-B2 serve peak: what the same fetch cost while the response was
/// built as a buffer and copied four more times on its way to the wire
/// (`serve_peak_bytes` as of 0efc7c86).
///
/// Kept, and kept exact, because it is the number every heap measurement
/// before 2026-09-23 was read against — the T114's 33 238 B transient
/// against 30 380 B of free heap, the 54 848 B a single-sync drain of 24
/// messages needed. A model that silently replaced those makes the
/// board's own logs unreadable; this one lets a test state the before and
/// the after in the same breath
/// (`pn_serve_cap_bounds_one_fetch.rs`).
///
/// Nothing in production calls it. It is a measurement, not a policy.
pub const fn serve_buffered_peak_bytes(cap_bytes: usize, resource_sdu: usize) -> usize {
    let framed = cap_bytes + RESPONSE_FRAME_BYTES;
    let response = 2 * cap_bytes;
    let wrapped = framed;
    let plaintext = RESOURCE_RANDOM_HASH_SIZE + framed;
    let encrypted = 16 + (plaintext / 16 + 1) * 16 + 32;
    let sdu = if resource_sdu == 0 { 1 } else { resource_sdu };
    let part_count = encrypted.div_ceil(sdu);
    let parts = encrypted + part_count * PART_BOOKKEEPING_BYTES;
    response + wrapped + plaintext + encrypted + parts
}

/// The largest SINGLE allocation the serve path asks for at `cap_bytes`
/// — [`serve_peak_bytes`] prices bytes, this prices the biggest block.
///
/// A bump allocator hands out blocks, not bytes: a heap with 30 KB free
/// in 4 KB pieces funds none of the copies below, and the census line
/// says so directly (`largest=` beside `free=`,
/// `leviculum-nrf/src/heap_census.rs`; the field failure it was built
/// for refused 340 B with 4 760 B free).
///
/// Since B2 the streamed serve asks for exactly three shapes of block,
/// and the largest of them is the answer:
///
/// * **one stored body**, up to `cap_bytes` — the whole record
///   `read_body` hands back. On the field's 24 × 256 B mailbox this is
///   256 B; on a mailbox holding one message the size of the whole cap
///   it is the cap. It is the largest block of all above a few hundred
///   bytes of cap, which is why the streamed serve's block bound tracks
///   the cap rather than the response.
/// * **the read/encrypt scratch**, `resource_sdu`.
/// * **one part**, at most `resource_sdu` plus
///   [`PART_BOOKKEEPING_BYTES`].
pub const fn serve_largest_block_bytes(cap_bytes: usize, resource_sdu: usize) -> usize {
    let framed = cap_bytes + RESPONSE_FRAME_BYTES;
    let sdu = if resource_sdu == 0 { 1 } else { resource_sdu };
    let scratch = if sdu < framed { sdu } else { framed };
    let plaintext = RESOURCE_RANDOM_HASH_SIZE + framed;
    let transfer = 16 + (plaintext / 16 + 1) * 16 + 32;
    let part_payload = if transfer < sdu { transfer } else { sdu };
    let part = part_payload + PART_BOOKKEEPING_BYTES;
    let mut largest = cap_bytes;
    if scratch > largest {
        largest = scratch;
    }
    if part > largest {
        largest = part;
    }
    largest
}

pub const fn serve_cap_for_peak(budget_bytes: usize, resource_sdu: usize) -> usize {
    serve_cap_for_heap(budget_bytes, usize::MAX, resource_sdu)
}

/// The largest serve cap a heap of `free_bytes` free, whose largest
/// single block is `largest_bytes`, funds: every term of
/// [`serve_peak_bytes`] summed fits `free_bytes`, AND the largest term
/// that is one allocation ([`serve_largest_block_bytes`]) fits
/// `largest_bytes`.
///
/// Two budgets because a heap has two: a fragmented one can have the
/// bytes and still refuse the block. Both conditions are monotone in
/// `cap_bytes`, so one search satisfies both.
///
/// [`serve_cap_for_peak`] is this with no block bound — the shape a boot
/// plan asks in, where the heap is not yet cut up by anything.
pub const fn serve_cap_for_heap(
    free_bytes: usize,
    largest_bytes: usize,
    resource_sdu: usize,
) -> usize {
    let mut low = 0usize;
    let mut high = free_bytes;
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if serve_peak_bytes(mid, resource_sdu) <= free_bytes
            && serve_largest_block_bytes(mid, resource_sdu) <= largest_bytes
        {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low
}

/// The cap one fetch may serve to, read at serve time from the heap the
/// node actually has: the larger of `boot_cap` — what the boot plan
/// funds, which is a worst case with every term at its maximum at once —
/// and what the live heap funds under the same model, less
/// `margin_bytes` of room for what can still arrive while the serve is
/// in flight.
///
/// The margin is not optional politeness: the serve is not atomic, and
/// the transients its caller already prices (an inbound sync batch, an
/// upload, one more endpoint link) can start between the moment the cap
/// is read and the moment the serve path holds all five of its copies
/// live. Sizing to the whole free heap hands that window a panic. The
/// caller owns the number, because only the caller knows which
/// transients its own configuration admits
/// (`heap_census::SERVE_MARGIN_BYTES`, `leviculum-nrf`).
///
/// The boot cap is a floor rather than a competitor: the plan reserved
/// that much for serving before anything else could claim it, so a live
/// reading below it means the reading is missing what the plan already
/// holds, not that the plan was wrong.
pub const fn serve_cap_for_live_heap(
    boot_cap: usize,
    free_bytes: usize,
    largest_bytes: usize,
    margin_bytes: usize,
    resource_sdu: usize,
) -> usize {
    let funded = serve_cap_for_heap(
        free_bytes.saturating_sub(margin_bytes),
        largest_bytes.saturating_sub(margin_bytes),
        resource_sdu,
    );
    if funded > boot_cap {
        funded
    } else {
        boot_cap
    }
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
    /// Heap-funded bound on ONE fetch response, in accounted bytes — the
    /// same accounting `sync_limit_kb` is applied in, and always the
    /// tighter of the two that bites.
    ///
    /// `None` on a host, where the announced sync limit is the only
    /// bound. A board sets it to what its heap plan funds
    /// ([`serve_cap_for_peak`] of its boot budget's slack,
    /// `heap_census::budget_serve_cap`, `leviculum-nrf`) and raises it
    /// per `/get` to what the live heap funds
    /// ([`serve_cap_for_live_heap`], through [`PropagationNode::set_serve_cap_bytes`]):
    /// serving a fetch
    /// costs several times the response it ships
    /// ([`serve_peak_bytes`]), and a T114 died in exactly that transient
    /// on 2026-09-23 while answering a 24-message fetch it had already
    /// read out of its store.
    ///
    /// It bounds serving, it does not refuse it: what does not fit this
    /// round stays in the store, is purged by nothing, and is listed
    /// again on the client's next sync round — which is how the
    /// reference client collects it (`message_get_response` ingests the
    /// short response, confirms only what it got, and the next
    /// `message_list_response` puts the rest back in `wants`,
    /// `reference/LXMF/LXMF/LXMRouter.py:1607-1644` and `:1576-1596`).
    pub serve_cap_bytes: Option<usize>,
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
            serve_cap_bytes: None,
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

/// What one fetch will serve, decided in full before a byte of it exists
/// (Codeberg #384 B2).
///
/// The role used to answer a fetch with the response bytes. It answers
/// with this instead: the ids it chose, each served body's length, and the
/// exact length of the msgpack the caller will ship. Everything the
/// protocol decides — who is served, in what order, what is purged — is
/// decided here, under the cap, against one directory snapshot; the bytes
/// are read from the store later, one record at a time, by
/// [`PropagationNode::fetch_source`].
///
/// **Why the split matters and is not cosmetic.** A store write that lands
/// between the plan and the stream cannot change what is served: the plan
/// is the answer, and a record that vanished under it fails the build
/// rather than shortening the response (a resource whose parts disagree
/// with its advertisement is worse than no resource). And the plan is the
/// bound: [`serve_peak_bytes`] prices a serve whose only whole copy is the
/// transfer, which is only true because no step between the plan and the
/// wire holds the response.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FetchPlan {
    /// The served messages in wire order: transient id and the length of
    /// the body as it ships, i.e. stamped length less [`STAMP_SIZE`].
    served: Vec<(TransientId, u32)>,
    /// Length of `msgpack [bin, ...]` over exactly those bodies.
    encoded_len: usize,
    served_bytes: u64,
}

impl FetchPlan {
    /// How many messages this fetch serves.
    pub fn count(&self) -> usize {
        self.served.len()
    }

    pub fn is_empty(&self) -> bool {
        self.served.is_empty()
    }

    /// The served ids, in wire order — what the caller reports and what a
    /// client confirms back as `haves`.
    pub fn served_ids(&self) -> Vec<TransientId> {
        self.served.iter().map(|(id, _)| *id).collect()
    }

    /// Servable bytes, stamps already stripped: the `bytes=` of the
    /// `PN_GET` line and the stats' served counter.
    pub fn served_bytes(&self) -> u64 {
        self.served_bytes
    }

    /// The exact length of the msgpack response, known before it is built
    /// — the number the caller compares against the link MDU to decide
    /// packet or resource
    /// (`response_fits_packet`, `leviculum-core/src/node/mod.rs`).
    pub fn encoded_len(&self) -> usize {
        self.encoded_len
    }
}

/// A `/get` answered.
#[derive(Debug, Clone, PartialEq)]
pub enum GetOutcome {
    /// The list form (`wants` and `haves` both absent,
    /// `reference/LXMF/LXMF/LXMRouter.py:1491-1504`). Short by
    /// construction — 32 B per id, bounded by the mailbox — so it stays
    /// bytes.
    List { response: Vec<u8>, count: usize },
    /// The fetch/acknowledge form: `purged` were deleted on the client's
    /// explicit confirmation, `plan` says what goes out with stamps
    /// stripped.
    Fetch {
        plan: FetchPlan,
        purged: Vec<TransientId>,
    },
}

/// The fetch response as a byte stream read out of the store
/// (Codeberg #384 B2).
///
/// It is the msgpack `[bin, …]` of [`MessageGetResponse::Messages`], byte
/// for byte — `a_streamed_fetch_is_the_encoded_fetch` holds the two
/// against each other — produced without the array ever existing: the
/// array header, then per planned message its `bin` header and its body,
/// read from the store when the part cutter asks for it and dropped when
/// the next one is asked for.
///
/// One record is live at a time. That is the whole of what the streamed
/// serve costs beyond the transfer itself, and it is term 1 of
/// [`serve_peak_bytes`].
pub struct FetchSource<'a, S> {
    store: &'a S,
    plan: &'a FetchPlan,
    /// The `[` of the array: `msgpack::array_header_len` bytes.
    array_header: Vec<u8>,
    array_pos: usize,
    index: usize,
    /// `bin` header of the record being streamed.
    record_header: Vec<u8>,
    record_pos: usize,
    /// The record being streamed, stamp already stripped. The one body
    /// copy a streamed serve holds.
    body: Option<Vec<u8>>,
    body_pos: usize,
}

impl<'a, S: PropagationStore> FetchSource<'a, S> {
    fn new(store: &'a S, plan: &'a FetchPlan) -> Self {
        let mut array_header = Vec::new();
        msgpack::array(&mut array_header, plan.served.len());
        Self {
            store,
            plan,
            array_header,
            array_pos: 0,
            index: 0,
            record_header: Vec::new(),
            record_pos: 0,
            body: None,
            body_pos: 0,
        }
    }

    /// Pull the record at `index` out of the store and frame it.
    ///
    /// A record that is gone, or whose length is not the one the plan
    /// measured, fails the whole build: the advertisement is computed from
    /// the plan's total, so a short record would ship parts that do not
    /// hash to what was advertised, and the receiver only finds out after
    /// paying for every one of them.
    fn load(&mut self) -> Result<(), SourceError> {
        let (transient_id, servable) = self.plan.served[self.index];
        let mut body = self
            .store
            .read_body(&transient_id)
            .map_err(|_| SourceError::Unavailable)?
            .ok_or(SourceError::Unavailable)?;
        if body.len() != servable as usize + STAMP_SIZE {
            return Err(SourceError::LengthChanged);
        }
        // Strip the propagation stamp in place (`:1549` strips
        // `STAMP_SIZE` from the tail) — a truncation, not a copy.
        body.truncate(servable as usize);
        self.record_header.clear();
        msgpack::bin_header(&mut self.record_header, body.len());
        self.record_pos = 0;
        self.body_pos = 0;
        self.body = Some(body);
        Ok(())
    }
}

impl<S: PropagationStore> ResourceSource for FetchSource<'_, S> {
    fn total_len(&self) -> usize {
        self.plan.encoded_len
    }

    fn rewind(&mut self) -> Result<(), SourceError> {
        self.array_pos = 0;
        self.index = 0;
        self.record_header.clear();
        self.record_pos = 0;
        self.body = None;
        self.body_pos = 0;
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, SourceError> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.array_pos < self.array_header.len() {
                let take = core::cmp::min(buf.len(), self.array_header.len() - self.array_pos);
                buf[..take]
                    .copy_from_slice(&self.array_header[self.array_pos..self.array_pos + take]);
                self.array_pos += take;
                return Ok(take);
            }
            if self.index >= self.plan.served.len() {
                return Ok(0);
            }
            if self.body.is_none() {
                self.load()?;
            }
            if self.record_pos < self.record_header.len() {
                let take = core::cmp::min(buf.len(), self.record_header.len() - self.record_pos);
                buf[..take]
                    .copy_from_slice(&self.record_header[self.record_pos..self.record_pos + take]);
                self.record_pos += take;
                return Ok(take);
            }
            let finished = match &self.body {
                Some(body) if self.body_pos < body.len() => {
                    let take = core::cmp::min(buf.len(), body.len() - self.body_pos);
                    buf[..take].copy_from_slice(&body[self.body_pos..self.body_pos + take]);
                    self.body_pos += take;
                    return Ok(take);
                }
                _ => true,
            };
            if finished {
                self.index += 1;
                self.body = None;
                self.body_pos = 0;
                self.record_pos = 0;
                self.record_header.clear();
            }
        }
    }
}

/// The propagation-node role over one [`PropagationStore`]./// The propagation-node role over one [`PropagationStore`].
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

    /// Re-bound what one fetch may serve
    /// ([`PropagationNodeConfig::serve_cap_bytes`]) after construction.
    ///
    /// The funded cap is not a constant of the build: it is a property of
    /// the heap the node is running on, and a node that learns its
    /// budget later (or whose free heap moves) sets it here rather than
    /// rebuilding the role. Changing it loses nothing — the bound
    /// decides what this round serves, never what the store keeps.
    pub fn set_serve_cap_bytes(&mut self, cap: Option<usize>) {
        self.config.serve_cap_bytes = cap;
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

        // One directory pass for the whole request: ids AND stored
        // lengths, so nothing below this line has to read a body to
        // decide anything. Until B2 the serve read every candidate body
        // just to measure it (#384).
        let mut owned: alloc::collections::BTreeMap<TransientId, u32> =
            alloc::collections::BTreeMap::new();
        self.store.for_each(&mut |meta: &StoredMessage| {
            if &meta.destination_hash == remote_delivery_hash {
                owned.insert(meta.transient_id, meta.size);
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
                if owned.contains_key(transient_id)
                    && self.store.purge(transient_id).unwrap_or(false)
                {
                    purged.push(*transient_id);
                    owned.remove(transient_id);
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
        // A third cap, tighter than both when a node's heap plan funds
        // less than it announces (`serve_cap_bytes`): serving a response
        // costs more than its length ([`serve_peak_bytes`]), and a board
        // that answers past what that plan funds dies in the transient
        // instead of answering. It bounds the same accounted sum the other
        // two bound, so the three compose by `min` and the dominance
        // argument at [`serve_peak_bytes`] carries over unchanged.
        let our_limit = match self.config.serve_cap_bytes {
            Some(funded) => our_limit.min(funded as f64),
            None => our_limit,
        };
        let limit = client_limit.map_or(our_limit, |client| client.min(our_limit));

        // Overheads exactly as the reference budgets them (:1532-1533).
        let per_message_overhead = 16.0;
        let mut cumulative_size = 24.0;

        let mut served: Vec<(TransientId, u32)> = Vec::new();
        let mut served_bytes = 0u64;
        let mut payload_bytes = 0usize;
        if let Some(wants) = &request.wants {
            for transient_id in wants {
                let Some(&stored_size) = owned.get(transient_id) else {
                    // Gone or never ours to give: skipped silently, the
                    // response is simply shorter (:1535 membership test; the
                    // concept paper's §1 records that absence has no error).
                    continue;
                };
                let stored_size = stored_size as usize;
                if stored_size < STAMP_SIZE {
                    // Not a body this role wrote; nothing servable in it.
                    continue;
                }
                let next_size = cumulative_size + stored_size as f64 + per_message_overhead;
                if next_size > limit {
                    // Too big for this round; the reference keeps scanning
                    // rather than stopping (:1547 `pass`).
                    continue;
                }
                cumulative_size = next_size;
                // Serve the message without its propagation stamp
                // (:1549 strips `STAMP_SIZE` from the tail).
                let servable = stored_size - STAMP_SIZE;
                served_bytes += servable as u64;
                payload_bytes += msgpack::bin_header_len(servable) + servable;
                served.push((*transient_id, servable as u32));
                // Serving the same id twice would ship the body twice and
                // break the plan's one-pass arithmetic; a `wants` list may
                // legally repeat one.
                owned.remove(transient_id);
            }
        }

        // Keep the duplicate guard alive for what was just confirmed
        // drained, so a client retry of the original upload is not
        // re-stored.
        for transient_id in &purged {
            self.processed.insert(*transient_id, now_secs);
        }

        let encoded_len = msgpack::array_header_len(served.len()) + payload_bytes;
        Ok(GetOutcome::Fetch {
            plan: FetchPlan {
                served,
                encoded_len,
                served_bytes,
            },
            purged,
        })
    }

    /// The bytes [`FetchPlan`] describes, materialised.
    ///
    /// The single-packet path needs them — a response under the link MDU
    /// is a few hundred bytes and one `Vec` is cheaper than a resource —
    /// and so does every test that wants to read what was served. The
    /// streamed path
    /// ([`fetch_source`](Self::fetch_source)) never calls it; that is the
    /// whole point of #384 B2.
    pub fn encode_fetch(&self, plan: &FetchPlan) -> Result<Vec<u8>, GetError> {
        let mut bodies = Vec::with_capacity(plan.served.len());
        for (transient_id, servable) in &plan.served {
            let mut body = self.store.read_body(transient_id)?.unwrap_or_default();
            if body.len() != *servable as usize + STAMP_SIZE {
                return Err(GetError::Store(StorageError::NotFound));
            }
            body.truncate(*servable as usize);
            bodies.push(body);
        }
        Ok(MessageGetResponse::Messages(bodies).encode()?)
    }

    /// The bytes [`FetchPlan`] describes, as a stream read out of the
    /// store one record at a time — what a board hands to
    /// `send_response_resource_from_source`
    /// (`leviculum-core/src/node/mod.rs`).
    pub fn fetch_source<'a>(&'a self, plan: &'a FetchPlan) -> FetchSource<'a, S> {
        FetchSource::new(&self.store, plan)
    }

    /// Periodic maintenance: purge expired messages    /// Periodic maintenance: purge expired messages
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
