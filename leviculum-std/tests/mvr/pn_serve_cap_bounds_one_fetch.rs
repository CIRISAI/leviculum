//! mvr: a fetch the heap plan does not fund is served in part, not died in.
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
//! The 5 450 B is term 3 of [`serve_peak_bytes`], the resource path's
//! `plaintext` (`RESOURCE_RANDOM_HASH_SIZE + response + frame`, and
//! `5450 = 4 + 5427 + 19`) — one term further along the path than the
//! 5 446 B `packed` copy the same board died in on 2026-09-22, which
//! #384 B1 removed. The serve did not get smaller than the heap; it got
//! smaller than it was.
//!
//! **What this test fixes in place:** the free heap at the moment the
//! census measured it, [`FREE_AT_PANIC`], and the largest response that
//! fits it ([`serve_cap_for_peak`]). A fetch for the full 24 is served
//! down to that cap; what does not fit stays stored, is purged by
//! nothing, and is served on the next round.
//!
//! Red before the bound existed: the role served all 24, a 5 427 B
//! response whose modelled transient is 33 238 B — 2 858 B more than the
//! heap the board had, which is the panic, in arithmetic.
//!
//! **Two tests, two rules.** The first fixes the MODEL's inverse: given
//! a heap, what response does it fund. The second fixes what the board
//! actually serves to, which is that number less a margin for what can
//! arrive while the serve runs
//! ([`BOARD_SERVE_MARGIN_BYTES`], #388 order 138) — 2 540 B and nine
//! messages where the bare model says 4 954 B and eighteen. Do not read
//! the first test's 18 as a promise about a board: the margin-free cap
//! is measured overrunning the same heap in
//! `pn_serve_cap_survives_a_shrinking_heap`.
//!
//! **What it cannot prove:** that a real board survives it. Only a board
//! can, and only the rig can run one; what this fixes is the size of
//! what the board is allowed to try.

use leviculum_lxmf::propagation::{
    MessageGetRequest, PropagationUpload, TransferLimit, TransientId,
};
use leviculum_lxmf::propagation_node::{
    serve_cap_for_live_heap, serve_cap_for_peak, serve_largest_block_bytes, serve_peak_bytes,
    GetOutcome, PropagationNode, PropagationNodeConfig, UploadOutcome,
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

/// What the BOOT plan funds on that same board (`HEAP_BUDGET … slack=784`
/// → `serve_cap=88`, `heap_census::budget_serve_cap`,
/// `leviculum-nrf`): 88 B, less than one stored message, so a board held
/// to it lists 24 messages and serves none of them.
const BOOT_CAP: usize = 88;

/// Heap the board keeps clear of the serve because it can be claimed
/// WHILE the serve is in flight (`SERVE_MARGIN_BYTES`,
/// `leviculum-nrf/src/heap_census.rs`): one inbound sync batch at
/// `BOARD_SYNC_LIMIT_KB` (8 000 B), one queued upload at
/// `BOARD_TRANSFER_LIMIT_KB` (4 000 B), and one more endpoint link at
/// `budget_per_link()` — 2 688 B on the T114's own boot line
/// (`HEAP_BUDGET links=4 ble_links=4 per_link=2688 …`, same capture).
/// All three can coexist, so they are summed.
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

/// A node holding the field's 24 messages for one mailbox, serving to
/// `cap` accounted bytes per fetch.
fn field_node(cap: usize) -> (PropagationNode<MemoryPropagationStore>, Vec<TransientId>) {
    let mut role = PropagationNode::new(
        MemoryPropagationStore::new(64 * 1024),
        PropagationNodeConfig {
            sync_limit_kb: BOARD_SYNC_LIMIT_KB,
            serve_cap_bytes: Some(cap),
            ..PropagationNodeConfig::default()
        },
    );
    let stored: Vec<TransientId> = (0..FIELD_MESSAGES)
        .map(
            |seed| match role.handle_upload(&upload(seed), 0, |_, _| None) {
                UploadOutcome::Accepted { transient_id, .. } => transient_id,
                other => panic!("the store must accept the field shape: {other:?}"),
            },
        )
        .collect();
    (role, stored)
}

/// Serve one fetch for every id in `stored` and report what came back.
fn fetch_all(
    role: &mut PropagationNode<MemoryPropagationStore>,
    stored: &[TransientId],
) -> (Vec<u8>, Vec<TransientId>) {
    let GetOutcome::Fetch {
        response, served, ..
    } = role
        .handle_get(&fetch_for(stored), &[7u8; 16], 0)
        .expect("the fetch is well formed")
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    (response, served)
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

#[test]
fn a_fetch_past_the_funded_cap_is_served_in_part_and_finished_next_round() {
    let cap = serve_cap_for_peak(FREE_AT_PANIC, BOARD_RESOURCE_SDU);
    let (mut role, stored) = field_node(cap);
    let mailbox = [7u8; 16];

    // Round one: the client asks for all 24.
    let GetOutcome::Fetch {
        response,
        served,
        served_bytes,
        purged,
    } = role
        .handle_get(&fetch_for(&stored), &mailbox, 0)
        .expect("the fetch is well formed")
    else {
        panic!("a fetch request must produce a fetch outcome");
    };

    eprintln!(
        "SERVE_CAP free={FREE_AT_PANIC} cap={cap} served={} of {FIELD_MESSAGES} \
response={} peak={}",
        served.len(),
        response.len(),
        serve_peak_bytes(response.len(), BOARD_RESOURCE_SDU)
    );

    // The bound, on the wire: the response the node ships fits the cap
    // its heap plan funds. Before the bound this was 5 427 B against a
    // 4 954 B cap.
    assert!(
        response.len() <= cap,
        "served {} B against a funded cap of {cap} B",
        response.len()
    );

    // The bound, in the heap: the transient that response costs fits the
    // heap the board had. Before the bound the same 24 messages modelled
    // at 33 238 B against 30 380 B of free heap -- the panic.
    let peak = serve_peak_bytes(response.len(), BOARD_RESOURCE_SDU);
    assert!(
        peak <= FREE_AT_PANIC,
        "a {} B response costs {peak} B of transient, against {FREE_AT_PANIC} B of heap",
        response.len()
    );

    // Bounded serving, not refusal: it served what it could, and it is
    // not one message.
    assert!(
        served.len() > 1 && served.len() < usize::from(FIELD_MESSAGES),
        "a bounded serve is a subset, not all and not nothing: {} of {FIELD_MESSAGES}",
        served.len()
    );
    assert_eq!(served_bytes, (served.len() * FIELD_BODY_BYTES) as u64);

    // Nothing was purged -- not what was served, and not what was not.
    // Deletion is the client's `haves` confirmation and nothing else
    // (`reference/LXMF/LXMF/LXMRouter.py:1622-1638`), so a round that
    // serves 18 of 24 loses none of the 24.
    assert!(purged.is_empty(), "a fetch must purge nothing by itself");
    assert_eq!(
        role.store().len(),
        usize::from(FIELD_MESSAGES),
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
        .handle_get(
            &MessageGetRequest {
                wants: None,
                haves: None,
                transfer_limit_kb: None,
            }
            .encode()
            .expect("the list request is well formed"),
            &mailbox,
            0,
        )
        .expect("the list request is well formed")
    else {
        panic!("an empty request is the list form");
    };
    assert_eq!(count, rest.len(), "the next list offers exactly the rest");

    let GetOutcome::Fetch {
        served: served_two, ..
    } = role
        .handle_get(&fetch_for(&rest), &mailbox, 0)
        .expect("the fetch is well formed")
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    assert_eq!(
        served_two.len(),
        rest.len(),
        "two rounds must deliver all {FIELD_MESSAGES}"
    );
    assert_eq!(
        served.len() + served_two.len(),
        usize::from(FIELD_MESSAGES),
        "nothing served twice, nothing lost"
    );
}

/// The cap the board serves to is the heap it HAS, not the heap its boot
/// plan feared — and the margin is what keeps that from being the panic
/// again.
///
/// Order 137 left the cap at the boot plan's worst case: every term at
/// its maximum at once, 784 B of slack, 88 B of funded response. A
/// stored message is 256 B stamped, so a board held to 88 B lists 24
/// messages and serves **none** of them — which is what this test's
/// first half measures, and it is the red this order removes.
///
/// The heap that board actually stood on six seconds before it died was
/// [`FREE_AT_PANIC`] free with [`LARGEST_AT_PANIC`] in one block. Under
/// the serve-peak model, less [`BOARD_SERVE_MARGIN_BYTES`] for what can
/// still arrive mid-serve, that funds 2 540 B — nine of the 24.
///
/// **Nine, not eighteen.** 4 954 B (18 messages) is what the same heap
/// funds with no margin at all, and the second half of
/// `a_serve_survives_the_heap_shrinking_under_it` measures what that
/// costs: 24 974 B of serve transient plus 14 688 B of margin is
/// 39 662 B against 30 380 B of heap, i.e. the same panic one arrival
/// later. The margin is the difference between a cap that fits the
/// instant it was computed and one that fits the instant it is spent.
#[test]
fn a_boot_cap_below_one_message_is_raised_by_the_heap_the_board_has() {
    // Today's rule: the boot plan's worst case, and nothing else.
    let (mut booted, stored) = field_node(BOOT_CAP);
    let (boot_response, boot_served) = fetch_all(&mut booted, &stored);
    assert!(
        boot_served.is_empty() && boot_response.len() < 8,
        "an 88 B cap cannot fit a 256 B message: served {} in {} B",
        boot_served.len(),
        boot_response.len()
    );

    // The rule this order asks for: the larger of that floor and what
    // the live census funds, margin first.
    let live_cap = serve_cap_for_live_heap(
        BOOT_CAP,
        FREE_AT_PANIC,
        LARGEST_AT_PANIC,
        BOARD_SERVE_MARGIN_BYTES,
        BOARD_RESOURCE_SDU,
    );
    let (mut live, stored) = field_node(live_cap);
    let (response, served) = fetch_all(&mut live, &stored);
    let peak = serve_peak_bytes(response.len(), BOARD_RESOURCE_SDU);
    let block = serve_largest_block_bytes(response.len(), BOARD_RESOURCE_SDU);

    // The firmware's own line, at the board's own numbers
    // (`SERVE_CAP`, `Engine::read_serve_cap`, `leviculum-nrf/src/pn.rs`).
    eprintln!(
        "SERVE_CAP boot_cap={BOOT_CAP} live_cap={live_cap} \
margin={BOARD_SERVE_MARGIN_BYTES} largest={LARGEST_AT_PANIC} free={FREE_AT_PANIC} \
served={} of {FIELD_MESSAGES} response={} peak={peak} block={block}",
        served.len(),
        response.len(),
    );

    assert_eq!(
        live_cap, 2_540,
        "the heap the board had funds 2 540 B of fetch response"
    );
    assert_eq!(
        served.len(),
        9,
        "2 540 B of accounted cap is 24 B of preamble plus nine 256 B \
         messages at 16 B of overhead each (24 + 9·272 = 2 472, and a \
         tenth would be 2 744)"
    );

    // The margin is what the serve must survive: every byte it holds
    // live, plus everything that can arrive while it holds them, inside
    // the heap the census measured.
    assert!(
        peak + BOARD_SERVE_MARGIN_BYTES <= FREE_AT_PANIC,
        "a {} B response costs {peak} B of transient; with \
         {BOARD_SERVE_MARGIN_BYTES} B of concurrent arrivals that is \
         {} B against {FREE_AT_PANIC} B of heap",
        response.len(),
        peak + BOARD_SERVE_MARGIN_BYTES
    );
    assert!(
        block + BOARD_SERVE_MARGIN_BYTES <= LARGEST_AT_PANIC,
        "the serve's largest single allocation is {block} B; the \
         allocator can hand out {LARGEST_AT_PANIC} B and the margin \
         claims {BOARD_SERVE_MARGIN_BYTES} B of it"
    );

    // And it is a serve, not a refusal: nine now, the rest next round,
    // the store intact -- the same contract the first test pins at the
    // unmargined cap.
    assert_eq!(
        live.store().len(),
        usize::from(FIELD_MESSAGES),
        "a bounded serve keeps every message it did not ship"
    );
}
