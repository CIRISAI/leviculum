//! The answer to an `/offer` a peer wants more of than the sender can hold
//! (leviculum#388): which bodies one round carries, where the cursor lands,
//! the bytes the streamed envelope produces, and what it holds while it
//! produces them.
//!
//! The peer is in its Python shape. Its `/offer` response is what
//! `offer_request` returns (`reference/LXMF/LXMF/LXMRouter.py:2317-2328`):
//! msgpack `True` when it wants every offered id, the list of wanted ids
//! as bins when it wants some. Its sync limit is the reference's
//! `SYNC_LIMIT`, 10 240 KB, so its limit is not what bounds the answer;
//! the sender's heap is. The envelope it receives is
//! `msgpack([timestamp, [lxmf_data, ...]])` (`LXMPeer.py:466`).
//!
//! The buffered answer this replaces held every wanted body, the list of
//! wanted ids and the envelope grown around them at once; the test that
//! measures it is the "before" the streamed source is compared with.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use leviculum_core::resource::{ResourceSource, SourceError};
use leviculum_lxmf::peering::{build_offer, offer_reply, sync_envelope_len, OFFER_BYTES_LIMIT};
use leviculum_lxmf::propagation_store::{MemoryPropagationStore, PropagationStore};
use leviculum_lxmf::{
    plan_sync_batch, response_action, OfferPlan, OfferReply, OfferResponse, Peer, PeerRecord,
    PeerSyncEnvelope, ResponseAction, SyncSource, TransientId, WantedIds,
};

// A per-thread counting allocator, as in
// `leviculum-nrf/heap-budget/tests/peaks.rs`: the harness runs tests on
// several threads and a global counter would read the neighbours'.
struct Counting;

thread_local! {
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every call forwards to `System` with the caller's layout; the
// bookkeeping touches only const-initialised thread locals.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = LIVE.try_with(|live| {
            let now = live.get() + layout.size();
            live.set(now);
            let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
        });
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _ = LIVE.try_with(|live| live.set(live.get().saturating_sub(layout.size())));
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

/// The resource part size at Reticulum's base MTU, the scratch the core's
/// streamed build reads with.
const SDU: usize = 464;

/// A ten body store, each body `size` bytes and distinct.
fn store_of(sizes: &[usize]) -> (MemoryPropagationStore, Vec<TransientId>) {
    let mut store = MemoryPropagationStore::new(1 << 20);
    let mut ids = Vec::new();
    for (n, &size) in sizes.iter().enumerate() {
        let mut id = [0u8; 32];
        id[0] = n as u8 + 1;
        let body: Vec<u8> = (0..size).map(|i| (i * 7 + n * 31) as u8).collect();
        store.append(&id, 1_000 + n as u64, 20, &body).unwrap();
        ids.push(id);
    }
    (store, ids)
}

/// A Python propagation node as a peer: the reference's default limits
/// (`propagation_transfer_limit` 256 KB, `SYNC_LIMIT` 10 240 KB), cursor at
/// zero, stamp cost at the reference's 16 with its flexibility of 3.
fn python_peer() -> Peer {
    PeerRecord {
        destination_hash: [1; 16],
        identity_hash: None,
        public_keys: None,
        peering_key: None,
        transfer_limit_kb: 256,
        sync_limit_kb: 10_240,
        stamp_cost: 16,
        stamp_cost_flexibility: 3,
        peering_cost: 18,
        peering_timebase: 1,
        last_heard: 0,
        cursor: 0,
        is_static: false,
    }
    .into_peer()
}

/// What a store holds, in append order, as `build_offer` sees it.
fn plan_for(store: &MemoryPropagationStore) -> OfferPlan {
    let mut entries = Vec::new();
    store.for_each(&mut |meta| entries.push(*meta)).unwrap();
    entries.sort_by_key(|meta| meta.sequence);
    build_offer(&python_peer(), &entries, OFFER_BYTES_LIMIT).unwrap()
}

fn sequence_of(store: &MemoryPropagationStore, id: &TransientId) -> u64 {
    store.meta(id).unwrap().unwrap().sequence
}

/// `True`, as `umsgpack.packb(True)`.
fn python_want_all() -> OfferResponse {
    OfferResponse::decode(&[0xc3]).unwrap()
}

/// A wanted list, as `umsgpack.packb([bytes, ...])`: array, bin8 each.
fn python_want_list(ids: &[TransientId]) -> OfferResponse {
    let mut wire = vec![0x90 | ids.len() as u8];
    for id in ids {
        wire.extend_from_slice(&[0xc4, 32]);
        wire.extend_from_slice(id);
    }
    OfferResponse::decode(&wire).unwrap()
}

/// Read a source to its end, twice (the core builds in two passes), and
/// return the bytes of the second pass.
fn drain(source: &mut dyn ResourceSource) -> Result<Vec<u8>, SourceError> {
    let mut scratch = vec![0u8; SDU];
    let mut out = Vec::with_capacity(source.total_len());
    for _ in 0..2 {
        out.clear();
        source.rewind()?;
        loop {
            let n = source.read(&mut scratch)?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&scratch[..n]);
        }
    }
    Ok(out)
}

fn bodies(store: &MemoryPropagationStore, ids: &[TransientId]) -> Vec<Vec<u8>> {
    ids.iter()
        .map(|id| store.read_body(id).unwrap().unwrap())
        .collect()
}

/// A Python peer wants all ten 4 000 B bodies; the sender can afford two.
/// It sends the first two in plan order, the cursor stops just below the
/// third, and nothing above it is lost: the next round offers it again.
#[test]
fn a_python_want_all_is_answered_with_the_prefix_that_fits() {
    let (store, ids) = store_of(&[4_000; 10]);
    let plan = plan_for(&store);
    assert_eq!(plan.ids, ids, "the plan offers all ten in append order");

    let response = python_want_all();
    let OfferReply::Send(wanted) = offer_reply(&response, &plan) else {
        panic!("a want-all is a send");
    };
    // Two bodies encode to 8 017 B, three to 12 020 B.
    let cap = 9_000;
    let batch = plan_sync_batch(&store, &plan, &wanted, |_, len| len <= cap).unwrap();
    assert_eq!(batch.count, 2);
    assert_eq!(batch.withheld, 8);
    assert_eq!(batch.payload, 8_000);
    assert_eq!(batch.encoded_len, sync_envelope_len(2, 2 * (3 + 4_000)));
    assert_eq!(batch.cursor_target, sequence_of(&store, &ids[2]) - 1);
    assert!(batch.cursor_target < plan.cursor_target);

    // The streamed envelope is the buffered one over the same two bodies,
    // and the receiver's decoder reads it back.
    let mut source = SyncSource::new(&store, &plan, wanted, batch, 1_700_000_000.5);
    assert_eq!(source.total_len(), batch.encoded_len);
    let streamed = drain(&mut source).unwrap();
    let buffered = PeerSyncEnvelope {
        timestamp: 1_700_000_000.5,
        messages: bodies(&store, &ids[..2]),
    }
    .encode();
    assert_eq!(streamed, buffered);
    let decoded = PeerSyncEnvelope::decode(&streamed).unwrap();
    assert_eq!(decoded.messages, bodies(&store, &ids[..2]));
}

/// A Python peer wants every other body. The declined ones are not
/// withheld (the peer has them), so the cursor stops below the first
/// WANTED body that did not fit, not below the first declined one.
#[test]
fn a_python_want_list_is_answered_in_plan_order_and_declined_ids_do_not_hold_the_cursor() {
    let (store, ids) = store_of(&[4_000; 10]);
    let plan = plan_for(&store);
    // The peer lists them out of order; the batch walks the plan.
    let asked = [ids[6], ids[0], ids[4], ids[2], ids[8]];
    let response = python_want_list(&asked);
    let OfferReply::Send(wanted) = offer_reply(&response, &plan) else {
        panic!("a want list naming offered ids is a send");
    };
    let batch = plan_sync_batch(&store, &plan, &wanted, |_, len| len <= 9_000).unwrap();
    assert_eq!(batch.count, 2, "ids[0] and ids[2]");
    assert_eq!(batch.withheld, 3, "ids[4], ids[6], ids[8]");
    assert_eq!(batch.end, 3, "the walk ends at ids[2]");
    assert_eq!(batch.cursor_target, sequence_of(&store, &ids[4]) - 1);

    let mut source = SyncSource::new(&store, &plan, wanted, batch, 2.0);
    let streamed = drain(&mut source).unwrap();
    let expected = PeerSyncEnvelope {
        timestamp: 2.0,
        messages: bodies(&store, &[ids[0], ids[2]]),
    }
    .encode();
    assert_eq!(streamed, expected);
}

/// Everything wanted fits: the batch is the whole want and the cursor
/// lands on the plan's target, as the buffered answer's did.
#[test]
fn a_want_that_fits_is_sent_whole() {
    let (store, ids) = store_of(&[300, 1_000, 200]);
    let plan = plan_for(&store);
    let response = python_want_all();
    let OfferReply::Send(wanted) = offer_reply(&response, &plan) else {
        panic!("send");
    };
    let batch = plan_sync_batch(&store, &plan, &wanted, |_, _| true).unwrap();
    assert_eq!((batch.count, batch.withheld), (3, 0));
    assert_eq!(batch.cursor_target, plan.cursor_target);
    let mut source = SyncSource::new(&store, &plan, wanted, batch, 3.0);
    let streamed = drain(&mut source).unwrap();
    assert_eq!(
        streamed,
        PeerSyncEnvelope {
            timestamp: 3.0,
            messages: bodies(&store, &ids),
        }
        .encode()
    );
    // And the collected action names the same bodies.
    assert_eq!(
        response_action(&response, &plan),
        ResponseAction::SendMessages(ids.clone())
    );
}

/// Not even the first wanted body fits: nothing is sent and the cursor
/// does not move past it.
#[test]
fn a_first_body_that_does_not_fit_sends_nothing_and_keeps_the_cursor() {
    let (store, ids) = store_of(&[4_000; 3]);
    let plan = plan_for(&store);
    let response = python_want_all();
    let OfferReply::Send(wanted) = offer_reply(&response, &plan) else {
        panic!("send");
    };
    let batch = plan_sync_batch(&store, &plan, &wanted, |_, len| len < 4_000).unwrap();
    assert_eq!((batch.count, batch.withheld), (0, 3));
    assert_eq!(batch.cursor_target, sequence_of(&store, &ids[0]) - 1);
}

/// Sixteen bodies cross the msgpack list header from one byte to three;
/// the planned length and the streamed bytes agree across it.
#[test]
fn the_list_header_grows_at_sixteen_bodies() {
    let (store, ids) = store_of(&[60; 20]);
    let plan = plan_for(&store);
    let response = python_want_all();
    let OfferReply::Send(wanted) = offer_reply(&response, &plan) else {
        panic!("send");
    };
    for take in [15usize, 16, 17] {
        let batch = plan_sync_batch(&store, &plan, &wanted, |count, _| count <= take).unwrap();
        assert_eq!(batch.count, take);
        let mut source = SyncSource::new(&store, &plan, wanted, batch, 4.0);
        let streamed = drain(&mut source).unwrap();
        assert_eq!(streamed.len(), batch.encoded_len, "take={take}");
        assert_eq!(
            streamed,
            PeerSyncEnvelope {
                timestamp: 4.0,
                messages: bodies(&store, &ids[..take]),
            }
            .encode()
        );
    }
}

/// A wanted id purged before the answer is skipped, as the buffered path
/// skipped it; one purged between the plan and the stream fails the build
/// instead of shipping a resource shorter than it advertised.
#[test]
fn a_purged_body_is_skipped_before_the_plan_and_fails_the_stream_after_it() {
    let (mut store, ids) = store_of(&[500; 4]);
    let plan = plan_for(&store);
    store.purge(&ids[1]).unwrap();
    let response = python_want_all();
    let OfferReply::Send(wanted) = offer_reply(&response, &plan) else {
        panic!("send");
    };
    let batch = plan_sync_batch(&store, &plan, &wanted, |_, _| true).unwrap();
    assert_eq!(batch.count, 3);
    assert_eq!(batch.cursor_target, plan.cursor_target);
    {
        let mut source = SyncSource::new(&store, &plan, wanted, batch, 5.0);
        let streamed = drain(&mut source).unwrap();
        assert_eq!(
            PeerSyncEnvelope::decode(&streamed).unwrap().messages,
            bodies(&store, &[ids[0], ids[2], ids[3]])
        );
    }

    store.purge(&ids[3]).unwrap();
    let mut source = SyncSource::new(&store, &plan, wanted, batch, 5.0);
    assert_eq!(drain(&mut source).err(), Some(SourceError::LengthChanged));
}

/// What one answer holds, before and after, for the field's shape: a
/// Python peer wanting ten 4 000 B bodies.
///
/// Before: the buffered answer (`response_action`'s collected list, every
/// body read, `PeerSyncEnvelope::encode` grown around them) held more than
/// twice the payload before the resource path copied anything. After: the
/// planner and the source hold nothing; the only buffer is the core's one
/// part of scratch, which the reader here supplies.
#[test]
fn the_streamed_answer_holds_no_body_where_the_buffered_one_held_them_all() {
    let (store, ids) = store_of(&[4_000; 10]);
    let plan = plan_for(&store);
    let response = python_want_all();

    let (encoded, buffered) = measured_peak(|| {
        let ResponseAction::SendMessages(wanted) = response_action(&response, &plan) else {
            panic!("send");
        };
        let messages: Vec<Vec<u8>> = wanted
            .iter()
            .filter_map(|id| store.read_body(id).unwrap())
            .collect();
        PeerSyncEnvelope {
            timestamp: 6.0,
            messages,
        }
        .encode()
    });
    println!("SYNC_ANSWER path=buffered peak={buffered} payload=40000");
    assert!(
        buffered > 2 * 40_000,
        "the buffered answer held {buffered} B for 40 000 B of bodies"
    );

    let OfferReply::Send(wanted) = offer_reply(&response, &plan) else {
        panic!("send");
    };
    let mut scratch = vec![0u8; SDU];
    let (streamed, held) = measured_peak(|| {
        let batch = plan_sync_batch(&store, &plan, &wanted, |_, _| true).unwrap();
        let mut source = SyncSource::new(&store, &plan, wanted, batch, 6.0);
        let mut out = 0usize;
        loop {
            let n = source.read(&mut scratch).unwrap();
            if n == 0 {
                break;
            }
            out += n;
        }
        out
    });
    println!("SYNC_ANSWER path=streamed peak={held} streamed={streamed}");
    assert_eq!(streamed, encoded.len());
    assert_eq!(held, 0, "the planner and the source allocate nothing");
    assert_eq!(ids.len(), 10);
}

/// The borrowed reply and the collected action agree on every response
/// shape, so the board (reply) and lnpnd (action) answer alike.
#[test]
fn the_borrowed_reply_is_the_collected_action() {
    let (store, ids) = store_of(&[100; 3]);
    let plan = plan_for(&store);
    let stranger = [0xEE; 32];
    for response in [
        OfferResponse::decode(&[0xc2]).unwrap(),
        python_want_all(),
        python_want_list(&[ids[2], stranger]),
        python_want_list(&[stranger]),
    ] {
        let action = response_action(&response, &plan);
        let collected = match offer_reply(&response, &plan) {
            OfferReply::Send(wanted) => ResponseAction::SendMessages(
                plan.ids
                    .iter()
                    .filter(|id| wanted.wants(id))
                    .copied()
                    .collect(),
            ),
            OfferReply::Concluded => ResponseAction::Concluded,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(action, collected, "{response:?}");
    }
    assert!(matches!(
        offer_reply(&python_want_list(&[stranger]), &plan),
        OfferReply::Concluded
    ));
    let _ = WantedIds::All;
}
