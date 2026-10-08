//! The propagation role's transient peaks (Codeberg #388, order 510),
//! asserted on the host.
//!
//! Two halves. The first pins the model's building blocks against the real
//! code they describe: `Vec` growth against a real `Vec`, the stable sort's
//! scratch and a growing `Vec`'s old-plus-new block against a counting
//! allocator, the envelope and offer encodings against the real encoders.
//! A model that drifts from the code goes red here before any budget
//! figure is believed. The second asks each operation's peak at the
//! field's shapes against the role budget, prints the table, and finds the
//! smallest inputs that leave the field's 340 bytes unfunded.
//!
//! Run: `cargo test -p leviculum-heap-budget --test peaks -- --nocapture`

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use leviculum_core::constants::RESOURCE_HASHMAP_LEN;
use leviculum_core::resource::{ResourceSource, RESOURCE_ADV_OVERHEAD, STANDARD_LINK_MDU};
use leviculum_heap_budget::peak::{
    buffered_offer_response_peak, flush_peak, inbound_resource_peak, offer_encoding,
    offer_response_fits, offer_response_peak, peer_table_bytes, pushed_capacity,
    pushed_capacity_from, pushed_peak, pushed_peak_from, role_margin, stable_sort_scratch,
    store_tail_peak, sync_envelope, sync_round_peak, transfer_bytes, trimmed_fifo_bytes,
    upload_peak, ByteVec, PEER_BYTES, STORED_MESSAGE_BYTES, TRANSIENT_ID_BYTES,
};
use leviculum_heap_budget::{
    BOARD_MAX_PEERS, BOARD_SYNC_LIMIT_KB, BOARD_TRANSFER_LIMIT_KB, FIELD_FAILED_ALLOCATION_BYTES,
    LORA_OFFER_IDS, OFFER_RESPONSE_RESERVE_BYTES, PEERING_SLICE_BYTES, ROLE_BUDGET_BYTES,
    ROLE_STRUCTURAL_BYTES, SERVE_RESOURCE_SDU, SYNC_SLICE_BYTES,
};
use leviculum_lxmf::peering::{
    offer_budget_for_mdu, offer_encoded_len, offer_reply, plan_sync_batch, OfferPlan, OfferReply,
    OfferResponse, PeerOffer, PeerSyncEnvelope, SyncBatch, SyncSource,
};
use leviculum_lxmf::propagation::TransientId;
use leviculum_lxmf::propagation_store::{MemoryPropagationStore, PropagationStore, StoredMessage};

// ---------------------------------------------------------------------------
// A per-thread counting allocator: the test harness runs tests on several
// threads, and a global counter would read the neighbours' allocations.
// ---------------------------------------------------------------------------

struct Counting;

thread_local! {
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

fn note_alloc(size: usize) {
    let _ = LIVE.try_with(|live| {
        let now = live.get() + size;
        live.set(now);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
    });
}

fn note_dealloc(size: usize) {
    let _ = LIVE.try_with(|live| live.set(live.get().saturating_sub(size)));
}

// SAFETY: every call forwards to `System` with the caller's layout; the
// bookkeeping touches only const-initialised thread locals, which do not
// allocate. `realloc` is left to the default (alloc, copy, dealloc), which
// is exactly what `embedded-alloc`'s `LlffHeap` does on the board.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note_dealloc(layout.size());
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Bytes `f` held at its worst instant above what was live when it started.
fn measured_peak<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.with(Cell::get);
    PEAK.with(|peak| peak.set(base));
    let out = f();
    (out, PEAK.with(Cell::get) - base)
}

fn stored(sequence: u64) -> StoredMessage {
    StoredMessage {
        transient_id: [0; 32],
        destination_hash: [0; 16],
        size: 0,
        received_at: 0,
        stamp_value: 0,
        sequence,
    }
}

// ---------------------------------------------------------------------------
// The model against the code
// ---------------------------------------------------------------------------

#[test]
fn the_element_sizes_the_model_uses() {
    println!(
        "PEAK_CONST stored_message={STORED_MESSAGE_BYTES} peer={PEER_BYTES} \
         transient_id={TRANSIENT_ID_BYTES}"
    );
    // Pointer-free, so these are the target's sizes too; a field added to
    // either moves the round's figures and has to move this pin.
    assert_eq!(STORED_MESSAGE_BYTES, 72);
    assert_eq!(PEER_BYTES, 200);
    assert_eq!(TRANSIENT_ID_BYTES, 32);
}

#[test]
fn pushed_capacity_is_what_a_real_vec_reaches() {
    fn check<T: Default + Clone>(start: usize) {
        let elem = core::mem::size_of::<T>();
        for n in 0..300 {
            let mut v: Vec<T> = Vec::with_capacity(start);
            for _ in 0..n {
                v.push(T::default());
            }
            assert_eq!(
                pushed_capacity_from(elem, start, n),
                v.capacity(),
                "elem={elem} start={start} n={n}"
            );
        }
    }
    check::<u8>(0);
    check::<[u8; 32]>(0);
    check::<[u8; 32]>(8);
    check::<[u64; 9]>(0);
}

#[test]
fn a_growing_vec_holds_its_old_and_new_block() {
    for n in [1usize, 4, 5, 9, 17, 57, 65, 200] {
        let (v, measured) = measured_peak(|| {
            let mut v = Vec::new();
            for i in 0..n {
                v.push(stored(i as u64));
            }
            v
        });
        assert_eq!(measured, pushed_peak(STORED_MESSAGE_BYTES, n), "n={n}");
        drop(v);
    }
    // From a reserve, the way `too_large` grows.
    let (v, measured) = measured_peak(|| {
        let mut v: Vec<[u8; 32]> = Vec::with_capacity(8);
        for _ in 0..65 {
            v.push([0; 32]);
        }
        v
    });
    // The reserve is allocated inside the closure, so it counts too.
    assert_eq!(measured, pushed_peak_from(32, 8, 65));
    drop(v);
}

#[test]
fn the_stable_sort_scratch_is_what_sort_by_key_allocates() {
    for n in [0usize, 1, 20, 21, 56, 57, 58, 100, 200, 500] {
        let mut v: Vec<StoredMessage> = (0..n).map(|i| stored((n - i) as u64)).collect();
        let ((), measured) = measured_peak(|| v.sort_by_key(|m| m.sequence));
        assert_eq!(
            measured,
            stable_sort_scratch(STORED_MESSAGE_BYTES, n),
            "n={n}"
        );
    }
}

#[test]
fn the_envelope_model_is_what_encode_builds() {
    let shapes: [&[usize]; 8] = [
        &[],
        &[1],
        &[300],
        &[1_000],
        &[431],
        &[3_960, 3_960],
        &[200; 20],
        &[4_000; 10],
    ];
    for bodies in shapes {
        let envelope = PeerSyncEnvelope {
            timestamp: 1.0,
            messages: bodies.iter().map(|&len| vec![0u8; len]).collect(),
        };
        let (data, measured) = measured_peak(|| envelope.encode());
        let model = sync_envelope(bodies.iter().copied());
        assert_eq!(model.len, data.len(), "{bodies:?}");
        assert_eq!(model.capacity, data.capacity(), "{bodies:?}");
        assert_eq!(model.peak, measured, "{bodies:?}");
    }
}

#[test]
fn the_offer_model_is_what_encode_builds() {
    for ids in [0usize, 1, 10, 15, 16, 179] {
        let offer = PeerOffer {
            peering_key: [0; 32],
            transient_ids: vec![[0; 32]; ids],
        };
        let data = offer.encode();
        let model = offer_encoding(ids);
        assert_eq!(model.len, data.len(), "ids={ids}");
        assert_eq!(model.capacity, data.capacity(), "ids={ids}");
    }
}

#[test]
fn the_decoded_envelope_is_one_exact_block_per_message() {
    for bodies in [&[1_000usize][..], &[3_960, 3_960], &[200; 20]] {
        let envelope = PeerSyncEnvelope {
            timestamp: 1.0,
            messages: bodies.iter().map(|&len| vec![0u8; len]).collect(),
        };
        let data = envelope.encode();
        let (decoded, measured) = measured_peak(|| PeerSyncEnvelope::decode(&data));
        let decoded = decoded.ok();
        // Host `Vec` headers are 24 B, the target's 12: compare the payload
        // and the spine separately.
        let spine = bodies.len() * core::mem::size_of::<Vec<u8>>();
        let sum: usize = bodies.iter().sum();
        assert_eq!(measured, spine + sum, "{bodies:?}");
        assert!(decoded.is_some());
    }
}

#[test]
fn byte_vec_growth_matches_by_hand() {
    let v = ByteVec::new().write(1);
    assert_eq!((v.len, v.capacity, v.peak), (1, 8, 8));
    let v = v.write(8);
    assert_eq!((v.len, v.capacity, v.peak), (9, 16, 24));
    let v = v.write(100);
    assert_eq!((v.len, v.capacity, v.peak), (109, 109, 125));
    assert_eq!(pushed_capacity(1, 9), 16);
}

// ---------------------------------------------------------------------------
// The operations against the role budget
// ---------------------------------------------------------------------------

/// The link MDU at Reticulum's base MTU of 500 (`compute_link_mdu`,
/// `leviculum-core/src/link/mod.rs`): the largest body a single link packet
/// carries, the LoRa shape.
const LINK_MDU: usize = 431;

/// The envelope a client wraps one stamped body of `body` bytes in:
/// `[timestamp, [bin]]`.
fn upload_envelope(body: usize) -> usize {
    sync_envelope([body]).len
}

/// One phone's queued upload at the announced per-transfer limit.
const ONE_UPLOAD: usize = BOARD_TRANSFER_LIMIT_KB as usize * 1000;

struct Row {
    op: &'static str,
    inputs: String,
    peak: usize,
    concurrent: usize,
    /// The reserve's incoming resource term pays for the input, not the
    /// role (`MAX_INCOMING_RESOURCE_BYTES`); 0 elsewhere.
    reserve_funded: usize,
}

impl Row {
    fn margin(&self) -> isize {
        role_margin(self.peak - self.reserve_funded, self.concurrent)
    }
}

/// The offer response shapes of 510's table: bodies the peer wants (all
/// of the plan) and whether it answered with an explicit list (`listed`
/// ids) or `True` (0).
fn offer_shapes() -> Vec<(&'static str, Vec<usize>, usize)> {
    vec![
        ("1 KB body", vec![1_000], 0),
        ("link-MDU body", vec![LINK_MDU], 0),
        (
            "two 3 960 B bodies: 510's 8 KB board sync",
            vec![3_960, 3_960],
            0,
        ),
        (
            "ten 4 KB bodies: a Python peer, a LoRa offer, want all",
            vec![4_000; 10],
            0,
        ),
        (
            "ten 4 KB bodies: a Python peer, a LoRa offer, listed",
            vec![4_000; 10],
            10,
        ),
    ]
}

/// A store holding `bodies` in append order and the plan offering all of
/// them, as `build_offer` would for a peer whose limits admit them.
fn offered_store(bodies: &[usize]) -> (MemoryPropagationStore, OfferPlan) {
    let mut store = MemoryPropagationStore::new(1 << 20);
    let mut ids: Vec<TransientId> = Vec::new();
    for (n, &len) in bodies.iter().enumerate() {
        let mut id = [0u8; 32];
        id[0] = n as u8 + 1;
        store
            .append(&id, n as u64, 20, &vec![n as u8; len])
            .unwrap();
        ids.push(id);
    }
    let mut last = 0;
    store
        .for_each(&mut |meta| last = last.max(meta.sequence))
        .unwrap();
    let plan = OfferPlan {
        ids,
        cursor_target: last,
        skipped_oversize: 0,
        skipped_low_value: 0,
    };
    (store, plan)
}

/// What the board sends for a peer wanting all of `bodies` (`listed` > 0:
/// as an explicit list), with `concurrent` held beside it: the board's own
/// planner asked the budget's own fit test.
fn answer_of(bodies: &[usize], listed: usize, concurrent: usize) -> SyncBatch {
    let (store, plan) = offered_store(bodies);
    let response = if listed > 0 {
        OfferResponse::Wanted(plan.ids.clone())
    } else {
        OfferResponse::WantAll
    };
    let OfferReply::Send(wanted) = offer_reply(&response, &plan) else {
        panic!("a want is a send");
    };
    plan_sync_batch(&store, &plan, &wanted, |_, encoded_len| {
        offer_response_fits(
            encoded_len,
            plan.ids.len(),
            listed,
            SERVE_RESOURCE_SDU,
            concurrent,
        )
    })
    .unwrap()
}

fn field_rows() -> Vec<Row> {
    let sdu = SERVE_RESOURCE_SDU;
    let two = 2 * ONE_UPLOAD;
    let mut rows = Vec::new();
    // Offer response: what the board sends a peer, through the real
    // batch planner (`plan_sync_batch`) asking the budget's fit test with
    // two phones' uploads queued. `wanted` is how many the peer asked for;
    // the row's inputs say how many the board sends.
    for (what, bodies, listed) in offer_shapes() {
        let answer = answer_of(&bodies, listed, two);
        rows.push(Row {
            op: "offer_response",
            inputs: format!(
                "{what} wanted={} sent={} sent_bytes={} encoded={} offered={} listed={listed} \
                 buffered_510={}",
                bodies.len(),
                answer.count,
                answer.payload,
                answer.encoded_len,
                bodies.len(),
                buffered_offer_response_peak(bodies.iter().copied(), bodies.len(), sdu)
            ),
            peak: offer_response_peak(answer.encoded_len, bodies.len(), listed, sdu),
            concurrent: two,
            reserve_funded: 0,
        });
    }
    // Inbound resource: a phone's upload (beside the other phone's queued
    // one) or a peer's sync (beside both).
    for (what, data, decoded, concurrent) in [
        (
            "1 KB upload",
            upload_envelope(1_000),
            vec![1_000],
            ONE_UPLOAD,
        ),
        (
            "link-MDU upload",
            upload_envelope(LINK_MDU),
            vec![LINK_MDU],
            ONE_UPLOAD,
        ),
        (
            "upload at the announced sync limit",
            BOARD_SYNC_LIMIT_KB as usize * 1000,
            vec![BOARD_SYNC_LIMIT_KB as usize * 1000 - 14],
            ONE_UPLOAD,
        ),
        (
            // The largest pair a reference peer packs under the announced
            // limit: 24 + 2 × (2 460 + 16) = 4 976 < 5 000 (`LXMPeer.py:376`).
            "sync at the announced limit: two bodies",
            sync_envelope([2_460, 2_460]).len,
            vec![2_460, 2_460],
            two,
        ),
    ] {
        rows.push(Row {
            op: "inbound_resource",
            inputs: format!("{what} data={data} decoded={decoded:?}"),
            peak: inbound_resource_peak(data, decoded.iter().copied()),
            concurrent,
            reserve_funded: data,
        });
    }
    // Upload processing and its flush, beside the other phone's upload.
    for (what, body, stored) in [
        ("1 KB body", 1_000, true),
        ("link-MDU body", LINK_MDU, true),
        ("4 KB body: the transfer limit", ONE_UPLOAD - 16, true),
        ("8 KB body: refused by the store", 8_000 - 14, false),
    ] {
        let data = upload_envelope(body);
        rows.push(Row {
            op: "upload+flush",
            inputs: format!("{what} data={data} body={body} stored={stored}"),
            peak: upload_peak(data, body, stored),
            concurrent: ONE_UPLOAD,
            reserve_funded: 0,
        });
    }
    // The round's planning step: a store tail above the peer's cursor.
    for (what, tail, offered) in [
        ("16 records", 16, 10),
        ("56 records: last without sort scratch", 56, 10),
        ("57 records", 57, 10),
        ("200 records: a new peer, a full store", 200, 10),
    ] {
        rows.push(Row {
            op: "store_tail",
            inputs: format!("{what} tail={tail} offered={offered}"),
            peak: store_tail_peak(tail, offered),
            concurrent: two,
            reserve_funded: 0,
        });
    }
    rows
}

/// Every row's peak and margin, pinned: a change to the code a row models,
/// or to a budget term, moves a number here, and the diff says by how much.
/// Rows with a negative margin are today's overdraws; they are pinned as
/// such here and asked to fit in [`every_field_shape_fits_the_role_budget`].
#[test]
fn the_field_shapes_against_the_role_budget() {
    let headroom = ROLE_BUDGET_BYTES - ROLE_STRUCTURAL_BYTES;
    println!(
        "PEAK_BUDGET role={ROLE_BUDGET_BYTES} structural={ROLE_STRUCTURAL_BYTES} \
         headroom={headroom}"
    );
    // The offer rows are the streamed answer since order 511 (510 pinned
    // 3 538, 1 800, 24 996 and 149 584 here, the last two overdrawn by
    // 16 996 and 141 584 B; the inputs column prints that figure beside
    // each row as `buffered_510=`). The two ten body rows send one body
    // each, the rest waits above the cursor.
    let expected: [(usize, isize); 17] = [
        (1_784, 6_216),
        (1_165, 6_835),
        (4_984, 3_016),
        (5_416, 2_584),
        (5_736, 2_264),
        (2_028, 10_986),
        (890, 11_555),
        (10_000, 7_000),
        (9_881, 3_056),
        (4_014, 7_986),
        (1_738, 10_262),
        (15_950, -3_950),
        (23_940, -11_940),
        (2_980, 5_020),
        (6_912, 1_088),
        (8_712, -712),
        (32_832, -24_832),
    ];
    let rows = field_rows();
    for row in &rows {
        println!(
            "PEAK op={} peak={} concurrent={} reserve_funded={} margin={} | {}",
            row.op,
            row.peak,
            row.concurrent,
            row.reserve_funded,
            row.margin(),
            row.inputs
        );
    }
    assert_eq!(rows.len(), expected.len());
    for (row, (peak, margin)) in rows.iter().zip(expected) {
        assert_eq!(
            (row.peak, row.margin()),
            (peak, margin),
            "{} {}",
            row.op,
            row.inputs
        );
    }
}

/// The same rows, asked to fit. Red today, and ignored because making it
/// green is a design decision, not a fix. The offer response rows fit since
/// order 511 ([`every_offer_response_fits_beside_two_queued_uploads`]
/// asserts it unignored); what is left is the upload's flush copies and
/// the store tail collected whole. Run it with `--ignored` to see them.
#[test]
#[ignore = "#388: the upload flush and the store tail still overdraw; order 511 fixed the offer response"]
fn every_field_shape_fits_the_role_budget() {
    let over: Vec<String> = field_rows()
        .iter()
        .filter(|row| row.margin() < 0)
        .map(|row| format!("{} margin={} | {}", row.op, row.margin(), row.inputs))
        .collect();
    assert!(over.is_empty(), "overdrawn:\n{}", over.join("\n"));
}

/// #388's figure, reproduced: the smallest inputs to each operation at which
/// its peak leaves less than the field's refused 340 bytes of the role
/// budget, at the field's shape (two phones' uploads queued at the
/// announced 4 KB, or one beside the upload being processed).
///
/// All three are ordinary traffic, well inside every announced limit: one
/// stored message of 2 334 B offered to a peer, one 2 912 B upload, or a
/// peer whose cursor is 57 records behind. Which of them the field T114 hit
/// is not in its log (the census did not exist yet); the point is that each
/// one alone gets there.
#[test]
fn issue_388_the_smallest_inputs_that_leave_less_than_340_bytes() {
    let sdu = SERVE_RESOURCE_SDU;
    let two = 2 * ONE_UPLOAD;
    let short = |margin: isize| margin < FIELD_FAILED_ALLOCATION_BYTES as isize;

    // The offer response as 510 measured it: one body of 2 334 B was
    // enough. Streamed (order 511), no single body up to the transfer
    // limit gets there.
    let buffered = |body: usize| role_margin(buffered_offer_response_peak([body], 1, sdu), two);
    let body = (1..=ONE_UPLOAD).find(|&b| short(buffered(b)));
    println!(
        "ISSUE_388 op=offer_response path=buffered_510 body={body:?} margin={:?}",
        body.map(buffered)
    );
    assert_eq!(body, Some(2_334));
    assert!(!short(buffered(2_333)));
    let offer = |body: usize| {
        role_margin(
            offer_response_peak(sync_envelope([body]).len, 1, 0, sdu),
            two,
        )
    };
    let streamed = (1..=ONE_UPLOAD).find(|&b| short(offer(b)));
    println!(
        "ISSUE_388 op=offer_response path=streamed body={streamed:?} margin_at_transfer_limit={}",
        offer(ONE_UPLOAD)
    );
    assert_eq!(
        streamed,
        None,
        "a streamed answer of one body up to the transfer limit leaves {} B",
        offer(ONE_UPLOAD)
    );

    let upload = |b: usize| role_margin(upload_peak(upload_envelope(b), b, true), ONE_UPLOAD);
    let uploaded = (1..=ONE_UPLOAD).find(|&b| short(upload(b)));
    println!(
        "ISSUE_388 op=upload+flush body={uploaded:?} margin={:?}",
        uploaded.map(upload)
    );
    assert_eq!(
        uploaded,
        Some(2_912),
        "one 2 912 B upload, flushed beside the other phone's queued 4 KB, leaves {:?} B",
        uploaded.map(upload)
    );
    assert!(!short(upload(2_911)));

    let tail = |n: usize| role_margin(store_tail_peak(n, n.min(10)), two);
    let records = (1..2_000).find(|&n| short(tail(n)));
    println!(
        "ISSUE_388 op=store_tail records={records:?} margin={:?}",
        records.map(tail)
    );
    assert_eq!(
        records,
        Some(57),
        "a store tail of 57 records, sorted with heap scratch beside two queued uploads, \
         leaves {:?} B",
        records.map(tail)
    );

    // The round as a whole is the larger of its two steps.
    let encoded = sync_envelope([2_334]).len;
    assert_eq!(
        sync_round_peak(57, 10, encoded, 0, sdu),
        store_tail_peak(57, 10).max(offer_response_peak(encoded, 10, 0, sdu))
    );
}

/// `too_large` is reserved at 8 and trimmed to 64 after the push, so it
/// reaches 65 entries and doubles to 128: 4 KiB held for 2 KiB of ids, and
/// 6 KiB at the growth.
#[test]
fn the_too_large_fifo_outgrows_its_bound() {
    let (capacity, peak) = trimmed_fifo_bytes(TRANSIENT_ID_BYTES, 8, 64);
    println!("TOO_LARGE capacity={capacity} growth_peak={peak} bound=64 reserve=8");
    assert_eq!((capacity, peak), (4_096, 6_144));
    assert_eq!(flush_peak(1_000), 3_000);
}

/// §5 sized the cap from a 104 B record; in RAM the `Peer` is inline in the
/// map at 200 B, and by the census's model a full table is 7 152 B, most of
/// the 8 KiB slice before the offer list the derivation also put there.
#[test]
fn the_peer_table_at_the_cap_against_its_slice() {
    let table = peer_table_bytes(BOARD_MAX_PEERS);
    println!(
        "PEER_TABLE peers={BOARD_MAX_PEERS} census_model={table} record_model={} \
         slice={PEERING_SLICE_BYTES}",
        104 * BOARD_MAX_PEERS
    );
    assert_eq!(table, 7_152);
    assert!(table <= PEERING_SLICE_BYTES);
}

// ---------------------------------------------------------------------------
// The streamed offer response (order 511)
// ---------------------------------------------------------------------------

/// 510's two red offer rows, answered by the streamed path: every offer
/// shape fits the role beside two phones' queued uploads, with the reserve
/// clear. The ten body Python offer is answered with the prefix that fits
/// and the rest waits above the cursor for the next round.
#[test]
fn every_offer_response_fits_beside_two_queued_uploads() {
    let two = 2 * ONE_UPLOAD;
    for (what, bodies, listed) in offer_shapes() {
        let answer = answer_of(&bodies, listed, two);
        let peak =
            offer_response_peak(answer.encoded_len, bodies.len(), listed, SERVE_RESOURCE_SDU);
        let margin = role_margin(peak, two);
        println!(
            "OFFER_RESPONSE {what} wanted={} sent={} withheld={} encoded={} peak={peak} \
             margin={margin} buffered_510={}",
            bodies.len(),
            answer.count,
            answer.withheld,
            answer.encoded_len,
            buffered_offer_response_peak(bodies.iter().copied(), bodies.len(), SERVE_RESOURCE_SDU),
        );
        assert!(answer.count >= 1, "{what}: at least one body goes out");
        assert_eq!(answer.count + answer.withheld, bodies.len(), "{what}");
        assert!(
            margin >= OFFER_RESPONSE_RESERVE_BYTES as isize,
            "{what}: margin {margin}"
        );
        // Maximal: one more body would not have fitted.
        if answer.withheld > 0 {
            let more = sync_envelope(bodies[..=answer.count].iter().copied()).len;
            assert!(!offer_response_fits(
                more,
                bodies.len(),
                listed,
                SERVE_RESOURCE_SDU,
                two
            ));
        }
    }
}

/// The answer's model against the code it describes, for the parts a host
/// test reaches: the planner and the streamed source allocate nothing (the
/// core's scratch is the reader's, here the test's), and the decoded list a
/// `Wanted` response holds is `listed` exact ids. The parts are the core's
/// owned-path parts byte for byte
/// (`a_plain_source_and_a_buffer_build_the_same_resource`,
/// `leviculum-core/src/resource/outgoing.rs`), priced by [`transfer_bytes`]
/// as the measured serve mvr prices them.
#[test]
fn the_streamed_answer_holds_only_what_the_model_counts() {
    let bodies = [4_000usize; 10];
    let (store, plan) = offered_store(&bodies);
    let wire = OfferResponse::Wanted(plan.ids.clone()).encode();
    let (response, decoded) = measured_peak(|| OfferResponse::decode(&wire).unwrap());
    assert_eq!(decoded, 10 * TRANSIENT_ID_BYTES, "listed ids, exact");
    let OfferReply::Send(wanted) = offer_reply(&response, &plan) else {
        panic!("send");
    };
    let mut scratch = vec![0u8; SERVE_RESOURCE_SDU];
    let (streamed, held) = measured_peak(|| {
        let batch = plan_sync_batch(&store, &plan, &wanted, |_, _| true).unwrap();
        let mut source = SyncSource::new(&store, &plan, wanted, batch, 1.0);
        let mut total = 0;
        loop {
            let n = source.read(&mut scratch).unwrap();
            if n == 0 {
                break;
            }
            total += n;
        }
        (total, source.total_len())
    });
    assert_eq!(streamed.0, streamed.1);
    assert_eq!(streamed.0, sync_envelope(bodies).len);
    assert_eq!(held, 0);
    // The model's terms beside the parts, at that shape.
    let tid = TRANSIENT_ID_BYTES;
    let encoded = sync_envelope(bodies).len;
    assert_eq!(
        offer_response_peak(encoded, 10, 10, SERVE_RESOURCE_SDU),
        pushed_capacity(tid, 10) * tid
            + 10 * tid
            + SERVE_RESOURCE_SDU
            + transfer_bytes(encoded, SERVE_RESOURCE_SDU)
    );
}

/// The announced sync limit, derived: what it is, that it is the largest
/// that fits, and the shape it was derived at.
#[test]
fn the_announced_sync_limit_is_the_largest_answer_the_role_funds() {
    let two = 2 * ONE_UPLOAD;
    let fits = |kb: u64| {
        offer_response_fits(
            kb as usize * 1000,
            LORA_OFFER_IDS,
            LORA_OFFER_IDS,
            SERVE_RESOURCE_SDU,
            two,
        )
    };
    let at = |kb: u64| {
        role_margin(
            offer_response_peak(
                kb as usize * 1000,
                LORA_OFFER_IDS,
                LORA_OFFER_IDS,
                SERVE_RESOURCE_SDU,
            ),
            two,
        )
    };
    println!(
        "SYNC_LIMIT derived_kb={BOARD_SYNC_LIMIT_KB} slice={SYNC_SLICE_BYTES} \
         reserve={OFFER_RESPONSE_RESERVE_BYTES} lora_ids={LORA_OFFER_IDS} \
         margin_at_limit={} margin_one_kb_more={}",
        at(BOARD_SYNC_LIMIT_KB),
        at(BOARD_SYNC_LIMIT_KB + 1)
    );
    assert_eq!(BOARD_SYNC_LIMIT_KB, 5);
    assert!(fits(BOARD_SYNC_LIMIT_KB));
    assert!(!fits(BOARD_SYNC_LIMIT_KB + 1));
    // The ids a LoRa offer carries, as the offer planner counts them.
    assert_eq!(LORA_OFFER_IDS, 10);
    assert!(offer_encoded_len(LORA_OFFER_IDS) <= offer_budget_for_mdu(STANDARD_LINK_MDU));
    assert!(offer_encoded_len(LORA_OFFER_IDS + 1) > offer_budget_for_mdu(STANDARD_LINK_MDU));
    // The role's headroom beside two uploads is the slice.
    assert_eq!(
        ROLE_BUDGET_BYTES - ROLE_STRUCTURAL_BYTES - two,
        SYNC_SLICE_BYTES
    );
}

/// The reserve's arithmetic, re-added at the derived limit: the
/// advertisement held three times over (cached, send copy, encrypted
/// packet) at the part count of an answer at the limit, the allocator's
/// block headers, and the field's refused allocation.
#[test]
fn the_offer_response_reserve_covers_what_the_model_leaves_out() {
    let encoded = BOARD_SYNC_LIMIT_KB as usize * 1000;
    let transfer = transfer_bytes(encoded, SERVE_RESOURCE_SDU);
    let parts = leviculum_core::crypto::token_len(
        leviculum_core::resource::RESOURCE_RANDOM_HASH_SIZE + encoded,
    )
    .div_ceil(SERVE_RESOURCE_SDU);
    let advertisement = RESOURCE_ADV_OVERHEAD + RESOURCE_HASHMAP_LEN * parts;
    // The parts, their spine and hashmap, the scratch, the plan, the list,
    // the decoded response: one block each.
    let blocks = parts + 5;
    let need = FIELD_FAILED_ALLOCATION_BYTES + 3 * advertisement + blocks * 8;
    println!(
        "OFFER_RESERVE parts={parts} advertisement={advertisement} blocks={blocks} need={need} \
         reserve={OFFER_RESPONSE_RESERVE_BYTES} transfer={transfer}"
    );
    assert!(need <= OFFER_RESPONSE_RESERVE_BYTES, "need {need}");
}
