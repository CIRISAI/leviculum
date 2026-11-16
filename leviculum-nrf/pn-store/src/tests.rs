//! Host tests over regions the record log itself wrote.
//!
//! The load-bearing property is agreement: [`scan`] must see exactly what
//! `RecordLog::for_each_seq` sees, on bytes the log produced — reclaim,
//! purge and interruption included — because the board answers every read
//! from the synchronous scan while the log alone performs the writes. A
//! drift between the two would be a store that proves uploads it cannot
//! list.

extern crate std;

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;
use core::cell::RefCell;

use leviculum_lxmf::peering::{
    build_offer, OfferBuilder, OfferPlan, Peer, PeerRecord, PeerStore, OFFER_BYTES_LIMIT,
};
use leviculum_lxmf::propagation_store::{PropagationBodyStore, PropagationStore, StoredMessage};
use leviculum_record_log::sim::{block_on, SimNor};
use leviculum_record_log::{RecordLog, KEY_LEN, SECTOR_SIZE};

use super::*;

const SECTORS: u32 = 4;
/// The field-median stored object (concept page §2).
const FIELD_BODY: usize = 304;

fn key(n: u32) -> [u8; KEY_LEN] {
    let mut k = [0u8; KEY_LEN];
    k[0..4].copy_from_slice(&n.to_le_bytes());
    k[28..32].copy_from_slice(&n.to_be_bytes());
    k
}

/// A message body: destination hash in the first 16 bytes, then filler.
fn msg_body(dest: u8, len: usize) -> Vec<u8> {
    let mut body = vec![dest; len.max(MIN_BODY_LEN)];
    body[DESTINATION_LENGTH..].fill(0xAB);
    body
}

/// The board's memory-mapped view, as a test double: a byte copy of the
/// simulated part, refreshed by [`sync`] after every flush — exactly the
/// "reads see what the log last committed" relationship the firmware has
/// for free.
#[derive(Clone)]
struct SharedRegion(Rc<RefCell<Vec<u8>>>);

impl SharedRegion {
    fn new(sectors: u32) -> Self {
        Self(Rc::new(RefCell::new(vec![
            0xFF;
            (sectors * SECTOR_SIZE) as usize
        ])))
    }
}

impl Region for SharedRegion {
    fn with_bytes<T>(&self, f: impl FnOnce(&[u8]) -> T) -> T {
        f(&self.0.borrow())
    }
}

fn sync(region: &SharedRegion, log: &mut RecordLog<SimNor>) {
    region
        .0
        .borrow_mut()
        .copy_from_slice(log.flash_mut().bytes());
}

async fn fresh(sectors: u32) -> RecordLog<SimNor> {
    RecordLog::open(SimNor::new(sectors), 0, sectors * SECTOR_SIZE)
        .await
        .unwrap()
}

/// Apply one [`FlushOp`] to the log — the same two verbs the record-store
/// task performs on the board.
async fn apply(log: &mut RecordLog<SimNor>, op: &FlushOp) {
    match op {
        FlushOp::Append {
            key,
            time,
            tag,
            body,
        } => {
            log.append(key, *time, *tag, body).await.unwrap();
        }
        FlushOp::Purge { offset, key } | FlushOp::ConfirmedPurge { offset, key } => {
            if let Some(record) = log.record_at(*offset).await.unwrap() {
                if &record.key == key {
                    log.purge(&record).await.unwrap();
                }
            }
        }
    }
}

/// Drain a message store's queue into the log and refresh the region.
fn flush_messages(
    store: &mut PnStore<SharedRegion>,
    region: &SharedRegion,
    log: &mut RecordLog<SimNor>,
) {
    block_on(async {
        while let Some(op) = store.peek_op().cloned() {
            apply(log, &op).await;
            store.op_done(true);
            sync(region, log);
        }
    });
}

/// Drain a peer store's queue into the log and refresh the region.
fn flush_peers(
    store: &mut PnPeerStore<SharedRegion>,
    region: &SharedRegion,
    log: &mut RecordLog<SimNor>,
) {
    block_on(async {
        while let Some(op) = store.peek_op().cloned() {
            apply(log, &op).await;
            store.op_done(true);
            sync(region, log);
        }
    });
}

fn directory<R: Region>(store: &PnStore<R>) -> Vec<StoredMessage> {
    let mut out = Vec::new();
    store.for_each(&mut |meta| out.push(*meta)).unwrap();
    out.sort_by_key(|meta| meta.sequence);
    out
}

fn ordered_directory<R: Region>(store: &PnStore<R>) -> Vec<StoredMessage> {
    let mut out = Vec::new();
    store
        .for_each_ordered(&mut |meta| {
            out.push(*meta);
            ControlFlow::Continue(())
        })
        .unwrap();
    out
}

fn ordered_offer<R: Region>(store: &PnStore<R>, peer: &Peer, budget: usize) -> Option<OfferPlan> {
    let mut builder = OfferBuilder::new(peer, budget);
    store
        .for_each_ordered(&mut |entry| builder.push(entry))
        .unwrap();
    builder.finish()
}

async fn seeded_ordering_log(region: &SharedRegion, pages: u32) -> RecordLog<SimNor> {
    let mut log = fresh(pages).await;
    let mut purge_at = 0;
    for n in 0..32 {
        let len = if n % 5 == 0 { 1_900 } else { FIELD_BODY };
        let offset = log
            .append(&key(n), n, (n % 4) as u8, &msg_body(n as u8, len))
            .await
            .unwrap();
        if n == 31 {
            purge_at = offset;
        }
    }
    let purged = log.record_at(purge_at).await.unwrap().unwrap();
    log.purge(&purged).await.unwrap();
    log.append(&key(996), 40, 0xB0, &[0xAB; 64]).await.unwrap();
    log.append(
        &key(997),
        41,
        TAG_PEER,
        &encode_peer_record(&full_peer(4, 0)),
    )
    .await
    .unwrap();
    log.append(&key(998), 42, 0, &[0xAB; MIN_BODY_LEN - 1])
        .await
        .unwrap();
    sync(region, &mut log);
    log
}

#[test]
fn mapped_ordered_traversal_filters_and_feeds_bounded_offers_in_one_region_view() {
    block_on(async {
        let inner = SharedRegion::new(SECTORS);
        let _log = seeded_ordering_log(&inner, SECTORS).await;
        let scanned = PnStore::new(inner.clone(), SECTORS);
        let mut expected = directory(&scanned);
        let mut physical = Vec::new();
        scanned
            .for_each(&mut |entry| physical.push(*entry))
            .unwrap();
        assert_ne!(
            physical, expected,
            "reclaim separates physical and append order"
        );
        for excluded in [31, 996, 997, 998] {
            assert!(!expected
                .iter()
                .any(|entry| entry.transient_id == key(excluded)));
        }
        let reads = Rc::new(Cell::new(0));
        let mut store = PnStore::new(
            CountingRegion {
                inner,
                reads: reads.clone(),
            },
            SECTORS,
        );
        let masked = expected[1].transient_id;
        assert!(store.purge(&masked).unwrap());
        expected.retain(|entry| entry.transient_id != masked);
        store
            .append(&key(999), 43, 3, &msg_body(1, FIELD_BODY))
            .unwrap();
        assert!(store.contains(&key(999)).unwrap());
        reads.set(0);
        assert_eq!(ordered_directory(&store), expected);
        assert_eq!(
            reads.get(),
            1,
            "page collection and walk share one mapped view"
        );
        assert_eq!(store.ordered_traversal_heap_bound(), SECTORS as usize * 8);
        let mut visited = 0;
        store
            .for_each_ordered(&mut |entry| {
                assert_eq!(*entry, expected[0]);
                visited += 1;
                ControlFlow::Break(())
            })
            .unwrap();
        assert_eq!(visited, 1);
        assert_eq!(reads.get(), 2);

        let mut peer = full_peer(7, 0).into_peer();
        peer.stamp_cost = 2;
        peer.stamp_cost_flexibility = 0;
        for cursor in [
            0,
            expected[0].sequence,
            expected[expected.len() / 2].sequence,
        ] {
            peer.cursor = cursor;
            for transfer_limit in [1, 4] {
                peer.transfer_limit_kb = transfer_limit;
                for sync_limit in [1, 8] {
                    peer.sync_limit_kb = sync_limit;
                    for budget in [0, 35, 70, 127, OFFER_BYTES_LIMIT] {
                        let before = reads.get();
                        assert_eq!(
                            ordered_offer(&store, &peer, budget),
                            build_offer(&peer, &expected, budget)
                        );
                        assert_eq!(reads.get(), before + 1);
                    }
                }
            }
        }
        store.op_done(false);
        assert!(ordered_directory(&store)
            .iter()
            .any(|entry| entry.transient_id == masked));
    });
}

#[test]
fn confirmed_purges_keep_owned_targets_until_authoritative_commit() {
    {
        block_on(async {
            let region = SharedRegion::new(SECTORS);
            let mut log = fresh(SECTORS).await;
            for n in [1, 2] {
                log.append(&key(n), n, 3, &msg_body(n as u8, FIELD_BODY))
                    .await
                    .unwrap();
            }
            sync(&region, &mut log);
            let mut store = PnStore::new(region.clone(), SECTORS);
            assert_eq!(
                store.purge_confirmed(&key(99)).unwrap(),
                ConfirmedPurge::Absent
            );
            store
                .append(&key(3), 3, 0, &msg_body(3, FIELD_BODY))
                .unwrap();
            assert_eq!(
                store.purge_confirmed(&key(3)).unwrap(),
                ConfirmedPurge::Committed
            );
            assert_eq!(
                store.pending_ops(),
                0,
                "RAM-only removal owes no flash write"
            );

            assert!(store.purge(&key(2)).unwrap());
            assert!(matches!(store.peek_op(), Some(FlushOp::Purge { .. })));
            store.op_done(false);
            assert_eq!(
                store.purge_confirmed(&key(1)).unwrap(),
                ConfirmedPurge::Queued
            );
            let explicit = store.peek_op().cloned().unwrap();
            let (offset, owned_id) = match &explicit {
                FlushOp::ConfirmedPurge { offset, key } => (*offset, *key),
                other => panic!("{other:?}"),
            };
            assert_eq!(owned_id, key(1));
            assert!(!store.contains(&owned_id).unwrap());
            assert!(!store.confirmed_target_deleted(offset, &owned_id));
            assert_eq!(store.peek_op(), Some(&explicit));
            assert!(!store.confirmed_target_deleted(offset, &owned_id));

            // The device commits but its reply is an I/O failure. The owned
            // target is retired once; only authoritative bytes prove deletion.
            apply(&mut log, &explicit).await;
            sync(&region, &mut log);
            assert_eq!(store.op_done(false), Some(explicit));
            assert!(store.confirmed_target_deleted(offset, &owned_id));
            assert!(!store.contains(&owned_id).unwrap());
            assert!(store.contains(&key(2)).unwrap());
        });
    }
}

#[test]
fn sync_scan_agrees_with_the_logs_own_iteration() {
    block_on(async {
        let mut log = fresh(SECTORS).await;
        for n in 0..7u32 {
            log.append(
                &key(n),
                1000 + n,
                (n % 5) as u8,
                &msg_body(n as u8, FIELD_BODY),
            )
            .await
            .unwrap();
        }
        // One purged record: the scan must see it as not-live, exactly as
        // the log reports it.
        let mut third = None;
        log.for_each(|r| {
            if r.key == key(3) {
                third = Some(*r);
            }
        })
        .await
        .unwrap();
        log.purge(&third.unwrap()).await.unwrap();

        let mut from_log: Vec<([u8; KEY_LEN], u32, u8, bool, u64)> = Vec::new();
        log.for_each_seq(|page_seq, r| {
            from_log.push((
                r.key,
                r.time,
                r.tag,
                r.is_live(),
                sequence(page_seq, (r.offset % SECTOR_SIZE) as u16),
            ));
        })
        .await
        .unwrap();

        let mut from_scan: Vec<([u8; KEY_LEN], u32, u8, bool, u64)> = Vec::new();
        scan(log.flash_mut().bytes(), |r| {
            from_scan.push((*r.key, r.time, r.tag, r.live, r.sequence()));
        });

        from_log.sort();
        from_scan.sort();
        assert_eq!(from_log, from_scan);
    });
}

#[test]
fn sequence_mapping_is_append_order_across_reclaim() {
    block_on(async {
        let region = SharedRegion::new(3);
        let mut log = fresh(3).await;
        let per_page = 4084 / leviculum_record_log::record_stride(FIELD_BODY);
        let total = (3 * per_page + 2) as u32;
        for n in 0..total {
            log.append(&key(n), n, 1, &msg_body(n as u8, FIELD_BODY))
                .await
                .unwrap();
        }
        sync(&region, &mut log);
        let store = PnStore::new(region.clone(), 3);
        let dir = directory(&store);
        // Page 0 was reclaimed on the lap; what survives is a suffix of
        // the append order and the sequences are strictly increasing.
        assert!(dir.len() < total as usize);
        let first = total - dir.len() as u32;
        for (i, meta) in dir.iter().enumerate() {
            assert_eq!(meta.transient_id, key(first + i as u32));
        }
        assert!(dir.windows(2).all(|w| w[0].sequence < w[1].sequence));
    });
}

#[test]
fn bench_and_peer_records_are_invisible_to_the_message_store() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        log.append(&key(1), 1, 3, &msg_body(1, FIELD_BODY))
            .await
            .unwrap();
        // A bench record (TAG_BENCH = 0xB0, record_store.rs) and a peer
        // record, both above the message clamp.
        log.append(&key(2), 2, 0xB0, &msg_body(2, 64))
            .await
            .unwrap();
        log.append(&key(3), 3, TAG_PEER, &msg_body(3, PEER_RECORD_LEN))
            .await
            .unwrap();
        sync(&region, &mut log);

        let store = PnStore::new(region, SECTORS);
        let dir = directory(&store);
        assert_eq!(dir.len(), 1);
        assert_eq!(dir[0].transient_id, key(1));
        assert_eq!(store.count().unwrap(), 1);
        assert!(!store.contains(&key(2)).unwrap());
        assert!(store.read_body(&key(3)).unwrap().is_none());
    });
}

#[test]
fn a_queued_append_is_deduplicated_but_not_offered() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut store = PnStore::new(region.clone(), SECTORS);

        store
            .append(&key(1), 1000, 12, &msg_body(1, FIELD_BODY))
            .unwrap();
        // Visible to duplicate detection, invisible to the directory: a
        // record that is not yet durable must not be offered or listed.
        assert!(store.contains(&key(1)).unwrap());
        assert!(store.read_body(&key(1)).unwrap().is_some());
        assert_eq!(directory(&store).len(), 0);
        assert_eq!(store.newest_sequence().unwrap(), 0);

        flush_messages(&mut store, &region, &mut log);
        let dir = directory(&store);
        assert_eq!(dir.len(), 1);
        assert_eq!(dir[0].transient_id, key(1));
        assert_eq!(dir[0].stamp_value, 12);
        assert_eq!(dir[0].received_at, 1000);
        // The sequence is the real on-flash one: page 0, first record.
        assert_eq!(dir[0].sequence, sequence(0, 12));
        assert!(store.contains(&key(1)).unwrap());
    });
}

#[test]
fn purge_of_a_queued_append_never_reaches_flash() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut store = PnStore::new(region.clone(), SECTORS);

        store
            .append(&key(1), 1, 0, &msg_body(1, FIELD_BODY))
            .unwrap();
        assert!(store.purge(&key(1)).unwrap());
        assert_eq!(store.pending_ops(), 0);
        assert!(!store.contains(&key(1)).unwrap());
        flush_messages(&mut store, &region, &mut log);
        assert_eq!(log.count().await.unwrap(), (0, 0));
    });
}

#[test]
fn purge_masks_and_a_failed_flush_unmasks() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut store = PnStore::new(region.clone(), SECTORS);
        store
            .append(&key(1), 1, 0, &msg_body(1, FIELD_BODY))
            .unwrap();
        flush_messages(&mut store, &region, &mut log);

        assert!(store.purge(&key(1)).unwrap());
        assert_eq!(directory(&store).len(), 0, "masked while queued");
        assert!(!store.contains(&key(1)).unwrap());

        // The flash refused the purge: the mask is dropped and the record
        // honestly reappears.
        let dropped = store.op_done(false).unwrap();
        assert!(matches!(dropped, FlushOp::Purge { .. }));
        assert_eq!(directory(&store).len(), 1);

        // Purge again, and this time the flush lands.
        assert!(store.purge(&key(1)).unwrap());
        flush_messages(&mut store, &region, &mut log);
        assert_eq!(directory(&store).len(), 0);
        assert_eq!(log.count().await.unwrap(), (0, 1));
    });
}

#[test]
fn stamp_value_above_the_clamp_is_stored_clamped() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut store = PnStore::new(region.clone(), SECTORS);
        store
            .append(&key(1), 1, 200, &msg_body(1, FIELD_BODY))
            .unwrap();
        flush_messages(&mut store, &region, &mut log);
        let dir = directory(&store);
        assert_eq!(dir[0].stamp_value, MESSAGE_TAG_MAX);
    });
}

fn full_peer(n: u8, cursor: u64) -> PeerRecord {
    PeerRecord {
        destination_hash: [n; 16],
        identity_hash: Some([n ^ 0xFF; 16]),
        public_keys: Some([n.wrapping_add(2); 64]),
        peering_key: Some(([n.wrapping_add(1); 32], 18)),
        transfer_limit_kb: 4,
        sync_limit_kb: 32,
        stamp_cost: 13,
        stamp_cost_flexibility: 3,
        peering_cost: 1,
        peering_timebase: 1_700_000_000 + n as u64,
        last_heard: 1_700_000_100 + n as u64,
        cursor,
        is_static: n.is_multiple_of(2),
        traffic: Default::default(),
    }
}

#[test]
fn peer_record_round_trip() {
    let full = full_peer(7, sequence(3, 512));
    assert_eq!(decode_peer_record(&encode_peer_record(&full)), Some(full));

    let minimal = PeerRecord {
        identity_hash: None,
        public_keys: None,
        peering_key: None,
        is_static: false,
        ..full_peer(9, 0)
    };
    assert_eq!(
        decode_peer_record(&encode_peer_record(&minimal)),
        Some(minimal)
    );

    // A record written before #388 pass 3 (no trailing key bytes) still
    // decodes, with the keys absent — an upgrade keeps its peer table.
    let legacy_source = PeerRecord {
        public_keys: None,
        ..full_peer(4, sequence(1, 64))
    };
    let legacy_body = &encode_peer_record(&legacy_source)[..PEER_RECORD_LEN_LEGACY];
    assert_eq!(decode_peer_record(legacy_body), Some(legacy_source));

    // Wrong version and wrong length are refused, never misparsed.
    let mut wrong = encode_peer_record(&full_peer(1, 0));
    wrong[0] = 2;
    assert_eq!(decode_peer_record(&wrong), None);
    assert_eq!(decode_peer_record(&wrong[..PEER_RECORD_LEN - 1]), None);
}

#[test]
fn peer_upsert_appends_then_purges_and_survives_interruption() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut store = PnPeerStore::new(region.clone());

        store.save(&full_peer(7, 100)).unwrap();
        flush_peers(&mut store, &region, &mut log);

        // The upsert: append first, then the purge of the old record.
        store.save(&full_peer(7, 200)).unwrap();
        assert!(matches!(store.peek_op(), Some(FlushOp::Append { .. })));

        // Interruption: only the append lands. Both records are now live
        // on the part, and a rebuilt store takes the newer by sequence.
        let append = store.peek_op().cloned().unwrap();
        apply(&mut log, &append).await;
        sync(&region, &mut log);
        let rebuilt = PnPeerStore::new(region.clone());
        let records = rebuilt.load_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].cursor, 200);

        // The rest of the flush retires the superseded record.
        store.op_done(true);
        flush_peers(&mut store, &region, &mut log);
        let mut live_peer_records = 0;
        scan(log.flash_mut().bytes(), |r| {
            if r.live && r.tag == TAG_PEER {
                live_peer_records += 1;
            }
        });
        assert_eq!(live_peer_records, 1);
        assert_eq!(store.load_all().unwrap()[0].cursor, 200);
    });
}

#[test]
fn peer_remove_purges_all_records() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut store = PnPeerStore::new(region.clone());
        store.save(&full_peer(7, 1)).unwrap();
        store.save(&full_peer(9, 2)).unwrap();
        flush_peers(&mut store, &region, &mut log);

        store.remove(&[7; 16]).unwrap();
        assert!(store
            .load_all()
            .unwrap()
            .iter()
            .all(|r| r.destination_hash != [7; 16]));
        flush_peers(&mut store, &region, &mut log);

        let rebuilt = PnPeerStore::new(region.clone());
        let records = rebuilt.load_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].destination_hash, [9; 16]);
    });
}

#[test]
fn load_all_reflects_pending_saves_before_flush() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut store = PnPeerStore::new(region.clone());
        store.save(&full_peer(7, 100)).unwrap();
        flush_peers(&mut store, &region, &mut log);

        // A save not yet flushed already answers load_all with the new
        // value, once — never the flash value beside it.
        store.save(&full_peer(7, 300)).unwrap();
        let records = store.load_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].cursor, 300);
    });
}

#[test]
fn queued_heap_bytes_track_the_flush_queues() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);

        // Message store: an unflushed append pins at least its body.
        let mut store = PnStore::new(region.clone(), SECTORS);
        assert_eq!(store.queued_heap_bytes(), 0);
        let body = msg_body(1, FIELD_BODY);
        store.append(&key(1), 1000, 0, &body).unwrap();
        assert!(
            store.queued_heap_bytes() >= body.len(),
            "queued append body not accounted"
        );
        flush_messages(&mut store, &region, &mut log);
        // Flushed: the bodies are gone; only ring capacity may linger.
        assert!(store.queued_heap_bytes() < body.len());

        // Peer store: an unflushed save pins its pending mirror.
        let mut peers = PnPeerStore::new(region.clone());
        assert_eq!(peers.queued_heap_bytes(), 0);
        peers.save(&full_peer(7, 100)).unwrap();
        assert!(peers.queued_heap_bytes() > 0);
    });
}

/// The board's own region: 16 pages of 4 KiB (leviculum `memory.x`,
/// `STORE : ORIGIN = 0x000DA000, LENGTH = 0x10000`).
const BOARD_SECTORS: u32 = 16;

/// `free_space` answers the trait's question — bytes a message body could
/// still use ACROSS the region — and not the log's `free_bytes`, which is
/// the active page's room once the region has lapped.
///
/// The reproducer is the 2026-09-13 field reading: both T114 and pocket
/// printed `PN_STATS store=0 free_bytes=0 capacity=65344` for hours while
/// their mount line said `live=2 free_bytes=2600` and `STORE stats
/// appends=38 fails=0` showed the log appending happily. The third phase
/// below is that state exactly — a lapped region whose active page is
/// full, holding two live records — and there the two numbers differ by
/// the whole store.
#[test]
fn free_space_measures_the_region_and_not_the_active_page() {
    block_on(async {
        let region = SharedRegion::new(BOARD_SECTORS);
        let mut log = fresh(BOARD_SECTORS).await;
        sync(&region, &mut log);
        let mut store = PnStore::new(region.clone(), BOARD_SECTORS);
        let stride = leviculum_record_log::record_stride(FIELD_BODY) as u64;
        let per_page = SECTOR_PAYLOAD as u64 / stride;

        // Phase 1: two live records in a 16-page region. Close to
        // capacity, and exactly capacity minus what they occupy.
        store
            .append(&key(1), 1, 5, &msg_body(1, FIELD_BODY))
            .unwrap();
        store
            .append(&key(2), 2, 5, &msg_body(2, FIELD_BODY))
            .unwrap();
        // A queued append already costs its bytes: the trait's number is
        // what a NEXT body could use.
        assert_eq!(store.free_space(), store.capacity() - 2 * stride);
        flush_messages(&mut store, &region, &mut log);
        assert_eq!(store.free_space(), store.capacity() - 2 * stride);
        assert!(
            store.free_space() > store.capacity() * 9 / 10,
            "two records do not fill a 64 KiB region: {}",
            store.free_space()
        );

        // Phase 2: fill every page. What is left is the per-page tails no
        // record fits into — page granularity, which the doc comment
        // states this number does not model.
        let total = (BOARD_SECTORS as u64 * per_page) as u32;
        for n in 3..=total {
            store
                .append(&key(n), n as u64, 5, &msg_body(n as u8, FIELD_BODY))
                .unwrap();
        }
        flush_messages(&mut store, &region, &mut log);
        let live = store.count().unwrap() as u64;
        assert_eq!(store.free_space(), store.capacity() - live * stride);
        assert!(
            store.free_space() < SECTOR_PAYLOAD as u64 * 2,
            "a full region has at most the page tails left: {}",
            store.free_space()
        );

        // One more append does NOT fail: the log reclaims its oldest page
        // round-robin, which is the board's eviction (see `append`'s own
        // doc comment). The number rises by the page that went.
        let before = store.free_space();
        store
            .append(
                &key(total + 1),
                total as u64 + 1,
                5,
                &msg_body(0xEE, FIELD_BODY),
            )
            .expect("the round-robin log never reports Full for a body that fits a page");
        flush_messages(&mut store, &region, &mut log);
        assert!(
            store.free_space() > before + (per_page - 2) * stride,
            "the reclaimed page's bytes came back: {before} -> {}",
            store.free_space()
        );

        // Phase 3: the field state. Purge everything but two records on a
        // region that has lapped. The log's own number still reads the
        // active page's room — near nothing, and on the boards it read
        // exactly 0 — while the store is all but empty.
        let keys: Vec<[u8; KEY_LEN]> = directory(&store)
            .iter()
            .map(|meta| meta.transient_id)
            .collect();
        for id in keys.iter().take(keys.len() - 2) {
            assert!(store.purge(id).unwrap());
        }
        flush_messages(&mut store, &region, &mut log);
        assert_eq!(store.count().unwrap(), 2);
        assert!(
            log.free_bytes() < SECTOR_PAYLOAD,
            "phase 3 wants the lapped-and-nearly-full active page the \
             boards had: {}",
            log.free_bytes()
        );
        assert!(
            store.free_space() > store.capacity() * 9 / 10,
            "two live records, so the store is nearly empty whatever the \
             active page says: {} of {}",
            store.free_space(),
            store.capacity()
        );
    });
}

/// The chunked read the outbound peer sync streams with (leviculum#388)
/// yields the body `read_body` yields, at every chunk size, from flash and
/// from the queue alike, and says `None` for an absent id.
#[test]
fn read_body_at_reassembles_what_read_body_returns() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut store = PnStore::new(region.clone(), SECTORS);
        let mut flushed = msg_body(1, 1_000);
        for (i, byte) in flushed.iter_mut().enumerate().skip(DESTINATION_LENGTH) {
            *byte = (i * 13) as u8;
        }
        store.append(&key(1), 1000, 12, &flushed).unwrap();
        flush_messages(&mut store, &region, &mut log);
        let queued = msg_body(2, 300);
        store.append(&key(2), 1001, 12, &queued).unwrap();

        for (id, body) in [(key(1), &flushed), (key(2), &queued)] {
            assert_eq!(store.read_body(&id).unwrap().as_ref(), Some(body));
            for chunk in [1usize, 7, 464, 4096] {
                let mut out = Vec::new();
                let mut buf = vec![0u8; chunk];
                loop {
                    let n = store
                        .read_body_at(&id, out.len(), &mut buf)
                        .unwrap()
                        .unwrap();
                    if n == 0 {
                        break;
                    }
                    out.extend_from_slice(&buf[..n]);
                }
                assert_eq!(&out, body, "chunk={chunk}");
            }
        }
        let mut buf = [0u8; 8];
        assert_eq!(store.read_body_at(&key(9), 0, &mut buf).unwrap(), None);
        assert_eq!(
            store.read_body_at(&key(1), 5_000, &mut buf).unwrap(),
            Some(0)
        );
    });
}

// ---- the flush settling the role (leviculum#413, census row 81) ----

/// A propagation-node role over the board's store adapter, at stamp cost 0
/// so an upload is accepted on its envelope alone.
fn role(region: &SharedRegion) -> PropagationNode<PnStore<SharedRegion>> {
    PropagationNode::new(
        PnStore::new(region.clone(), SECTORS),
        leviculum_lxmf::PropagationNodeConfig {
            stamp_cost: 0,
            ..leviculum_lxmf::PropagationNodeConfig::default()
        },
    )
}

/// One client upload envelope for destination 7, unique per `seed`.
fn upload(seed: u8) -> Vec<u8> {
    let mut lxmf = vec![7u8; DESTINATION_LENGTH];
    lxmf.resize(200, seed);
    leviculum_lxmf::PropagationUpload::single(1_700_000_000.0, lxmf, [0xEE; 32]).encode()
}

/// Upload `bytes` and return whether the role called it a duplicate.
fn upload_is_duplicate(role: &mut PropagationNode<PnStore<SharedRegion>>, bytes: &[u8]) -> bool {
    match role.handle_upload(bytes, 1000, |_, _| Some(0)) {
        leviculum_lxmf::UploadOutcome::Accepted { duplicate, .. } => duplicate,
        other => panic!("expected acceptance, got {other:?}"),
    }
}

/// Hand the role's front op to the log the way the record-store task does,
/// failures included, and settle it. Returns whether the op landed.
fn flush_front(
    role: &mut PropagationNode<PnStore<SharedRegion>>,
    region: &SharedRegion,
    log: &mut RecordLog<SimNor>,
) -> bool {
    let op = role.store().peek_op().cloned().expect("an op to flush");
    let ok = match &op {
        FlushOp::Append {
            key,
            time,
            tag,
            body,
        } => block_on(log.append(key, *time, *tag, body)).is_ok(),
        FlushOp::Purge { .. } | FlushOp::ConfirmedPurge { .. } => {
            unreachable!("only appends in these tests")
        }
    };
    sync(region, log);
    assert_eq!(settle_message_op(role, ok), Some(op));
    ok
}

/// The flash refused the append (the SoftDevice found no gap, past the
/// task's retries): the id is forgotten, so the client's retry is stored
/// rather than proven as a duplicate of a message that never landed.
#[test]
fn an_append_the_flash_refused_forgets_its_id_and_the_retry_is_stored() {
    let region = SharedRegion::new(SECTORS);
    let mut log = block_on(fresh(SECTORS));
    sync(&region, &mut log);
    let mut role = role(&region);
    let bytes = upload(1);

    assert!(!upload_is_duplicate(&mut role, &bytes));
    log.flash_mut().fail_next_op();
    assert!(!flush_front(&mut role, &region, &mut log));
    assert_eq!(directory(role.store()).len(), 0, "nothing reached flash");

    assert!(!upload_is_duplicate(&mut role, &bytes), "the retry is new");
    assert!(flush_front(&mut role, &region, &mut log));
    assert_eq!(directory(role.store()).len(), 1);
}

/// The store was not mounted, so the task refused the op without touching
/// the part: the same `false`, the same forgetting.
#[test]
fn an_append_refused_by_an_unmounted_store_forgets_its_id() {
    let region = SharedRegion::new(SECTORS);
    let mut role = role(&region);
    let bytes = upload(2);

    assert!(!upload_is_duplicate(&mut role, &bytes));
    assert!(matches!(
        settle_message_op(&mut role, false),
        Some(FlushOp::Append { .. })
    ));
    assert_eq!(role.store().pending_ops(), 0);
    assert!(!upload_is_duplicate(&mut role, &bytes), "the retry is new");
}

/// A full region is not a failure: the log reclaims its oldest page and
/// the append lands, so the id stays remembered and the retry is the proven
/// duplicate it should be.
#[test]
fn an_append_into_a_full_region_lands_and_its_retry_is_a_duplicate() {
    let region = SharedRegion::new(SECTORS);
    let mut log = block_on(fresh(SECTORS));
    sync(&region, &mut log);
    let mut role = role(&region);
    let filler = (SECTORS * SECTOR_SIZE) as usize / FIELD_BODY + 8;
    for n in 0..filler as u32 {
        role.store_mut()
            .append(&key(n), n.into(), 0, &msg_body(1, FIELD_BODY))
            .unwrap();
        flush_messages(role.store_mut(), &region, &mut log);
    }
    assert!(
        !role.store().contains(&key(0)).unwrap(),
        "the region cycled its oldest page"
    );

    let bytes = upload(3);
    assert!(!upload_is_duplicate(&mut role, &bytes));
    assert!(flush_front(&mut role, &region, &mut log));
    assert!(
        upload_is_duplicate(&mut role, &bytes),
        "the retry is proven"
    );
    assert_eq!(role.store().pending_ops(), 0, "and not stored twice");
}

// ---- stamp values at cost 0 (audit 2026-10-12 D10) ----

/// A client upload for destination 7 whose stamp is worth at least
/// `at_least`, found against the real 1000-round workblock the way the board
/// measures one. Returns the envelope, its transient id and the value.
fn priced_upload(at_least: u16) -> (Vec<u8>, [u8; 32], u16) {
    let mut lxmf = vec![7u8; DESTINATION_LENGTH];
    lxmf.resize(200, 0x5A);
    let probe = leviculum_lxmf::PropagationUpload::single(1_700_000_000.0, lxmf.clone(), [0; 32]);
    let transient_id = *leviculum_lxmf::PropagationUpload::decode(&probe.encode())
        .unwrap()
        .transient_id();
    let stream = workblock(&transient_id);
    for n in 0u64.. {
        let mut stamp = [0u8; 32];
        stamp[..8].copy_from_slice(&n.to_le_bytes());
        let value = stream.value(&stamp);
        if value >= at_least {
            let bytes =
                leviculum_lxmf::PropagationUpload::single(1_700_000_000.0, lxmf, stamp).encode();
            return (bytes, transient_id, value);
        }
    }
    unreachable!("the counter space holds a stamp of every value")
}

/// The fully expanded workblock of `transient_id`, as the board's grind
/// leaves it after its last slice.
fn workblock(transient_id: &[u8; 32]) -> leviculum_lxmf::WorkblockStream {
    use leviculum_lxmf::constants::WORKBLOCK_EXPAND_ROUNDS_PN;
    let mut stream = leviculum_lxmf::WorkblockStream::new(WORKBLOCK_EXPAND_ROUNDS_PN);
    stream.advance(transient_id, WORKBLOCK_EXPAND_ROUNDS_PN);
    stream
}

/// A stock peer announcing the reference's cost 16 with flexibility 3
/// (`PROPAGATION_COST`, `reference/LXMF/LXMF/LXMRouter.py:54`): it takes
/// only ids worth 13 or more.
fn demanding_peer() -> leviculum_lxmf::peering::PeerTable {
    use leviculum_lxmf::peering::{PeerTable, PeeringConfig};
    use leviculum_lxmf::propagation::PropagationNodeAnnounce;
    let mut table = PeerTable::new(PeeringConfig::default());
    let announce = PropagationNodeAnnounce {
        legacy_support: false,
        timebase: 1,
        enabled: true,
        transfer_limit_kb: 256,
        sync_limit_kb: 10_240,
        stamp_cost: 16,
        stamp_cost_flexibility: 3,
        peering_cost: 18,
        metadata: vec![],
    };
    let _ = table.handle_announce([1; 16], &announce, Some(2), 200, false);
    assert_eq!(table.get(&[1; 16]).expect("peered").min_accepted_cost(), 13);
    table
}

/// What the demanding peer would be offered from `store` now.
fn offered_to_demanding_peer(store: &PnStore<SharedRegion>) -> Vec<[u8; 32]> {
    use leviculum_lxmf::peering::{build_offer, OFFER_BYTES_LIMIT};
    let table = demanding_peer();
    let peer = table.get(&[1; 16]).expect("peered");
    build_offer(peer, &directory(store), OFFER_BYTES_LIMIT)
        .map(|plan| plan.ids)
        .unwrap_or_default()
}

/// D10, the accept half: a board configured at a cost its flexibility
/// cancels (min accepted cost 0) takes a message before it has met any
/// demanding peer. The board's own role, over the board's store, must store
/// the stamp's true value, or a stock peer that peers later is never
/// offered it (`build_offer` steps its cursor past a low-valued id for good,
/// `reference/LXMF/LXMF/LXMPeer.py:340`).
#[test]
fn a_message_accepted_at_cost_0_before_any_demanding_peer_is_offered_to_one() {
    let region = SharedRegion::new(SECTORS);
    let mut log = block_on(fresh(SECTORS));
    sync(&region, &mut log);
    // The flexibility named: the role's default is 0 since order 1294.
    let mut role = PropagationNode::new(
        PnStore::new(region.clone(), SECTORS),
        leviculum_lxmf::PropagationNodeConfig {
            stamp_cost_flexibility: 3,
            ..board_role_config(3, 1, b"board".to_vec(), 4_000)
        },
    );
    assert_eq!(role.min_accepted_cost(), 0, "cost 3 less flexibility 3");

    let (bytes, transient_id, value) = priced_upload(13);
    // The board's grind, as `Engine::validate_step` runs it: the role asks
    // for a value only when it wants one, and then gets the measured one.
    let outcome = role.handle_upload(&bytes, 1000, |id, stamp| Some(workblock(id).value(stamp)));
    let leviculum_lxmf::UploadOutcome::Accepted { stamp_value, .. } = outcome else {
        panic!("expected acceptance, got {outcome:?}");
    };
    flush_messages(role.store_mut(), &region, &mut log);

    assert_eq!(stamp_value as u16, value, "stored at its true value");
    assert_eq!(offered_to_demanding_peer(role.store()), vec![transient_id]);
}

/// D10, the boot half: a store an older board wrote holds value 0 for what
/// it took at cost 0 (and two other records: one truly worth 0, one priced
/// below the peer's minimum).
/// Before the walk the demanding peer is offered nothing; after it the
/// valuable message is offered at its true value, nothing else was
/// re-written, the marker is down, and the next boot owes no walk.
#[test]
fn a_store_at_value_0_is_offered_once_the_boot_walk_priced_it() {
    use leviculum_lxmf::propagation::PropagationUpload;
    let region = SharedRegion::new(SECTORS);
    let mut log = block_on(fresh(SECTORS));
    sync(&region, &mut log);
    let mut store = PnStore::new(region.clone(), SECTORS);

    let (bytes, valuable, value) = priced_upload(13);
    let body = |bytes: &[u8]| {
        let upload = PropagationUpload::decode(bytes).unwrap();
        let mut body = upload.unstamped_lxmf().to_vec();
        body.extend_from_slice(upload.propagation_stamp());
        body
    };
    store.append(&valuable, 1000, 0, &body(&bytes)).unwrap();
    // A stamp truly worth 0: the walk measures it and leaves it in place.
    let mut worthless = None;
    for seed in 0u8.. {
        let upload = upload(seed);
        let decoded = PropagationUpload::decode(&upload).unwrap();
        if workblock(decoded.transient_id()).value(decoded.propagation_stamp()) == 0 {
            worthless = Some((*decoded.transient_id(), body(&upload)));
            break;
        }
    }
    let (worthless, worthless_body) = worthless.expect("half of all stamps are worth 0");
    store.append(&worthless, 1001, 0, &worthless_body).unwrap();
    store
        .append(&key(9), 1002, 5, &msg_body(7, FIELD_BODY))
        .unwrap();
    flush_messages(&mut store, &region, &mut log);

    assert!(
        offered_to_demanding_peer(&store).is_empty(),
        "stored at 0, the message is below the peer's minimum"
    );

    let mut walk = RepriceWalk::at_boot(&store).expect("no marker yet: a walk is owed");
    let mut measured = 0;
    let mut rewritten = 0;
    while let Some(next) = walk.next(&store) {
        measured += 1;
        let value = workblock(&next.transient_id).value(&next.stamp);
        rewritten += walk.priced(&mut store, value) as u32;
        flush_messages(&mut store, &region, &mut log);
    }
    walk.finish(&mut store);
    flush_messages(&mut store, &region, &mut log);

    assert_eq!(
        measured, 2,
        "the two value-0 records, and not the priced one"
    );
    assert_eq!(rewritten, 1, "only the valuable one is re-written");
    let entries = directory(&store);
    assert_eq!(entries.len(), 3, "re-written, not duplicated");
    let stored = entries
        .iter()
        .find(|entry| entry.transient_id == valuable)
        .expect("still stored");
    assert_eq!(stored.stamp_value as u16, value);
    assert_eq!(stored.received_at, 1000, "the receive time travels along");
    assert_eq!(offered_to_demanding_peer(&store), vec![valuable]);
    assert_eq!(
        store.read_body(&valuable).unwrap(),
        Some(body(&bytes)),
        "the body is the same bytes"
    );

    assert!(store.stamp_values_true());
    let rebooted = PnStore::new(region.clone(), SECTORS);
    assert_eq!(RepriceWalk::at_boot(&rebooted), None, "the walk runs once");
}

/// A re-write whose purge never reached flash leaves the message twice;
/// pricing it again keeps one copy at the right value.
#[test]
fn a_rewrite_interrupted_before_its_purge_heals_on_the_next_walk() {
    let region = SharedRegion::new(SECTORS);
    let mut log = block_on(fresh(SECTORS));
    sync(&region, &mut log);
    let mut store = PnStore::new(region.clone(), SECTORS);
    store
        .append(&key(1), 5, 0, &msg_body(7, FIELD_BODY))
        .unwrap();
    flush_messages(&mut store, &region, &mut log);

    assert!(store.set_stamp_value(&key(1), 14));
    // The append lands; the board resets before the purge.
    let append = store.peek_op().cloned().unwrap();
    block_on(apply(&mut log, &append));
    sync(&region, &mut log);
    assert_eq!(directory(&PnStore::new(region.clone(), SECTORS)).len(), 2);

    let mut store = PnStore::new(region.clone(), SECTORS);
    let mut walk = RepriceWalk::at_boot(&store).expect("no marker");
    assert_eq!(
        walk.next(&store).map(|next| next.transient_id),
        Some(key(1))
    );
    assert!(walk.priced(&mut store, 14));
    flush_messages(&mut store, &region, &mut log);
    let entries = directory(&store);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].stamp_value, 14);
}

// ---- the upload the gate admits is one the store holds (audit 2026-10-12 D11) ----

/// One client upload whose stored body (`lxmf_data ‖ stamp`) is `body_len`
/// bytes, for destination 7.
fn upload_with_body(body_len: usize) -> Vec<u8> {
    let mut lxmf = vec![7u8; DESTINATION_LENGTH];
    lxmf.resize(body_len - STAMP_LEN, 0x5A);
    leviculum_lxmf::PropagationUpload::single(1_700_000_000.0, lxmf, [0xEE; 32]).encode()
}

/// A client's single-message resource that the advertisement gate admits
/// must be one the store can hold. The resource protocol proves the
/// transfer before the role ever sees the bytes, and a store refusal after
/// that sends nothing (`UploadOutcome::StoreFailed`), so a message the gate
/// admits and the record log cannot take (`MAX_BODY`, one page) was
/// acknowledged to its sender and lost. The audit's case is 4 500 B: under
/// the 5 000 B sync limit, over the 4 042 B body bound. A conforming upload,
/// up to the announced per-transfer limit, is still taken.
#[test]
fn a_client_upload_the_gate_admits_is_one_the_store_holds() {
    let region = SharedRegion::new(SECTORS);
    let mut log = block_on(fresh(SECTORS));
    sync(&region, &mut log);
    let mut role = PropagationNode::new(
        PnStore::new(region.clone(), SECTORS),
        board_role_config(0, 0, b"board".to_vec(), 4_000),
    );
    // The body whose reference envelope (14 B around it) is exactly the
    // announced per-transfer limit.
    let at_transfer_limit = leviculum_heap_budget::BOARD_TRANSFER_LIMIT_KB as usize * 1000 - 14;
    for (body_len, conforming) in [
        (4_500, false),
        (leviculum_record_log::MAX_BODY + 1, false),
        (leviculum_record_log::MAX_BODY, false),
        (at_transfer_limit, true),
        (FIELD_BODY, true),
    ] {
        let bytes = upload_with_body(body_len);
        let admitted = board_accepts_resource_of(&role, bytes.len() as u64, false);
        let stored = match role.handle_upload(&bytes, 1000, |_, _| Some(0)) {
            leviculum_lxmf::UploadOutcome::Accepted { transient_id, .. } => {
                flush_messages(role.store_mut(), &region, &mut log);
                role.store().contains(&transient_id).unwrap_or(false)
            }
            _ => false,
        };
        assert!(
            !admitted || stored,
            "a {body_len} B body in a {} B resource was admitted and not stored",
            bytes.len()
        );
        if conforming {
            assert_eq!(bytes.len(), body_len + 14, "a reference envelope");
            assert!(
                admitted && stored,
                "a conforming {body_len} B body was refused"
            );
        }
    }
}

// ---------------------------------------------------------------------
// The recalled propagation announce (audit 2026-10-12 D5) and the
// board's autopeer depth (D2)
// ---------------------------------------------------------------------

/// A propagation announce's `app_data` as a neighbour sends it.
fn pn_announce(timebase: u64, stamp_cost: u64, enabled: bool) -> Vec<u8> {
    leviculum_lxmf::PropagationNodeAnnounce {
        legacy_support: false,
        timebase,
        enabled,
        transfer_limit_kb: 256,
        sync_limit_kb: 10_240,
        stamp_cost,
        stamp_cost_flexibility: 3,
        peering_cost: 10,
        metadata: Vec::new(),
    }
    .encode()
    .unwrap()
}

fn live_recall_records(log: &mut RecordLog<SimNor>) -> Vec<[u8; KEY_LEN]> {
    let mut keys = Vec::new();
    scan(log.flash_mut().bytes(), |r| {
        if r.live && r.tag == TAG_RECALL {
            keys.push(*r.key);
        }
    });
    keys
}

/// The minimal reproduction of D5: a neighbour's propagation announce,
/// heard before a reset, is still recallable after it. Before the fix the
/// board's only recall was the RAM announce cache, which a reset empties.
#[test]
fn a_propagation_announce_is_recalled_after_a_reset() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut store = PnPeerStore::new(region.clone());
        let app_data = pn_announce(1_700_000_000, 13, true);

        assert_eq!(
            store.remember_announce(&[5; 16], &app_data, 100),
            Remembered::Written
        );
        // A queued write is already what a recall answers.
        assert_eq!(store.recalled_announce(&[5; 16]), Some(app_data.clone()));
        flush_peers(&mut store, &region, &mut log);

        // The reset: a fresh adapter over the same flash, nothing in RAM.
        let rebooted = PnPeerStore::new(region.clone());
        assert_eq!(rebooted.recalled_announce(&[5; 16]), Some(app_data));
        assert_eq!(rebooted.recalled_announce(&[6; 16]), None);
        // The recall is not a peer: the table restores nothing from it.
        assert!(rebooted.load_all().unwrap().is_empty());
    });
}

/// Flash wear: one write per change of what the announce says, not one
/// per announce. A propagation node's timebase moves on every announce,
/// so it is not part of "a change"; a new stamp cost is.
#[test]
fn a_repeated_announce_writes_nothing_and_a_changed_one_writes_once() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut store = PnPeerStore::new(region.clone());

        store.remember_announce(&[5; 16], &pn_announce(1_000, 13, true), 100);
        flush_peers(&mut store, &region, &mut log);

        for later in 1..5u64 {
            assert_eq!(
                store.remember_announce(
                    &[5; 16],
                    &pn_announce(1_000 + later * 21_600, 13, true),
                    200
                ),
                Remembered::Unchanged
            );
            assert_eq!(store.pending_ops(), 0);
        }

        let raised = pn_announce(90_000, 16, true);
        assert_eq!(
            store.remember_announce(&[5; 16], &raised, 300),
            Remembered::Written
        );
        flush_peers(&mut store, &region, &mut log);
        assert_eq!(
            live_recall_records(&mut log).len(),
            1,
            "the old copy is purged"
        );
        assert_eq!(
            PnPeerStore::new(region.clone()).recalled_announce(&[5; 16]),
            Some(raised)
        );

        // A disabled announce is a change too, and the recall then says so,
        // as the reference's recall would: the peering declines it.
        let disabled = pn_announce(95_000, 16, false);
        assert_eq!(
            store.remember_announce(&[5; 16], &disabled, 400),
            Remembered::Written
        );
        assert_eq!(store.recalled_announce(&[5; 16]), Some(disabled));

        // Not a propagation announce, or too long to keep: refused.
        assert_eq!(
            store.remember_announce(&[7; 16], b"not msgpack", 500),
            Remembered::Refused
        );
    });
}

/// The region keeps at most [`RECALL_SLOTS`] announces; the oldest copy
/// makes room for a new node.
#[test]
fn the_recall_keeps_at_most_its_slots_and_drops_the_oldest() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut store = PnPeerStore::new(region.clone());

        for n in 0..=RECALL_SLOTS as u8 {
            store.remember_announce(&[n; 16], &pn_announce(1_000, 13, true), n as u32);
            flush_peers(&mut store, &region, &mut log);
        }
        assert_eq!(live_recall_records(&mut log).len(), RECALL_SLOTS);
        let rebooted = PnPeerStore::new(region.clone());
        assert_eq!(
            rebooted.recalled_announce(&[0; 16]),
            None,
            "the oldest went"
        );
        assert!(rebooted.recalled_announce(&[1; 16]).is_some());
        assert!(rebooted
            .recalled_announce(&[RECALL_SLOTS as u8; 16])
            .is_some());
    });
}

/// Round-robin reclaim erases the oldest page whole, so an announce written
/// once and never changed would be lost a lap later. A copy that has fallen
/// into the older half of the region is rewritten when its node announces,
/// even unchanged.
#[test]
fn an_unchanged_announce_whose_copy_ages_into_the_older_half_is_rewritten() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut peers = PnPeerStore::new(region.clone());
        let mut messages = PnStore::new(region.clone(), SECTORS);

        peers.remember_announce(&[5; 16], &pn_announce(1_000, 13, true), 100);
        flush_peers(&mut peers, &region, &mut log);
        assert_eq!(
            peers.remember_announce(&[5; 16], &pn_announce(2_000, 13, true), 100),
            Remembered::Unchanged
        );

        // Message traffic moves the log two pages on: the copy's page is
        // now the next to be reclaimed but one.
        let mut n = 0u32;
        while live_page_lag(&mut log) < SECTORS / 2 {
            messages
                .append(&key(1_000 + n), 1, 0, &msg_body(1, FIELD_BODY))
                .unwrap();
            flush_messages(&mut messages, &region, &mut log);
            n += 1;
        }
        assert_eq!(
            peers.remember_announce(&[5; 16], &pn_announce(3_000, 13, true), 200),
            Remembered::Written
        );
        flush_peers(&mut peers, &region, &mut log);
        assert_eq!(live_recall_records(&mut log).len(), 1);
        assert_eq!(
            live_page_lag(&mut log),
            0,
            "the copy sits in the newest page"
        );
    });
}

/// Pages between the newest page and the page of the live recall copy.
fn live_page_lag(log: &mut RecordLog<SimNor>) -> u32 {
    let mut newest = 0u32;
    let mut copy = None;
    scan(log.flash_mut().bytes(), |r| {
        newest = newest.max(r.page_seq);
        if r.live && r.tag == TAG_RECALL {
            copy = Some(r.page_seq);
        }
    });
    newest - copy.unwrap()
}

/// The whole of D5 through the seams, in the engine's order: the board hears
/// a neighbour's propagation announce and keeps it, resets, and the
/// neighbour then syncs to it over a link with no path known. The board
/// judges the depth by the link (683, D6) and recalls the announce from
/// flash, so the peering admits it (`PN_PEER action=add reason=inbound_sync`
/// on the board). The RAM-only recall the board had before declines it as
/// not a node.
#[test]
fn after_a_reset_a_syncing_neighbour_is_peered_from_its_recalled_announce() {
    use leviculum_lxmf::{DeclineReason, PeerChange, PeerTable};
    use leviculum_sync_batch::sender_depth;

    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let neighbour = [0x42; 16];
        // Heard at three hops, inside the board's depth but not peered: the
        // announce came back as a path response, which never peers
        // (`Handlers.py:80`).
        let mut store = PnPeerStore::new(region.clone());
        store.remember_announce(&neighbour, &pn_announce(1_700_000_000, 13, true), 100);
        flush_peers(&mut store, &region, &mut log);

        // The reset.
        let rebooted = PnPeerStore::new(region.clone());
        let mut table = PeerTable::new(board_peering_config(1, 18, Vec::new()));
        assert!(table.restore(rebooted.load_all().unwrap(), 100).is_empty());
        assert!(table.get(&neighbour).is_none());

        // The sync lands over a three-hop link; the path table is empty.
        let hops = sender_depth(None, Some(3));
        assert_eq!(hops, Some(3));

        // Before: the announce cache alone, empty after a reset.
        let mut before = PeerTable::new(board_peering_config(1, 18, Vec::new()));
        assert_eq!(
            before.handle_inbound_sync_recalled(neighbour, None, hops, 1_700_000_500),
            PeerChange::Declined(DeclineReason::NotANode)
        );

        // After: the RAM cache misses, the region answers.
        let recalled = recall_for_inbound_sync(None, &rebooted, &neighbour);
        assert!(recalled.is_some());
        assert_eq!(
            table.handle_inbound_sync_recalled(neighbour, recalled.as_deref(), hops, 1_700_000_500),
            PeerChange::Added
        );

        // A fresher RAM copy wins over the region's.
        let fresher = pn_announce(1_700_000_400, 13, false);
        assert_eq!(
            recall_for_inbound_sync(Some(fresher.clone()), &rebooted, &neighbour),
            Some(fresher)
        );
    });
}

/// D2: the board peers a propagation node six hops out, as lxmd's
/// generated config and lnpnd do, and declines one at seven.
#[test]
fn the_board_peers_a_node_six_hops_out_and_declines_seven() {
    use leviculum_lxmf::{DeclineReason, PeerChange, PeerTable, PropagationNodeAnnounce};

    let announce = PropagationNodeAnnounce::decode(&pn_announce(1_700_000_000, 13, true)).unwrap();
    let mut table = PeerTable::new(board_peering_config(1, 18, Vec::new()));
    assert_eq!(
        table.handle_announce([6; 16], &announce, Some(6), 1_700_000_100, false),
        PeerChange::Added
    );
    assert_eq!(
        table.handle_announce([7; 16], &announce, Some(7), 1_700_000_100, false),
        PeerChange::Declined(DeclineReason::TooDeep)
    );
}

/// Run a list write the way the record-store task does: build the ops over
/// the region, apply them in order, refresh the view.
fn write_list(
    region: &SharedRegion,
    log: &mut RecordLog<SimNor>,
    list: BoardList,
    hashes: &[[u8; 16]],
) -> bool {
    let Some(ops) = board_list_write_ops(region, list, hashes) else {
        return false;
    };
    run_ops(region, log, &ops);
    true
}

fn run_ops(region: &SharedRegion, log: &mut RecordLog<SimNor>, ops: &[FlushOp]) {
    block_on(async {
        for op in ops {
            apply(log, op).await;
            sync(region, log);
        }
    });
}

fn list_identity(seed: u8) -> leviculum_core::identity::Identity {
    let mut private = [0u8; 64];
    for (index, byte) in private.iter_mut().enumerate() {
        *byte = seed.wrapping_add(index as u8);
    }
    leviculum_core::identity::Identity::from_private_key_bytes(&private).unwrap()
}

/// D4 through the seam, in the engine's boot order: a static peer written
/// at flash time is read back after a reset, is in the table before any
/// announce, is owed a path request, survives the 14-day cull, and is never
/// rotated out. Before this the board had no such record and its peering
/// config carried the default empty list.
#[test]
fn a_static_peer_set_at_flash_time_is_in_the_table_at_boot_and_never_culled() {
    use leviculum_lxmf::peering::MAX_UNREACHABLE_SECS;
    use leviculum_lxmf::PeerTable;

    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let home = [0x51; 16];
        assert!(write_list(
            &region,
            &mut log,
            BoardList::StaticPeers,
            &[home]
        ));

        // The reset.
        let rebooted = PnPeerStore::new(region.clone());
        let lists = rebooted.load_board_lists();
        assert_eq!(lists.static_peers, vec![home]);
        assert!(rebooted.load_all().unwrap().is_empty(), "not a peer record");
        assert!(directory(&PnStore::new(region.clone(), SECTORS)).is_empty());

        let mut table = PeerTable::new(board_peering_config(1, 18, lists.static_peers));
        assert!(table.restore(rebooted.load_all().unwrap(), 100).is_empty());
        assert_eq!(table.activate_static_peers(), vec![home]);
        assert!(table.get(&home).is_some_and(|peer| peer.is_static));
        assert_eq!(
            table.static_peers_unheard(),
            vec![home],
            "path request owed"
        );

        let now = 1_700_000_000 + MAX_UNREACHABLE_SECS + 24 * 60 * 60;
        assert!(table.cull(now).is_empty());
        assert!(table.rotate(now, |_| Default::default(), |_| 0).is_empty());
        assert!(table.get(&home).is_some());
    });
}

/// A list record lives in the message log and round-robin reclaim erases
/// pages whole, so a list written once at flash time would be lost a lap
/// later. A copy that has fallen into the older half is rewritten, same
/// content, and the read after a reset still finds it.
#[test]
fn a_board_list_ageing_into_the_older_half_is_rewritten_unchanged() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut messages = PnStore::new(region.clone(), SECTORS);
        let allowed = [[0x0A; 16], [0x0B; 16]];
        assert!(write_list(
            &region,
            &mut log,
            BoardList::AuthAllowed,
            &allowed
        ));
        assert!(
            board_list_refresh_ops(&region).is_empty(),
            "a fresh copy is not rewritten"
        );

        let mut n = 0u32;
        loop {
            let mut newest = 0u32;
            let mut copy = 0u32;
            scan(log.flash_mut().bytes(), |r| {
                newest = newest.max(r.page_seq);
                if r.live && r.tag == TAG_AUTH {
                    copy = r.page_seq;
                }
            });
            if newest - copy >= SECTORS / 2 {
                break;
            }
            messages
                .append(&key(2_000 + n), 1, 0, &msg_body(1, FIELD_BODY))
                .unwrap();
            flush_messages(&mut messages, &region, &mut log);
            n += 1;
        }
        let refresh = board_list_refresh_ops(&region);
        assert_eq!(refresh.len(), 2, "one append, one purge");
        run_ops(&region, &mut log, &refresh);
        assert!(board_list_refresh_ops(&region).is_empty());

        let mut live = 0;
        scan(log.flash_mut().bytes(), |r| {
            live += (r.live && r.tag == TAG_AUTH) as usize
        });
        assert_eq!(live, 1, "the old copy is purged");
        assert_eq!(
            PnPeerStore::new(region.clone())
                .load_board_lists()
                .auth_allowed,
            allowed.to_vec()
        );
    });
}

/// D18 through the seam: with a non-empty auth list an unlisted client's
/// `/get` is refused with the reference's `ERROR_NO_ACCESS`, the answer
/// lnpnd gives (`lnpnd/src/engine.rs`, the `auth_allowed` check before
/// `handle_get`), and a listed one is served its own mailbox. An empty list
/// is open, as the board was before.
#[test]
fn an_unlisted_client_is_refused_its_mailbox_and_a_listed_one_served() {
    use leviculum_lxmf::propagation::PeerError;
    use leviculum_lxmf::propagation_node::get_mailbox;

    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let listed = list_identity(1);
        let stranger = list_identity(2);

        // Empty: open, as today.
        let open = PnPeerStore::new(region.clone()).load_board_lists();
        assert!(open.auth_allowed.is_empty());
        assert_eq!(
            board_get_mailbox(Some(&stranger), &open.auth_allowed),
            get_mailbox(Some(&stranger))
        );

        assert!(write_list(
            &region,
            &mut log,
            BoardList::AuthAllowed,
            &[*listed.hash()]
        ));
        let lists = PnPeerStore::new(region.clone()).load_board_lists();

        assert_eq!(
            board_get_mailbox(Some(&listed), &lists.auth_allowed),
            get_mailbox(Some(&listed))
        );
        assert!(board_get_mailbox(Some(&listed), &lists.auth_allowed).is_ok());
        assert_eq!(
            board_get_mailbox(Some(&stranger), &lists.auth_allowed),
            Err(PeerError::NoAccess)
        );
        assert_eq!(
            board_get_mailbox(None, &lists.auth_allowed),
            Err(PeerError::NoIdentity)
        );

        // The clear: an empty list written over it opens the mailbox again.
        assert!(write_list(&region, &mut log, BoardList::AuthAllowed, &[]));
        let cleared = PnPeerStore::new(region.clone()).load_board_lists();
        assert!(cleared.auth_allowed.is_empty());
        assert!(board_get_mailbox(Some(&stranger), &cleared.auth_allowed).is_ok());
    });
}

/// The control list is its own record, and a list past its cap is refused
/// whole with nothing queued: a list that arrives shorter than it was sent
/// is a permission set nobody authorised.
#[test]
fn each_list_is_its_own_record_and_an_over_long_one_is_refused() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        assert!(write_list(
            &region,
            &mut log,
            BoardList::ControlAllowed,
            &[[0xC1; 16]]
        ));
        assert!(write_list(
            &region,
            &mut log,
            BoardList::StaticPeers,
            &[[0x51; 16], [0x52; 16]]
        ));

        let too_many: Vec<[u8; 16]> = (0..=BOARD_LIST_MAX as u8).map(|n| [n; 16]).collect();
        assert!(board_list_write_ops(&region, BoardList::AuthAllowed, &too_many).is_none());

        // A rewrite replaces, it does not accumulate.
        assert!(write_list(
            &region,
            &mut log,
            BoardList::StaticPeers,
            &[[0x53; 16]]
        ));

        let lists = PnPeerStore::new(region.clone()).load_board_lists();
        assert_eq!(lists.control_allowed, vec![[0xC1; 16]]);
        assert_eq!(lists.static_peers, vec![[0x53; 16]]);
        assert!(lists.auth_allowed.is_empty());
        let mut live_static = 0;
        scan(log.flash_mut().bytes(), |r| {
            live_static += (r.live && r.tag == TAG_STATIC) as usize
        });
        assert_eq!(live_static, 1);
    });
}

/// A list write cut between its append and its purge leaves two live
/// copies: the read takes the newer, and the next refresh purges the older
/// without changing what is read.
#[test]
fn a_list_write_interrupted_before_its_purge_reads_new_and_heals() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        assert!(write_list(
            &region,
            &mut log,
            BoardList::AuthAllowed,
            &[[1; 16]]
        ));
        let ops = board_list_write_ops(&region, BoardList::AuthAllowed, &[[2; 16]]).unwrap();
        run_ops(&region, &mut log, &ops[..1]);
        assert_eq!(
            read_board_list(&region, BoardList::AuthAllowed),
            vec![[2; 16]]
        );

        let refresh = board_list_refresh_ops(&region);
        assert!(!refresh.is_empty());
        run_ops(&region, &mut log, &refresh);
        assert_eq!(
            read_board_list(&region, BoardList::AuthAllowed),
            vec![[2; 16]]
        );
        let mut live = 0;
        scan(log.flash_mut().bytes(), |r| {
            live += (r.live && r.tag == TAG_AUTH) as usize
        });
        assert_eq!(live, 1);
        assert!(board_list_refresh_ops(&region).is_empty());
    });
}

// ---- 867: the oversize refusal prints its line ----

/// A link from a client to a propagation node whose link is in `AcceptApp`,
/// as the board's role decides every advertisement: the client node, the
/// node the role sits on, and the link's id on each side.
mod oversize_upload {
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use core::cell::Cell;

    use leviculum_core::link::LinkId;
    use leviculum_core::resource::{ResourceError, ResourceStrategy};
    use leviculum_core::{
        Action, Clock, Destination, DestinationType, Direction, Identity, InterfaceId,
        MemoryStorage, NodeCore, NodeCoreBuilder, NodeEvent, ProofStrategy, TickOutput,
    };
    use rand_core::OsRng;

    const IFACE: InterfaceId = InterfaceId(0);

    /// A clock that stands still unless a test moves it: the refusal is
    /// decided at once, and a failed transfer waits for its watchdog.
    #[derive(Clone)]
    pub struct Still(&'static Cell<u64>);

    impl Clock for Still {
        fn now_ms(&self) -> u64 {
            self.0.get()
        }
    }

    pub type Node = NodeCore<OsRng, Still, MemoryStorage>;

    fn node(now: &'static Cell<u64>) -> Node {
        NodeCoreBuilder::new().build(OsRng, Still(now), MemoryStorage::with_defaults())
    }

    pub fn frames(out: &TickOutput) -> Vec<Vec<u8>> {
        out.actions
            .iter()
            .map(|a| match a {
                Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
            })
            .collect()
    }

    pub struct Pair {
        pub client: Node,
        pub board: Node,
        pub client_link: LinkId,
        /// The board's clock, for a test that waits out a watchdog.
        pub board_now: &'static Cell<u64>,
    }

    pub fn establish() -> Pair {
        let identity = Identity::generate(&mut OsRng);
        let signing_key = identity.ed25519_verifying().to_bytes();
        let board_now: &'static Cell<u64> = Box::leak(Box::new(Cell::new(1_780_000_000_000)));
        let mut board = node(board_now);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "lxmf",
            &["propagation"],
        )
        .expect("destination");
        dest.set_accepts_links(true);
        dest.set_proof_strategy(ProofStrategy::All);
        dest.set_resource_strategy(ResourceStrategy::AcceptApp);
        let dest_hash = *dest.hash();
        board.register_destination(dest);
        let mut client = node(Box::leak(Box::new(Cell::new(1_780_000_000_000))));
        let (client_link, _routed, out) = client.connect(dest_hash, &signing_key).expect("connect");
        let mut to_board = frames(&out);
        let mut up = false;
        for _ in 0..8 {
            let mut to_client = Vec::new();
            for frame in to_board {
                let out = board.handle_packet(IFACE, &frame);
                up |= out
                    .events
                    .iter()
                    .any(|e| matches!(e, NodeEvent::LinkEstablished { .. }));
                to_client.extend(frames(&out));
            }
            to_board = Vec::new();
            for frame in to_client {
                to_board.extend(frames(&client.handle_packet(IFACE, &frame)));
            }
        }
        assert!(up, "the board's side of the link is established");
        Pair {
            client,
            board,
            client_link,
            board_now,
        }
    }

    /// The client's verdict on one frame from the board: the resource it
    /// names as refused, if the frame is a refusal.
    pub fn refused_by_board(pair: &mut Pair, frame: &[u8]) -> Option<[u8; 32]> {
        pair.client
            .handle_packet(IFACE, frame)
            .events
            .iter()
            .find_map(|e| match e {
                NodeEvent::ResourceFailed {
                    resource_hash,
                    error: ResourceError::RejectedByRemote,
                    is_sender: true,
                    ..
                } => Some(*resource_hash),
                _ => None,
            })
    }
}

/// 867, the 866 shape: a 122 953 B upload advertised to a board's
/// propagation node is refused at its advertisement with one RCL naming it,
/// as the reference lxmd refuses it (`reference/LXMF/LXMF/LXMRouter.py:2220-2224`),
/// and the board now says so: `PN_REJECT reason=too_large via=resource
/// bytes=122953 limit=<the store bound>`, lnpnd's words and keys since 1355.
/// The RCL is the positive control: the refusal on the wire is what it was
/// before the line existed.
#[test]
fn an_oversize_upload_is_refused_with_one_rcl_and_says_so() {
    let region = SharedRegion::new(SECTORS);
    let mut log = block_on(fresh(SECTORS));
    sync(&region, &mut log);
    let role = PropagationNode::new(
        PnStore::new(region.clone(), SECTORS),
        board_role_config(0, 0, b"board".to_vec(), 4_000),
    );
    let mut pair = oversize_upload::establish();

    let upload = vec![0x5Au8; 122_953];
    let (hash, out) = pair
        .client
        .send_resource(&pair.client_link, &upload, None, false)
        .expect("the client advertises its upload");
    let adv = oversize_upload::frames(&out);
    assert_eq!(adv.len(), 1, "one advertisement");
    let out = pair
        .board
        .handle_packet(leviculum_core::InterfaceId(0), &adv[0]);
    let (board_link, data_size) = out
        .events
        .iter()
        .find_map(|e| match e {
            leviculum_core::NodeEvent::ResourceAdvertised {
                link_id, data_size, ..
            } => Some((*link_id, *data_size)),
            _ => None,
        })
        .expect("the advertisement is parked for the role");
    assert_eq!(data_size, 122_953);

    // The role's verdict, as `pn.rs` takes it: a client upload (no peering
    // key validated on the link), so the store bound is the cap.
    let refusal =
        board_resource_refusal(&role, data_size, false, false).expect("refused for its size");
    assert!(!board_accepts_resource_of(&role, data_size, false));
    assert_eq!(
        std::format!("PN_REJECT {refusal}"),
        std::format!(
            "PN_REJECT reason=too_large via=resource bytes=122953 limit={MAX_UPLOAD_RESOURCE_BYTES}"
        )
    );

    let rcl = oversize_upload::frames(&pair.board.reject_resource(&board_link).expect("rejected"));
    assert_eq!(rcl.len(), 1, "exactly one frame answers the advertisement");
    assert_eq!(
        oversize_upload::refused_by_board(&mut pair, &rcl[0]),
        Some(hash),
        "the one frame is an RCL naming the advertised resource"
    );
}

/// The limit a refusal names is the one the resource failed, under lnpnd's
/// reason word: a validated peer's batch is held to the announced sync limit
/// alone (`over_sync_limit`), a client's upload to the store bound
/// (`too_large`, the board's delivery limit), and a resource within both is
/// not refused. A 300 KB upload, which lnpnd stores local only, is
/// `too_large` here (order 1355).
#[test]
fn the_size_refusal_names_lnpnds_reason_and_the_limit_it_failed() {
    let region = SharedRegion::new(SECTORS);
    let role = PropagationNode::new(
        PnStore::new(region, SECTORS),
        board_role_config(0, 0, b"board".to_vec(), 4_000),
    );
    let sync_limit = leviculum_heap_budget::BOARD_SYNC_LIMIT_KB * 1000;
    assert_eq!(role.resource_limit_bytes(), sync_limit);
    assert!(MAX_UPLOAD_RESOURCE_BYTES < sync_limit);
    assert_eq!(
        board_resource_refusal(&role, sync_limit + 1, true, false),
        Some(ResourceRefusal {
            reason: "over_sync_limit",
            bytes: sync_limit + 1,
            limit: sync_limit
        })
    );
    assert_eq!(board_resource_refusal(&role, sync_limit, true, false), None);
    assert_eq!(
        board_resource_refusal(&role, MAX_UPLOAD_RESOURCE_BYTES + 1, false, false),
        Some(ResourceRefusal {
            reason: "too_large",
            bytes: MAX_UPLOAD_RESOURCE_BYTES + 1,
            limit: MAX_UPLOAD_RESOURCE_BYTES
        })
    );
    assert_eq!(
        board_resource_refusal(&role, MAX_UPLOAD_RESOURCE_BYTES, false, false),
        None
    );
    let refusal = board_resource_refusal(&role, 300_000, false, false).expect("refused");
    assert_eq!(
        std::format!("PN_REJECT {refusal}"),
        std::format!(
            "PN_REJECT reason=too_large via=resource bytes=300000 limit={MAX_UPLOAD_RESOURCE_BYTES}"
        )
    );
}

/// After a sync message the store did not keep, a peer's batch is refused
/// `store_full` until the sweep, as on lnpnd; a client's upload is not held
/// to it, and a batch over the sync limit still names that limit first.
#[test]
fn a_store_full_board_refuses_peer_batches_and_not_client_uploads() {
    let region = SharedRegion::new(SECTORS);
    let role = PropagationNode::new(
        PnStore::new(region, SECTORS),
        board_role_config(0, 0, b"board".to_vec(), 4_000),
    );
    let sync_limit = role.resource_limit_bytes();
    let refusal = board_resource_refusal(&role, 1_000, true, true).expect("refused");
    assert_eq!(
        std::format!("PN_REJECT {refusal}"),
        "PN_REJECT reason=store_full via=resource bytes=1000 limit=0"
    );
    assert_eq!(board_resource_refusal(&role, 1_000, false, true), None);
    assert_eq!(board_resource_refusal(&role, 1_000, true, false), None);
    assert_eq!(
        board_resource_refusal(&role, sync_limit + 1, true, true).map(|r| r.reason),
        Some("over_sync_limit")
    );
}

// ---- 878: a verdict the node refuses prints its line and retires ----

/// 878: an accept the node refuses leaves `inbound_transfers` as it was and
/// prints its `PN_REJECT` line. The board's arm pushes the link before
/// `accept_resource` runs, and only a completion or the link's close
/// removed it again, so a refused accept held the `/offer` gate's
/// in-flight count up until the link closed.
///
/// The late `incoming_full` cannot be produced on the wire (a link with a
/// parked advertisement ignores every other one,
/// `leviculum-core/src/node/mvr_incoming_resource_list.rs:582-585`), so the
/// refusal here is the other one the public verdict carries: a second
/// accept on a link whose advertisement the first one already took. The
/// arm is the same `Err`; the first transfer's entry is the one that must
/// survive the retire.
#[test]
fn a_refused_accept_leaves_inbound_transfers_as_it_was() {
    use leviculum_core::resource::ResourceError;

    let mut pair = oversize_upload::establish();
    let (_hash, out) = pair
        .client
        .send_resource(&pair.client_link, &[0x5Au8; 300], None, false)
        .expect("the client advertises");
    let adv = oversize_upload::frames(&out);
    let board_link = pair
        .board
        .handle_packet(leviculum_core::InterfaceId(0), &adv[0])
        .events
        .iter()
        .find_map(|e| match e {
            leviculum_core::NodeEvent::ResourceAdvertised { link_id, .. } => Some(*link_id),
            _ => None,
        })
        .expect("the advertisement is parked for the role");

    // The board's arm, as `pn.rs` runs it for a validated peer's accept.
    let mut inbound = vec![board_link];
    assert!(pair.board.accept_resource(&board_link).is_ok());
    let before = inbound.clone();

    inbound.push(board_link);
    let refused = pair.board.accept_resource(&board_link);
    assert_eq!(
        refused.as_ref().err(),
        Some(&ResourceError::NoPendingResource)
    );
    // Positive control: without the retire the entry stays.
    assert_eq!(inbound.len(), before.len() + 1);
    retire_refused_transfer(&mut inbound, &board_link);
    assert_eq!(inbound, before, "the earlier transfer keeps its entry");

    let mut link = [0u8; 8];
    link.copy_from_slice(&board_link.as_bytes()[..8]);
    let line = std::format!(
        "PN_REJECT {}",
        VerdictRefusal {
            error: ResourceError::TransferInProgress,
            link,
            accept: true,
        }
    );
    let hex: std::string::String = link.iter().map(|b| std::format!("{b:02x}")).collect();
    assert_eq!(
        line,
        std::format!("PN_REJECT reason=transfer-in-progress link={hex} accept=1")
    );
}

/// The retire takes only the entry this accept pushed: another link's
/// entry and an absent link are left alone.
#[test]
fn the_retire_takes_only_the_last_entry_for_its_link() {
    let mut inbound = vec![1u8, 2, 1];
    retire_refused_transfer(&mut inbound, &1);
    assert_eq!(inbound, vec![1, 2]);
    retire_refused_transfer(&mut inbound, &3);
    assert_eq!(inbound, vec![1, 2]);
}

/// 879: a peer's transfer the board accepted and then lost with the link
/// still up (the sender went silent, the receiver's part watchdog gave up)
/// retires its `inbound_transfers` entry and says so in one
/// `PN_SYNC_FAILED` line. An earlier transfer on the same link keeps its
/// entry. Before 879 only completion and close retired one, so the
/// `/offer` gate counted a sync that was not running.
#[test]
fn a_failed_inbound_transfer_retires_its_entry_and_says_so() {
    use leviculum_core::resource::ResourceError;

    let mut pair = oversize_upload::establish();
    let (_hash, out) = pair
        .client
        .send_resource(&pair.client_link, &[0x5Au8; 300], None, false)
        .expect("the client advertises");
    let adv = oversize_upload::frames(&out);
    let board_link = pair
        .board
        .handle_packet(leviculum_core::InterfaceId(0), &adv[0])
        .events
        .iter()
        .find_map(|e| match e {
            leviculum_core::NodeEvent::ResourceAdvertised { link_id, .. } => Some(*link_id),
            _ => None,
        })
        .expect("the advertisement is parked for the role");

    // An earlier transfer's entry, then the one this accept pushes, as
    // `pn.rs` pushes it for a validated peer.
    let mut inbound = vec![board_link];
    let earlier = inbound.clone();
    let part_request = pair
        .board
        .accept_resource(&board_link)
        .expect("the board accepts");
    inbound.push(board_link);

    // The part request never reaches the client: the receiver's watchdog
    // is the only thing left to end the transfer.
    drop(part_request);
    let mut failed = None;
    for _ in 0..200 {
        pair.board_now.set(pair.board_now.get() + 5_000);
        let out = pair.board.handle_timeout();
        failed = out.events.iter().find_map(|e| match e {
            leviculum_core::NodeEvent::ResourceFailed {
                link_id,
                error,
                is_sender: false,
                ..
            } => Some((*link_id, *error)),
            _ => None,
        });
        if failed.is_some() {
            break;
        }
    }
    let (failed_link, error) = failed.expect("the receiver's watchdog fails the transfer");
    assert_eq!(failed_link, board_link);
    assert_eq!(error, ResourceError::Timeout);
    assert!(
        pair.board.link(&board_link).is_some(),
        "the link outlives its transfer"
    );

    // Positive control: without the retire the failed transfer's entry stays.
    assert_eq!(inbound.len(), earlier.len() + 1);
    assert!(retire_failed_transfer(&mut inbound, &failed_link));
    assert_eq!(inbound, earlier, "the earlier transfer keeps its entry");

    let mut link = [0u8; 8];
    link.copy_from_slice(&failed_link.as_bytes()[..8]);
    let line = std::format!("PN_SYNC_FAILED {}", SyncFailure { link, error });
    let hex: std::string::String = link.iter().map(|b| std::format!("{b:02x}")).collect();
    assert_eq!(
        line,
        std::format!("PN_SYNC_FAILED link={hex} reason=timeout")
    );
    std::println!("{line}");
}

/// A link that holds no entry (a client's upload, a link already
/// forgotten) retires nothing, so the board prints nothing for it.
#[test]
fn a_failure_on_a_link_without_an_entry_retires_nothing() {
    let mut inbound = vec![1u8, 2, 1];
    assert!(!retire_failed_transfer(&mut inbound, &3));
    assert_eq!(inbound, vec![1, 2, 1]);
    assert!(retire_failed_transfer(&mut inbound, &1));
    assert_eq!(inbound, vec![1, 2]);
}

#[derive(Clone)]
struct CountingRegion {
    inner: SharedRegion,
    reads: Rc<Cell<usize>>,
}

impl Region for CountingRegion {
    fn with_bytes<T>(&self, f: impl FnOnce(&[u8]) -> T) -> T {
        self.reads.set(self.reads.get() + 1);
        self.inner.with_bytes(f)
    }
}

// ---------------------------------------------------------------------
// Order 1355: the board keeps lnpnd's rules (1294, 1295)
// ---------------------------------------------------------------------

/// The board's role at the decided defaults, as `pn.rs` builds it from a
/// board with no configuration record.
fn default_board_role(region: &SharedRegion) -> PropagationNode<PnStore<SharedRegion>> {
    use leviculum_core::pn_config_store::StoredPnConfig;
    PropagationNode::new(
        PnStore::new(region.clone(), SECTORS),
        board_role_config(
            StoredPnConfig::DEFAULT.stamp_cost,
            StoredPnConfig::DEFAULT.peering_cost,
            b"board".to_vec(),
            4_000,
        ),
    )
}

/// lnpnd's `the_default_floor_refuses_a_stamp_of_12_and_takes_13`, on the
/// board's role over the board's store: an upload whose stamp is worth 12 is
/// refused with the reference's `ERROR_INVALID_STAMP` and nothing reaches the
/// queue, one worth 13 is stored. The validation is the board's own, the
/// streamed workblock judged at the role's `min_accepted_cost`
/// (`Engine::validate_step`). And the announce says so: field 5 is
/// `[13, 0, 1]`, beside the board's own limits in fields 3 and 4.
#[test]
fn the_board_default_floor_refuses_a_stamp_of_12_and_takes_13() {
    use leviculum_lxmf::propagation::PropagationSignal;
    let region = SharedRegion::new(SECTORS);
    let mut log = block_on(fresh(SECTORS));
    sync(&region, &mut log);
    let mut role = default_board_role(&region);
    let min_cost = role.min_accepted_cost();
    assert_eq!(min_cost, leviculum_lxmf::STOCK_PEER_MIN_STAMP_VALUE);

    let mut lxmf = vec![7u8; DESTINATION_LENGTH];
    lxmf.resize(200, 0x33);
    let probe = leviculum_lxmf::PropagationUpload::single(1_700_000_000.0, lxmf.clone(), [0; 32]);
    let transient_id = *leviculum_lxmf::PropagationUpload::decode(&probe.encode())
        .unwrap()
        .transient_id();
    let stream = workblock(&transient_id);
    let (mut twelve, mut thirteen) = (None, None);
    for n in 0u64..1 << 20 {
        let mut stamp = [0u8; 32];
        stamp[..8].copy_from_slice(&n.to_le_bytes());
        match stream.value(&stamp) {
            12 if twelve.is_none() => twelve = Some(stamp),
            value if value >= 13 && thirteen.is_none() => thirteen = Some(stamp),
            _ => {}
        }
        if twelve.is_some() && thirteen.is_some() {
            break;
        }
    }
    let (twelve, thirteen) = (twelve.expect("a 12"), thirteen.expect("a 13"));

    let low = leviculum_lxmf::PropagationUpload::single(1_700_000_000.0, lxmf.clone(), twelve);
    match role.handle_upload(&low.encode(), 1000, |_, stamp| {
        stream.validated(stamp, min_cost)
    }) {
        leviculum_lxmf::UploadOutcome::InvalidStamp { reject } => assert_eq!(
            PropagationSignal::decode(&reject).expect("a signal"),
            PropagationSignal::InvalidStamp
        ),
        other => panic!("a stamp worth 12 must be refused: {other:?}"),
    }
    assert_eq!(role.store().pending_ops(), 0, "nothing queued for flash");

    let good = leviculum_lxmf::PropagationUpload::single(1_700_000_000.0, lxmf, thirteen);
    match role.handle_upload(&good.encode(), 1000, |_, stamp| {
        stream.validated(stamp, min_cost)
    }) {
        leviculum_lxmf::UploadOutcome::Accepted {
            stamp_value,
            duplicate,
            ..
        } => {
            assert!(stamp_value >= 13);
            assert!(!duplicate);
        }
        other => panic!("a stamp worth 13 must be taken: {other:?}"),
    }
    flush_messages(role.store_mut(), &region, &mut log);
    assert!(role.store().contains(&transient_id).unwrap());

    let announce = leviculum_lxmf::PropagationNodeAnnounce::decode(&role.announce_app_data(1000))
        .expect("the board's announce decodes");
    assert_eq!(
        (
            announce.stamp_cost,
            announce.stamp_cost_flexibility,
            announce.peering_cost
        ),
        (13, 0, 1),
        "field 5"
    );
    assert_eq!(
        announce.transfer_limit_kb,
        leviculum_heap_budget::BOARD_TRANSFER_LIMIT_KB
    );
    assert_eq!(
        announce.sync_limit_kb,
        leviculum_heap_budget::BOARD_SYNC_LIMIT_KB
    );
}

/// 1295's rule on the board: a client upload above the board's announced
/// transfer limit that the store can still hold (a body of 3 985 to 4 042 B)
/// is stored and proven, local only, and the offer the board builds skips it
/// for good even toward a stock peer that takes 256 KB, advancing the cursor
/// past it; the plain builder, the positive control, would have offered it.
#[test]
fn an_upload_above_the_transfer_limit_is_stored_local_only_and_never_offered() {
    let region = SharedRegion::new(SECTORS);
    let mut log = block_on(fresh(SECTORS));
    sync(&region, &mut log);
    let mut role = PropagationNode::new(
        PnStore::new(region.clone(), SECTORS),
        board_role_config(0, 0, b"board".to_vec(), 4_000),
    );
    let local_body = 4_000;
    assert!(local_body <= leviculum_record_log::MAX_BODY);
    let mut ids = Vec::new();
    for body_len in [local_body, 3_000] {
        let bytes = upload_with_body(body_len);
        assert!(board_accepts_resource_of(&role, bytes.len() as u64, false));
        match role.handle_upload(&bytes, 1000, |_, _| Some(13)) {
            leviculum_lxmf::UploadOutcome::Accepted {
                transient_id, size, ..
            } => {
                assert_eq!(size as usize, body_len);
                ids.push(transient_id);
            }
            other => panic!("a {body_len} B body must be stored: {other:?}"),
        }
        flush_messages(role.store_mut(), &region, &mut log);
    }
    assert!(role.is_local_only(local_body as u32));
    assert!(!role.is_local_only(3_000));
    let stored = ordered_directory(role.store());
    assert_eq!(stored.len(), 2, "both stored");

    let table = demanding_peer();
    let peer = table.get(&[1; 16]).expect("peered");
    let mut builder = board_offer_builder(&role, peer, OFFER_BYTES_LIMIT);
    role.store()
        .for_each_ordered(&mut |meta| builder.push(meta))
        .unwrap();
    let plan = builder.finish().expect("a plan");
    assert_eq!(plan.ids, vec![ids[1]], "only the shared message is offered");
    assert_eq!(
        plan.skipped_oversize, 1,
        "the local one is skipped for good"
    );
    assert_eq!(plan.cursor_target, stored[1].sequence);

    let mut control = OfferBuilder::new(peer, OFFER_BYTES_LIMIT);
    role.store()
        .for_each_ordered(&mut |meta| control.push(meta))
        .unwrap();
    assert_eq!(control.finish().expect("a plan").ids, ids);
}

/// The duplicate verdict at `now` for one upload envelope.
fn duplicate_at(role: &mut PropagationNode<PnStore<SharedRegion>>, bytes: &[u8], now: u64) -> bool {
    match role.handle_upload(bytes, now, |_, _| Some(0)) {
        leviculum_lxmf::UploadOutcome::Accepted { duplicate, .. } => duplicate,
        other => panic!("expected acceptance, got {other:?}"),
    }
}

/// 1294 item 4 on the board: a message its recipient collected is still a
/// duplicate after a restart, from the purged record the collection left on
/// flash, without any record format of its own. It holds until the ring
/// reclaims the record's page; past that the copy is new again. A message
/// the expiry sweep took is no departure, and a message never seen is new
/// (both positive controls: the verdict is not "everything is a duplicate").
#[test]
fn a_collected_message_stays_a_duplicate_across_a_restart_until_its_page_is_reclaimed() {
    const EXPIRY: u64 = leviculum_lxmf::propagation_node::MESSAGE_EXPIRY_SECS;
    let region = SharedRegion::new(SECTORS);
    let mut log = block_on(fresh(SECTORS));
    sync(&region, &mut log);
    let mut first = role(&region);
    let collected = upload(21);
    let expired = upload(22);
    assert!(!duplicate_at(&mut first, &collected, 1000));
    assert!(!duplicate_at(&mut first, &expired, 1000));
    flush_messages(first.store_mut(), &region, &mut log);
    let collected_id = *leviculum_lxmf::PropagationUpload::decode(&collected)
        .unwrap()
        .transient_id();
    assert_eq!(
        first.store_mut().purge_confirmed(&collected_id),
        Ok(ConfirmedPurge::Queued)
    );
    flush_messages(first.store_mut(), &region, &mut log);

    // The restart: a fresh role and store over the same flash.
    let mut rebooted = role(&region);
    assert!(
        duplicate_at(&mut rebooted, &collected, 2000),
        "a collected message is not stored again after a restart"
    );
    assert!(!duplicate_at(&mut rebooted, &upload(23), 2000));
    assert_eq!(rebooted.store().pending_ops(), 1, "the new one is queued");
    flush_messages(rebooted.store_mut(), &region, &mut log);

    // The sweep takes the expired one; after a restart it is a new copy.
    let swept = rebooted.tick(1000 + EXPIRY + 1);
    assert_eq!(swept.len(), 1, "the one 1000 s message still live");
    flush_messages(rebooted.store_mut(), &region, &mut log);
    let mut again = role(&region);
    assert!(!duplicate_at(&mut again, &expired, 1000 + EXPIRY + 2));
    let _ = again.store_mut().purge(
        leviculum_lxmf::PropagationUpload::decode(&expired)
            .unwrap()
            .transient_id(),
    );

    // One lap of the ring later the purged record is gone, and so is the
    // memory of the departure.
    let filler = (SECTORS * SECTOR_SIZE) as usize / FIELD_BODY + 8;
    for n in 0..filler as u32 {
        again
            .store_mut()
            .append(&key(1000 + n), 3000, 0, &msg_body(1, FIELD_BODY))
            .unwrap();
        flush_messages(again.store_mut(), &region, &mut log);
    }
    let mut lapped = role(&region);
    assert!(!duplicate_at(&mut lapped, &collected, 3000));
}

fn flush_counted(
    store: &mut PnStore<CountingRegion>,
    region: &SharedRegion,
    log: &mut RecordLog<SimNor>,
) {
    block_on(async {
        while let Some(op) = store.peek_op().cloned() {
            apply(log, &op).await;
            store.op_done(true);
            sync(region, log);
        }
    });
}

/// 1295's sweep on the board: the expiry tick reads the region only when
/// something in it can have expired. The first tick measures the oldest
/// receive time; a tick with nothing expired then reads nothing; a tick
/// that expires walks once, the one after it once more, and then it rests.
/// An append older than the measured time (a re-priced record, a clock set
/// back) lowers it, so its expiry is not missed.
#[test]
fn the_expiry_sweep_reads_the_region_only_when_something_expired() {
    const EXPIRY: u64 = leviculum_lxmf::propagation_node::MESSAGE_EXPIRY_SECS;
    let region = SharedRegion::new(SECTORS);
    let mut log = block_on(fresh(SECTORS));
    sync(&region, &mut log);
    let reads = Rc::new(Cell::new(0));
    let counting = CountingRegion {
        inner: region.clone(),
        reads: reads.clone(),
    };
    let mut role = PropagationNode::new(
        PnStore::new(counting, SECTORS),
        board_role_config(0, 0, b"board".to_vec(), 4_000),
    );
    for (n, at) in [(1u32, 1000u32), (2, 2000), (3, 3000)] {
        role.store_mut()
            .append(&key(n), at.into(), 0, &msg_body(1, FIELD_BODY))
            .unwrap();
    }
    flush_counted(role.store_mut(), &region, &mut log);
    let tick = |role: &mut PropagationNode<PnStore<CountingRegion>>,
                log: &mut RecordLog<SimNor>,
                now: u64| {
        let before = reads.get();
        let evicted = role.tick(now).len();
        flush_counted(role.store_mut(), &region, log);
        (evicted, reads.get() - before)
    };

    assert_eq!(
        tick(&mut role, &mut log, 999 + EXPIRY),
        (0, 1),
        "the first tick measures"
    );
    assert_eq!(
        tick(&mut role, &mut log, 999 + EXPIRY),
        (0, 0),
        "nothing expired: no walk"
    );
    assert_eq!(
        tick(&mut role, &mut log, 1000 + EXPIRY),
        (0, 0),
        "not yet past the window"
    );
    assert_eq!(
        tick(&mut role, &mut log, 1001 + EXPIRY),
        (1, 2),
        "one walk and the purge's own scan"
    );
    assert_eq!(
        tick(&mut role, &mut log, 1001 + EXPIRY),
        (0, 1),
        "measured once more"
    );
    assert_eq!(
        tick(&mut role, &mut log, 1001 + EXPIRY),
        (0, 0),
        "and at rest"
    );

    role.store_mut()
        .append(&key(4), 1500, 0, &msg_body(1, FIELD_BODY))
        .unwrap();
    flush_counted(role.store_mut(), &region, &mut log);
    assert_eq!(
        tick(&mut role, &mut log, 1600 + EXPIRY).0,
        1,
        "an append older than the measured time is not missed"
    );
}

/// The expiry tick's cost on a full 16-page region of field-sized records,
/// measured on the host: the walk every tick paid before 1355 against the
/// tick at rest. Prints `EXPIRY_TICK` lines for the order's report; the
/// assertion is the read count, the timing is evidence only.
#[test]
fn expiry_tick_cost_on_a_full_board_region() {
    const PAGES: u32 = 16;
    const EXPIRY: u64 = leviculum_lxmf::propagation_node::MESSAGE_EXPIRY_SECS;
    let region = SharedRegion::new(PAGES);
    let mut log = block_on(fresh(PAGES));
    sync(&region, &mut log);
    let reads = Rc::new(Cell::new(0));
    let mut store = PnStore::new(
        CountingRegion {
            inner: region.clone(),
            reads: reads.clone(),
        },
        PAGES,
    );
    let records = (PAGES * SECTOR_SIZE) as usize / (FIELD_BODY + 48);
    for n in 0..records as u32 {
        store
            .append(&key(n), 1000 + u64::from(n), 0, &msg_body(1, FIELD_BODY))
            .unwrap();
        flush_counted(&mut store, &region, &mut log);
    }
    let now = 999 + EXPIRY;
    let mut visits = 0;
    let started = std::time::Instant::now();
    PropagationStore::for_each(&store, &mut |_| visits += 1).unwrap();
    let walk = started.elapsed();
    let mut role = PropagationNode::new(store, board_role_config(0, 0, b"b".to_vec(), 4_000));
    let _ = role.tick(now);
    let before = reads.get();
    let started = std::time::Instant::now();
    let evicted = role.tick(now);
    let rest = started.elapsed();
    assert!(evicted.is_empty());
    assert_eq!(reads.get(), before, "a tick at rest reads nothing");
    std::eprintln!(
        "EXPIRY_TICK records={visits} walk_us={} rest_us={}",
        walk.as_micros(),
        rest.as_micros()
    );
}

/// Fill the log with message records until `peer`'s newest copy has fallen
/// `lag` pages behind the newest page, or is gone; `refresh` runs the
/// engine's half-lap refresh after every message. Returns whether the
/// peer's record is still live.
fn age_peer_by_messages(
    store: &mut PnPeerStore<SharedRegion>,
    messages: &mut PnStore<SharedRegion>,
    region: &SharedRegion,
    log: &mut RecordLog<SimNor>,
    peer: [u8; 16],
    laps: u32,
    refresh: bool,
) -> bool {
    let start = {
        let mut newest = 0u32;
        scan(log.flash_mut().bytes(), |r| newest = newest.max(r.page_seq));
        newest
    };
    let mut n = 0u32;
    loop {
        let mut newest = 0u32;
        scan(log.flash_mut().bytes(), |r| newest = newest.max(r.page_seq));
        if newest - start >= laps * SECTORS {
            break;
        }
        messages
            .append(&key(5_000 + n), 1, 0, &msg_body(1, FIELD_BODY))
            .unwrap();
        flush_messages(messages, region, log);
        if refresh {
            store.refresh_ageing_peers();
            flush_peers(store, region, log);
        }
        n += 1;
    }
    let mut live = false;
    scan(log.flash_mut().bytes(), |r| {
        live |= r.live && r.tag == TAG_PEER && r.key == &peer_key(&peer)
    });
    live
}

/// Order 1651, item 3: a stored peer that neither announces nor syncs for a
/// lap of the region is erased with its page by round-robin reclaim, and
/// the next boot restores the table without it and without a drop event
/// (the T114's 15 -> 14 -> 13 across its panic reboots). With the
/// half-lap refresh the engine now runs, the same peer survives two laps,
/// unchanged, as one live copy.
#[test]
fn a_quiet_peer_record_survives_reclaim_only_through_the_half_lap_refresh() {
    block_on(async {
        // Without the refresh: the record is reclaimed.
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut messages = PnStore::new(region.clone(), SECTORS);
        let mut store = PnPeerStore::new(region.clone());
        let quiet = full_peer(0x51, 7);
        store.save(&quiet).unwrap();
        flush_peers(&mut store, &region, &mut log);
        assert!(!age_peer_by_messages(
            &mut store,
            &mut messages,
            &region,
            &mut log,
            quiet.destination_hash,
            1,
            false,
        ));
        assert!(
            PnPeerStore::new(region.clone())
                .load_all()
                .unwrap()
                .is_empty(),
            "the reboot restores a table without the quiet peer"
        );

        // With it: the record outlives two laps.
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        sync(&region, &mut log);
        let mut messages = PnStore::new(region.clone(), SECTORS);
        let mut store = PnPeerStore::new(region.clone());
        store.save(&quiet).unwrap();
        flush_peers(&mut store, &region, &mut log);
        assert!(
            store.refresh_ageing_peers().is_empty(),
            "a fresh copy is not rewritten"
        );
        assert!(age_peer_by_messages(
            &mut store,
            &mut messages,
            &region,
            &mut log,
            quiet.destination_hash,
            2,
            true,
        ));
        let mut copies = 0;
        scan(log.flash_mut().bytes(), |r| {
            copies += (r.live && r.tag == TAG_PEER) as usize
        });
        assert_eq!(copies, 1, "each refresh purges the copy it replaces");
        assert_eq!(
            PnPeerStore::new(region.clone()).load_all().unwrap(),
            vec![quiet]
        );
    });
}

/// A peer record whose body does not decode is left out of the load; the
/// store counts it so the engine can say a peer went missing.
#[test]
fn an_undecodable_peer_record_is_counted() {
    block_on(async {
        let region = SharedRegion::new(SECTORS);
        let mut log = fresh(SECTORS).await;
        log.append(&peer_key(&[0x33; 16]), 1, TAG_PEER, &[0xFF; 5])
            .await
            .unwrap();
        sync(&region, &mut log);
        let store = PnPeerStore::new(region.clone());
        assert!(store.load_all().unwrap().is_empty());
        assert_eq!(store.undecodable_peer_records(), 1);
    });
}

// ---- the serve gated live, not reserved (order 1676, LV-1006) ----

/// The board's `/get` path end to end on a host: the role in the board's
/// own configuration at the measured node box's boot cap (0 B), two
/// stored messages for the client, and the heap the board reads at the
/// `/get` set one byte under what serving the first of them needs.
///
/// "Need" here is per request: the serve gate is live, so what decides is
/// whether the heap left after `SERVE_MARGIN_BYTES` funds the transient of
/// ONE stored message, not the 17 264 B the full announced cap would cost.
/// The threshold is searched, not hard-coded, so the test moves with the
/// margin and the model; it is printed for the report.
///
/// One byte under: the fetch is the heap refusal (`heap_low`), its wire
/// answer the empty list, and nothing is purged. A heap too tight to fund
/// even the list answers `ERROR_THROTTLED`, also `heap_low`. At the
/// threshold the same fetch is served. Positive control in both
/// directions, so neither half can pass vacuously.
#[test]
fn a_heap_that_funds_no_message_refuses_the_get_and_the_next_one_is_served() {
    use leviculum_lxmf::propagation::{
        MessageGetRequest, MessageListResponse, PeerError, TransferLimit,
    };
    use leviculum_lxmf::GetOutcome;

    let boot_cap = leviculum_heap_budget::budget_serve_cap(leviculum_heap_budget::NODE_BOX_BYTES);
    assert_eq!(
        boot_cap, 0,
        "the boot plan funds no serve: the gate is live"
    );

    let region = SharedRegion::new(SECTORS);
    let mut log = block_on(fresh(SECTORS));
    sync(&region, &mut log);
    let mut role = PropagationNode::new(
        PnStore::new(region.clone(), SECTORS),
        board_role_config(0, 0, b"bench".to_vec(), boot_cap),
    );
    let mut ids = Vec::new();
    for seed in [1u8, 2] {
        match role.handle_upload(&upload(seed), 1000, |_, _| Some(0)) {
            leviculum_lxmf::UploadOutcome::Accepted { transient_id, .. } => ids.push(transient_id),
            other => panic!("expected acceptance, got {other:?}"),
        }
        assert!(flush_front(&mut role, &region, &mut log));
    }
    let mailbox = [7u8; DESTINATION_LENGTH];
    let fetch = MessageGetRequest {
        wants: Some(ids.clone()),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(1000)),
    }
    .encode()
    .unwrap();
    let list = MessageGetRequest::list().encode().unwrap();

    // The smallest free heap (largest block equal to it, an unfragmented
    // heap) at which the live cap funds one stored message.
    let one_message = directory(role.store())
        .iter()
        .map(|meta| meta.size as usize)
        .min()
        .unwrap()
        + 24
        + 16;
    let funds =
        |free: usize| leviculum_heap_budget::live_serve_cap(boot_cap, free, free) >= one_message;
    let mut low = 0usize;
    let mut high = leviculum_heap_budget::HEAP_SIZE;
    assert!(funds(high) && !funds(low));
    while high - low > 1 {
        let mid = low + (high - low) / 2;
        if funds(mid) {
            high = mid;
        } else {
            low = mid;
        }
    }
    let threshold = high;
    std::eprintln!(
        "SERVE_GATE one_message={one_message} threshold_free={threshold} margin={} need_at_cap={}",
        leviculum_heap_budget::SERVE_MARGIN_BYTES,
        leviculum_heap_budget::SERVE_PEAK_BYTES
    );
    assert!(threshold > leviculum_heap_budget::SERVE_MARGIN_BYTES);

    // One byte under: refused for the heap, the empty list on the wire.
    let under = board_answer_get(
        &mut role,
        boot_cap,
        threshold - 1,
        threshold - 1,
        &fetch,
        &mailbox,
        1000,
    )
    .unwrap();
    assert!(
        under.heap_low,
        "one byte under the need is the heap refusal"
    );
    assert!(under.live_cap < one_message);
    let GetOutcome::Fetch { plan, purged } = &under.outcome else {
        panic!("a fetch request must produce a fetch outcome");
    };
    assert!(plan.is_empty() && purged.is_empty());
    assert_eq!(role.encode_fetch(plan).unwrap(), vec![0x90]);
    assert_eq!(directory(role.store()).len(), 2, "the mail stays");

    // A fragmented heap is refused the same way: the bytes are there,
    // the block is not. One byte under the largest single allocation one
    // message's serve asks for, after the margin.
    let block = leviculum_lxmf::propagation_node::serve_largest_block_bytes(
        one_message,
        leviculum_heap_budget::SERVE_RESOURCE_SDU,
    );
    let fragmented = board_answer_get(
        &mut role,
        boot_cap,
        2 * threshold,
        leviculum_heap_budget::SERVE_MARGIN_BYTES + block - 1,
        &fetch,
        &mailbox,
        1000,
    )
    .unwrap();
    assert!(fragmented.heap_low, "the largest block decides as well");
    let control = board_answer_get(
        &mut role,
        boot_cap,
        2 * threshold,
        leviculum_heap_budget::SERVE_MARGIN_BYTES + block,
        &fetch,
        &mailbox,
        1000,
    )
    .unwrap();
    assert!(!control.heap_low, "and one byte more block serves");

    // Tighter still: not even the list is funded, the stock client's
    // first request is answered ERROR_THROTTLED.
    let starved = board_answer_get(
        &mut role,
        boot_cap,
        leviculum_heap_budget::SERVE_MARGIN_BYTES,
        leviculum_heap_budget::SERVE_MARGIN_BYTES,
        &list,
        &mailbox,
        1000,
    )
    .unwrap();
    assert!(starved.heap_low);
    let GetOutcome::List { response, .. } = &starved.outcome else {
        panic!("the list request must return a list");
    };
    assert_eq!(
        MessageListResponse::decode(response).unwrap(),
        MessageListResponse::Error(PeerError::Throttled)
    );

    // The heap frees to the threshold: the next fetch is served.
    let freed = board_answer_get(
        &mut role, boot_cap, threshold, threshold, &fetch, &mailbox, 1000,
    )
    .unwrap();
    assert!(!freed.heap_low);
    let GetOutcome::Fetch { plan, .. } = &freed.outcome else {
        panic!("a fetch request must produce a fetch outcome");
    };
    assert_eq!(
        plan.count(),
        1,
        "exactly one message is funded at the threshold"
    );

    // And with the heap the field board stood on (30 380 B free), both.
    let field =
        board_answer_get(&mut role, boot_cap, 30_380, 30_320, &fetch, &mailbox, 1000).unwrap();
    assert!(!field.heap_low);
    let GetOutcome::Fetch { plan, .. } = &field.outcome else {
        panic!("a fetch request must produce a fetch outcome");
    };
    assert_eq!(plan.served_ids(), ids);
}
