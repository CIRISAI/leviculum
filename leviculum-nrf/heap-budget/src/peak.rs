//! The propagation role's transient heap peaks, one pure function per
//! operation (Codeberg #388, order 510).
//!
//! [`ROLE_BUDGET_BYTES`] and the census's `pn_*` terms are a steady state
//! model: what the role holds BETWEEN events. The allocations that break
//! it are transient and live inside single operations of
//! `leviculum-nrf/src/pn.rs`, where no host test reaches: an offer response
//! held the wanted bodies and their encoded envelope at once (510), an inbound
//! resource is decoded while its data is still live, a flush clones each
//! append body twice. Each function below is the peak of one of them as a
//! function of its inputs, so the next 340 byte failure is predicted here
//! instead of found in a field log. The offer response has been streamed
//! since order 511; [`offer_response_peak`] is its new shape and
//! [`buffered_offer_response_peak`] the one 510 measured.
//!
//! # What a figure counts
//!
//! Every block the operation holds live at its worst instant, INCLUDING its
//! input and including blocks a census term also counts while they sit
//! between events (a queued store write is `pn_flush=`, the offer plan is
//! `pn_out=`): the census term is part of the figure, not on top of it.
//! Requested bytes on `thumbv7em`, so a `Vec` header is [`VEC_BYTES`], not
//! the host's 24. The allocator's own per-block header is not in it, so a
//! board pays a little more than every figure here; like
//! [`serve_peak_bytes`](leviculum_lxmf::propagation_node::serve_peak_bytes),
//! these are lower bounds on the board's cost and exact bounds on what the
//! code asks for.
//!
//! # Growth
//!
//! A `Vec` filled by `push` or `extend_from_slice` is not the size of its
//! contents. `alloc`'s amortised growth takes the larger of twice the old
//! capacity and what the write needs, never less than 8 elements for bytes
//! and 4 for anything up to 1 KiB ([`pushed_capacity`], [`ByteVec`]). And
//! `embedded-alloc` 0.7's `LlffHeap` implements no `realloc`, so the default
//! one runs: allocate the new block, copy, free the old. While a `Vec` grows
//! it holds both blocks, which is why the growth terms below carry a peak
//! beside the capacity. Both rules are pinned against the real `Vec` and
//! the real encoders by `tests/peaks.rs`.
//!
//! # Against the budget
//!
//! [`role_margin`] asks what [`ROLE_BUDGET_BYTES`] leaves for a peak once
//! its structural term and whatever the role holds beside the operation
//! (the field shape: two phones' queued uploads) are spent. Negative is an
//! overdraw the boot plan does not fund.

use leviculum_core::crypto::token_len;
use leviculum_core::resource::RESOURCE_RANDOM_HASH_SIZE;
use leviculum_lxmf::constants::STAMP_SIZE;
use leviculum_lxmf::peering::{Peer, OFFER_ID_BYTES};
use leviculum_lxmf::propagation::TransientId;
use leviculum_lxmf::propagation_node::PART_BOOKKEEPING_BYTES;
use leviculum_lxmf::propagation_store::StoredMessage;

use crate::{ROLE_BUDGET_BYTES, ROLE_STRUCTURAL_BYTES};

/// The allocation the field T114 183004F712B4A7FE refused 24 s after an
/// upload, `memory allocation of 340 bytes failed` with 4 760 B free
/// (#388, 2026-09-12). A peak that leaves less than this of the budget is
/// the shape that failure had.
pub const FIELD_FAILED_ALLOCATION_BYTES: usize = 340;

/// One `Vec` header (pointer, capacity, length) on `thumbv7em`: three
/// 4-byte words. Pinned rather than computed for the reason
/// [`LINK_BYTES`](crate::LINK_BYTES) is: on the host it is 24. The firmware
/// asserts `size_of::<Vec<u8>>()` against it
/// (`leviculum-nrf/src/heap_census.rs`).
pub const VEC_BYTES: usize = 12;

/// One transient id, the element of every id list the round builds.
pub const TRANSIENT_ID_BYTES: usize = core::mem::size_of::<TransientId>();

/// One `StoredMessage`, the element of the store tail. Pointer-free, so the
/// host and the target agree on it; `tests/peaks.rs` pins the number.
pub const STORED_MESSAGE_BYTES: usize = core::mem::size_of::<StoredMessage>();

/// One `Peer`, the value of the peer table's map. Pointer-free, as above.
pub const PEER_BYTES: usize = core::mem::size_of::<Peer>();

/// The stable sort's stack scratch (`core::slice::sort::stable`,
/// `AlignedStorage<T, 4096>`): a sort whose scratch fits it allocates
/// nothing.
pub const SORT_STACK_SCRATCH_BYTES: usize = 4096;

/// Longest slice the stable sort orders by insertion, with no scratch.
pub const SORT_INSERTION_MAX: usize = 20;

/// The stable sort's heap ceiling for its scratch (`MAX_FULL_ALLOC_BYTES`).
const SORT_MAX_FULL_ALLOC_BYTES: usize = 8_000_000;

/// The stable sort's scratch floor in elements (`SMALL_SORT_GENERAL_SCRATCH_LEN`).
const SORT_MIN_SCRATCH_LEN: usize = 48;

/// `alloc`'s smallest non-zero capacity for an element of `elem` bytes
/// (`RawVec::MIN_NON_ZERO_CAP`).
const fn min_non_zero_cap(elem: usize) -> usize {
    if elem == 1 {
        8
    } else if elem <= 1024 {
        4
    } else {
        1
    }
}

const fn max(a: usize, b: usize) -> usize {
    if a > b {
        a
    } else {
        b
    }
}

/// Capacity of a `Vec` that started at `start` slots (0, or a
/// `with_capacity` reserve) after `n` single pushes.
pub const fn pushed_capacity_from(elem: usize, start: usize, n: usize) -> usize {
    let mut cap = start;
    while cap < n {
        cap = max(max(cap * 2, cap + 1), min_non_zero_cap(elem));
    }
    cap
}

/// Capacity of a `Vec` grown from empty by `n` pushes.
pub const fn pushed_capacity(elem: usize, n: usize) -> usize {
    pushed_capacity_from(elem, 0, n)
}

/// Peak bytes while such a `Vec` reaches `n`: its last growth holds the new
/// block and the one it is copied out of.
pub const fn pushed_peak_from(elem: usize, start: usize, n: usize) -> usize {
    let cap = pushed_capacity_from(elem, start, n);
    if cap == start {
        return cap * elem;
    }
    // The block the last growth copied out of: what the same pushes had
    // reached one element before the capacity was exceeded.
    let mut prev = start;
    while pushed_capacity_from(elem, prev, prev + 1) < cap {
        prev = pushed_capacity_from(elem, prev, prev + 1);
    }
    (cap + prev) * elem
}

/// [`pushed_peak_from`] from empty.
pub const fn pushed_peak(elem: usize, n: usize) -> usize {
    pushed_peak_from(elem, 0, n)
}

/// Heap scratch `slice::sort_by_key` takes for `n` elements of `elem`
/// bytes: none up to [`SORT_INSERTION_MAX`] or while the scratch fits the
/// stack buffer, otherwise `max(n − n/2, min(n, 8 MB / elem), 48)` elements
/// (driftsort's `alloc_len`).
pub const fn stable_sort_scratch(elem: usize, n: usize) -> usize {
    if n <= SORT_INSERTION_MAX || elem == 0 {
        return 0;
    }
    let full = if n < SORT_MAX_FULL_ALLOC_BYTES / elem {
        n
    } else {
        SORT_MAX_FULL_ALLOC_BYTES / elem
    };
    let alloc_len = max(max(n - n / 2, full), SORT_MIN_SCRATCH_LEN);
    if alloc_len <= SORT_STACK_SCRATCH_BYTES / elem {
        0
    } else {
        alloc_len * elem
    }
}

/// A byte `Vec` written by a sequence of `extend_from_slice`/`push` calls,
/// grown the way `alloc` grows it: `len`, the final `capacity`, and `peak`,
/// the most it held at once (old and new block during a growth).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteVec {
    pub len: usize,
    pub capacity: usize,
    pub peak: usize,
}

impl ByteVec {
    /// An empty `Vec::new()`.
    pub const fn new() -> Self {
        Self {
            len: 0,
            capacity: 0,
            peak: 0,
        }
    }

    /// One write of `bytes` bytes.
    pub const fn write(self, bytes: usize) -> Self {
        let need = self.len + bytes;
        if need <= self.capacity {
            return Self { len: need, ..self };
        }
        let capacity = max(max(self.capacity * 2, need), min_non_zero_cap(1));
        Self {
            len: need,
            capacity,
            peak: max(self.peak, self.capacity + capacity),
        }
    }

    /// A msgpack array header as `rmp` writes it: the marker, then the
    /// length bytes as a second write.
    pub const fn array_header(self, n: usize) -> Self {
        if n <= 15 {
            self.write(1)
        } else if n <= 65_535 {
            self.write(1).write(2)
        } else {
            self.write(1).write(4)
        }
    }

    /// A msgpack `bin` of `len` bytes as `rmp` writes it: marker, length,
    /// payload, three writes.
    pub const fn bin(self, len: usize) -> Self {
        let header = if len < 256 {
            self.write(1).write(1)
        } else if len < 65_536 {
            self.write(1).write(2)
        } else {
            self.write(1).write(4)
        };
        header.write(len)
    }

    /// A msgpack float64: marker, then eight bytes.
    pub const fn f64(self) -> Self {
        self.write(1).write(8)
    }
}

impl Default for ByteVec {
    fn default() -> Self {
        Self::new()
    }
}

/// `PeerSyncEnvelope::encode` (`leviculum-lxmf/src/peering.rs`) of these
/// bodies: `[timestamp, [bin, …]]`.
pub fn sync_envelope<I>(bodies: I) -> ByteVec
where
    I: IntoIterator<Item = usize> + Clone,
{
    let count = bodies.clone().into_iter().count();
    let mut out = ByteVec::new().array_header(2).f64().array_header(count);
    for body in bodies {
        out = out.bin(body);
    }
    out
}

/// `PeerOffer::encode` (`leviculum-lxmf/src/peering.rs`) of `ids` ids:
/// `[key, [bin, …]]`.
pub const fn offer_encoding(ids: usize) -> ByteVec {
    let mut out = ByteVec::new()
        .array_header(2)
        .bin(OFFER_ID_BYTES - 2)
        .array_header(ids);
    let mut i = 0;
    while i < ids {
        out = out.bin(OFFER_ID_BYTES - 2);
        i += 1;
    }
    out
}

/// The request `NodeCore::send_request` packs around a body of `body`
/// bytes (`leviculum-core/src/node/mod.rs`, "Build msgpack: fixarray(3)"):
/// `fixarray(3)` pushed, the float64 as push plus extend, the 16-byte path
/// hash as `bin8` (two pushes and an extend), then the body.
pub const fn request_packing(body: usize) -> ByteVec {
    ByteVec::new()
        .write(1)
        .write(1)
        .write(8)
        .write(1)
        .write(1)
        .write(16)
        .write(body)
}

/// Heap one outgoing resource holds for `payload` bytes cut at
/// `resource_sdu`: the token-encrypted stream as parts plus their
/// bookkeeping, the third term of
/// [`serve_peak_bytes`](leviculum_lxmf::propagation_node::serve_peak_bytes)
/// for a payload that carries no response frame
/// (`OutgoingResource::new_with_flags`, `leviculum-core/src/resource/outgoing.rs`:
/// the payload is borrowed, never copied, and no compression is linked into
/// the firmware).
pub const fn transfer_bytes(payload: usize, resource_sdu: usize) -> usize {
    let sdu = if resource_sdu == 0 { 1 } else { resource_sdu };
    let transfer = token_len(RESOURCE_RANDOM_HASH_SIZE + payload);
    transfer + transfer.div_ceil(sdu) * PART_BOOKKEEPING_BYTES
}

/// The answer to an `/offer` that wants messages: `Engine::on_offer_response`
/// (`leviculum-nrf/src/pn.rs`), at the instant the core's streamed build
/// cuts the last part. `encoded_len` is the envelope the batch encodes to
/// (`SyncBatch::encoded_len`, `leviculum-lxmf/src/peering.rs`), `offered`
/// the ids the plan named, `listed` the ids of a `Wanted` response (0 for
/// `True`).
///
/// Since leviculum#388 order 511 the answer is streamed out of the store
/// (`SyncSource`, `NodeCore::send_resource_from_source`), and live together
/// at the peak are only:
///
/// 1. the round's plan in `self.outbound` (`pn_out=`), grown by
///    `build_offer`'s pushes, borrowed by the answer and never cloned;
/// 2. the decoded wanted list, `with_capacity(listed)`, borrowed by the
///    source rather than re-collected;
/// 3. the core builder's one part of scratch, `min(sdu, len)`;
/// 4. the resource's parts ([`transfer_bytes`]), the one whole copy.
///
/// What 510 counted and is gone: `plan.clone()` (the plan is borrowed);
/// `response_action`'s collected id list (the reply borrows the response,
/// `offer_reply`); every wanted body and the spine holding them (the source
/// copies a part's worth of a body at a time straight out of mapped flash,
/// `read_body_at`); `envelope.encode()` and its growth (the envelope is
/// framed on the stack as it streams). The source itself allocates nothing
/// (`the_streamed_answer_holds_no_body_where_the_buffered_one_held_them_all`,
/// `leviculum-lxmf/tests/peer_sync_batch.rs`).
///
/// Not in it, as in
/// [`serve_peak_bytes`](leviculum_lxmf::propagation_node::serve_peak_bytes):
/// the advertisement the build caches, its send copy and the packet built
/// from it, and the allocator's per-block headers. They are what
/// [`OFFER_RESPONSE_RESERVE_BYTES`](crate::OFFER_RESPONSE_RESERVE_BYTES)
/// keeps clear.
pub const fn offer_response_peak(
    encoded_len: usize,
    offered: usize,
    listed: usize,
    resource_sdu: usize,
) -> usize {
    let tid = TRANSIENT_ID_BYTES;
    let plan = pushed_capacity(tid, offered) * tid;
    let wanted = listed * tid;
    let sdu = if resource_sdu == 0 { 1 } else { resource_sdu };
    let scratch = if sdu < encoded_len {
        sdu
    } else if encoded_len < 16 {
        16
    } else {
        encoded_len
    };
    plan + wanted + scratch + transfer_bytes(encoded_len, sdu)
}

/// 510's model of the same answer, buffered: the plan and its clone, the
/// response's list and `response_action`'s collect, every body and its
/// spine, and `envelope.encode()` grown around them beside the parts. Kept,
/// and kept exact, because 510's table and the `[HEAP_PEAK]` lines of boards
/// running before order 511 are read against it; the test table prints it
/// beside the streamed figure.
pub fn buffered_offer_response_peak<I>(bodies: I, offered: usize, resource_sdu: usize) -> usize
where
    I: IntoIterator<Item = usize> + Clone,
{
    let (count, payload) = bodies
        .clone()
        .into_iter()
        .fold((0, 0), |(count, sum), body| (count + 1, sum + body));
    let tid = TRANSIENT_ID_BYTES;
    let plan = pushed_capacity(tid, offered) * tid + offered * tid;
    let wanted = count * tid + pushed_capacity(tid, count) * tid;
    let want_all = offered * tid;
    let held = plan + max(wanted, want_all) + count * VEC_BYTES + payload;
    let envelope = sync_envelope(bodies);
    let growing = held + envelope.peak;
    let sending = held + envelope.capacity + transfer_bytes(envelope.len, resource_sdu);
    max(growing, sending)
}

/// Whether an answer of `encoded_len` bytes fits what the role has left
/// beside `concurrent` bytes it holds, with
/// [`OFFER_RESPONSE_RESERVE_BYTES`](crate::OFFER_RESPONSE_RESERVE_BYTES)
/// kept clear: the test the board's batch planner asks of every prefix
/// (`plan_sync_batch`), and the one the announced sync limit is derived
/// from ([`BOARD_SYNC_LIMIT_KB`](crate::BOARD_SYNC_LIMIT_KB)).
pub const fn offer_response_fits(
    encoded_len: usize,
    offered: usize,
    listed: usize,
    resource_sdu: usize,
    concurrent: usize,
) -> bool {
    role_margin(
        offer_response_peak(encoded_len, offered, listed, resource_sdu),
        concurrent,
    ) >= crate::OFFER_RESPONSE_RESERVE_BYTES as isize
}

/// A completed inbound resource: `Engine::on_inbound_resource`
/// (`leviculum-nrf/src/pn.rs`). `data_len` is the resource's data, which the
/// `NodeEvent` holds across the call and the boot plan funds in its reserve
/// (`MAX_INCOMING_RESOURCE_BYTES`); `decoded` the message lengths
/// `PeerSyncEnvelope::decode` finds in it.
///
/// Two routes, and the sender picks which, so the board has to fund the
/// worse:
///
/// * a peer's sync: `data` plus the decoded batch, one exact block per
///   message and the spine (`with_capacity(count)`), which becomes the
///   `pn_batch=` term without a copy (`Vec` into `VecDeque` reuses the
///   block);
/// * a client's upload: the decoded envelope is dropped before the
///   `let … else` arm runs (the scrutinee does not outlive the pattern
///   test), then `data.to_vec()` copies the whole resource into the work
///   queue: `2 × data_len`.
pub fn inbound_resource_peak<I>(data_len: usize, decoded: I) -> usize
where
    I: IntoIterator<Item = usize>,
{
    let (count, sum) = decoded
        .into_iter()
        .fold((0, 0), |(count, sum), body| (count + 1, sum + body));
    data_len + max(count * VEC_BYTES + sum, data_len)
}

/// One append through `Engine::flush` (`leviculum-nrf/src/pn.rs`):
/// the queued op's body (`pn_flush=`), `peek_op().cloned()`'s copy of it,
/// and `execute`'s `body.clone()` into the `PnOp` the record-store task
/// receives. Three bodies; the second and third are the two extra copies.
pub const fn flush_peak(body_len: usize) -> usize {
    3 * body_len
}

/// One upload taken off the work queue (`Work::Upload`,
/// `leviculum-nrf/src/pn.rs`): `data` (the envelope, `data_len`) is held by
/// the arm throughout, and `body_len` is the stamped body it carries.
///
/// * `handle_upload`: `PropagationUpload::decode`'s unstamped copy
///   (`body_len − STAMP_SIZE`), `ingest_split`'s stamped body, and, when the
///   store takes it, the adapter's queued copy (`FlushOp::Append`);
/// * then, `stored` only, the flush with `data` still live ([`flush_peak`]).
///
/// An upload above the record log's `MAX_BODY` (a client may send up to the
/// 8 KB the resource layer admits) is refused by the store after the first
/// two copies exist, so `stored` is false and the flush never runs.
pub const fn upload_peak(data_len: usize, body_len: usize, stored: bool) -> usize {
    let unstamped = body_len.saturating_sub(STAMP_SIZE);
    let queued = if stored { body_len } else { 0 };
    let handle = unstamped + body_len + queued;
    let flush = if stored { flush_peak(body_len) } else { 0 };
    data_len + max(handle, flush)
}

/// The sync round's planning step, `Engine::on_sync_link_up`
/// (`leviculum-nrf/src/pn.rs`): `tail` store records above the peer's
/// cursor, `offered` ids planned from them.
///
/// `entries` is collected by push with no reserve (one `StoredMessage` per
/// record above the cursor, which for a new peer is the whole store) and
/// stays live to the end of the function. Three instants:
///
/// * its last growth ([`pushed_peak`]);
/// * the sort, with its scratch ([`stable_sort_scratch`]);
/// * the offer: `build_offer`'s id list (grown by push), its clone into the
///   `PeerOffer`, the encoded offer and the packed request.
pub const fn store_tail_peak(tail: usize, offered: usize) -> usize {
    let entry = STORED_MESSAGE_BYTES;
    let tid = TRANSIENT_ID_BYTES;
    let entries = pushed_capacity(entry, tail) * entry;
    let growing = pushed_peak(entry, tail);
    let sorting = entries + stable_sort_scratch(entry, tail);
    let offer = offer_encoding(offered);
    let request = request_packing(offer.len);
    let offering = entries
        + max(
            pushed_peak(tid, offered),
            pushed_capacity(tid, offered) * tid + offered * tid + offer.capacity + request.peak,
        );
    max(max(growing, sorting), offering)
}

/// One outbound sync round as a whole: the larger of its planning step
/// ([`store_tail_peak`]) and its answer ([`offer_response_peak`]). The two
/// never overlap: `entries` is dropped when the planning step returns, and
/// the answer arrives on a later event.
pub const fn sync_round_peak(
    tail: usize,
    offered: usize,
    encoded_len: usize,
    listed: usize,
    resource_sdu: usize,
) -> usize {
    max(
        store_tail_peak(tail, offered),
        offer_response_peak(encoded_len, offered, listed, resource_sdu),
    )
}

/// What [`ROLE_BUDGET_BYTES`] leaves of itself once its structural term,
/// `concurrent` bytes the role holds beside the operation, and the
/// operation's `peak` are spent. Negative is an overdraw.
pub const fn role_margin(peak: usize, concurrent: usize) -> isize {
    ROLE_BUDGET_BYTES as isize
        - ROLE_STRUCTURAL_BYTES as isize
        - concurrent as isize
        - peak as isize
}

/// The peer table's node structure at `peers` entries by the census's
/// B-tree model (`btree_bytes_model`, `leviculum-core/src/heap_census.rs`),
/// on the target's 4-byte words: the `Peer` is inline in the map, so this,
/// not §5's 104 B record, is what a full table costs in RAM.
pub const fn peer_table_bytes(peers: usize) -> usize {
    const NODE_CAPACITY: usize = 11;
    const NODE_TYPICAL_FILL: usize = 7;
    const WORD: usize = 4;
    if peers == 0 {
        return 0;
    }
    peers.div_ceil(NODE_TYPICAL_FILL) * (2 * WORD + NODE_CAPACITY * (16 + PEER_BYTES))
}

/// `Engine::too_large` (`leviculum-nrf/src/pn.rs`): reserved at
/// `reserve` slots, bounded at `bound` entries by a `remove(0)` that runs
/// AFTER the push, so the vector reaches `bound + 1` before it is trimmed
/// and grows to hold that. Returns (capacity bytes, growth peak bytes).
pub const fn trimmed_fifo_bytes(elem: usize, reserve: usize, bound: usize) -> (usize, usize) {
    (
        pushed_capacity_from(elem, reserve, bound + 1) * elem,
        pushed_peak_from(elem, reserve, bound + 1),
    )
}
