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
use core::cell::RefCell;

use leviculum_lxmf::peering::{PeerRecord, PeerStore};
use leviculum_lxmf::propagation_store::{PropagationStore, StoredMessage};
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
        FlushOp::Purge { offset, key } => {
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
            store.set_free_bytes(log.free_bytes());
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

fn directory(store: &PnStore<SharedRegion>) -> Vec<StoredMessage> {
    let mut out = Vec::new();
    store.for_each(&mut |meta| out.push(*meta)).unwrap();
    out.sort_by_key(|meta| meta.sequence);
    out
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
        let store = PnStore::new(region.clone(), 3, log.free_bytes());
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

        let store = PnStore::new(region, SECTORS, log.free_bytes());
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
        let mut store = PnStore::new(region.clone(), SECTORS, log.free_bytes());

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
        let mut store = PnStore::new(region.clone(), SECTORS, log.free_bytes());

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
        let mut store = PnStore::new(region.clone(), SECTORS, log.free_bytes());
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
        let mut store = PnStore::new(region.clone(), SECTORS, log.free_bytes());
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
    }
}

#[test]
fn peer_record_round_trip() {
    let full = full_peer(7, sequence(3, 512));
    assert_eq!(decode_peer_record(&encode_peer_record(&full)), Some(full));

    let minimal = PeerRecord {
        identity_hash: None,
        peering_key: None,
        is_static: false,
        ..full_peer(9, 0)
    };
    assert_eq!(
        decode_peer_record(&encode_peer_record(&minimal)),
        Some(minimal)
    );

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
        let mut store = PnStore::new(region.clone(), SECTORS, log.free_bytes());
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
