//! mvr: a serve sized to the heap must still fit the heap it is spent
//! on, not only the heap it was measured on.
//!
//! **The named failure mode:** the board reads its free heap, sizes one
//! fetch response to all of it, and dies in the transient because
//! something else arrived while the serve was in flight. That is the
//! 2026-09-23 panic with one more step of arithmetic:
//!
//! ```text
//! [HEAP_CENSUS] used=67924 free=30380 largest=30320 …
//! [PANIC_PMRT] memory allocation of 5450 bytes failed
//! ```
//!
//! 30 380 B free funds a 14 296 B response under the serve-peak model
//! ([`serve_peak_bytes`], 30 368 B of transient) — 52 messages of the
//! field's size. It funds it only if nothing else happens. Serving is
//! not atomic: the cap is read when the `/get` comes off the work queue
//! and the path holds the transfer live across several awaits, and in
//! that window the engine still admits an inbound sync batch (8 000 B),
//! an upload (4 000 B) and one more endpoint link (2 696 B) — every one
//! of them already priced by the heap census, every one of them able to
//! start while a serve runs. The margined cap is 7 252 B (26 messages),
//! and the margin-free one costs 25 099 + 14 680 = 39 779 B against
//! 30 380 B of heap once those arrivals land.
//!
//! The numbers moved with #384 B2 — the same heap funded 4 954 B before
//! the response was streamed out of the store — but the rule did not:
//! a cap read from the free heap and spent on all of it is a panic one
//! arrival later, whatever the multiplier is.
//!
//! **What this test does:** it serves through the real path
//! (`send_response_resource_from_source` over a real established link,
//! the same builder `pn_serve_peak_outgrows_the_board_heap` measures
//! with — the streamed serve a board runs since #384 B2) under
//! a counting allocator, with the margin's worth of concurrent arrivals
//! claimed and held for the whole serve — the worst case, where every
//! byte the margin admits lands in the window between the cap decision
//! and the peak. It asserts the serve completes, that everything live
//! at the peak fits the heap the census measured, and that no single
//! allocation exceeds the largest block that allocator could still hand
//! out.
//!
//! Its second half is the positive control, and it is the red: the same
//! measurement at the cap a margin-free rule would have chosen
//! overruns that heap. Without it this test would pass on any cap small
//! enough, and prove nothing about the margin.
//!
//! **What it cannot prove:** that a real board survives it. This host
//! has gigabytes; what is measured here is the size of what the board
//! would be asked for. Only the rig closes that.

use leviculum_lxmf::propagation::{
    MessageGetRequest, PropagationUpload, TransferLimit, TransientId,
};
use leviculum_lxmf::propagation_node::{
    serve_cap_for_live_heap, serve_cap_for_peak, FetchPlan, GetOutcome, PropagationNode,
    PropagationNodeConfig, UploadOutcome,
};
use leviculum_lxmf::propagation_store::MemoryPropagationStore;

use crate::alloc_probe;
use crate::pn_serve_peak_outgrows_the_board_heap::established_pair;

/// The board's free heap six seconds before the fetch that killed it
/// (`[HEAP_CENSUS] … free=30380`, `lora_pn_board_offer_past_the_link`,
/// T114 `DEC9947D`, 2026-09-23 08:08 UTC).
const FREE_AT_PANIC: usize = 30_380;

/// The largest single block that allocator could still serve at the
/// same instant (`… largest=30320`).
const LARGEST_AT_PANIC: usize = 30_320;

/// What the boot plan funds on that board (`HEAP_BUDGET … slack=720` →
/// `serve_cap=194`), the floor the live reading is taken against.
const BOOT_CAP: usize = 194;

/// One inbound sync batch at the announced `BOARD_SYNC_LIMIT_KB`
/// (`leviculum-nrf/src/pn.rs`).
const ARRIVING_SYNC_BATCH_BYTES: usize = 8 * 1000;

/// One queued upload at the announced `BOARD_TRANSFER_LIMIT_KB`.
const ARRIVING_UPLOAD_BYTES: usize = 4 * 1000;

/// One more endpoint link, at this tree's own `budget_per_link()`
/// (2 696 B). #384 B2 took it to 2 680 B by shrinking `OutgoingResource`
/// by the joined-ciphertext copy it kept beside its parts; the
/// `frame_turnaround_ms` a link now records off its first hop
/// (#36/#374) put 8 B back, and the `acquisition_ms` beside it
/// (#36/#374, the burst's one channel acquisition) another 8.
const ARRIVING_LINK_BYTES: usize = 2_696;

/// The margin the firmware takes off before it spends a live reading
/// (`SERVE_MARGIN_BYTES`, `leviculum-nrf/src/heap_census.rs`): the sum
/// of the three above, because all three can coexist.
const BOARD_SERVE_MARGIN_BYTES: usize =
    ARRIVING_SYNC_BATCH_BYTES + ARRIVING_UPLOAD_BYTES + ARRIVING_LINK_BYTES;

/// The link SDU the serve transient is sized against on a board
/// (`SERVE_RESOURCE_SDU`, `leviculum-nrf/src/pn.rs`).
const BOARD_RESOURCE_SDU: usize = 464;

/// The mailbox this measures: eight messages of 3 000 B of servable
/// body, which is deeper than either cap and NOT the field's 24 × 224 B.
///
/// Both departures are load-bearing, and both are consequences of the
/// streamed serve (#384 B2):
///
/// * **Deeper than the cap**, because the live cap now funds the field's
///   whole mailbox in one round
///   (`pn_serve_cap_bounds_one_fetch.rs`), so a 24 × 224 B store could
///   no longer tell the margined cap from the margin-free one and the
///   positive control below would measure nothing.
/// * **Bigger messages**, because term 1 of [`serve_peak_bytes`] is the
///   one stored record the source holds while it streams it, and a
///   224 B record makes that term invisible. 3 000 B is an ordinary
///   message on a board that announces a 4 KB per-transfer limit, and
///   at this size the margin-free cap's measured peak really does
///   overrun the heap once the arrivals land — which is the only way
///   this test can claim the margin is load-bearing rather than
///   decorative.
const MAILBOX_MESSAGES: u8 = 8;
const FIELD_BODY_BYTES: usize = 3_000;

/// One stored message of exactly [`FIELD_BODY_BYTES`] servable bytes,
/// incompressible because a real LXMF body is ciphertext — this host
/// links bz2 and the firmware does not, and a compressible filler would
/// measure a path no board takes.
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

/// A node holding [`MAILBOX_MESSAGES`] and serving to `cap`, with the
/// plan for a client that asked for every one of them. The plan, not
/// the bytes: since #384 B2 the bytes only exist as resource parts.
fn planned_at_cap(cap: usize) -> (PropagationNode<MemoryPropagationStore>, FetchPlan) {
    let mut role = PropagationNode::new(
        MemoryPropagationStore::new(256 * 1024),
        PropagationNodeConfig {
            // Raised above the board's announced 8 KB so the SERVE cap
            // is the bound under test: the announced limit is a second
            // bound on the same sum, and it would otherwise decide both
            // halves and neither would measure the margin.
            sync_limit_kb: 64,
            serve_cap_bytes: Some(cap),
            ..PropagationNodeConfig::default()
        },
    );
    let wants: Vec<TransientId> = (0..MAILBOX_MESSAGES)
        .map(
            |seed| match role.handle_upload(&upload(seed), 0, |_, _| None) {
                UploadOutcome::Accepted { transient_id, .. } => transient_id,
                other => panic!("the store must accept the field shape: {other:?}"),
            },
        )
        .collect();
    let request = MessageGetRequest {
        wants: Some(wants),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(1000)),
    }
    .encode()
    .expect("the fetch is well formed");
    let GetOutcome::Fetch { plan, .. } = role
        .handle_get(&request, &[7u8; 16], 0)
        .expect("the fetch is well formed")
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    (role, plan)
}

/// Everything the board holds live at the peak of one serve of
/// `response`, measured through the real path, with the margin's worth
/// of concurrent arrivals claimed first and held throughout.
///
/// The arrivals are claimed BEFORE the serve rather than interleaved
/// with it because that is the worst case and the only one a
/// single-threaded measurement can state exactly: every byte the margin
/// admits has landed by the time the serve reaches its peak. A real
/// board's arrivals land somewhere inside the window, which costs the
/// same or less.
fn peak_under_arrivals(
    role: &PropagationNode<MemoryPropagationStore>,
    plan: &FetchPlan,
) -> (usize, usize, bool) {
    let mut serving = established_pair();
    let request_id = [0x5Au8; 16];
    let mut source = role.fetch_source(plan);
    let probe = alloc_probe::Probe::armed();
    // The margin, as three real claims in the order a board sees them:
    // a peer's sync batch, a phone's upload, a third peer's link.
    let arrivals: Vec<Vec<u8>> = vec![
        vec![0u8; ARRIVING_SYNC_BATCH_BYTES],
        vec![0u8; ARRIVING_UPLOAD_BYTES],
        vec![0u8; ARRIVING_LINK_BYTES],
    ];
    let served = serving
        .serving
        .send_response_resource_from_source(&serving.link_id, &request_id, &mut source)
        .is_ok();
    let peak = probe.peak();
    let largest = alloc_probe::largest_block();
    drop(arrivals);
    drop(probe);
    // Nothing is allocated before the window on the streamed path: the
    // response has no buffer to own. The peak is the whole figure.
    (peak, largest, served)
}

#[test]
fn a_serve_survives_the_heap_shrinking_under_it() {
    // The cap the firmware computes at serve time: the larger of the
    // boot floor and what the live census funds, margin first.
    let live_cap = serve_cap_for_live_heap(
        BOOT_CAP,
        FREE_AT_PANIC,
        LARGEST_AT_PANIC,
        BOARD_SERVE_MARGIN_BYTES,
        BOARD_RESOURCE_SDU,
    );
    let (role, plan) = planned_at_cap(live_cap);
    let served = plan.count();
    let response_len = plan.encoded_len();
    let (peak, largest_block, completed) = peak_under_arrivals(&role, &plan);

    eprintln!(
        "SERVE_CAP boot_cap={BOOT_CAP} live_cap={live_cap} \
margin={BOARD_SERVE_MARGIN_BYTES} largest={LARGEST_AT_PANIC} free={FREE_AT_PANIC} \
served={served} of {MAILBOX_MESSAGES} response={response_len} peak={peak} \
block={largest_block}"
    );

    // 1. It serves. A cap that refuses everything would satisfy every
    //    bound below and deliver no mail.
    assert!(
        completed && served > 1,
        "the serve must complete and ship more than one message: \
         completed={completed} served={served}"
    );

    // 2. Nothing host-only was counted: every block the path made is
    //    under the ceiling that excludes this host's bz2 scratch.
    assert!(
        largest_block < alloc_probe::BLOCK_CEILING_BYTES,
        "a counted allocation ({largest_block} B) reached the ceiling \
         that is supposed to exclude only host-only scratch"
    );

    // 3. The whole point: the serve plus everything that arrived while
    //    it ran fits the heap the board had. `peak` already contains
    //    the arrivals -- they were claimed inside the window.
    assert!(
        peak <= FREE_AT_PANIC,
        "a {response_len} B response served beside \
         {BOARD_SERVE_MARGIN_BYTES} B of arrivals held {peak} B live, \
         against {FREE_AT_PANIC} B of heap"
    );

    // 4. And no single block outgrew what the allocator could hand out.
    //    Bytes are not blocks: this is the check `free=` alone cannot
    //    make.
    assert!(
        largest_block <= LARGEST_AT_PANIC,
        "one allocation of {largest_block} B, against a largest \
         servable block of {LARGEST_AT_PANIC} B"
    );

    // 5. Positive control, and the red this rule removes: the cap the
    //    same heap funds with NO margin serves twice as many messages
    //    and overruns the heap as soon as the arrivals land. Measured,
    //    not modelled -- the margin has to be load-bearing on the real
    //    path or it is decoration.
    let unmargined_cap = serve_cap_for_peak(FREE_AT_PANIC, BOARD_RESOURCE_SDU);
    let (greedy_role, greedy_plan) = planned_at_cap(unmargined_cap);
    let greedy_served = greedy_plan.count();
    let (greedy_peak, _, greedy_completed) = peak_under_arrivals(&greedy_role, &greedy_plan);
    eprintln!(
        "SERVE_CAP_UNMARGINED cap={unmargined_cap} served={greedy_served} \
response={} peak={greedy_peak} free={FREE_AT_PANIC}",
        greedy_plan.encoded_len()
    );
    assert!(
        greedy_completed && greedy_served > served,
        "the margin-free cap must be the greedier one: {greedy_served} \
         against {served}"
    );
    assert!(
        greedy_peak > FREE_AT_PANIC,
        "the margin-free cap held only {greedy_peak} B live against \
         {FREE_AT_PANIC} B of heap -- if it fits, this test is no longer \
         measuring why the margin exists"
    );
}
