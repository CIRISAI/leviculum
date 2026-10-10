#![no_std]
//! The firmware's boot heap budget (Codeberg #388): how many endpoint links
//! the 96 KiB heap admits, the fetch response the boot plan funds, and what
//! the propagation role may still serve from the heap the board has at the
//! moment a `/get` arrives.
//!
//! Pure `usize` arithmetic, moved out of `leviculum-nrf/src/heap_census.rs`
//! so a host test can ask the #388 question (two phones plus the role on a
//! 96 KiB heap) without a board. The census (reading the allocator, printing
//! the line) stays in the firmware and calls in here. Policy:
//! `docs/src/concepts/firmware-host-test-seam.md`.
//!
//! The one term that is a property of the target rather than of the plan is
//! the size of one boxed `Link`, which on a host triple is not what it is on
//! `thumbv7em`. It is pinned here as [`LINK_BYTES`] and the firmware asserts
//! `size_of::<Link>()` against it at compile time, so a change to `Link`
//! fails the firmware build until this number (and the tests that print it)
//! moves with it.

/// Size of one `leviculum_core::link::Link` on `thumbv7em-none-eabihf`,
/// the firmware's target. Pinned rather than computed because this crate's
/// tests run on the host, where pointer-sized fields make `Link` larger; the
/// firmware asserts `size_of::<Link>() == LINK_BYTES`
/// (`leviculum-nrf/src/heap_census.rs`), so the two cannot drift silently.
pub const LINK_BYTES: usize = 2_648;

use leviculum_core::resource::RESOURCE_SDU_OVERHEAD;
use leviculum_lxmf::propagation_node::{
    serve_cap_for_live_heap, serve_cap_for_peak, serve_peak_bytes,
};

/// Heap size in bytes (64 KiB).
///
/// Bumped DOWN from 96 KiB after the Bug #32 RNG fix. Sequence:
/// 1. Direct-RNG-access panic (~26 s cycle) was the dominant fault.
/// 2. RNG fix landed (commit f093099) → device runs longer, exposing
///    a SECOND failure mode: stack overflow into BSS, manifesting as
///    HardFault in `embassy_sync::AtomicWaker::wake` (Cell::replace
///    on corrupted memory), `embedded_alloc::llff:78` allocator
///    panic, or PC=0x0 jumps. Stack peak runs into __ebss territory.
/// 3. Reducing heap from 96 KiB to 64 KiB shifts __ebss DOWN by 32
///    KiB, giving stack 32 KiB more headroom. Observed `heap_used`
///    peak is ~50 KiB (78% of 64 KiB pool); fits with 14 KiB margin.
///    With 96 KiB pool, stack peak crashed into BSS within ~25 s.
///
/// Earlier "stack_free=0 was a measurement artifact" diagnosis (in
/// Stage 1B's commit message) was WRONG — the artifact was real
/// for the OLD stack_free implementation (commit 42f05e2 fixes that),
/// but stack overflow at HEAP=96K is also real. Both bugs coexist.
///
/// Public since #388: the boot `HEAP_BUDGET` assertion
/// (`leviculum_nrf::heap_census::log_budget_and_assert`) judges its total against
/// this pool.
pub const HEAP_SIZE: usize = 96 * 1024;

/// The largest incoming resource transfer the binaries accept
/// (`NodeCoreBuilder::max_incoming_resource_size`), stated once so the
/// builder call, the propagation role's announced sync limit
/// ([`BOARD_SYNC_LIMIT_KB`]) and the boot `HEAP_BUDGET`'s
/// reserve term cannot drift apart (#388).
pub const MAX_INCOMING_RESOURCE_BYTES: usize = 8 * 1024;

/// Slots in the per-connection HVN drain table (`HVN_DRAIN`, `leviculum-nrf/src/ble/mod.rs`).
///
/// The #255 design headroom of 4 that this table was pre-sized to is
/// now spent: #372 raised `CONN_COUNT` to meet it, so every slot is
/// claimable. Growing further means growing both together (and paying
/// the `memory.x` bill first).
pub const MAX_LINKS: usize = 4;

/// The channel depth every BLE packet queue uses.
pub const QUEUE_DEPTH: usize = 4;

/// The MTU this interface reports to the core — the largest packet the
/// core hands it, and therefore the unit of every queue-depth × packet
/// bound below.
pub const INTERFACE_MTU: usize = 564;

/// Worst-case heap bytes ONE BLE session pins (#388, the `per_link=`
/// slice of the boot `HEAP_BUDGET`): its private `LINK_OUT` queue
/// full, the packet its pump currently holds plus that packet's
/// fragment copies, and one full reassembly in its defragmenter. The
/// running actuals are the census's `ble_s<slot>=` and `ble_defrag=`
/// terms; this constant is what they must stay under.
pub const SESSION_BUDGET_BYTES: usize = QUEUE_DEPTH * INTERFACE_MTU // LINK_OUT full
    + 2 * INTERFACE_MTU // pump: the packet + its fragment copies
    + INTERFACE_MTU; // defragmenter: one in-progress reassembly

/// Worst case of the two node-facing channels (`BLE_INCOMING` /
/// `BLE_OUTGOING`) — shared across links, so the budget counts it
/// once, in its reserve term.
pub const CHANNEL_BUDGET_BYTES: usize = 2 * QUEUE_DEPTH * INTERFACE_MTU;

/// Per-transfer (upload) limit announced in field 3, kilobytes of 1000
/// (`propagation_transfer_limit`, `reference/LXMF/LXMF/LXMPeer.py:370`).
/// The crate default, stated here because the heap budget below sums it.
pub const BOARD_TRANSFER_LIMIT_KB: u64 = 4;

/// Per-sync limit announced in field 4, kilobytes of 1000 (#388), DERIVED
/// since order 511 from what the board can afford to ANSWER: the largest
/// limit whose answer, an offer response of that many encoded bytes
/// streamed out of the store, fits the role's sync slice
/// ([`SYNC_SLICE_BYTES`]) beside two phones' queued uploads with
/// [`OFFER_RESPONSE_RESERVE_BYTES`] kept clear ([`derive_sync_limit_kb`],
/// at the end of this file with its arithmetic). Was a stated 8, chosen
/// for receiving; 510's table showed an 8 KB answer overdrawing the role
/// by 16 996 B, so peers were told a limit the board could not keep.
pub const BOARD_SYNC_LIMIT_KB: u64 = derive_sync_limit_kb();

// The announced sync limit must stay receivable, or it is a lie peers
// pay for with a dead resource transfer. The limit bounds the WIRE
// resource, framing included (#388 pass 3 item 3): the peer's packing
// loop counts 24 B up front and 16 B per message and stops strictly
// below the limit (`per_message_overhead`/`cumulative_size`,
// `reference/LXMF/LXMF/LXMPeer.py:359-360`, strict skip at `:376`),
// while the shipped `msgpack([ts, [bin, ...]])` (`:466`) costs at most
// 13 B base + 3 B per message — each accounted term dominates its wire
// term (asserted at their definitions in `leviculum_lxmf::peering`,
// pinned by that crate's largest-legal-batch test), so the resource is
// strictly smaller than the accounted sum, which is strictly below
// this limit.
const _: () = assert!(BOARD_SYNC_LIMIT_KB as usize * 1000 <= MAX_INCOMING_RESOURCE_BYTES);

/// The link SDU the serve transient is sized against: Reticulum's
/// smallest negotiated MTU (500) less the resource per-part overhead.
/// The smallest SDU is the worst case, because it makes the most parts
/// and therefore the most per-part bookkeeping.
pub const SERVE_RESOURCE_SDU: usize = 500 - RESOURCE_SDU_OVERHEAD;

/// Heap the plan sets aside for ANSWERING a `/get` — the one term of the
/// budget that pays for what the board sends rather than what it holds,
/// and **the capacity decision, as a number the Lead can move.**
///
/// Zero today, which is not an oversight: every other term of
/// [`ROLE_BUDGET_BYTES`] is already spent, and the arithmetic below says
/// what raising this costs. It is in the role's sum, so raising it comes
/// out of [`max_endpoint_links`] automatically — the
/// budget cannot be told to fund a serve without being told where from,
/// and a configuration that overdraws refuses at compile time.
///
/// What it buys, at today's SDU (via
/// [`leviculum_lxmf::propagation_node::serve_cap_for_peak`], the inverse
/// of [`SERVE_PEAK_BYTES`]'s model):
///
/// ```text
/// reserve + slack   funded fetch response   messages of 224 B
///        656 B                     170 B                    0
///      5 000 B                   2 128 B                    7
///     17 264 B                   8 000 B     the full announced cap
///     20 000 B                   9 304 B                   34
///     30 380 B                  14 296 B                   52
/// ```
///
/// (30 380 B is what the T114 had free when the 24-message fetch of
/// 2026-09-23 killed it. Under the buffered serve it funded 4 954 B and
/// 18 of those 24; since #384 B2 streamed the response out of the store
/// it funds 14 296 B, and the whole announced 8 KB cap costs 17 264 B
/// instead of 48 922.)
///
/// And what it costs: the slack is 656 B and one endpoint link is
/// [`budget_per_link`] ≈ 2 712 B, so raising this
/// still has to come out of the four links the budget affords — and
/// [`MAX_LINKS`] is also four, asserted at boot and at
/// compile time. **At four claimable BLE sessions there is no link to
/// sell, so the BOOT-funded cap is 170 B.** What makes the board serve
/// is not this number but the live one
/// ([`live_serve_cap`]): the heap the T114 actually
/// stood on funds 7 240 B after the margin, which is all 24 of the
/// field's messages in one fetch. Raising this reserve would buy a
/// bigger floor, not a bigger serve, and it would cost a link.
pub const SERVE_RESERVE_BYTES: usize = 0;

/// Peak heap this board holds live AT ONCE while it serves one `/get`
/// fetch at the announced [`BOARD_SYNC_LIMIT_KB`] — the term the role
/// budget below was missing, and the one a T114 died in on 2026-09-22.
///
/// Its own record: `PN_GET dst=… form=fetch count=24 bytes=5376
/// purged=0` as the last line of one boot, then
/// `PANIC_PMRT … "memory allocation of 5446 bytes failed"` on the next.
/// The request arrived, reassembled and was served; the board died
/// between the serve and the response, and the Python peer saw a
/// timeout, not a crash.
///
/// The 5 446 is arithmetic, not coincidence. Twenty-four bodies of
/// 224 B encode to a 5 427 B `MessageGetResponse`, and 5 427 + 19 B of
/// request/response framing is 5 446: the board died on `packed` in
/// `NodeCore::send_response` — the framed copy that path built BEFORE
/// it compared the result with the link MDU and threw away when it did.
/// It never reached the Resource fallback that would have cost it
/// several more. That allocation is gone (#384 B1): the MDU check now
/// runs on a computed length and a refusal allocates nothing.
///
/// The arithmetic lives at [`serve_peak_bytes`], beside the serve path
/// it describes, and is pinned by a measured mvr
/// (`one_fetch_serve_holds_many_copies_of_its_own_response`,
/// `leviculum-std/tests/mvr/pn_serve_peak_outgrows_the_board_heap.rs`),
/// which counts requested bytes through the real path. It measured
/// 51 662 B live for that 5 427 B response before #384 B1, 29 842 B
/// after it, and since B2 streamed the response out of the store the
/// path holds no whole copy of it at all: the transfer, one record and
/// one part's scratch.
///
/// **[`ROLE_BUDGET_BYTES`] below funds [`SERVE_RESERVE_BYTES`] of this,
/// which is zero, and that is the open decision, not an oversight** —
/// see the arithmetic there. What the plan does fund is the bound the
/// serve is held to ([`budget_serve_cap`]), so the
/// board answers with what it can afford instead of attempting this
/// number.
pub const SERVE_PEAK_BYTES: usize = serve_peak_bytes(SYNC_SLICE_BYTES, SERVE_RESOURCE_SDU);

/// The role's slice of the boot heap budget (`HEAP_BUDGET role=`, #388):
/// the worst case of the census's `pn_*` terms. Arithmetic:
///
/// * [`SYNC_SLICE_BYTES`] — one sync's transient: an inbound batch's
///   parsed messages (`pn_batch`, bounded by the announced
///   [`BOARD_SYNC_LIMIT_KB`], which is derived to fit here) or the
///   streamed answer to one outbound offer;
/// * `pn_work` — queued upload payloads, one per phone at the announced
///   [`BOARD_TRANSFER_LIMIT_KB`], two phones (the #388 field scenario);
/// * 5 KiB — everything structural: the peer table (§5's `104·16` plus
///   the 64 B of public keys each peer now keeps (#388 pass 3, ~1 KiB
///   at `BOARD_MAX_PEERS` — grown from the previous 4 KiB term
///   because the keys did not fit its slack), the offer plan
///   (`pn_out`), the per-link maps, the role's duplicate cache and the
///   store adapters' queued writes (`pn_flush`).
///
/// * [`SERVE_RESERVE_BYTES`] — what the plan sets aside for ANSWERING a
///   `/get`, the one term that pays for what the board sends. **Zero
///   today**, and the boot line says what that funds
///   (`HEAP_BUDGET … serve_cap=`).
///
/// The fourth term is in this sum since #388/#384 and is zero, which is
/// a different statement from the one it replaced: the transient exists,
/// the plan funds none of it, and the serve is now BOUNDED by what the
/// plan funds instead of attempting the announced cap and dying in it.
/// Every other term above bounds something the board *receives* or
/// *holds*; nothing bounded what it costs to *answer* a `/get`, and
/// answering one killed a T114 on 2026-09-22 and again on 2026-09-23.
/// The arithmetic, at today's constants on a T114:
///
/// * serve cap at the role's sync slice [`SYNC_SLICE_BYTES`] = 8 KB (the
///   announced [`BOARD_SYNC_LIMIT_KB`] bounds the serve below that since
///   order 511, so this is an upper bound, kept for comparison);
/// * [`SERVE_PEAK_BYTES`] at that cap = **17 264 B**, a bit over twice
///   the cap (97 036 B before #384 B1, 48 922 B after it; B2 streamed
///   the response out of the store and the four whole copies between
///   the store and the advertisement stopped existing);
/// * what the plan leaves for it = [`SERVE_RESERVE_BYTES`] plus
///   [`budget_slack`], **656 B** on the T114 — the
///   remainder after `max_endpoint_links` floors its division, and it
///   cannot grow without taking a link away from
///   [`MAX_LINKS`];
/// * deficit against the full announced cap = **16 608 B**;
/// * so the BOOT-funded fetch response,
///   [`budget_serve_cap`], is **170 B** — about one
///   minimal message, and that is what `serve_cap=` on the boot line
///   says. It is a floor, not the serve: every `/get` re-reads the cap
///   from the heap the board HAS
///   ([`live_serve_cap`]), and the 30 380 B that
///   T114 stood on funds 7 240 B after the margin — all 24 of the
///   field's messages in one fetch, where the buffered path funded nine.
///
/// Funding a bigger FLOOR still means taking bytes from a term above,
/// from the link count, or from the reserve, and it would not change
/// what the board serves in the field. That decision is the Lead's;
/// [`SERVE_RESERVE_BYTES`] is where it is made, and the boot line
/// reports the gap (`HEAP_BUDGET … serve= slack= serve_cap=`, and the
/// `HEAP_BUDGET_UNFUNDED` line beside it) until it is.
pub const ROLE_BUDGET_BYTES: usize = SYNC_SLICE_BYTES
    + 2 * BOARD_TRANSFER_LIMIT_KB as usize * 1000
    + ROLE_STRUCTURAL_BYTES
    + SERVE_RESERVE_BYTES;

/// The third term of [`ROLE_BUDGET_BYTES`]: everything structural the role
/// holds (peer table, offer plan, per-link maps, duplicate cache, queued
/// store writes). Named so the per-operation peaks ([`peak`]) can say what
/// the budget leaves for a transient once this and the queued uploads are
/// spent.
pub const ROLE_STRUCTURAL_BYTES: usize = 5 * 1024;

/// The board peer-table cap (`PeeringConfig::max_peers`,
/// `leviculum-nrf/src/pn.rs`), §5's derivation
/// (`docs/src/concepts/propagation-node-on-a-board.md`, "Our peer record,
/// and the cap") re-checked against the worst observed free heap (39 308 B,
/// Pocket, end of the 12.69 h field run): an 8 KiB peering slice gives
/// `104·N + 6144 ≤ 8192`, N ≤ 19, and 16 keeps margin for the sync round's
/// own buffers. Asserted below rather than stated in a comment, so moving
/// the cap or a term of it is a compile error until the derivation agrees.
///
/// The 104 B is §5's persisted record. What the table costs in RAM by the
/// census's own model is [`peak::peer_table_bytes`], which is larger; the
/// test `the_peer_table_at_the_cap_against_its_slice` prints both.
pub const BOARD_MAX_PEERS: usize = 16;

/// §5's per-peer record, aligned (`propagation-node-on-a-board.md`).
pub const PEER_RECORD_BYTES: usize = 104;

/// §5's peering slice of the heap.
pub const PEERING_SLICE_BYTES: usize = 8 * 1024;

const _: () = assert!(
    PEER_RECORD_BYTES * BOARD_MAX_PEERS + leviculum_lxmf::peering::OFFER_BYTES_LIMIT
        <= PEERING_SLICE_BYTES
);
// "N ≤ 19": the cap is below the largest N the slice admits, not at it.
const _: () = assert!(
    (PEERING_SLICE_BYTES - leviculum_lxmf::peering::OFFER_BYTES_LIMIT) / PEER_RECORD_BYTES == 19
);

/// Fragmentation reserve in the boot heap budget (#388): heap the
/// budget leaves unclaimed so the allocator can still serve mid-sized
/// blocks when the free list is cut up. The number to reason from is
/// the census's `largest=`: at the rig baseline (56 K used) the loss
/// was 41 B (`free=41632 largest=41591`), while the field failure
/// refused 340 B with 4 760 free — fragmentation loss grows with fill,
/// so the reserve is sized at roughly the largest single event-path
/// allocation (an MTU packet plus its copies) times a churn factor,
/// not at the idle loss. Re-derive from `largest=` under two-phone
/// load when the field census arrives.
const FRAG_RESERVE_BYTES: usize = 6 * 1024;

/// One 11-slot B-tree node of the boxed link table, amortised per link
/// in the budget's `per_link=` term (the value is a `Box` pointer since
/// #388; the `Link` blocks are counted separately).
const LINK_MAP_BYTES_PER_LINK: usize = 64;

/// The budget's `per_link=` term: what EVERY endpoint link costs
/// regardless of carrier — one boxed `Link` plus its link-table node
/// share. A link's BLE session (queue, pump, defragmenter) exists only
/// for links riding a GATT connection and is budgeted separately, once
/// per claimable session ([`MAX_LINKS`] ×
/// [`SESSION_BUDGET_BYTES`]) — a LoRa-backed link has no BLE
/// session. `const fn`: the binaries also assert the whole sum at
/// compile time against their concrete `NodeCore`.
pub const fn budget_per_link() -> usize {
    LINK_BYTES + LINK_MAP_BYTES_PER_LINK
}

/// The budget's `reserve=` term (fragmentation + shared BLE channels +
/// one incoming resource at the binaries' cap).
pub const fn budget_reserve() -> usize {
    FRAG_RESERVE_BYTES + CHANNEL_BUDGET_BYTES + MAX_INCOMING_RESOURCE_BYTES
}

/// How many endpoint links a node box of `node_box` bytes affords — the
/// `links=` term of the boot line, the value the binaries hand to
/// `NodeCoreBuilder::max_links`, and the count [`budget_total`] sums, so
/// the enforced cap and the budget cannot drift (#388 pass 3).
///
/// Derivation: everything that is not a per-link cost is fixed —
/// `node_box`, the role, the reserve, and the
/// [`MAX_LINKS`] BLE sessions (budgeted whether or not a
/// link currently rides them, because a claimable GATT connection can
/// fill its queues). What remains of [`HEAP_SIZE`] is divided by
/// [`budget_per_link`], the carrier-independent cost of one more link.
/// With today's numbers (T114: node box 31 040 B, role 21 120 B,
/// reserve 18 848 B, sessions 4 × 3 948 B, per-link 2 712 B) the
/// division yields 4 — the heap affords exactly the BLE-session count,
/// and no extra LoRa-backed links until a fixed term shrinks. The
/// binaries assert `>=` [`MAX_LINKS`]: a node box grown past
/// that line is a budget violation at compile time, not a dead session
/// in the field.
pub const fn max_endpoint_links(node_box: usize) -> usize {
    let fixed = node_box + ROLE_BUDGET_BYTES + budget_reserve() + MAX_LINKS * SESSION_BUDGET_BYTES;
    if fixed >= HEAP_SIZE {
        0
    } else {
        (HEAP_SIZE - fixed) / budget_per_link()
    }
}

/// The budget's `total=` for a node box of `node_box` bytes — the one
/// sum both the boot line and the binaries' compile-time assertions
/// use, so they cannot drift. `<=` [`HEAP_SIZE`] holds by
/// construction of [`max_endpoint_links`]; the assertions keep it as a
/// belt against a future edit decoupling the two.
pub const fn budget_total(node_box: usize) -> usize {
    node_box
        + max_endpoint_links(node_box) * budget_per_link()
        + MAX_LINKS * SESSION_BUDGET_BYTES
        + ROLE_BUDGET_BYTES
        + budget_reserve()
}

/// What the budget leaves unclaimed on a node box of `node_box` bytes:
/// the remainder after [`max_endpoint_links`] floors its division. It is
/// NOT spare capacity to spend — one more link costs
/// [`budget_per_link`], so the slack is what is left over after the last
/// affordable link, and spending it on anything else is spending the
/// allocator's elbow room.
///
/// Named because the propagation role's unfunded outbound serve
/// transient ([`SERVE_PEAK_BYTES`]) is measured against it:
/// on a T114 this is 656 B against a 17 264 B need (it was 880 B against
/// 48 922 B before the serve was streamed, #384 B2).
pub const fn budget_slack(node_box: usize) -> usize {
    HEAP_SIZE.saturating_sub(budget_total(node_box))
}

/// The largest fetch response this plan funds — `serve_cap=` on the boot
/// line, and the FLOOR the role is configured with
/// (`PropagationNodeConfig::serve_cap_bytes`, set in
/// `leviculum_nrf::pn::Engine::new` from this same arithmetic, so the printed
/// number and the enforced one cannot drift). A floor since #388 order
/// 138 and not the last word: every `/get` re-reads the cap from the
/// heap the board has at that moment ([`live_serve_cap`]), which is the
/// difference between listing 24 messages and serving none of them and
/// serving nine of them.
///
/// The inverse of [`SERVE_PEAK_BYTES`]'s model
/// ([`serve_cap_for_peak`]), asked of the heap the plan actually leaves
/// for serving: [`budget_slack`] — the remainder after
/// [`max_endpoint_links`] floors its division — plus
/// [`SERVE_RESERVE_BYTES`], the plan's deliberate serve
/// reservation, which is zero until the Lead funds it.
///
/// The response buffer is not added on top, and since #384 B2 there is
/// no response buffer: the model's terms are the record being streamed,
/// the builder's one-part scratch and the transfer itself
/// (`serve_peak_bytes`, `leviculum-lxmf/src/propagation_node.rs`). The
/// `plaintext` block the T114 died in on 2026-09-23 (5 450 B = 4 + 5 427
/// + 19) is one of the four whole copies that stopped existing.
///
/// At the slack this board prints today, 656 B, this is **170 B**: about
/// the smallest LXMF message the store takes, and still nothing like a
/// field one (224 B of body accounts to 296 B). It was 88 B before B2.
/// That is the honest number, not a failure of the arithmetic — and it
/// is a FLOOR: what makes the board serve is [`live_serve_cap`], which
/// reads the heap the board has.
pub const fn budget_serve_cap(node_box: usize) -> usize {
    serve_cap_for_peak(
        budget_slack(node_box) + SERVE_RESERVE_BYTES,
        SERVE_RESOURCE_SDU,
    )
}

/// Heap kept clear of the serve transient because it can be claimed
/// WHILE the serve is in flight — the margin of
/// [`serve_cap_for_live_heap`], and the reason a live reading of the
/// free heap is not simply spent.
///
/// Serving one `/get` is not atomic: the cap is read when the request
/// is taken off the work queue, and the path then holds the transfer
/// and the record it is streaming live (`serve_peak_bytes`,
/// `leviculum-lxmf`) across several awaits. Anything the board admits
/// during that window allocates beside them. The margin is the sum of the
/// transients the census already prices and the engine can start in
/// that window, each at the size its own announce or budget bounds it
/// to:
///
/// ```text
///   SYNC_SLICE_BYTES                 8 000 B   one inbound sync batch
/// + BOARD_TRANSFER_LIMIT_KB · 1000   4 000 B   one queued upload
/// + budget_per_link()                2 712 B   one more endpoint link
/// =                                 14 712 B
/// ```
///
/// All three can coexist: the batch arrives from a peer over one link,
/// the upload from a phone over another, and a third peer may connect
/// while both are in progress — which is the [`MAX_LINKS`]
/// = 4 shape the field boards run in. They are summed rather than
/// maximised for that reason.
///
/// What is NOT in it: anything the board holds ACROSS the window rather
/// than claiming inside it (the node box, the peer table, the store
/// adapters). Those are already spent when the free heap is read, so
/// counting them again would charge the serve twice.
pub const SERVE_MARGIN_BYTES: usize =
    SYNC_SLICE_BYTES + BOARD_TRANSFER_LIMIT_KB as usize * 1000 + budget_per_link();

// Pinned because the derivation above is the board's whole serve
// decision: a change to `Link`, to an announced limit or to the sum
// itself moves what one fetch may serve. The host-side mvrs read this
// number from here (`leviculum-std/tests/mvr/pn_serve_cap_bounds_one_fetch.rs`
// and `…/pn_serve_cap_survives_a_shrinking_heap.rs`), but their expected
// served counts are calibrated at it. Moving it legitimately: move the
// pin, re-check those counts, and say what the board serves now.
const _: () = assert!(SERVE_MARGIN_BYTES == 14_712);

/// The cap one fetch may serve to, from the heap the board HAS rather
/// than the heap its boot plan feared (#388, order 138).
///
/// [`budget_serve_cap`] is a worst case: every term of the plan at its
/// maximum at once, which on a T114 leaves 656 B of slack and funds a
/// 170 B response — about one minimal message, nothing like a field one.
/// The heap that board actually had when it died was 30 380 B free
/// (`[HEAP_CENSUS] … free=30380 largest=30320`,
/// `lora_pn_board_offer_past_the_link`, 2026-09-23 08:08 UTC), and under
/// the streamed serve (#384 B2) that funds **7 240 B — all 24** of the
/// messages it was refusing to serve, in one fetch. Before B2 the same
/// heap funded 2 540 B and nine of them.
///
/// `free` and `largest` are the census's own two figures and both bind:
/// the summed model must fit the bytes, and every term that is one
/// allocation must fit the largest block the allocator can still hand
/// out. A heap with the bytes in pieces funds no serve, and only
/// `largest=` can say so.
///
/// [`SERVE_MARGIN_BYTES`] comes off both before either is spent.
pub fn live_serve_cap(boot_cap: usize, free: usize, largest: usize) -> usize {
    serve_cap_for_live_heap(
        boot_cap,
        free,
        largest,
        SERVE_MARGIN_BYTES,
        SERVE_RESOURCE_SDU,
    )
}

// The per-operation transient peaks (#388, order 510). Declared last so
// the line numbers the book cites above stay where they are.
pub mod peak;
pub use peak::{
    flush_peak, inbound_resource_peak, offer_response_fits, offer_response_peak, role_margin,
    store_tail_peak, sync_round_peak, upload_peak, FIELD_FAILED_ALLOCATION_BYTES,
};

/// The role's slice for one sync's transient (#388, order 511): an inbound
/// batch (`pn_batch=`) or the answer to one outbound offer, never both at
/// once on the same round. The first term of [`ROLE_BUDGET_BYTES`], and
/// what is left of the role's headroom once two phones' queued uploads are
/// spent. 8 000 B: the stated sync limit it replaces, so the boot budget,
/// the link count and [`SERVE_MARGIN_BYTES`] do not move; what moves is
/// the announced limit, which is now derived to fit inside it.
pub const SYNC_SLICE_BYTES: usize = 8_000;

/// The bound one `/get` fetch response is held to on a board
/// (`PropagationNodeConfig::serve_limit_bytes`), beside the heap-funded
/// cap: the role's sync slice, which is what the stated 8 KB sync limit
/// bounded the serve to before order 511 derived the announced limit from
/// the outbound answer. Kept apart so a smaller announcement does not
/// shrink what one client fetch carries (the field mailbox of 24 messages
/// accounts to 6 552 B and drains in one fetch,
/// `leviculum-std/tests/mvr/pn_serve_cap_bounds_one_fetch.rs`).
pub const BOARD_SERVE_LIMIT_BYTES: usize = SYNC_SLICE_BYTES;

/// Heap an offer response leaves clear of the role's headroom: what its
/// model ([`peak::offer_response_peak`]) does not count, plus the field's
/// refused allocation.
///
/// ```text
///   FIELD_FAILED_ALLOCATION_BYTES            340 B  the next event's allocation
/// + 3 × (RESOURCE_ADV_OVERHEAD + 4 × parts)  534 B  the cached advertisement,
///                                                   its send copy and the
///                                                   encrypted packet, 11 parts
/// + 16 blocks × 8 B                          128 B  allocator block headers
/// =                                        1 002 B  ≤ 1 024
/// ```
///
/// The 11 parts are those of an answer at the derived limit; the test
/// `the_offer_response_reserve_covers_what_the_model_leaves_out` re-adds
/// the sum at [`BOARD_SYNC_LIMIT_KB`] so a limit that grows past it fails.
pub const OFFER_RESPONSE_RESERVE_BYTES: usize = 1_024;

/// Ids one `/offer` carries on a LoRa link, the field's shape: what fits
/// the standard link MDU (431 B) less the request envelope, the peering
/// key and the list header (`offer_budget_for_mdu`, `leviculum-lxmf`).
pub const LORA_OFFER_IDS: usize = (leviculum_core::resource::STANDARD_LINK_MDU
    - leviculum_lxmf::peering::REQUEST_ENVELOPE_BYTES
    - leviculum_lxmf::peering::OFFER_KEY_BYTES
    - 1)
    / leviculum_lxmf::peering::OFFER_ID_BYTES;

/// [`BOARD_SYNC_LIMIT_KB`]'s derivation: the largest whole kilobyte count
/// `kb` for which an answer of `kb × 1000` encoded bytes, to a LoRa offer
/// of [`LORA_OFFER_IDS`] ids answered with an explicit list of all of them
/// (the larger of the two response shapes), fits
/// ([`peak::offer_response_fits`]) beside two phones' queued uploads at the
/// announced per-transfer limit.
///
/// The announced limit bounds the wire resource a peer sends (the
/// reference's packing loop stops strictly below it, see the assertion
/// after [`BOARD_SYNC_LIMIT_KB`]) and so the inbound batch; deriving it
/// from the answer makes the board announce only what it could also send
/// back in one round. Board peers plan their offers to it (`build_offer`);
/// a Python peer, whose limit is the reference's 10 240 KB, is answered
/// with the prefix of its want that fits the heap at that moment
/// (`plan_sync_batch`), the rest on the next round.
pub const fn derive_sync_limit_kb() -> u64 {
    let two_uploads = 2 * BOARD_TRANSFER_LIMIT_KB as usize * 1000;
    let mut kb = SYNC_SLICE_BYTES / 1000;
    while kb > 0 {
        if peak::offer_response_fits(
            kb * 1000,
            LORA_OFFER_IDS,
            LORA_OFFER_IDS,
            SERVE_RESOURCE_SDU,
            two_uploads,
        ) {
            return kb as u64;
        }
        kb -= 1;
    }
    0
}

// A message at the announced per-transfer limit must be syncable in one
// round, or a Python peer keeps it unhandled forever: its packing loop
// drops a message over the transfer limit as handled (`LXMPeer.py:370-373`)
// but merely skips one whose `24 + size + 16` reaches the sync limit
// (`:375-376`), every round, without marking it.
const _: () = assert!(
    BOARD_SYNC_LIMIT_KB * 1000
        > BOARD_TRANSFER_LIMIT_KB * 1000
            + leviculum_lxmf::peering::OFFER_BASE_SIZE
            + leviculum_lxmf::peering::OFFER_PER_MESSAGE_OVERHEAD
);
// And the inbound batch it bounds must fit the slice it is held in.
const _: () = assert!(BOARD_SYNC_LIMIT_KB as usize * 1000 <= SYNC_SLICE_BYTES);

// The held-announce hold (Codeberg #598). Declared after everything the
// book cites by line, like `peak`, so those citations stay where they are.

/// Size of one held-announce entry (`HeldAnnounce` in
/// `leviculum-core/src/transport.rs`: the raw announce's `Vec` header, the
/// hop count and the peer it came through) on `thumbv7em-none-eabihf`.
/// Pinned for the reason [`LINK_BYTES`] is: on the host it is 48. The
/// firmware asserts `leviculum_core::transport::HELD_ANNOUNCE_ENTRY_BYTES`
/// against it (`leviculum-nrf/src/heap_census.rs`).
pub const HELD_ANNOUNCE_ENTRY_BYTES: usize = 32;

/// The raw announce one held entry pins, at its bound: the largest packet
/// the core accepts. The announces held on the rig ran 167 to 183 B (589's
/// btvirt runs), and app data takes one up to the MTU, which a burst does
/// not choose against.
pub const HELD_ANNOUNCE_RAW_BYTES: usize = leviculum_core::constants::MTU;

/// Interfaces a board registers (`leviculum_nrf::iface_bytes::IFACE_COUNT`:
/// USB, LoRa, BLE). The firmware asserts the two agree.
pub const BOARD_INTERFACES: usize = 3;

/// Whether the ingress burst limiter runs on each of the board's interfaces,
/// indexed by interface id (`leviculum_nrf::iface_bytes`: 0 USB serial,
/// 1 LoRa, 2 BLE). The binaries register every interface with its entry
/// (`leviculum_nrf::iface_bytes::apply_board_ingress_control`), so the flag the core
/// enforces and the count the hold is budgeted for are this one table.
///
/// The USB serial interface is off (Codeberg #606). It is a point-to-point
/// link to the one host daemon behind it: no peer can storm it, and every
/// announce the host relays into the board arrives there. With the limiter
/// on, a host that has just learned a dozen paths made the board hold
/// [`BOARD_MAX_HELD_ANNOUNCES`] of them and refuse the rest, and a refused
/// stationary destination is learned again only on its next announce, an
/// hour or more away. `lnsd` turns the limiter off for the same reason on
/// its point-to-point links: the dial-out IP types
/// (`leviculum_std::config::ingress_control_default_for_type`) and the
/// shared-instance IPC client (`leviculum-std/src/interfaces/local.rs`,
/// `ingress_control: Some(false)`, after Python's
/// `LocalClientInterface.should_ingress_limit() -> False`). LoRa and BLE
/// are shared media with arbitrary peers, the surface the limiter exists
/// for, and keep it on.
pub const BOARD_INGRESS_CONTROL: [bool; BOARD_INTERFACES] = [false, true, true];

/// The board interfaces that can hold announces: those
/// [`BOARD_INGRESS_CONTROL`] leaves the limiter on for. An interface with it
/// off never reaches the hold, so the hold's worst case is every one of
/// these full at once.
pub const BOARD_HOLDING_INTERFACES: usize = count_holding_interfaces();

const fn count_holding_interfaces() -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < BOARD_INTERFACES {
        if BOARD_INGRESS_CONTROL[i] {
            n += 1;
        }
        i += 1;
    }
    n
}

/// Heap the hold pins with `per_iface` announces held on each of `ifaces`
/// interfaces, every raw announce at [`HELD_ANNOUNCE_RAW_BYTES`], by the
/// census's B-tree model on the target's 4-byte words (as
/// [`peak::peer_table_bytes`]): the per-interface map's nodes keyed by a
/// 16-byte destination, the raw blocks, and the outer map from an
/// interface index to that map (a 12-byte `BTreeMap` header).
pub const fn held_announce_bytes(per_iface: usize, ifaces: usize) -> usize {
    const NODE_CAPACITY: usize = 11;
    const NODE_TYPICAL_FILL: usize = 7;
    const WORD: usize = 4;
    const fn nodes(len: usize, entry: usize) -> usize {
        len.div_ceil(NODE_TYPICAL_FILL) * (2 * WORD + NODE_CAPACITY * entry)
    }
    if per_iface == 0 || ifaces == 0 {
        return 0;
    }
    let per_queue =
        nodes(per_iface, 16 + HELD_ANNOUNCE_ENTRY_BYTES) + per_iface * HELD_ANNOUNCE_RAW_BYTES;
    nodes(ifaces, WORD + 3 * WORD) + ifaces * per_queue
}

/// Heap the board lets the hold take, and the decision as a number the
/// Lead can move: [`BOARD_MAX_HELD_ANNOUNCES`] is derived from it.
///
/// **Not in [`budget_total`], and the boot plan funds none of it.** The
/// plan's slack is [`budget_slack`], 656 B on a T114, and one interface's
/// first held announce costs 1 220 B (a 536 B map node, the 184 B outer
/// node, a 500 B raw announce). Putting this slice into the sum would cost
/// three of the four endpoint links the heap affords, below [`MAX_LINKS`].
/// So it is held to the heap the board HAS, as the serve is
/// ([`live_serve_cap`]), and it is sized so that on the tightest heap the
/// rig has recorded it leaves both of what the live heap must keep clear:
///
/// ```text
///   free at the 2026-09-23 T114 panic (two phones + role)  30 380 B
/// − SERVE_MARGIN_BYTES        claimed while a /get is in   14 712 B
/// − FRAG_RESERVE_BYTES        the allocator's elbow room    6 144 B
/// =                           what a hold may take          9 524 B
/// ```
///
/// 8 KiB is under that with 1.3 KiB to spare. What it buys is
/// [`BOARD_MAX_HELD_ANNOUNCES`] = 6 per interface at the MTU bound
/// (7 256 B for both holding interfaces full, LoRa and BLE; the USB serial
/// interface holds none, [`BOARD_INGRESS_CONTROL`]); at the rig's 183 B
/// announces the same cap costs 3 452 B. While all three interfaces held,
/// the same slice bought 4 (7 792 B).
///
/// Why 6 is enough: a burst holds one slot per NEW destination (a repeat
/// replaces its slot), held announces are released one per
/// `IC_HELD_RELEASE_INTERVAL` (5 s) once the burst calms, and an announce
/// refused at the cap is the one Python also refuses when its own queue is
/// full: its destination is learned from its next announce, or at once by
/// a path request, which bypasses the hold. Against that, the core's
/// default of 256 is 148 016 B on ONE interface at the MTU bound, more
/// than the heap, and 66 864 B at the rig's 183 B announces, two thirds
/// of it.
pub const HELD_ANNOUNCE_SLICE_BYTES: usize = 8 * 1024;

/// The board's hold cap per interface (`NodeCoreBuilder::max_held_announces`
/// in the binaries): the largest count whose worst case on every
/// holding interface ([`BOARD_HOLDING_INTERFACES`]) fits
/// [`HELD_ANNOUNCE_SLICE_BYTES`].
pub const BOARD_MAX_HELD_ANNOUNCES: usize = derive_max_held_announces();

/// [`BOARD_MAX_HELD_ANNOUNCES`]'s derivation, by search, as
/// [`derive_sync_limit_kb`].
pub const fn derive_max_held_announces() -> usize {
    let mut cap = 0;
    while held_announce_bytes(cap + 1, BOARD_HOLDING_INTERFACES) <= HELD_ANNOUNCE_SLICE_BYTES {
        cap += 1;
    }
    cap
}

// Pinned, so a change to the entry, the raw bound, the interface count, the
// ingress table or the slice moves these numbers in the diff, not silently
// on the board.
// Moving them legitimately: move the pin and say what the board holds now.
const _: () = assert!(
    !BOARD_INGRESS_CONTROL[0],
    "USB serial holds no announce (#606)"
);
const _: () = assert!(BOARD_HOLDING_INTERFACES == 2);
const _: () = assert!(BOARD_MAX_HELD_ANNOUNCES == 6);
const _: () =
    assert!(held_announce_bytes(BOARD_MAX_HELD_ANNOUNCES, BOARD_HOLDING_INTERFACES) == 7_256);
// The slice must leave what the live heap keeps clear on the recorded
// worst heap (the arithmetic at `HELD_ANNOUNCE_SLICE_BYTES`).
const _: () =
    assert!(HELD_ANNOUNCE_SLICE_BYTES + SERVE_MARGIN_BYTES + FRAG_RESERVE_BYTES <= 30_380);
