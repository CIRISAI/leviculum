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
        FlushOp::Purge { .. } => unreachable!("only appends in these tests"),
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
    let mut role = PropagationNode::new(
        PnStore::new(region.clone(), SECTORS),
        board_role_config(3, 1, b"board".to_vec(), 4_000),
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
        table.restore(rebooted.load_all().unwrap());
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
        table.restore(rebooted.load_all().unwrap());
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
