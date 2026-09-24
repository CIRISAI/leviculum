//! mvr: the field's whole mailbox is served in ONE fetch, out of the
//! store, on the heap the board actually had.
//!
//! **The named failure mode:** a client lists 24 messages, asks for all
//! 24, and the board dies building the answer. Its own record from the
//! 648650a3 full run, 2026-09-23 08:08 UTC, T114 `DEC9947D` serving
//! `lora_pn_board_offer_past_the_link`:
//!
//! ```text
//! [HEAP_CENSUS] used=67924 free=30380 largest=30320 …
//! PN_GET dst=c5aa516a… form=list  count=24 bytes=0    purged=0   t=606288
//! [PANIC_PMRT] memory allocation of 5450 bytes failed
//! PN_GET dst=c5aa516a… form=fetch count=24 bytes=5376 purged=0   t=612441
//! [HEAP] HEAP_BUDGET … total=97520 slack=784 serve=48922
//! [HEAP] HEAP_BUDGET_UNFUNDED serve=48922 slack=784 deficit=48138
//! ```
//!
//! The 5 450 B was the resource path's `plaintext` copy
//! (`RESOURCE_RANDOM_HASH_SIZE + response + frame`, and
//! `5450 = 4 + 5427 + 19`) — one of four whole copies of the answer the
//! path held before it had cut a single part.
//!
//! **Two orders, two different answers.** #384 B1 removed four copies
//! and B2/order 141 bounded what was left: a fetch the heap could not
//! fund was served in part, nine messages of the 24, and the rest next
//! round. That was correct and it was not the feature — a propagation
//! node whose client needs three syncs to collect one evening's mail is
//! a node the field notices. **This order removes the materialisation
//! instead of the mail**: the role hands over a [`FetchPlan`] and the
//! resource builder reads the records out of the store as it cuts the
//! parts, so the only whole copy that ever exists is the transfer
//! itself.
//!
//! **What this test fixes in place:** the free heap at the moment the
//! census measured it ([`FREE_AT_PANIC`]), and what it funds under each
//! model. The same 24-message fetch that modelled at 33 238 B of
//! transient — 2 858 B more than the board had, which is the panic in
//! arithmetic — models at 11 875 B streamed and MEASURES at 7 154 B
//! through the real path
//! (`a_streamed_serve_holds_only_the_transfer`,
//! `pn_serve_peak_outgrows_the_board_heap.rs`). The cap the live heap
//! funds goes from 2 540 B (nine messages) to 7 256 B (all 24, with room
//! for two more), and the free heap a single-sync drain needs falls from
//! 54 848 B to 28 928 B — below the 30 380 B the board had.
//!
//! **Four tests, four rules.** The first is the field case: the board's
//! own heap, the field's own mailbox, one round. The second fixes the
//! MODEL's inverse on a mailbox deeper than any cap, because "serve what
//! fits and keep the rest" has to keep working for the mailbox that is
//! bigger than the heap however big the heap gets. The third fixes the
//! boot floor and the live raise. The fourth fixes what the bound costs
//! the CLIENT, which is the half a periculum cell asserts.
//!
//! **What it cannot prove:** that a real board survives it. Only a board
//! can, and only the rig can run one; what this fixes is the size of
//! what the board is allowed to try.

use leviculum_lxmf::propagation::{
    MessageGetRequest, MessageListResponse, PropagationUpload, TransferLimit, TransientId,
};
use leviculum_lxmf::propagation_node::{
    serve_buffered_peak_bytes, serve_cap_for_live_heap, serve_cap_for_peak,
    serve_largest_block_bytes, serve_peak_bytes, FetchPlan, GetOutcome, PropagationNode,
    PropagationNodeConfig, UploadOutcome,
};
use leviculum_lxmf::propagation_store::MemoryPropagationStore;

/// The board's free heap when its census measured it, 2026-09-23
/// 08:08 UTC, six seconds before the fetch that killed it
/// (`[HEAP_CENSUS] … free=30380`).
const FREE_AT_PANIC: usize = 30_380;

/// The largest single block that same allocator could still hand out at
/// that instant (`[HEAP_CENSUS] … largest=30320`). 60 B below `free`:
/// the T114's heap was barely fragmented, and the bound that bites here
/// is the summed one. It is read anyway, because the term that decides
/// a serve is one allocation and only this figure prices it.
const LARGEST_AT_PANIC: usize = 30_320;

/// What the BOOT plan funds on that same board (`HEAP_BUDGET … slack=808`
/// → `serve_cap=222`, `heap_census::budget_serve_cap`,
/// `leviculum-nrf`): 222 B, about the smallest LXMF message the store
/// takes and well under a field one, so a board held to it lists 24
/// messages and serves none of them. It was 88 B before the serve was
/// streamed; the floor moved a little, and it is still not what makes
/// the board serve.
const BOOT_CAP: usize = 222;

/// Heap the board keeps clear of the serve because it can be claimed
/// WHILE the serve is in flight (`SERVE_MARGIN_BYTES`,
/// `leviculum-nrf/src/heap_census.rs`): one inbound sync batch at
/// `BOARD_SYNC_LIMIT_KB` (8 000 B), one queued upload at
/// `BOARD_TRANSFER_LIMIT_KB` (4 000 B), and one more endpoint link at
/// `budget_per_link()` — 2 688 B in this tree. It was 2 680 B between
/// #384 B2, which shrank `Link` by the joined-ciphertext copy
/// `OutgoingResource` kept beside its parts, and the `frame_turnaround_ms`
/// a link now records off its first hop (#36/#374), which put the 8 B
/// back. All three can coexist, so they are summed.
///
/// Mirrored here as a literal for the same reason
/// [`BOARD_RESOURCE_SDU`] is: `leviculum-nrf` is a thumbv7em crate this
/// host cannot link. The firmware computes it from those three
/// constants; what this test pins is the rule, at the board's numbers.
const BOARD_SERVE_MARGIN_BYTES: usize = BOARD_SYNC_LIMIT_KB as usize * 1000 + 4 * 1000 + 2_688;

/// The link SDU the serve transient is sized against on a board
/// (`SERVE_RESOURCE_SDU`, `leviculum-nrf/src/pn.rs`): Reticulum's
/// smallest negotiated MTU less the resource per-part overhead.
const BOARD_RESOURCE_SDU: usize = 464;

/// The board announces this per-sync limit (`BOARD_SYNC_LIMIT_KB`,
/// `leviculum-nrf/src/pn.rs`). It bounds the wire response and did not
/// bite here: 24 messages account to 6 552 B, well under 8 000.
const BOARD_SYNC_LIMIT_KB: u64 = 8;

/// The field shape: 24 stored messages, 224 B of servable body each.
const FIELD_MESSAGES: u8 = 24;
const FIELD_BODY_BYTES: usize = 224;

/// A mailbox deeper than any cap this heap funds, for the two tests
/// about what happens when the mail does NOT fit. 80 messages against a
/// margin-free cap of 14 296 B (52 messages) and a margined one of
/// 7 256 B (26): both bite, and the difference between them is
/// measurable. The field's own 24 no longer are — which is the point of
/// this order and the reason these two tests stopped using them.
const DEEP_MAILBOX_MESSAGES: u8 = 80;

/// The board's whole heap (`HEAP_SIZE`, `leviculum-nrf/src/lib.rs:252`),
/// mirrored as a literal for the reason [`BOARD_RESOURCE_SDU`] is: that
/// crate is thumbv7em and this host cannot link it.
const BOARD_HEAP_BYTES: usize = 96 * 1024;

/// One stored message of exactly [`FIELD_BODY_BYTES`] servable bytes,
/// incompressible for the same reason as in
/// `pn_serve_peak_outgrows_the_board_heap`: a real LXMF body is
/// ciphertext.
fn upload(seed: u8) -> Vec<u8> {
    let mut lxmf_data = vec![7u8; 16];
    let mut state = 0x2545_F491_4F6C_DD1Du64 ^ u64::from(seed);
    while lxmf_data.len() < FIELD_BODY_BYTES {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        lxmf_data.extend_from_slice(&state.to_le_bytes());
    }
    lxmf_data.truncate(FIELD_BODY_BYTES);
    PropagationUpload::single(1_700_000_000.0, lxmf_data, [0xEE; 32]).encode()
}

/// A node holding `messages` messages for one mailbox, serving to `cap`
/// accounted bytes per fetch, announcing `sync_limit_kb`.
///
/// The announced limit is a parameter because it is a SECOND bound on
/// the same accounted sum, and the two tests below are about different
/// ones: the field case runs at the board's own 8 KB announcement (which
/// does not bite — 24 messages account to 6 552 B), while the
/// deeper-than-the-cap case has to raise it, or the announced limit
/// would be what bounds the round and the serve cap would never be
/// measured.
fn mailbox_node(
    cap: usize,
    messages: u8,
    sync_limit_kb: u64,
) -> (PropagationNode<MemoryPropagationStore>, Vec<TransientId>) {
    let mut role = PropagationNode::new(
        MemoryPropagationStore::new(256 * 1024),
        PropagationNodeConfig {
            sync_limit_kb,
            serve_cap_bytes: Some(cap),
            ..PropagationNodeConfig::default()
        },
    );
    let stored: Vec<TransientId> = (0..messages)
        .map(
            |seed| match role.handle_upload(&upload(seed), 0, |_, _| None) {
                UploadOutcome::Accepted { transient_id, .. } => transient_id,
                other => panic!("the store must accept the field shape: {other:?}"),
            },
        )
        .collect();
    (role, stored)
}

/// The field's node: [`FIELD_MESSAGES`] messages, serving to `cap`.
fn field_node(cap: usize) -> (PropagationNode<MemoryPropagationStore>, Vec<TransientId>) {
    mailbox_node(cap, FIELD_MESSAGES, BOARD_SYNC_LIMIT_KB)
}

/// Serve one fetch for every id in `stored` and report the plan.
fn fetch_all(
    role: &mut PropagationNode<MemoryPropagationStore>,
    stored: &[TransientId],
) -> FetchPlan {
    let GetOutcome::Fetch { plan, .. } = role
        .handle_get(&fetch_for(stored), &[7u8; 16], 0)
        .expect("the fetch is well formed")
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    plan
}

/// The client's request: every id it was just listed, with a transfer
/// limit far above anything the node would serve — the shape the Python
/// client sent the board (`count=24`).
fn fetch_for(wants: &[TransientId]) -> Vec<u8> {
    MessageGetRequest {
        wants: Some(wants.to_vec()),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(1000)),
    }
    .encode()
    .expect("the fetch is well formed")
}

/// The reference client's list request: no `wants`, no `haves`
/// (`message_list_response` is the callback for it,
/// `reference/LXMF/LXMF/LXMRouter.py:1562`).
fn list_request() -> Vec<u8> {
    MessageGetRequest {
        wants: None,
        haves: None,
        transfer_limit_kb: None,
    }
    .encode()
    .expect("the list request is well formed")
}

/// **The field case, and the red this order removes.** On the heap the
/// T114 actually stood on, the field's whole 24-message mailbox is
/// served in ONE fetch — where the same heap, the same messages and the
/// same margin funded nine before the response was streamed out of the
/// store.
///
/// The before and the after are computed side by side rather than taken
/// on trust: [`serve_buffered_peak_bytes`] is the pre-B2 model, kept
/// exact, so both numbers come from one run.
#[test]
fn the_field_mailbox_drains_in_one_fetch_where_the_buffered_serve_managed_nine() {
    let live_cap = serve_cap_for_live_heap(
        BOOT_CAP,
        FREE_AT_PANIC,
        LARGEST_AT_PANIC,
        BOARD_SERVE_MARGIN_BYTES,
        BOARD_RESOURCE_SDU,
    );
    assert_eq!(
        live_cap, 7_256,
        "the heap the board had funds 7 256 B of fetch response"
    );

    let (mut role, stored) = field_node(live_cap);
    let plan = fetch_all(&mut role, &stored);
    let peak = serve_peak_bytes(plan.encoded_len(), BOARD_RESOURCE_SDU);
    let block = serve_largest_block_bytes(plan.encoded_len(), BOARD_RESOURCE_SDU);
    let buffered = serve_buffered_peak_bytes(plan.encoded_len(), BOARD_RESOURCE_SDU);
    let budget = FREE_AT_PANIC - BOARD_SERVE_MARGIN_BYTES;

    eprintln!(
        "SERVE_ONE_ROUND live_cap={live_cap} margin={BOARD_SERVE_MARGIN_BYTES} \
free={FREE_AT_PANIC} largest={LARGEST_AT_PANIC} served={} of {FIELD_MESSAGES} \
response={} peak={peak} buffered_peak={buffered} block={block} budget={budget}",
        plan.count(),
        plan.encoded_len(),
    );

    // The feature: one round, the whole mailbox.
    assert_eq!(
        plan.count(),
        usize::from(FIELD_MESSAGES),
        "the field's mailbox must drain in one fetch"
    );
    assert_eq!(
        plan.served_bytes(),
        (usize::from(FIELD_MESSAGES) * FIELD_BODY_BYTES) as u64
    );
    assert_eq!(
        plan.encoded_len(),
        5_427,
        "the encoded response is the board's own PN_GET figure"
    );

    // The red, as arithmetic: the same response through the buffered
    // path costs more than the whole heap the board had, never mind the
    // margin. That is the 2026-09-23 panic.
    assert_eq!(buffered, 33_238);
    assert!(
        buffered > FREE_AT_PANIC,
        "the buffered serve of this response ({buffered} B) must not fit \
         the {FREE_AT_PANIC} B the board had -- it is why it died"
    );

    // The green: streamed, it fits with the margin still unspent.
    assert_eq!(peak, 11_875);
    assert!(
        peak + BOARD_SERVE_MARGIN_BYTES <= FREE_AT_PANIC,
        "a {} B response costs {peak} B of transient; with \
         {BOARD_SERVE_MARGIN_BYTES} B of concurrent arrivals that is \
         {} B against {FREE_AT_PANIC} B of heap",
        plan.encoded_len(),
        peak + BOARD_SERVE_MARGIN_BYTES
    );
    assert!(
        block + BOARD_SERVE_MARGIN_BYTES <= LARGEST_AT_PANIC,
        "the serve's largest single allocation is {block} B; the \
         allocator can hand out {LARGEST_AT_PANIC} B and the margin \
         claims {BOARD_SERVE_MARGIN_BYTES} B of it"
    );

    // And the client's confirmation empties the store, so the round is
    // a drain and not a peek.
    let confirm = MessageGetRequest {
        wants: None,
        haves: Some(plan.served_ids()),
        transfer_limit_kb: None,
    }
    .encode()
    .expect("the confirmation is well formed");
    let GetOutcome::Fetch { purged, .. } = role
        .handle_get(&confirm, &[7u8; 16], 0)
        .expect("the confirmation is well formed")
    else {
        panic!("a haves-only request is a fetch outcome");
    };
    assert_eq!(purged.len(), usize::from(FIELD_MESSAGES));
    assert!(role.store().is_empty());
}

/// A mailbox deeper than the cap is still served in part and finished
/// next round. The cap got much bigger; the rule did not change, and a
/// node that serves everything it is asked for is a node waiting for a
/// mailbox one message deeper than its heap.
#[test]
fn a_fetch_past_the_funded_cap_is_served_in_part_and_finished_next_round() {
    let cap = serve_cap_for_peak(FREE_AT_PANIC, BOARD_RESOURCE_SDU);
    let (mut role, stored) = mailbox_node(cap, DEEP_MAILBOX_MESSAGES, 64);
    let mailbox = [7u8; 16];

    // Round one: the client asks for all of them.
    let GetOutcome::Fetch { plan, purged } = role
        .handle_get(&fetch_for(&stored), &mailbox, 0)
        .expect("the fetch is well formed")
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    let served = plan.served_ids();

    eprintln!(
        "SERVE_CAP free={FREE_AT_PANIC} cap={cap} served={} of \
{DEEP_MAILBOX_MESSAGES} response={} peak={}",
        served.len(),
        plan.encoded_len(),
        serve_peak_bytes(plan.encoded_len(), BOARD_RESOURCE_SDU)
    );

    // The bound, on the wire: the response the node ships fits the cap
    // its heap plan funds.
    assert!(
        plan.encoded_len() <= cap,
        "served {} B against a funded cap of {cap} B",
        plan.encoded_len()
    );

    // The bound, in the heap: the transient that response costs fits the
    // heap the board had.
    let peak = serve_peak_bytes(plan.encoded_len(), BOARD_RESOURCE_SDU);
    assert!(
        peak <= FREE_AT_PANIC,
        "a {} B response costs {peak} B of transient, against \
         {FREE_AT_PANIC} B of heap",
        plan.encoded_len()
    );

    // Bounded serving, not refusal: it served what it could, and it is
    // neither one message nor all of them.
    assert!(
        served.len() > 1 && served.len() < usize::from(DEEP_MAILBOX_MESSAGES),
        "a bounded serve is a subset, not all and not nothing: {} of \
         {DEEP_MAILBOX_MESSAGES}",
        served.len()
    );
    assert_eq!(
        plan.served_bytes(),
        (served.len() * FIELD_BODY_BYTES) as u64
    );

    // Nothing was purged -- not what was served, and not what was not.
    // Deletion is the client's `haves` confirmation and nothing else
    // (`reference/LXMF/LXMF/LXMRouter.py:1622-1638`).
    assert!(purged.is_empty(), "a fetch must purge nothing by itself");
    assert_eq!(
        role.store().len(),
        usize::from(DEEP_MAILBOX_MESSAGES),
        "every message the round did not serve must still be in the store"
    );

    // Round two: the client confirms what it received and asks for the
    // rest, exactly as the reference client does -- `[None, haves]`
    // first (:1632-1638), then a fresh list and a fresh `wants`
    // (:1576-1596).
    let confirm = MessageGetRequest {
        wants: None,
        haves: Some(served.clone()),
        transfer_limit_kb: None,
    }
    .encode()
    .expect("the confirmation is well formed");
    let GetOutcome::Fetch { purged, .. } = role
        .handle_get(&confirm, &mailbox, 0)
        .expect("the confirmation is well formed")
    else {
        panic!("a haves-only request is a fetch outcome with an empty response");
    };
    assert_eq!(
        purged.len(),
        served.len(),
        "the confirmation purges exactly what was served"
    );

    let rest: Vec<TransientId> = stored
        .iter()
        .copied()
        .filter(|id| !served.contains(id))
        .collect();
    let GetOutcome::List { count, .. } = role
        .handle_get(&list_request(), &mailbox, 0)
        .expect("the list request is well formed")
    else {
        panic!("an empty request is the list form");
    };
    assert_eq!(count, rest.len(), "the next list offers exactly the rest");

    let plan_two = fetch_all(&mut role, &rest);
    assert_eq!(
        plan_two.count(),
        rest.len(),
        "two rounds must deliver all {DEEP_MAILBOX_MESSAGES}"
    );
    assert_eq!(
        served.len() + plan_two.count(),
        usize::from(DEEP_MAILBOX_MESSAGES),
        "nothing served twice, nothing lost"
    );
}

/// The cap the board serves to is the heap it HAS, not the heap its boot
/// plan feared — and the margin is what keeps that from being the panic
/// again.
///
/// The boot plan is every term at its maximum at once: 808 B of slack,
/// 222 B of funded response. A stored field message is 256 B stamped, so
/// a board held to the floor lists 24 messages and serves **none** of
/// them, which is what this test's first half measures. The heap that
/// board actually stood on funds 7 256 B — the whole mailbox.
///
/// **7 256, not 14 296.** The margin-free cap is what the same heap
/// funds with nothing left over, and
/// `pn_serve_cap_survives_a_shrinking_heap` measures what spending it
/// costs: the serve fits the instant it is computed and overruns the
/// heap the instant anything arrives.
#[test]
fn a_boot_cap_below_one_message_is_raised_by_the_heap_the_board_has() {
    // The floor: the boot plan's worst case, and nothing else.
    let (mut booted, stored) = field_node(BOOT_CAP);
    let boot_plan = fetch_all(&mut booted, &stored);
    assert!(
        boot_plan.is_empty() && boot_plan.encoded_len() < 8,
        "a 222 B cap cannot fit a 256 B message: served {} in {} B",
        boot_plan.count(),
        boot_plan.encoded_len()
    );
    assert_eq!(
        booted.encode_fetch(&boot_plan).unwrap().len(),
        boot_plan.encoded_len(),
        "an empty serve is still a well-formed, measured response"
    );

    // The rule: the larger of that floor and what the live census funds,
    // margin first.
    let live_cap = serve_cap_for_live_heap(
        BOOT_CAP,
        FREE_AT_PANIC,
        LARGEST_AT_PANIC,
        BOARD_SERVE_MARGIN_BYTES,
        BOARD_RESOURCE_SDU,
    );
    let (mut live, stored) = field_node(live_cap);
    let plan = fetch_all(&mut live, &stored);

    // The firmware's own line, at the board's own numbers
    // (`SERVE_CAP`, `Engine::read_serve_cap`, `leviculum-nrf/src/pn.rs`).
    eprintln!(
        "SERVE_CAP boot_cap={BOOT_CAP} live_cap={live_cap} \
margin={BOARD_SERVE_MARGIN_BYTES} largest={LARGEST_AT_PANIC} free={FREE_AT_PANIC} \
served={} of {FIELD_MESSAGES} response={}",
        plan.count(),
        plan.encoded_len(),
    );

    assert_eq!(live_cap, 7_256);
    assert_eq!(
        plan.count(),
        usize::from(FIELD_MESSAGES),
        "7 256 B of accounted cap is 24 B of preamble and 24 messages at \
         272 B each (24 + 24Â·272 = 6 552, and a 27th would be 7 368)"
    );
    assert!(
        live_cap < serve_cap_for_peak(FREE_AT_PANIC, BOARD_RESOURCE_SDU),
        "the margined cap must be strictly below what the bare heap funds"
    );

    // And it is a serve, not a refusal: the store keeps everything until
    // the client confirms.
    assert_eq!(
        live.store().len(),
        usize::from(FIELD_MESSAGES),
        "a fetch keeps every message until its confirmation"
    );
}

/// One client sync drains one serve cap, and since this order that is
/// the field's whole mailbox.
///
/// The bound the tests above pin is a bound on ONE fetch. What a stock
/// client sees is a bound on one SYNC, and they are the same number
/// because the reference router issues exactly one fetch per sync:
/// `message_list_response` builds `wants` from the list and sends a
/// single `MESSAGE_GET_PATH`
/// (`reference/LXMF/LXMF/LXMRouter.py:1576-1596`), and
/// `message_get_response` ingests whatever came back, confirms it with a
/// `[None, haves]` request, declares `PR_COMPLETE` and reports
/// `propagation_transfer_last_result = len(request_receipt.response)`
/// (`reference/LXMF/LXMF/LXMRouter.py:1622-1643`). There is no second
/// round inside a sync and no re-request of the remainder: the count the
/// caller reads is what that one fetch returned.
///
/// **Why this is written down here.** periculum's
/// `lora_pn_board_offer_past_the_link` fills a board's mailbox with 24
/// messages and closes on `lxmf_sync expect_count = 24` — one helper
/// call to `request_messages_from_propagation_node`
/// (`reference/LXMF/LXMF/LXMRouter.py:502`), one fetch, one count. On
/// the 648650a3 firmware the board died in that fetch and the client
/// reported `sync_failed_state_0xf2` (`PR_TRANSFER_FAILED`). Under order
/// 141's bound it would have answered 9 and the cell's step would have
/// had to change. It does not have to now: the cell's `expect_count =
/// 24` is the number this test measures.
#[test]
fn one_client_sync_drains_the_whole_field_mailbox() {
    let live_cap = serve_cap_for_live_heap(
        BOOT_CAP,
        FREE_AT_PANIC,
        LARGEST_AT_PANIC,
        BOARD_SERVE_MARGIN_BYTES,
        BOARD_RESOURCE_SDU,
    );
    let (mut role, _stored) = field_node(live_cap);
    let mailbox = [7u8; 16];

    // The client's own loop, one sync at a time: list what the node
    // holds, ask for all of it, ingest what came back, confirm it.
    let mut syncs = Vec::new();
    let mut drained = 0usize;
    while drained < usize::from(FIELD_MESSAGES) {
        let GetOutcome::List { response, count } = role
            .handle_get(&list_request(), &mailbox, 0)
            .expect("the list request is well formed")
        else {
            panic!("an empty request is the list form");
        };
        assert_eq!(
            count,
            usize::from(FIELD_MESSAGES) - drained,
            "the list offers everything the client has not confirmed"
        );
        let MessageListResponse::TransientIds(listed) = MessageListResponse::decode(&response)
            .expect("the node's list is the reference's list form")
        else {
            panic!("the node must list ids, not an error");
        };

        let GetOutcome::Fetch { plan, .. } = role
            .handle_get(&fetch_for(&listed), &mailbox, 0)
            .expect("the fetch is well formed")
        else {
            panic!("a fetch request must produce a fetch outcome");
        };
        let served = plan.served_ids();
        assert!(
            !served.is_empty(),
            "a sync that serves nothing never terminates: cap {live_cap} B"
        );

        // `message_get_response`'s confirmation, which is what deletes
        // on the node (:1622-1638). Until it lands the records stay.
        let confirm = MessageGetRequest {
            wants: None,
            haves: Some(served.clone()),
            transfer_limit_kb: None,
        }
        .encode()
        .expect("the confirmation is well formed");
        let GetOutcome::Fetch { purged, .. } = role
            .handle_get(&confirm, &mailbox, 0)
            .expect("the confirmation is well formed")
        else {
            panic!("a haves-only request is a fetch outcome");
        };
        assert_eq!(purged.len(), served.len());

        drained += served.len();
        syncs.push(served.len());
        assert!(syncs.len() <= 8, "the drain must terminate: {syncs:?}");
    }

    eprintln!(
        "SERVE_SYNCS live_cap={live_cap} field={FIELD_MESSAGES} syncs={} counts={syncs:?} \
one_sync_needs_free={:?}",
        syncs.len(),
        free_heap_for_one_sync()
    );

    // What the cell's `expect_count = 24` is compared against: the first
    // sync's count, which is now the whole field.
    assert_eq!(
        syncs,
        vec![usize::from(FIELD_MESSAGES)],
        "one sync must drain the field; periculum's \
         lora_pn_board_offer_past_the_link step 14 expects {FIELD_MESSAGES}"
    );
    assert_eq!(drained, usize::from(FIELD_MESSAGES));
    assert!(
        role.store().is_empty(),
        "the confirmed syncs purged the whole mailbox"
    );

    // And how much heap a single-sync drain actually needs, as a number
    // rather than a claim: below what the board had, where the buffered
    // serve needed 54 848 B — nearly twice it, and more than half the
    // board's entire heap.
    let needed = free_heap_for_one_sync().expect("the search must converge");
    assert_eq!(
        needed, 28_928,
        "the free heap a single-sync drain of {FIELD_MESSAGES} needs"
    );
    assert!(
        needed <= FREE_AT_PANIC,
        "one sync serves all {FIELD_MESSAGES} at {needed} B free; the board \
         had {FREE_AT_PANIC} B of a {BOARD_HEAP_BYTES} B heap, and the \
         buffered serve needed 54 848 B"
    );
}

/// The least free heap at which one fetch serves all [`FIELD_MESSAGES`],
/// searched rather than derived: [`serve_cap_for_live_heap`] is monotone
/// in the free bytes, so the first heap that serves 24 is the bound.
/// `None` if no heap up to twice the board's does.
fn free_heap_for_one_sync() -> Option<usize> {
    (0..=2 * BOARD_HEAP_BYTES).step_by(64).find(|&free| {
        let cap = serve_cap_for_live_heap(
            BOOT_CAP,
            free,
            free,
            BOARD_SERVE_MARGIN_BYTES,
            BOARD_RESOURCE_SDU,
        );
        let (mut role, stored) = field_node(cap);
        fetch_all(&mut role, &stored).count() == usize::from(FIELD_MESSAGES)
    })
}
