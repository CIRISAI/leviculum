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
