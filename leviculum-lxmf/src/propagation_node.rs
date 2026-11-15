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
use core::ops::ControlFlow;

use leviculum_core::constants::RESOURCE_HASHMAP_LEN;
use leviculum_core::resource::{ResourceSource, SourceError, RESOURCE_RANDOM_HASH_SIZE};
use leviculum_core::{Destination, Identity};

use crate::{
    constants::{DESTINATION_LENGTH, STAMP_SIZE},
    lev_status::{self, DepartureReason, MessageStatus},
    msgpack,
    node::APP_NAME,
    propagation::{
        MessageGetRequest, MessageGetResponse, PeerError, PropagationError,
        PropagationNodeAnnounce, PropagationSignal, PropagationUpload, TransferLimit, TransientId,
    },
    propagation_client::PROPAGATION_ASPECT,
    propagation_store::{
        ConfirmedPurge, PropagationBodyStore, PropagationStore, StoredMessage, MIN_BODY_LEN,
    },
    storage::StorageError,
};

/// Announce metadata key carrying the node's display name
/// (`PN_META_NAME`, `reference/LXMF/LXMF/LXMF.py:133`).
pub const PN_META_NAME: u64 = 0x01;

/// The stamp value a stock peer demands of every message we offer it:
/// its `PROPAGATION_COST` 16 minus its `PROPAGATION_COST_FLEX` 3
/// (`reference/LXMF/LXMF/LXMRouter.py:53-54`), applied by the offering
/// side (`min_accepted_cost`, `reference/LXMF/LXMF/LXMPeer.py:331`, the
/// drop at `:340-353`). A message stored below it never leaves this node
/// toward such a peer; [`PropagationNode::accepts_below_stock_peers`]
/// says when a configuration admits one.
pub const STOCK_PEER_MIN_STAMP_VALUE: u8 = 13;

/// Messages expire after 30 days
/// (`MESSAGE_EXPIRY`, `reference/LXMF/LXMF/LXMRouter.py:38`).
pub const MESSAGE_EXPIRY_SECS: u64 = 30 * 24 * 60 * 60;

/// How long a processed transient ID is remembered for duplicate detection:
/// six times the message expiry, as the reference prunes its own cache
/// (`clean_transient_id_caches`, `reference/LXMF/LXMF/LXMRouter.py:1011`).
pub const PROCESSED_ID_EXPIRY_SECS: u64 = 6 * MESSAGE_EXPIRY_SECS;

/// How long past [`PROCESSED_ID_EXPIRY_SECS`] a processed ID may stay: the
/// width of the cache's buckets, a week, which are dropped whole instead of
/// walked ([`crate::processed_ids`]). The cache holds every ID accepted in
/// up to 180 days, about 56 000 a day on the field node (leviculum#478); a
/// week longer only widens the duplicate window by a week.
pub const PROCESSED_ID_PRUNE_SLACK_SECS: u64 = crate::processed_ids::PROCESSED_BUCKET_SECS;

/// The duplicate list's count bound (order 1558, LV-1000): the processed-id
/// cache holds at most this many ids per message the store holds, and the
/// departures half as many ([`dup_caps`]).
///
/// **Deviation:** the reference bounds its duplicate set by time alone,
/// six message expiries, 180 days (`clean_transient_id_caches`,
/// `reference/LXMF/LXMF/LXMRouter.py:1018-1024`), and has no count bound:
/// at the growth soak's 2148 accepts a minute that is about 50 MB of ids a
/// day for 180 days, the field node's OOM candidate. An id is a duplicate
/// for us while the store holds its message ([`PropagationStore::contains`])
/// and, beyond that, only while a peer may still offer it back: a peer
/// offers a message once, in its next round, so the window that matters is
/// one sync round of every peer, not 180 days. Two ids per stored message
/// remember about one store's worth of departed ids on top of the stored
/// ones: on a full store that turns over in hours at the soak's rate and in
/// days at the field's, both far past any peer's round. Wire is unchanged;
/// semantics hold: an id forgotten that late and offered again is fetched
/// and stored once more, which the recipient's own duplicate check absorbs
/// (`locally_delivered_transient_ids`, `LXMRouter.py:1668`), as the
/// reference itself does past 180 days.
pub const DUP_IDS_PER_STORED: usize = 2;

/// The duplicate list's bound on a near-empty store: so many ids are kept
/// whatever the store holds, about a day of the field node's accepts, a
/// megabyte of processed ids.
pub const DUP_IDS_FLOOR: usize = 65_536;

/// The count bounds for a store holding `stored` messages: the processed
/// ids, then the departures ([`DUP_IDS_PER_STORED`]).
pub const fn dup_caps(stored: usize) -> (usize, usize) {
    let stored = if stored > DUP_IDS_FLOOR / DUP_IDS_PER_STORED {
        stored
    } else {
        DUP_IDS_FLOOR / DUP_IDS_PER_STORED
    };
    (
        stored.saturating_mul(DUP_IDS_PER_STORED),
        stored.saturating_mul(DUP_IDS_PER_STORED) / 2,
    )
}

/// How long one eviction walk's candidates serve later full appends: the
/// reference weighs its store once per store job, `JOB_STORE_INTERVAL` 120
/// times `PROCESSING_INTERVAL` 4 s (`reference/LXMF/LXMF/LXMRouter.py:31,876,899`),
/// so a choice this old is no staler than the reference's own.
pub const EVICTION_CANDIDATES_SECS: u64 = 480;

/// One eviction walk chooses candidates worth this fraction of the
/// capacity beyond what the append needs: 2 MB, about 6 000 messages, on a
/// 2 GB store, so a full store walks once per that many accepts instead of
/// on every one (order 1370: 789 ms an upload at 6 M messages on the coder
/// host, 15 of 20 uploads walking).
pub const EVICTION_CANDIDATE_FRACTION: u64 = 1024;

/// How many expired messages [`PropagationNode::tick_at_most`]'s caller in
/// a host hook takes per call: purging one and logging it costs a few
/// microseconds, so a backlog of tens of thousands (a node back from a long
/// stop) would hold the core lock for a whole sweep, 113 ms for 74 645 on
/// the coder host (order 1295's bench). A caller that gets this many back
/// comes again soon.
pub const MAX_EXPIRED_PER_TICK: usize = 128;

/// Bytes the request/response envelope adds to a response body on its way
/// to the wire: msgpack `fixarray(2)` (one byte) plus the 16-byte request
/// id as msgpack `bin8` (two header bytes plus its payload). Both the
/// single-packet path and the Resource path pack exactly this frame, and
/// since #384 both compute its length before building it rather than
/// after (`bin_len`, `leviculum-core/src/node/mod.rs:1958`).
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
/// 1. **body-read storage, ≤ R** — a synchronous store's default
///    [`PropagationBodyStore::read_body_at`] may hold one whole record,
///    while a card caller keeps the planned bodies it fetched. The
///    accounted cap bounds either by R; mapped stores read directly into
///    the part scratch and spend none of this conservative term.
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

/// Configuration of the role. The size defaults are lxmd's, 256 KB and 40
/// times that ([`crate::constants::PROPAGATION_LIMIT_KB`], [`crate::constants::SYNC_LIMIT_KB`]),
/// so lnpnd without a config and the interop helper's bare `pn_enable`
/// announce what stock lxmd does (order 1664; the board's 4/32 until then).
/// A board never runs them: `board_role_config` (`leviculum-nrf/pn-store`)
/// sets the concept paper's §2 limits ("What we announce",
/// `docs/src/concepts/propagation-node-on-a-board.md`) from `heap-budget`.
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
    /// Default 13 (Lead, 2026-09-12), the reference's own floor
    /// (`PROPAGATION_COST_MIN`, `reference/LXMF/LXMF/LXMRouter.py:52`,
    /// applied at `:137`): a stock peer offers only messages whose stored
    /// stamp value clears its cost minus flexibility
    /// (`reference/LXMF/LXMF/LXMPeer.py:331`, `:340`), so a store accepted
    /// at 13 travels through every default peer. User-settable; announcing
    /// below 13 is wire-legal and honoured.
    pub stamp_cost: u8,
    /// Announce field 5\[1\]: how far below our cost a stamp may fall and
    /// still be accepted (`PROPAGATION_COST_FLEX`,
    /// `reference/LXMF/LXMF/LXMRouter.py:53`).
    ///
    /// Default 0, where the reference's is 3: the reference pairs its 3
    /// with cost 16 (`PROPAGATION_COST`, `:54`), so it accepts 13 and
    /// up, and 13 is what every stock peer demands of what we offer it
    /// (`min_accepted_cost = max(0, cost - flexibility)`,
    /// `reference/LXMF/LXMF/LXMPeer.py:331`, filtered at `:340-353`).
    /// At our cost 13 a flexibility of 3 accepted 10 to 12, which no stock
    /// peer takes, so such a message stayed on this node for 30 days and
    /// reached nobody beyond it (leviculum#478, order 1294). Our clients
    /// mine to the announced cost, so 0 refuses none of theirs.
    pub stamp_cost_flexibility: u8,
    /// Announce field 5\[2\]: the peering cost another node mines to `/offer`
    /// us batches. Default 1 (Lead, 2026-09-12), not 0: 0 is wire-legal,
    /// but a stock peer's `peering_key_ready` returns false on a falsy cost
    /// (`reference/LXMF/LXMF/LXMPeer.py:228`), so it would never sync
    /// toward us. One bit of work is the cheapest cost it acts on.
    pub peering_cost: u8,
    /// Announce metadata: the node's display name, UTF-8.
    pub name: Option<Vec<u8>>,
    /// Message expiry. [`MESSAGE_EXPIRY_SECS`] unless a test shortens it.
    pub message_expiry_secs: u64,
    /// Heap-funded bound on ONE fetch response, in accounted bytes — the
    /// same accounting the client's limit and [`Self::serve_limit_bytes`]
    /// are applied in, and the tightest of them is the one that bites.
    ///
    /// `None` on a host, where the client's limit is the only bound. A
    /// board sets it to what its heap plan funds
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
    /// The bound one fetch response is held to, in accounted bytes, on top
    /// of the client's own limit; `None` (the host) adds none.
    ///
    /// A board announces a sync limit derived from what it can afford to
    /// SEND a peer in one round (`BOARD_SYNC_LIMIT_KB`,
    /// `leviculum-nrf/heap-budget`, leviculum#388 order 511), which is
    /// smaller than what it can afford to serve a client in one fetch;
    /// the serve is bounded by its own heap arithmetic
    /// ([`Self::serve_cap_bytes`]) and by this. Wire-neutral: the
    /// reference bounds a fetch by the client's limit alone, and a shorter
    /// response is the protocol's normal shape either way.
    pub serve_limit_bytes: Option<usize>,
    /// The node peers from its static peers only. Announce field 2 then
    /// says false, as the reference's does (`node_state = propagation_node
    /// and not from_static_only`, `reference/LXMF/LXMF/LXMRouter.py:326`):
    /// strangers neither autopeer nor select a closed node, and its
    /// existing autopeers unpeer it (`reference/LXMF/LXMF/Handlers.py:98-99`).
    /// The host that enforces the policy at the door owns the static list
    /// and sets this from the same knob.
    pub from_static_only: bool,
    /// Compute and store every accepted message's true stamp value, even
    /// at our own cost 0 and before any peer demands one, as the reference
    /// does (`validate_pn_stamp` expands the workblock at target cost 0
    /// too, `reference/LXMF/LXMF/LXStamper.py:95`). True on a host
    /// (leviculum#442 D6, Lew 2026-10-09): stored as 0, what the node took
    /// before its first demanding peer is skipped by every stock peer
    /// forever (`reference/LXMF/LXMF/LXMPeer.py:340`). A board sets false
    /// and computes only while a peer demands it
    /// ([`PropagationNode::set_compute_stamp_value`]), saving the
    /// workblock expansion per message.
    pub always_compute_stamp_value: bool,
    /// Destinations whose messages the cull keeps longest: their weight is
    /// a tenth of another message's of the same age and size
    /// (`priority_weight = 0.1`, `get_weight`,
    /// `reference/LXMF/LXMF/LXMRouter.py:1064`), lxmd's
    /// `prioritise_destinations` (`reference/LXMF/LXMF/Utilities/lxmd.py:214-217`,
    /// applied at `:446-450`). Empty by default and on a board.
    pub prioritised_destinations: Vec<[u8; DESTINATION_LENGTH]>,
}

impl Default for PropagationNodeConfig {
    fn default() -> Self {
        Self {
            transfer_limit_kb: crate::constants::PROPAGATION_LIMIT_KB,
            sync_limit_kb: crate::constants::SYNC_LIMIT_KB,
            // The Lead's decision of 2026-09-12, one source with the
            // board's persisted defaults: 13 is the reference's own
            // announce floor, and peering cost 1 rather than 0 because a
            // stock peer's `peering_key_ready` returns false on a falsy
            // cost (`reference/LXMF/LXMF/LXMPeer.py:228`), so it would
            // never sync toward us.
            stamp_cost: leviculum_core::pn_config_store::DEFAULT_STAMP_COST,
            // Cost 13 minus 0 accepts exactly what a stock peer will take
            // from us (`STOCK_PEER_MIN_STAMP_VALUE`).
            stamp_cost_flexibility: 0,
            peering_cost: leviculum_core::pn_config_store::DEFAULT_PEERING_COST,
            name: None,
            message_expiry_secs: MESSAGE_EXPIRY_SECS,
            serve_cap_bytes: None,
            serve_limit_bytes: None,
            from_static_only: false,
            always_compute_stamp_value: true,
            prioritised_destinations: Vec::new(),
        }
    }
}

/// The duplicate list's counts against their bounds
/// ([`PropagationNode::dup_counts`], [`dup_caps`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DupCounts {
    pub processed: usize,
    pub processed_cap: usize,
    pub departed: usize,
    /// 0 on a node that keeps no departures.
    pub departed_cap: usize,
}

/// When and why one message left the store ([`PropagationNode::departures`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Departure {
    pub at_secs: u64,
    pub reason: DepartureReason,
}

/// The departures a host asked the role to keep
/// ([`PropagationNode::track_departures`]).
struct Departures {
    left: alloc::collections::BTreeMap<TransientId, Departure>,
    /// Departures since the host last took them
    /// ([`PropagationNode::take_fresh_departures`]), for its log line.
    fresh: Vec<(TransientId, DepartureReason)>,
    /// The host answers `/lev/status`, so the announce says so
    /// ([`PropagationNode::serve_lev_status`]).
    serves_status: bool,
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

impl UploadOutcome {
    /// Whether this message counts against the peer that synced it: a
    /// stamp that does not hold up, an undecodable body, or a nested
    /// multi-message transfer. Any such message in an inbound sync batch
    /// throttles the sender once the batch drains
    /// (`reference/LXMF/LXMF/LXMRouter.py:2440-2450`). A store failure is
    /// ours, not the peer's, and does not count. The board's sync drain
    /// (`leviculum-nrf/src/pn.rs`, `perform_sync_step`) calls this; no rig
    /// cell syncs a bad stamp to a board (leviculum#413).
    pub fn counts_against_sender(&self) -> bool {
        match self {
            Self::InvalidStamp { .. } | Self::Malformed(_) | Self::PeerSyncForm => true,
            Self::Accepted { .. } | Self::StoreFailed(_) => false,
        }
    }
}

/// The mailbox a `/get` on a link drains: the `lxmf.delivery` destination
/// of the identity the link was identified with
/// (`message_get_request`, `reference/LXMF/LXMF/LXMRouter.py:1487`). A link
/// that never identified gets [`PeerError::NoIdentity`] (`:1483`), which
/// the caller answers as a [`crate::propagation::MessageListResponse`] error.
/// The board's engine (`leviculum-nrf/src/pn.rs`, the `Work::Get` arm)
/// calls this; no rig cell sends `/get` over an unidentified link (leviculum#413).
pub fn get_mailbox(remote: Option<&Identity>) -> Result<[u8; DESTINATION_LENGTH], PeerError> {
    let identity = remote.ok_or(PeerError::NoIdentity)?;
    let name_hash = Destination::compute_name_hash(APP_NAME, &["delivery"]);
    Ok(*Destination::compute_destination_hash(&name_hash, identity.hash()).as_bytes())
}

/// The `lxmf.propagation` destination hash of `identity`: the hash a peer
/// node is known by in the peer table.
pub fn propagation_hash_of(identity: &Identity) -> [u8; DESTINATION_LENGTH] {
    let name_hash = Destination::compute_name_hash(APP_NAME, &[PROPAGATION_ASPECT]);
    *Destination::compute_destination_hash(&name_hash, identity.hash()).as_bytes()
}

/// Whether an inbound propagation link that has carried no payload for
/// `no_data_for_ms` is torn down: strictly above
/// [`PROPAGATION_LINK_MAX_INACTIVITY_SECS`](crate::constants::PROPAGATION_LINK_MAX_INACTIVITY_SECS)
/// whole seconds, as `clean_links` does
/// (`reference/LXMF/LXMF/LXMRouter.py:968-974`). The input is the core's
/// `Link::no_data_for_ms`, which keepalives do not reset, so a client that
/// walked away without closing is let go even while its stack answers
/// keepalives; `None` (a link not yet established) is never idle.
///
/// One rule for both stacks: `lnpnd`'s `close_idle_links` and the board's
/// idle sweep (`leviculum-nrf/src/pn.rs`, audit 2026-10-12 D15) both ask
/// here, so a board no longer holds a silent link for the core's ~725 s
/// stale rule while `lnpnd` lets it go after 180.
pub fn link_idle_past_bound(no_data_for_ms: Option<u64>) -> bool {
    no_data_for_ms
        .is_some_and(|ms| ms / 1000 > crate::constants::PROPAGATION_LINK_MAX_INACTIVITY_SECS)
}

/// Who sent an inbound `/offer`, as the gates and the peering-key check
/// need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfferRemote {
    /// The identity hash, the second half of the peering-key material.
    pub identity_hash: [u8; DESTINATION_LENGTH],
    /// Its propagation destination, the peer-table key.
    pub propagation_hash: [u8; DESTINATION_LENGTH],
}

/// The remote of an inbound `/offer`. A link that never identified gets
/// [`PeerError::NoIdentity`] before any gate runs (`offer_request`,
/// `reference/LXMF/LXMF/LXMRouter.py:2267`), which the caller answers as an
/// `OfferResponse` error. The board's engine (`leviculum-nrf/src/pn.rs`,
/// `answer_offer_work`) calls this; no rig cell offers over an
/// unidentified link (leviculum#413).
pub fn offer_remote(remote: Option<&Identity>) -> Result<OfferRemote, PeerError> {
    let identity = remote.ok_or(PeerError::NoIdentity)?;
    Ok(OfferRemote {
        identity_hash: *identity.hash(),
        propagation_hash: propagation_hash_of(identity),
    })
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
/// are read later by [`PropagationNode::fetch_source`], or by an
/// asynchronous card owner before [`PropagationNode::finish_get`].
///
/// **Why the split matters and is not cosmetic.** A write between planning
/// and streaming cannot enlarge the funded answer or reorder its records.
/// A card owner may omit unavailable records before finalizing the plan;
/// once a stream starts, a disappeared or resized body fails the build
/// rather than changing its advertised length. Neither path assembles a
/// whole response, and [`serve_peak_bytes`] funds the body's read storage,
/// the part scratch and the encrypted transfer together.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FetchPlan {
    /// The served messages in wire order: transient id and the length of
    /// the body as it ships, i.e. stamped length less [`STAMP_SIZE`].
    served: Vec<(TransientId, u32)>,
    /// Length of `msgpack [bin, ...]` over exactly those bodies.
    encoded_len: usize,
    served_bytes: u64,
    /// Wanted bodies of the caller's mailbox that the heap cap
    /// (`serve_cap_bytes`) alone kept out of this round: the client's
    /// limit and the node's serve limit would have admitted them.
    heap_withheld: usize,
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

    /// Messages in wire order, with their unstamped lengths. A card caller
    /// reads only these records, never candidates excluded by the serve cap.
    pub fn messages(&self) -> impl Iterator<Item = (&TransientId, u32)> {
        self.served.iter().map(|(id, len)| (id, *len))
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

    /// How many wanted messages of the caller's mailbox this round left
    /// on the node because the heap cap was tighter than every other
    /// bound: the bodies a host whose free heap had been larger would
    /// have served. Zero when no heap cap is set or it did not bind.
    ///
    /// The messages stay in the store; a stock client keeps them listed
    /// and asks again on its next sync
    /// (`message_get_response`, `reference/LXMF/LXMF/LXMRouter.py:1620-1644`
    /// confirms only what it received).
    pub fn heap_withheld(&self) -> usize {
        self.heap_withheld
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

/// The fetch response as a byte stream read out of a synchronous body
/// source (Codeberg #384 B2), including a card caller's prefetched bodies.
///
/// The stream is byte-for-byte [`MessageGetResponse::Messages`]: an array
/// header followed by each planned body's bin header and unstamped bytes.
/// Headers live on the stack, and [`PropagationBodyStore::read_body_at`]
/// copies straight into the resource builder's scratch. No response buffer
/// or body copy belongs to this source.
pub struct FetchSource<'a, S: ?Sized> {
    store: &'a S,
    plan: &'a FetchPlan,
    array_header: [u8; 5],
    array_len: usize,
    array_pos: usize,
    index: usize,
    record_header: [u8; 5],
    record_len: usize,
    record_pos: usize,
    body_pos: usize,
}

impl<'a, S: PropagationBodyStore + ?Sized> FetchSource<'a, S> {
    /// Stream `plan` from `store`. For an asynchronously read card, `store`
    /// can be a borrowed `[(TransientId, Vec<u8>)]` returned by its owner.
    pub fn new(store: &'a S, plan: &'a FetchPlan) -> Self {
        let (array_header, array_len) = msgpack::array_header_bytes(plan.count());
        Self {
            store,
            plan,
            array_header,
            array_len,
            array_pos: 0,
            index: 0,
            record_header: [0; 5],
            record_len: 0,
            record_pos: 0,
            body_pos: 0,
        }
    }

    /// Validate the stored length before emitting this body's framing.
    /// Reading the expected stamp and one following byte distinguishes
    /// exact length from a shorter or longer record without allocating.
    fn load(&mut self) -> Result<(), SourceError> {
        let (transient_id, servable) = self.plan.served[self.index];
        let mut tail = [0u8; STAMP_SIZE + 1];
        let copied = self
            .store
            .read_body_at(&transient_id, servable as usize, &mut tail)
            .map_err(|_| SourceError::Unavailable)?
            .ok_or(SourceError::Unavailable)?;
        if copied != STAMP_SIZE {
            return Err(SourceError::LengthChanged);
        }
        let (header, len) = msgpack::bin_header_bytes(servable as usize);
        self.record_header = header;
        self.record_len = len;
        self.record_pos = 0;
        self.body_pos = 0;
        Ok(())
    }
}

impl<S: PropagationBodyStore + ?Sized> ResourceSource for FetchSource<'_, S> {
    fn total_len(&self) -> usize {
        self.plan.encoded_len
    }

    fn rewind(&mut self) -> Result<(), SourceError> {
        self.array_pos = 0;
        self.index = 0;
        self.record_len = 0;
        self.record_pos = 0;
        self.body_pos = 0;
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, SourceError> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.array_pos < self.array_len {
                let take = core::cmp::min(buf.len(), self.array_len - self.array_pos);
                buf[..take]
                    .copy_from_slice(&self.array_header[self.array_pos..self.array_pos + take]);
                self.array_pos += take;
                return Ok(take);
            }
            if self.index >= self.plan.served.len() {
                return Ok(0);
            }
            if self.record_len == 0 {
                self.load()?;
            }
            if self.record_pos < self.record_len {
                let take = core::cmp::min(buf.len(), self.record_len - self.record_pos);
                buf[..take]
                    .copy_from_slice(&self.record_header[self.record_pos..self.record_pos + take]);
                self.record_pos += take;
                return Ok(take);
            }
            let (transient_id, servable) = self.plan.served[self.index];
            if self.body_pos < servable as usize {
                let want = core::cmp::min(buf.len(), servable as usize - self.body_pos);
                let copied = self
                    .store
                    .read_body_at(&transient_id, self.body_pos, &mut buf[..want])
                    .map_err(|_| SourceError::Unavailable)?
                    .ok_or(SourceError::Unavailable)?;
                if copied == 0 || copied > want {
                    return Err(SourceError::LengthChanged);
                }
                self.body_pos += copied;
                return Ok(copied);
            }
            self.index += 1;
            self.record_len = 0;
            self.record_pos = 0;
            self.body_pos = 0;
        }
    }
}

/// A metadata-only `/get` answer, or the bounded work needed to serve it
/// from a medium whose bodies require asynchronous reads.
#[derive(Debug, Clone, PartialEq)]
pub enum PreparedGet {
    /// The list form is complete without reading bodies.
    Ready(GetOutcome),
    /// Flush the purges, read the planned bodies, then call
    /// [`PropagationNode::finish_get`].
    Fetch(PreparedFetch),
}

/// One `/get` directory snapshot, after its confirmations were purged.
///
/// The plan has already applied the client limit, board serve limit and
/// live-heap cap. A card caller reads only [`FetchPlan::messages`], and
/// retains those bodies for [`FetchSource`] after [`PropagationNode::finish_get`]
/// removes records whose bytes were unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFetch {
    pub plan: FetchPlan,
    /// The confirmed IDs purged before this work was returned. A deferred
    /// store must make these tombstones durable before sending the answer.
    pub purged: Vec<TransientId>,
}

/// The structural overhead the reference counts against the transfer limit
/// before the first message: a constant 24 bytes
/// (`reference/LXMF/LXMF/LXMRouter.py:1532-1533`).
const RESPONSE_BASE_OVERHEAD: f64 = 24.0;
/// The same arithmetic's per-message cost: 16 bytes each (`:1532-1533`).
const RESPONSE_PER_MESSAGE_OVERHEAD: f64 = 16.0;

/// The reference's running transfer-limit arithmetic. It runs against the
/// directory snapshot before synchronous streaming or asynchronous card
/// prefetching, so neither caller can collect an unbounded response.
struct TransferBudget {
    cumulative_size: f64,
    limit_bytes: f64,
}

impl TransferBudget {
    fn new(limit_bytes: f64) -> Self {
        Self {
            cumulative_size: RESPONSE_BASE_OVERHEAD,
            limit_bytes,
        }
    }

    /// Whether a body of `lxm_size` bytes still fits, taking it into the
    /// running total only when it does. A body that does not fit is passed
    /// over while the scan continues, exactly as the reference passes over
    /// an oversized message (`:1547`).
    fn admit(&mut self, lxm_size: usize) -> bool {
        let next_size = self.cumulative_size + lxm_size as f64 + RESPONSE_PER_MESSAGE_OVERHEAD;
        if next_size > self.limit_bytes {
            false
        } else {
            self.cumulative_size = next_size;
            true
        }
    }

    /// Whether any body at all could still fit: the running total only
    /// grows, and no store can hand out a body shorter than
    /// [`MIN_BODY_LEN`], so once that floor no longer fits, no later
    /// candidate can be served either.
    fn can_admit_any(&self) -> bool {
        self.cumulative_size + MIN_BODY_LEN as f64 + RESPONSE_PER_MESSAGE_OVERHEAD
            <= self.limit_bytes
    }
}

/// The propagation-node role over a metadata store. Synchronous
/// [`PropagationBodyStore`] implementations also support
/// [`Self::encode_fetch`] and [`Self::fetch_source`]; a card caller uses
/// [`Self::prepare_get`] and [`Self::finish_get`] around its owner reads.
pub struct PropagationNode<S> {
    store: S,
    config: PropagationNodeConfig,
    /// Recently processed transient IDs and when, for duplicate detection
    /// beyond the store's own lifetime (a drained-and-purged message must
    /// not be re-accepted from a stale retry,
    /// `locally_processed_transient_ids`,
    /// `reference/LXMF/LXMF/LXMRouter.py:2496,2499`).
    ///
    /// **Deviation:** the reference persists this whole set across
    /// restarts; ours is RAM-only and starts empty. A host persists the part
    /// a restart cannot rebuild from the store's own index, the ids that
    /// left the store early ([`Self::track_departures`]); a message still
    /// in the store is a duplicate by [`PropagationStore::contains`] alone.
    /// The board keeps no per-message state outside the log — a rewritten
    /// metadata page is the 18.6-day endurance failure the concept paper's
    /// §2 forbids — and needs none: a purged record stays on flash until
    /// its page is reclaimed, and the store answers from it
    /// ([`PropagationStore::departed`]). Past that reclaim a re-upload of
    /// an already-drained message is accepted again, which the client's
    /// own `has_message` filter tolerates (`message_list_response`,
    /// `reference/LXMF/LXMF/LXMRouter.py:1581`).
    ///
    /// Week buckets of 16-byte id prefixes, exact for at least the
    /// reference's window ([`crate::processed_ids`]) and held to
    /// [`dup_caps`] of the store's count.
    processed: crate::processed_ids::ProcessedIds,
    /// The store's message count as last read, what the duplicate list's
    /// count bound is taken from ([`dup_caps`]); 0, the floor's bound,
    /// until the list is large enough for a bound to bind
    /// ([`Self::update_dup_caps`]).
    stored_for_caps: usize,
    /// Messages expired or displaced since the role was built, for a host's
    /// status line ([`Self::evicted_total`]).
    evicted_total: u64,
    /// The heaviest messages the last eviction walk found, heaviest first,
    /// and when it ran: what the next full append evicts from without a
    /// walk, for [`EVICTION_CANDIDATES_SECS`] ([`Self::make_room`]).
    eviction_candidates: Vec<Eviction>,
    eviction_walked_at: u64,
    /// Ids that left the store (a confirmed collection, an own-mailbox
    /// delivery, a displacement, an expiry), with when and why, for a host
    /// to persist and to answer `/lev/status` from: `None` unless
    /// [`Self::track_departures`] asked for it, so a board pays nothing.
    departed: Option<Departures>,
    /// Compute true stamp values even when our own accepted cost is 0.
    /// Set by the engine while any known peer requires a stamp value above
    /// 0 of the messages it takes (`docs/src/concepts/
    /// propagation-node-on-a-board.md` §5: the offering side drops ids
    /// whose stored value is below the peer's minimum,
    /// `reference/LXMF/LXMF/LXMPeer.py:340`, and the record tag is written
    /// once at accept, so the decision is per-message at accept time). A
    /// host computes them regardless
    /// ([`PropagationNodeConfig::always_compute_stamp_value`]).
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
        self.processed.heap_bytes()
            + self.departed.as_ref().map_or(0, |departed| {
                leviculum_core::heap_census::btree_map_bytes(&departed.left)
                    + departed.fresh.capacity()
                        * core::mem::size_of::<(TransientId, DepartureReason)>()
            })
            + self.config.name.as_ref().map_or(0, |name| name.capacity())
            + self.eviction_candidates.capacity() * core::mem::size_of::<Eviction>()
    }

    /// Build the role over `store`. The announced limits are held to what
    /// the store can ever keep (`store_fit_kb`, LV-1001): a configured limit
    /// above it is clamped, so the per-message transfer limit and the
    /// per-sync limit we announce, enforce at the resource advertisement
    /// and offer by never promise more than the store holds.
    pub fn new(mut store: S, mut config: PropagationNodeConfig) -> Self {
        let fit_kb = Self::store_fit_kb(&store);
        config.transfer_limit_kb = config.transfer_limit_kb.min(fit_kb);
        config.sync_limit_kb = config.sync_limit_kb.min(fit_kb);
        store.count_sizes_up_to(Self::largest_offered_size_under(&config));
        let mut node = Self {
            store,
            config,
            processed: crate::processed_ids::ProcessedIds::new(),
            stored_for_caps: 0,
            evicted_total: 0,
            eviction_candidates: Vec::new(),
            eviction_walked_at: 0,
            departed: None,
            compute_stamp_value: false,
            calendar_jump_floor: 0,
        };
        node.processed.set_cap(dup_caps(0).0);
        node
    }

    /// Hold the duplicate list to the bounds the store's count gives
    /// ([`dup_caps`]), on every [`Self::tick`]. The count is read only once
    /// either half holds half its current bound: below that no bound can
    /// bind, and a board's store counts by walking its region, which a tick
    /// at rest must not read (`leviculum-nrf/pn-store`). A host's store
    /// counts from its index.
    fn update_dup_caps(&mut self) {
        let (processed_cap, departed_cap) = dup_caps(self.stored_for_caps);
        let departed = self.departed.as_ref().map_or(0, |d| d.left.len());
        if self.processed.len() >= processed_cap / 2 || departed >= departed_cap / 2 {
            self.refresh_dup_caps();
        }
    }

    /// Read the store's count and hold the processed ids to the bound it
    /// gives now. The departures follow at the host's next
    /// [`Self::bound_departures`].
    fn refresh_dup_caps(&mut self) {
        self.stored_for_caps = self.store.count().unwrap_or(self.stored_for_caps);
        self.processed.set_cap(dup_caps(self.stored_for_caps).0);
    }

    /// Hold the departures to their bound ([`dup_caps`]), the store's count
    /// read afresh first. A host that tracks departures calls it each time
    /// it takes them ([`Self::take_fresh_departures`]); the role's own
    /// paths do not, so a board, which keeps none, does not link the walk
    /// that finds the cut (4.3 KiB on the Pocket image, measured by
    /// `just nrf-flash-budget` with the walk in the role).
    pub fn bound_departures(&mut self) {
        let (_, cap) = dup_caps(self.stored_for_caps);
        let held = self.departed.as_ref().map_or(0, |d| d.left.len());
        if held <= cap.saturating_add(cap / 8) {
            return;
        }
        self.refresh_dup_caps();
        self.trim_departures();
    }

    /// The duplicate list's counts and bounds, for a host's census: held
    /// processed ids, their cap, held departures, their cap.
    pub fn dup_counts(&self) -> DupCounts {
        let (processed_cap, departed_cap) = dup_caps(self.stored_for_caps);
        DupCounts {
            processed: self.processed.len(),
            processed_cap,
            departed: self.departed.as_ref().map_or(0, |d| d.left.len()),
            departed_cap: if self.departed.is_some() {
                departed_cap
            } else {
                0
            },
        }
    }

    /// Hold the departures to their bound ([`dup_caps`]), newest kept:
    /// once they pass the bound by an eighth, the oldest go down to it, so
    /// the walk that finds the cut is paid once per eighth of the bound
    /// departures, not per departure.
    fn trim_departures(&mut self) {
        let (_, cap) = dup_caps(self.stored_for_caps);
        let Some(departed) = self.departed.as_mut() else {
            return;
        };
        let len = departed.left.len();
        if len <= cap.saturating_add(cap / 8) {
            return;
        }
        let mut times: Vec<u64> = departed.left.values().map(|left| left.at_secs).collect();
        let (_, cut, _) = times.select_nth_unstable(len - cap);
        let cut = *cut;
        // Strictly older than the cut go, and of those at the cut only as
        // many as the bound needs.
        let mut at_cut_to_drop = len - cap - times.iter().filter(|at| **at < cut).count();
        departed.left.retain(|_, left| {
            if left.at_secs < cut {
                return false;
            }
            if left.at_secs == cut && at_cut_to_drop > 0 {
                at_cut_to_drop -= 1;
                return false;
            }
            true
        });
    }

    /// The largest limit, in the reference's kilobytes of 1000 bytes, whose
    /// whole size the empty store could hold
    /// ([`PropagationStore::space_required`] at most
    /// [`PropagationStore::capacity`]).
    ///
    /// # Why the announce is held to the store (LV-1001)
    ///
    /// A peer's sync resource concludes before we store a byte of it, and
    /// the peer marks every message in it handled as it concludes
    /// (`resource_concluded`, `reference/LXMF/LXMF/LXMPeer.py:492-502`).
    /// A message the store cannot hold even empty fails its append after
    /// that point and is lost on both sides. Nothing in the `/offer`
    /// exchange can save it: the offer carries ids and no sizes, and an id
    /// left out of our answer is one the sender marks handled too, "it has
    /// already received it from another peer" (`offer_response`,
    /// `LXMPeer.py:442-448`). The limits we announce are what a peer sizes
    /// its offer by: a message above the per-message limit it skips and
    /// keeps (`LXMPeer.py:370-373`), and a sync above the per-sync limit we
    /// refuse at the advertisement, where its cursor has not moved
    /// ([`Self::accepts_resource_of`]; the reference's own check,
    /// `LXMRouter.py:2220-2224`). Holding both to the store's capacity
    /// closes the space path.
    ///
    /// The reference does not clamp: it stores whatever a sync brings and
    /// enforces its limit later in its clean pass (`LXMRouter.py:1183-1210`).
    /// The clamp changes only the numbers in fields 3 and 4 of the announce;
    /// a stock node reads them as it reads any other node's limits.
    fn store_fit_kb(store: &S) -> u64 {
        let capacity = store.capacity();
        let mut kb = capacity / 1000;
        while kb > 0 {
            let bytes = usize::try_from(kb.saturating_mul(1000)).unwrap_or(usize::MAX);
            if store.space_required(bytes) <= capacity {
                break;
            }
            kb -= 1;
        }
        kb
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
    /// cannot fail, so this only fills the stored value). Clearing it
    /// does not stop a host computing them
    /// ([`PropagationNodeConfig::always_compute_stamp_value`]).
    pub fn set_compute_stamp_value(&mut self, compute: bool) {
        self.compute_stamp_value = compute;
    }

    /// Whether the accept path computes true stamp values at cost 0: on a
    /// host always, on a board while a peer demands them.
    pub fn compute_stamp_value(&self) -> bool {
        self.config.always_compute_stamp_value || self.compute_stamp_value
    }

    /// Messages this role removed from its store by expiry or
    /// displacement since it was built; a collected or delivered message is
    /// not counted.
    pub fn evicted_total(&self) -> u64 {
        self.evicted_total
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

    /// Whether `outcome`, answered under the current config, is a refusal
    /// the heap cap ([`PropagationNodeConfig::serve_cap_bytes`]) alone
    /// caused: the node had mail to give or a list to send, and only the
    /// heap it stood on kept it from doing so.
    ///
    /// * The list form, when the heap cap funds no id at all while the
    ///   serve limit would: the answer is then `ERROR_THROTTLED`
    ///   ([`PeerError::Throttled`]), which a stock client takes as
    ///   invalid list data and tears the link down on
    ///   (`message_list_response`, `reference/LXMF/LXMF/LXMRouter.py:1602-1605`),
    ///   to ask again on its next sync.
    /// * The fetch form, when the round serves nothing and the heap cap
    ///   withheld at least one wanted message ([`FetchPlan::heap_withheld`]):
    ///   the answer is the empty list, which a stock client completes with
    ///   zero messages and nothing confirmed, so the mail stays on the node.
    ///
    /// A fetch that served some messages and withheld others is not a
    /// refusal; it is the short answer the protocol allows.
    ///
    /// Call it right after the `/get` that produced `outcome`, before the
    /// cap moves again.
    pub fn refused_for_heap(&self, outcome: &GetOutcome) -> bool {
        match outcome {
            GetOutcome::List { count, .. } => {
                let heap_zero = self
                    .config
                    .serve_cap_bytes
                    .map(Self::list_capacity)
                    .is_some_and(|capacity| capacity == 0);
                let limit_zero = self
                    .config
                    .serve_limit_bytes
                    .map(Self::list_capacity)
                    .is_some_and(|capacity| capacity == 0);
                *count == 0 && heap_zero && !limit_zero
            }
            GetOutcome::Fetch { plan, .. } => plan.is_empty() && plan.heap_withheld() > 0,
        }
    }

    /// The minimum stamp value an upload must prove:
    /// `max(0, cost − flexibility)`
    /// (`reference/LXMF/LXMF/LXMRouter.py:2242`).
    pub fn min_accepted_cost(&self) -> u8 {
        self.config
            .stamp_cost
            .saturating_sub(self.config.stamp_cost_flexibility)
    }

    /// Whether this configuration accepts stamps that a stock peer then
    /// refuses to take from us ([`STOCK_PEER_MIN_STAMP_VALUE`]): such a
    /// message stays here until it expires. A host warns at start; the
    /// configuration is honoured, a cost below 13 is the operator's call.
    pub fn accepts_below_stock_peers(&self) -> bool {
        self.min_accepted_cost() < STOCK_PEER_MIN_STAMP_VALUE
    }

    /// The seven-field announce app-data, exactly as the reference builds it
    /// (`get_propagation_node_app_data`,
    /// `reference/LXMF/LXMF/LXMRouter.py:324-336`): field 0 legacy `False`,
    /// field 1 the current timebase, field 2 whether the node is open to
    /// strangers (`False` when [`PropagationNodeConfig::from_static_only`]),
    /// fields 3 and 4 the limits, field 5
    /// `[stamp_cost, flexibility, peering_cost]`, field 6 the metadata map
    /// with the name under [`PN_META_NAME`].
    pub fn announce_app_data(&self, now_secs: u64) -> Vec<u8> {
        let mut metadata = Vec::new();
        if let Some(name) = &self.config.name {
            let mut raw = Vec::with_capacity(name.len() + 2);
            msgpack::bin(&mut raw, name);
            metadata.push((PN_META_NAME, raw));
        }
        if self.departed.as_ref().is_some_and(|d| d.serves_status) {
            metadata.push(lev_status::capability_metadata());
        }
        let announce = PropagationNodeAnnounce {
            legacy_support: false,
            timebase: now_secs,
            enabled: !self.config.from_static_only,
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
        data_size <= self.resource_limit_bytes()
    }

    /// The per-sync limit [`Self::accepts_resource_of`] holds a resource
    /// to, in bytes: what a refusal names as the cap it failed.
    pub fn resource_limit_bytes(&self) -> u64 {
        self.config.sync_limit_kb.saturating_mul(1000)
    }

    /// The announced per-message transfer limit in bytes, the cap above
    /// which a stored message is local only ([`Self::is_local_only`]).
    pub fn transfer_limit_bytes(&self) -> u64 {
        self.config.transfer_limit_kb.saturating_mul(1000)
    }

    /// Whether a message whose stored body (`lxmf_data ‖ stamp`) is
    /// `body_len` bytes may leave this node toward a peer: the offering
    /// side skips, for good, a message whose size plus
    /// [`crate::peering::OFFER_PER_MESSAGE_OVERHEAD`] exceeds the peer's
    /// transfer limit (`reference/LXMF/LXMF/LXMPeer.py:370-373`; ours at
    /// `build_offer`), and our own announced limit is the one we hold it
    /// to. A message that fails it is *local only* ([`Self::is_local_only`]).
    pub fn upload_fits_transfer_limit(&self, body_len: usize) -> bool {
        (body_len as u64).saturating_add(crate::peering::OFFER_PER_MESSAGE_OVERHEAD)
            <= self.transfer_limit_bytes()
    }

    /// The largest stored size a peer may be offered: a message above it
    /// is local only ([`Self::is_local_only`]).
    pub fn largest_offered_size(&self) -> u64 {
        Self::largest_offered_size_under(&self.config)
    }

    fn largest_offered_size_under(config: &PropagationNodeConfig) -> u64 {
        config
            .transfer_limit_kb
            .saturating_mul(1000)
            .saturating_sub(crate::peering::OFFER_PER_MESSAGE_OVERHEAD)
    }

    /// Each cursor's count of stored messages above it that may be offered
    /// to a peer, local-only ones left out: the per-peer unhandled count
    /// ([`PropagationStore::count_above_cursors`]).
    pub fn unhandled_above(&self, cursors: &[u64]) -> Result<Vec<u64>, StorageError> {
        self.store
            .count_above_cursors(cursors, self.largest_offered_size())
    }

    /// Whether a stored message of `size` bytes is local only: above our
    /// own transfer limit, so it is never offered to a peer and never
    /// counted as owed to one, and only its recipient collecting from this
    /// node takes it away. Only a client upload can be one: a stock peer
    /// offers us nothing above the limit we announce
    /// (`reference/LXMF/LXMF/LXMPeer.py:370`, our announced limit is its
    /// `propagation_transfer_limit`).
    ///
    /// The reference stores such an upload too (it checks only the sync
    /// limit at upload, `reference/LXMF/LXMF/LXMRouter.py:2220-2224`) and
    /// lets each peer's own limit decide; we decide by ours, so a message
    /// the sender was told is stored here stays here for its recipient
    /// (order 1294 refused it at upload; reversed by the reviewer on
    /// 2026-11-03, order 1295).
    pub fn is_local_only(&self, size: u32) -> bool {
        !self.upload_fits_transfer_limit(size as usize)
    }

    /// The stored size (`lxmf_data ‖ stamp`) of the message in one upload
    /// envelope, `None` when it does not decode (the accept path classifies
    /// that). A host asks before [`Self::handle_upload`], to refuse above
    /// its delivery limit and to log a local-only accept.
    pub fn upload_body_len(&self, envelope: &[u8]) -> Option<u64> {
        let upload = PropagationUpload::decode(envelope).ok()?;
        Some((upload.unstamped_lxmf().len() + STAMP_SIZE) as u64)
    }

    /// Accept one client upload envelope (`[timestamp, [lxmf_data ‖ stamp]]`,
    /// `propagation_packet`, `reference/LXMF/LXMF/LXMRouter.py:2234-2255`),
    /// from either the raw-link-packet or the single-message resource path.
    ///
    /// `validate` is called only when [`Self::min_accepted_cost`] is above
    /// zero (or [`Self::compute_stamp_value`] asks for true values: always
    /// on a host, on a board while a peer demands them), with the transient ID and the 32-byte stamp; it returns the
    /// stamp's value if valid at that cost (`validate_pn_stamp`,
    /// `reference/LXMF/LXMF/LXStamper.py:84-96`, over the
    /// [`crate::constants::WORKBLOCK_EXPAND_ROUNDS_PN`]-round workblock) or
    /// `None`. At cost 0 a host still computes the true value, as the
    /// reference does (`LXStamper.py:95`), so that every message it took
    /// is offerable to a peer that joins later
    /// (`reference/LXMF/LXMF/LXMPeer.py:340`, leviculum#442 D6). A board
    /// ([`PropagationNodeConfig::always_compute_stamp_value`] false) skips
    /// the work at cost 0 while no peer demands a value and stores 0: a
    /// deviation that touches no wire byte and only the local value used
    /// when offering to peers, which part 2's design accounts for (concept
    /// paper §5); what it took before its first demanding peer is then
    /// never offered to a stock peer.
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
        let stamp_value = if min_cost == 0 && !self.compute_stamp_value() {
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

        if self.knows(&transient_id, now_secs) {
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
            // The bytes eviction must free are the ones the store charges
            // this body, not its raw length: a block-backed store charges
            // whole blocks plus its framing, and its free_space frees
            // blocks, so a raw-byte target would never be reached.
            //
            // A body the empty store could not hold either evicts nothing:
            // culling would empty the store and the append would still fail.
            let needed = self.store.space_required(body.len());
            if needed <= self.store.capacity() {
                evicted = self.make_room(needed, now_secs);
                result = self
                    .store
                    .append(&transient_id, now_secs, stamp_value, &body);
            }
        }
        if let Err(error) = result {
            return UploadOutcome::StoreFailed(error);
        }
        self.remember_processed(transient_id, now_secs);

        UploadOutcome::Accepted {
            transient_id,
            destination_hash,
            size,
            stamp_value,
            duplicate: false,
            evicted,
        }
    }

    /// Whether this node holds `transient_id` or held it and let it go: in
    /// the store, in the processed-id cache, or among the store's own
    /// departures inside `message_expiry`. One reading for both places that
    /// ask: an ingest calls such a message a duplicate, and an `/offer`
    /// answers it unwanted (LV-1003), so a message we delivered and purged
    /// does not cross the wire again only to be dropped as a duplicate.
    ///
    /// **Deviation:** the reference's `offer_request` asks its store alone
    /// (`propagation_entries`, `reference/LXMF/LXMF/LXMRouter.py:2318`) and
    /// wants a drained message back, then drops the bodies at ingest
    /// (`:2496`). Wire and semantics hold: an unwanted id is one the offering
    /// peer keeps, exactly as for one we still store.
    ///
    /// Cost per id: one processed-cache lookup in RAM (a binary search per
    /// week bucket, [`crate::processed_ids`]), one store-index lookup, and
    /// on a store that remembers departures one more lookup
    /// ([`PropagationStore::departed`]; a host's default answers `false`
    /// without reading anything).
    pub fn knows(&self, transient_id: &TransientId, now_secs: u64) -> bool {
        // A store that remembers its own departures answers for them past a
        // restart; an expired message is not one (`track_departures`).
        let received_since = now_secs.saturating_sub(self.config.message_expiry_secs);
        self.processed.contains(transient_id)
            || self.store.contains(transient_id).unwrap_or(false)
            || self
                .store
                .departed(transient_id, received_since)
                .unwrap_or(false)
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

    /// Number of transient IDs fitting the encoded list-response ceiling.
    pub const fn list_capacity(response_bytes: usize) -> usize {
        let id_bytes = 34; // MessagePack bin8 header plus the 32-byte ID.
        let mut count = response_bytes / id_bytes;
        while count != 0 && msgpack::array_header_len(count) + count * id_bytes > response_bytes {
            count -= 1;
        }
        count
    }

    /// Requested allocation peak for a capped list: one exact metadata spine
    /// and one exact wire response. No second ID list or sorting allocation.
    pub const fn list_peak_bytes(response_bytes: usize) -> usize {
        let count = Self::list_capacity(response_bytes);
        if count == 0 {
            return 2; // Named throttling response, not a falsely empty list.
        }
        count * core::mem::size_of::<(TransientId, u32)>()
            + msgpack::array_header_len(count)
            + count * 34
    }

    /// Plan one `/get` for the caller's link-identified delivery mailbox.
    ///
    /// Decoding, listing, confirmation purges and bounded body selection
    /// use only the directory. Lengths and order belong to this snapshot;
    /// no body read can enlarge the serve beyond its funded cap.
    ///
    /// A malformed request is answered with msgpack nil by the caller
    /// (`reference/LXMF/LXMF/LXMRouter.py:1558-1560`).
    ///
    /// # Durability ordering
    ///
    /// `haves` are purged before this returns, as the reference deletes
    /// before answering (`LXMRouter.py:1508-1519`, `:1544-1551`). A store
    /// with a deferred write queue must flush the tombstones in
    /// [`PreparedFetch::purged`] before sending any response bytes.
    /// Committed confirmations enter the RAM duplicate cache immediately.
    /// Deferred writers call [`Self::note_confirmed_deletion`] per receipt.
    pub fn prepare_get(
        &mut self,
        request: &[u8],
        remote_delivery_hash: &[u8; DESTINATION_LENGTH],
        now_secs: u64,
    ) -> Result<PreparedGet, GetError> {
        let request = MessageGetRequest::decode(request)?;

        // Both fields absent: the list form
        // (reference/LXMF/LXMF/LXMRouter.py:1489-1504).
        if request.wants.is_none() && request.haves.is_none() {
            let ceiling = self
                .config
                .serve_limit_bytes
                .into_iter()
                .chain(self.config.serve_cap_bytes)
                .min();
            let capacity = ceiling.map(Self::list_capacity);
            if capacity == Some(0) {
                let mut response = Vec::with_capacity(2);
                msgpack::uint(&mut response, PeerError::Throttled.code() as u64);
                return Ok(PreparedGet::Ready(GetOutcome::List { response, count: 0 }));
            }
            let mut available: Vec<(TransientId, u32)> = match capacity {
                Some(count) => Vec::with_capacity(count),
                None => Vec::new(),
            };
            self.store.for_each(&mut |meta: &StoredMessage| {
                if &meta.destination_hash != remote_delivery_hash {
                    return;
                }
                if let Some(capacity) = capacity {
                    if capacity == 0 {
                        return;
                    }
                    let at = available.partition_point(|(_, size)| *size <= meta.size);
                    if at == capacity {
                        return;
                    }
                    if available.len() == capacity {
                        available.pop();
                    }
                    available.insert(at, (meta.transient_id, meta.size));
                } else {
                    available.push((meta.transient_id, meta.size));
                }
            })?;
            if capacity.is_none() {
                available.sort_by_key(|(_, size)| *size);
            }
            let count = available.len();
            let mut response = Vec::with_capacity(msgpack::array_header_len(count) + count * 34);
            msgpack::array(&mut response, count);
            for (id, _) in &available {
                msgpack::bin(&mut response, id);
            }
            return Ok(PreparedGet::Ready(GetOutcome::List { response, count }));
        }

        // Keep metadata only for IDs carried by this request, not the entire
        // store. A large card must not turn a small fetch into a full directory
        // allocation. A zero size denotes a requested ID not owned by this
        // delivery destination.
        let mut owned: alloc::collections::BTreeMap<TransientId, u32> =
            alloc::collections::BTreeMap::new();
        for id in request.wants.iter().chain(request.haves.iter()).flatten() {
            owned.insert(*id, 0);
        }
        self.store.for_each(&mut |meta: &StoredMessage| {
            if &meta.destination_hash == remote_delivery_hash {
                if let Some(size) = owned.get_mut(&meta.transient_id) {
                    *size = meta.size;
                }
            }
        })?;
        owned.retain(|_, size| *size != 0);

        // Deletion happens here and only here: on the client's explicit
        // `haves` confirmation, which it sends only after taking local
        // delivery (`message_get_response` builds `haves` from what it
        // ingested and requests `[None, haves]`,
        // reference/LXMF/LXMF/LXMRouter.py:1622-1638; the purge loop is
        // :1508-1519). A crash mid-transfer therefore deletes nothing.
        let mut purged = Vec::new();
        if let Some(haves) = &request.haves {
            for transient_id in haves {
                if !owned.contains_key(transient_id) {
                    continue;
                }
                match self.store.purge_confirmed(transient_id)? {
                    ConfirmedPurge::Absent => continue,
                    ConfirmedPurge::Committed => {
                        self.note_confirmed_deletion(transient_id, now_secs);
                    }
                    ConfirmedPurge::Queued => {}
                }
                purged.push(*transient_id);
                owned.remove(transient_id);
            }
        }

        // The client's transfer limit arrives in kilobytes of 1000 bytes as
        // element 2 and is the reference's only bound (:1526-1530); a host
        // holds the response to it and to nothing else of its own. Our
        // announced per-sync limit paces what peers offer us, not what a
        // client may take. Two bounds of ours compose with it by `min`,
        // each only where the node has one, wire-compatible under the
        // deviation rule: a response shorter than the request is the
        // protocol's normal shape (skipped IDs are re-requested on the
        // next sync, :1547 skips them the same way).
        //
        // * A board's serve limit (`serve_limit_bytes`), what it can afford
        //   to serve a client in one fetch.
        // * The heap cap (`serve_cap_bytes`), tighter when a node's heap
        //   plan funds less: serving a response costs more than its length
        //   ([`serve_peak_bytes`]), and a board that answers past what that
        //   plan funds dies in the transient instead of answering. It
        //   bounds the same accounted sum the others bound, so the
        //   dominance argument at [`serve_peak_bytes`] carries over
        //   unchanged.
        let client_limit = request.transfer_limit_kb.map(|limit| match limit {
            TransferLimit::Integer(kb) => (kb as f64) * 1000.0,
            TransferLimit::Float(kb) => kb * 1000.0,
        });
        let limit = [
            client_limit,
            self.config.serve_limit_bytes.map(|bytes| bytes as f64),
            self.config.serve_cap_bytes.map(|funded| funded as f64),
        ]
        .into_iter()
        .flatten()
        .fold(f64::INFINITY, f64::min);

        let mut budget = TransferBudget::new(limit);
        // The same round without the heap cap, kept only to count what
        // the cap alone withheld ([`FetchPlan::heap_withheld`]): a board
        // logs a round that served nothing for that reason as its heap
        // refusal. It never decides what is served.
        let unheaped_limit = [
            client_limit,
            self.config.serve_limit_bytes.map(|bytes| bytes as f64),
        ]
        .into_iter()
        .flatten()
        .fold(f64::INFINITY, f64::min);
        let heap_binds = limit < unheaped_limit;
        let mut unheaped = TransferBudget::new(unheaped_limit);
        let mut heap_withheld = 0usize;

        let mut served: Vec<(TransientId, u32)> = Vec::new();
        let mut served_bytes = 0u64;
        let mut payload_bytes = 0usize;
        if let Some(wants) = &request.wants {
            for transient_id in wants {
                if !budget.can_admit_any() && !(heap_binds && unheaped.can_admit_any()) {
                    break;
                }
                let Some(&stored_size) = owned.get(transient_id) else {
                    // Gone or never ours to give: skipped silently, the
                    // response is simply shorter (:1535 membership test; the
                    // concept paper's §1 records that absence has no error).
                    continue;
                };
                let stored_size = stored_size as usize;
                if stored_size < MIN_BODY_LEN {
                    // Not a body this role wrote; nothing servable in it.
                    continue;
                }
                if !budget.admit(stored_size) {
                    // Too big for this round; keep scanning (:1547 `pass`).
                    // Counted once when only the heap cap refused it: the
                    // running totals only grow, so a repeat of the id
                    // later in `wants` cannot fit either.
                    if heap_binds && unheaped.admit(stored_size) {
                        heap_withheld += 1;
                        owned.remove(transient_id);
                    }
                    continue;
                }
                if heap_binds {
                    let _ = unheaped.admit(stored_size);
                }
                let servable = stored_size - STAMP_SIZE;
                served_bytes += servable as u64;
                payload_bytes += msgpack::bin_header_len(servable) + servable;
                served.push((*transient_id, servable as u32));
                // A wants list may repeat an ID, but a plan serves it once.
                owned.remove(transient_id);
            }
        }

        let encoded_len = msgpack::array_header_len(served.len()) + payload_bytes;
        Ok(PreparedGet::Fetch(PreparedFetch {
            plan: FetchPlan {
                served,
                encoded_len,
                served_bytes,
                heap_withheld,
            },
            purged,
        }))
    }

    /// Finish a card fetch without encoding or copying its prefetched bodies.
    ///
    /// `bodies` may arrive in any order and omit unreadable planned IDs,
    /// which are skipped before the stream's advertised length is fixed.
    /// A changed length is a store error, not permission to recompute the
    /// snapshot's transfer budget. Keep this slice alive and hand it to
    /// [`FetchSource::new`] with the returned plan.
    pub fn finish_get(
        &mut self,
        mut prepared: PreparedFetch,
        bodies: &[(TransientId, Vec<u8>)],
    ) -> Result<GetOutcome, GetError> {
        let mut changed_length = false;
        let mut served_bytes = 0u64;
        let mut payload_bytes = 0usize;
        prepared.plan.served.retain(|(id, servable)| {
            let Some((_, body)) = bodies.iter().find(|(body_id, _)| body_id == id) else {
                return false;
            };
            if body.len() != *servable as usize + STAMP_SIZE {
                changed_length = true;
                return false;
            }
            served_bytes += u64::from(*servable);
            payload_bytes += msgpack::bin_header_len(*servable as usize) + *servable as usize;
            true
        });
        if changed_length {
            return Err(GetError::Store(StorageError::Corrupt));
        }
        prepared.plan.served_bytes = served_bytes;
        prepared.plan.encoded_len =
            msgpack::array_header_len(prepared.plan.count()) + payload_bytes;
        Ok(self.complete_get(prepared))
    }

    /// Remember an explicit client confirmation only after its exact deletion
    /// has committed, including an independently verified uncertain receipt.
    pub fn note_confirmed_deletion(&mut self, transient_id: &TransientId, now_secs: u64) {
        self.remember_processed(*transient_id, now_secs);
        self.note_departure(transient_id, now_secs, DepartureReason::Collected);
    }

    /// Remember a message the host delivered to its own mailbox and then
    /// purged: a duplicate from now on, and `delivered` to `/lev/status`.
    pub fn note_delivered_locally(&mut self, transient_id: &TransientId, now_secs: u64) {
        self.remember_processed(*transient_id, now_secs);
        self.note_departure(transient_id, now_secs, DepartureReason::Delivered);
    }

    fn remember_processed(&mut self, transient_id: TransientId, at_secs: u64) {
        // Before the cache drops ids at its bound, the bound is taken from
        // the store's count as it is now, not as the last tick read it.
        if self.processed.len() >= self.processed.cap() {
            self.refresh_dup_caps();
        }
        self.processed.insert(&transient_id, at_secs);
    }

    fn note_departure(
        &mut self,
        transient_id: &TransientId,
        now_secs: u64,
        reason: DepartureReason,
    ) {
        if let Some(departed) = self.departed.as_mut() {
            departed.left.insert(
                *transient_id,
                Departure {
                    at_secs: now_secs,
                    reason,
                },
            );
            departed.fresh.push((*transient_id, reason));
        }
    }

    /// Keep the ids that leave the store, with why, so a host can persist
    /// them ([`Self::departures`]) and a restart does not turn a
    /// re-offered, already-collected message into a revenant: the
    /// reference persists its whole duplicate set
    /// (`locally_processed_transient_ids`,
    /// `reference/LXMF/LXMF/LXMRouter.py:2496`, saved at `:1018-1024`).
    /// Expired ones are kept for `/lev/status` but are no duplicates
    /// ([`DepartureReason::blocks_reoffer`]).
    pub fn track_departures(&mut self) {
        self.departed.get_or_insert_with(|| Departures {
            left: alloc::collections::BTreeMap::new(),
            fresh: Vec::new(),
            serves_status: false,
        });
    }

    /// Track departures and say in the announce that this node answers
    /// `/lev/status` ([`crate::lev_status`]); the host serves the path.
    pub fn serve_lev_status(&mut self) {
        self.track_departures();
        if let Some(departed) = self.departed.as_mut() {
            departed.serves_status = true;
        }
    }

    /// The departed ids, when and why they left, held to the expiry window
    /// by [`Self::tick`]; empty unless [`Self::track_departures`] was
    /// called.
    pub fn departures(&self) -> impl Iterator<Item = (&TransientId, &Departure)> {
        self.departed
            .iter()
            .flat_map(|departed| departed.left.iter())
    }

    /// The departures since the last call, in the order they happened, for
    /// the host's `PN_DEPART` line; restored ones are not among them.
    pub fn take_fresh_departures(&mut self) -> Vec<(TransientId, DepartureReason)> {
        self.departed
            .as_mut()
            .map(|departed| core::mem::take(&mut departed.fresh))
            .unwrap_or_default()
    }

    /// What `/lev/status` answers for `transient_id`: held while the store
    /// has it, else why it left, else unknown.
    pub fn status_of(&self, transient_id: &TransientId) -> MessageStatus {
        if self.store.contains(transient_id).unwrap_or(false) {
            return MessageStatus::Held;
        }
        self.departed
            .as_ref()
            .and_then(|departed| departed.left.get(transient_id))
            .map_or(MessageStatus::Unknown, |departure| {
                MessageStatus::of_departure(departure.reason)
            })
    }

    /// The `/lev/status` answer for `ids`, decoded by the host
    /// ([`lev_status::decode_request`]), which also owns the refusals.
    pub fn answer_lev_status(&self, ids: &[TransientId]) -> Vec<u8> {
        let entries: Vec<_> = ids.iter().map(|id| (*id, self.status_of(id))).collect();
        lev_status::encode_answer(&entries)
    }

    /// Re-seed the duplicate cache with persisted departures, skipping
    /// any older than the expiry window. Tracking is switched on, so what
    /// was restored is persisted again.
    pub fn restore_departures(
        &mut self,
        entries: impl IntoIterator<Item = (TransientId, Departure)>,
        now_secs: u64,
    ) -> usize {
        self.track_departures();
        // The bound a restore is held to is the store's, not the floor's.
        self.refresh_dup_caps();
        let window = self.config.message_expiry_secs;
        let mut restored = 0;
        for (transient_id, departure) in entries {
            if now_secs.saturating_sub(departure.at_secs) > window {
                continue;
            }
            if departure.reason.blocks_reoffer() {
                self.remember_processed(transient_id, departure.at_secs);
            }
            if let Some(departed) = self.departed.as_mut() {
                departed.left.insert(transient_id, departure);
            }
            restored += 1;
        }
        self.bound_departures();
        restored
    }

    fn complete_get(&mut self, prepared: PreparedFetch) -> GetOutcome {
        GetOutcome::Fetch {
            plan: prepared.plan,
            purged: prepared.purged,
        }
    }

    /// Answer a synchronous `/get` from one directory snapshot. Bodies are
    /// not read until the caller encodes a packet or builds a resource from
    /// [`Self::fetch_source`]. A card caller instead flushes purges and
    /// awaits body reads between [`Self::prepare_get`] and [`Self::finish_get`].
    pub fn handle_get(
        &mut self,
        request: &[u8],
        remote_delivery_hash: &[u8; DESTINATION_LENGTH],
        now_secs: u64,
    ) -> Result<GetOutcome, GetError> {
        match self.prepare_get(request, remote_delivery_hash, now_secs)? {
            PreparedGet::Ready(outcome) => Ok(outcome),
            PreparedGet::Fetch(prepared) => Ok(self.complete_get(prepared)),
        }
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
        self.tick_at_most(now_secs, usize::MAX)
    }

    /// [`Self::tick`], expiring at most `max_expired` messages, oldest
    /// first over a store indexed by receive time: a host calls it under
    /// the core lock, and comes again soon when it got `max_expired` back
    /// ([`MAX_EXPIRED_PER_TICK`]).
    ///
    /// Neither half walks what did not expire. The store hands over only
    /// the entries received before `now - expiry`
    /// ([`PropagationStore::for_each_received_before`]); the processed-ID
    /// cache drops whole week buckets ([`crate::processed_ids`]). Both walked
    /// every entry on every tick before, 24.6 ms on the field node's Odroid
    /// at 300 000 messages (leviculum#478).
    pub fn tick_at_most(&mut self, now_secs: u64, max_expired: usize) -> Vec<Eviction> {
        let expiry = self.config.message_expiry_secs;
        let floor = self.calendar_jump_floor;
        let mut expired = Vec::new();
        // `now - received_at > expiry` is `received_at < now - expiry`.
        let before = now_secs.saturating_sub(expiry);
        let _ = self
            .store
            .for_each_received_before(before, &mut |meta: &StoredMessage| {
                if expired.len() >= max_expired {
                    return ControlFlow::Break(());
                }
                if floor > 0 && meta.received_at <= floor && now_secs > floor {
                    return ControlFlow::Continue(());
                }
                if now_secs.saturating_sub(meta.received_at) > expiry {
                    expired.push(Eviction {
                        transient_id: meta.transient_id,
                        size: meta.size,
                        age_secs: now_secs.saturating_sub(meta.received_at),
                        reason: EvictionReason::Expired,
                    });
                }
                ControlFlow::Continue(())
            });
        expired.retain(|eviction| self.store.purge(&eviction.transient_id).unwrap_or(false));
        self.evicted_total += expired.len() as u64;
        for eviction in &expired {
            self.note_departure(&eviction.transient_id, now_secs, DepartureReason::Expired);
        }

        self.processed.prune(now_secs, PROCESSED_ID_EXPIRY_SECS);
        self.update_dup_caps();
        if let Some(departed) = self.departed.as_mut() {
            departed
                .left
                .retain(|_, left| now_secs.saturating_sub(left.at_secs) <= expiry);
        }
        expired
    }

    /// Cull by the reference's weight until at least `needed` bytes are
    /// free: `priority × age_weight × size` with
    /// `age_weight = max(1, age / 4 days)` and `priority` 0.1 for a
    /// destination in [`PropagationNodeConfig::prioritised_destinations`],
    /// 1 otherwise (`get_weight`,
    /// `reference/LXMF/LXMF/LXMRouter.py:1056-1067`). The trigger
    /// differs from the reference by necessity: it culls when a *configured*
    /// limit is exceeded on a periodic job (`:1183-1218`), our store has a
    /// hard capacity and culls when an append reports [`StorageError::Full`].
    ///
    /// `needed` is a [`PropagationStore::space_required`] figure, so the
    /// loop compares like with like: `free_space` and it are the store's own
    /// accounting unit. The *weight* is still the raw `age × size` the
    /// reference computes over body bytes.
    ///
    /// **One walk serves many appends** (order 1370). The reference sorts
    /// its whole store by weight once per store job; this sorted the whole
    /// store on every full append, 789 ms an upload on the coder host at
    /// 6 M messages (a full 2 GB store), and a full store finds itself full
    /// again on most appends. A walk now keeps only the heaviest messages
    /// worth `needed` plus [`EVICTION_CANDIDATE_FRACTION`] of the capacity
    /// (a bounded heap, `O(n log k)`, `k` entries of memory, no copy of the
    /// store) and evicts from that list, heaviest first, exactly what the
    /// append needs. The rest serve the next full appends for
    /// [`EVICTION_CANDIDATES_SECS`], the reference's own store-job cadence;
    /// a candidate gone meanwhile is skipped, and messages that arrived
    /// since the walk wait for the next one, as they wait for the
    /// reference's next job. The receive-time index cannot replace the walk:
    /// the weight multiplies age by size, so no order by age alone is the
    /// reference's order.
    fn make_room(&mut self, needed: u64, now_secs: u64) -> Vec<Eviction> {
        let mut evicted = Vec::new();
        let stale = now_secs < self.eviction_walked_at
            || now_secs - self.eviction_walked_at >= EVICTION_CANDIDATES_SECS;
        if stale {
            self.eviction_candidates.clear();
        }
        let mut walked = false;
        while self.store.free_space() < needed {
            if self.eviction_candidates.is_empty() {
                if walked {
                    break;
                }
                let want = needed
                    .saturating_sub(self.store.free_space())
                    .saturating_add(self.store.capacity() / EVICTION_CANDIDATE_FRACTION);
                self.eviction_candidates = self.heaviest(want, now_secs);
                self.eviction_walked_at = now_secs;
                walked = true;
                if self.eviction_candidates.is_empty() {
                    break;
                }
            }
            // Heaviest last, so taking it is a pop.
            let Some(candidate) = self.eviction_candidates.pop() else {
                break;
            };
            if self.store.purge(&candidate.transient_id).unwrap_or(false) {
                self.note_departure(&candidate.transient_id, now_secs, DepartureReason::Evicted);
                evicted.push(Eviction {
                    age_secs: now_secs.saturating_sub(self.eviction_walked_at) + candidate.age_secs,
                    ..candidate
                });
            }
        }
        self.evicted_total += evicted.len() as u64;
        evicted
    }

    /// The heaviest stored messages whose sizes add up to at least `bytes`
    /// (all of them when the store holds less), lightest first: one walk,
    /// a min-heap of the best seen so far that drops its lightest while the
    /// rest still add up. Equal weights go to the message the walk met
    /// first, as the stable sort this replaced left them.
    fn heaviest(&self, bytes: u64, now_secs: u64) -> Vec<Eviction> {
        use alloc::collections::BinaryHeap;
        use core::cmp::{Ordering, Reverse};

        struct Weighed {
            weight: f64,
            walked: u64,
            eviction: Eviction,
        }
        impl Ord for Weighed {
            fn cmp(&self, other: &Self) -> Ordering {
                self.weight
                    .total_cmp(&other.weight)
                    .then(other.walked.cmp(&self.walked))
            }
        }
        impl PartialOrd for Weighed {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }
        impl PartialEq for Weighed {
            fn eq(&self, other: &Self) -> bool {
                self.cmp(other) == Ordering::Equal
            }
        }
        impl Eq for Weighed {}

        let prioritised = &self.config.prioritised_destinations;
        let mut best: BinaryHeap<Reverse<Weighed>> = BinaryHeap::new();
        let mut held = 0u64;
        let mut walked = 0u64;
        let _ = self.store.for_each(&mut |meta: &StoredMessage| {
            let age_secs = now_secs.saturating_sub(meta.received_at);
            let age_weight = (age_secs as f64 / (4.0 * 24.0 * 60.0 * 60.0)).max(1.0);
            let priority = if prioritised.contains(&meta.destination_hash) {
                0.1
            } else {
                1.0
            };
            let weight = priority * age_weight * meta.size as f64;
            walked += 1;
            // Already enough, and lighter than the lightest kept: it would
            // be pushed and popped again.
            if held >= bytes
                && best
                    .peek()
                    .is_some_and(|Reverse(lightest)| weight <= lightest.weight)
            {
                return;
            }
            best.push(Reverse(Weighed {
                weight,
                walked,
                eviction: Eviction {
                    transient_id: meta.transient_id,
                    size: meta.size,
                    age_secs,
                    reason: EvictionReason::Displaced,
                },
            }));
            held += u64::from(meta.size);
            while let Some(Reverse(lightest)) = best.peek() {
                let size = u64::from(lightest.eviction.size);
                if held - size < bytes {
                    break;
                }
                held -= size;
                best.pop();
            }
        });
        // Ascending by `Reverse` is heaviest first; reversed for `pop`.
        let mut lightest_first: Vec<Eviction> = best
            .into_sorted_vec()
            .into_iter()
            .map(|Reverse(weighed)| weighed.eviction)
            .collect();
        lightest_first.reverse();
        lightest_first
    }
}

impl<S: PropagationStore + PropagationBodyStore> PropagationNode<S> {
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

    /// The bytes [`FetchPlan`] describes, streamed directly from the body
    /// store into the resource part scratch — what a board hands to
    /// `send_response_resource_from_source`
    /// (`leviculum-core/src/node/mod.rs`).
    pub fn fetch_source<'a>(&'a self, plan: &'a FetchPlan) -> FetchSource<'a, S> {
        FetchSource::new(&self.store, plan)
    }
}

#[cfg(test)]
#[path = "propagation_node_tests.rs"]
mod tests;
