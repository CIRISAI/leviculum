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

use leviculum_heap_budget::peak::{
    flush_peak, inbound_resource_peak, offer_encoding, offer_response_peak, peer_table_bytes,
    pushed_capacity, pushed_capacity_from, pushed_peak, pushed_peak_from, role_margin,
    stable_sort_scratch, store_tail_peak, sync_envelope, sync_round_peak, trimmed_fifo_bytes,
    upload_peak, ByteVec, PEER_BYTES, STORED_MESSAGE_BYTES, TRANSIENT_ID_BYTES,
};
use leviculum_heap_budget::{
    BOARD_MAX_PEERS, BOARD_SYNC_LIMIT_KB, BOARD_TRANSFER_LIMIT_KB, FIELD_FAILED_ALLOCATION_BYTES,
    PEERING_SLICE_BYTES, ROLE_BUDGET_BYTES, ROLE_STRUCTURAL_BYTES, SERVE_RESOURCE_SDU,
};
use leviculum_lxmf::peering::{PeerOffer, PeerSyncEnvelope};
use leviculum_lxmf::propagation_store::StoredMessage;

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

fn field_rows() -> Vec<Row> {
    let sdu = SERVE_RESOURCE_SDU;
    let two = 2 * ONE_UPLOAD;
    let mut rows = Vec::new();
    // Offer response: what the board sends a peer. The round runs beside
    // two phones' queued uploads.
    for (what, bodies, offered) in [
        ("1 KB body", vec![1_000], 1),
        ("link-MDU body", vec![LINK_MDU], 1),
        (
            "8 KB: a board peer's sync limit, two bodies",
            vec![3_960, 3_960],
            2,
        ),
        (
            "ten 4 KB bodies: a Python peer, a LoRa offer",
            vec![4_000; 10],
            10,
        ),
    ] {
        rows.push(Row {
            op: "offer_response",
            inputs: format!("{what} bodies={bodies:?} offered={offered}"),
            peak: offer_response_peak(bodies.iter().copied(), offered, sdu),
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
            "8 KB upload: the resource limit",
            BOARD_SYNC_LIMIT_KB as usize * 1000,
            vec![BOARD_SYNC_LIMIT_KB as usize * 1000 - 14],
            ONE_UPLOAD,
        ),
        (
            "8 KB sync: two bodies",
            sync_envelope([3_960, 3_960]).len,
            vec![3_960, 3_960],
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
    let expected: [(usize, isize); 16] = [
        (3_538, 4_462),
        (1_800, 6_200),
        (24_996, -16_996),
        (149_584, -141_584),
        (2_028, 10_986),
        (890, 11_555),
        (16_000, 4_000),
        (15_881, 56),
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
    assert_eq!(rows.len(), expected.len());
    for (row, (peak, margin)) in rows.iter().zip(expected) {
        println!(
            "PEAK op={} peak={} concurrent={} reserve_funded={} margin={} | {}",
            row.op,
            row.peak,
            row.concurrent,
            row.reserve_funded,
            row.margin(),
            row.inputs
        );
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
/// green is a design decision, not a fix: which copies to remove first
/// (pass B, ordered by the reviewer), whether the board refuses a Python
/// peer's offer it cannot fund, and whether the store tail is walked in
/// pages instead of collected. Run it with `--ignored` to see what is left.
#[test]
#[ignore = "#388: today's overdraws; which reduction lands first is pass B's design decision"]
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

    let offer = |body: usize| role_margin(offer_response_peak([body], 1, sdu), two);
    let body = (1..=ONE_UPLOAD).find(|&b| short(offer(b)));
    println!(
        "ISSUE_388 op=offer_response body={body:?} margin={:?}",
        body.map(offer)
    );
    assert_eq!(
        body,
        Some(2_334),
        "one offered body of 2 334 B beside two queued 4 KB uploads leaves {:?} B",
        body.map(offer)
    );
    assert!(!short(offer(2_333)));

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
    assert_eq!(
        sync_round_peak(57, 10, [2_334], sdu),
        store_tail_peak(57, 10).max(offer_response_peak([2_334], 10, sdu))
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
