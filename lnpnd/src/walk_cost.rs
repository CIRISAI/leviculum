//! What the role's processed-id cache and its full-store eviction cost at
//! the field's scale (order 1370, leviculum#478).
//!
//! The processed cache holds every id accepted in up to 180 days
//! (`PROCESSED_ID_EXPIRY_SECS`); at the 56 000 accepts a day the order
//! quotes for miauhaus that is about 10 M ids in steady state. `make_room`
//! runs on a full store, a 2 GB one at the field's mean of 330 bytes holds
//! about 6 M messages. Both are priced here by hand; the suite pins their
//! complexity in `leviculum-lxmf`.

use std::collections::BTreeMap;
use std::time::Instant;

use leviculum_lxmf::constants::STAMP_SIZE;
use leviculum_lxmf::processed_ids::ProcessedIds;
use leviculum_lxmf::propagation_node::PROCESSED_ID_EXPIRY_SECS;
use leviculum_lxmf::propagation_store::StoredMessage;
use leviculum_lxmf::storage::StorageError;
use leviculum_lxmf::{
    PropagationNode, PropagationNodeConfig, PropagationStore, PropagationUpload, TransientId,
    UploadOutcome,
};

use crate::test_support::next_random;

const DAY: u64 = 24 * 60 * 60;
const NOW: u64 = 1_790_000_000;

/// Resident bytes of this process, from `/proc/self/statm`.
fn resident_bytes() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|field| field.parse().ok())
        .unwrap_or(0);
    pages * 4096
}

fn random_id(state: &mut u64) -> TransientId {
    let mut transient_id = [0u8; 32];
    for chunk in transient_id.chunks_mut(8) {
        chunk.copy_from_slice(&next_random(state).to_le_bytes());
    }
    transient_id
}

fn sizes_from_env(name: &str, default: &[usize]) -> Vec<usize> {
    std::env::var(name)
        .ok()
        .map(|value| {
            value
                .split(',')
                .filter_map(|size| size.trim().parse().ok())
                .collect()
        })
        .unwrap_or_else(|| default.to_vec())
}

/// The processed cache as it was before order 1370, the type itself
/// (`BTreeMap<TransientId, u64>`): `ids` ids received evenly over the
/// 180-day window ending at [`NOW`], then the daily prune a day past the
/// oldest's window and absent-id lookups.
fn price_processed_btree(ids: usize) {
    let mut state = 0x243f_6a88_85a3_08d3 ^ ids as u64;
    let window = PROCESSED_ID_EXPIRY_SECS;
    let rss_before = resident_bytes();
    let built = Instant::now();
    let mut cache: BTreeMap<TransientId, u64> = BTreeMap::new();
    for n in 0..ids {
        let at = NOW - window + (n as u64 * window) / ids as u64;
        cache.insert(random_id(&mut state), at);
    }
    let build_ms = built.elapsed().as_millis();
    let rss = resident_bytes().saturating_sub(rss_before);
    let census = leviculum_core::heap_census::btree_map_bytes(&cache);

    let absent: Vec<TransientId> = (0..10_000).map(|_| random_id(&mut state)).collect();
    let looked = Instant::now();
    let hits = absent.iter().filter(|id| cache.contains_key(*id)).count();
    let lookup_ns = looked.elapsed().as_nanos() / absent.len() as u128;
    assert_eq!(hits, 0);

    // The prune a day past the oldest id's window, as the map was pruned.
    let prune_now = NOW + DAY;
    let pruned = Instant::now();
    cache.retain(|_, received| prune_now.saturating_sub(*received) <= window);
    let prune_ms = pruned.elapsed().as_millis();
    println!(
        "BENCH_PROCESSED repr=btree_map ids={ids} build_ms={build_ms} rss_bytes={rss} \
         rss_per_id={} census_bytes={census} lookup_ns={lookup_ns} prune_ms={prune_ms} kept={}",
        rss / ids as u64,
        cache.len()
    );
}

/// The processed cache as it is since order 1370
/// ([`leviculum_lxmf::processed_ids`]), priced as [`price_processed_btree`]
/// prices the map: built and sealed, then absent-id lookups and the daily
/// prune that seals the day just ended and drops the oldest.
fn price_processed_days(ids: usize, bucket_days: u64) {
    let mut state = 0x243f_6a88_85a3_08d3 ^ ids as u64;
    let window = PROCESSED_ID_EXPIRY_SECS;
    let rss_before = resident_bytes();
    let built = Instant::now();
    let mut cache = ProcessedIds::with_bucket_secs(bucket_days * DAY);
    for n in 0..ids {
        let at = NOW - window + (n as u64 * window) / ids as u64;
        cache.insert(&random_id(&mut state), at);
    }
    let sealed = Instant::now();
    cache.prune(NOW, window);
    let seal_ms = sealed.elapsed().as_millis();
    let build_ms = built.elapsed().as_millis();
    let rss = resident_bytes().saturating_sub(rss_before);
    let census = cache.heap_bytes();

    let absent: Vec<TransientId> = (0..10_000).map(|_| random_id(&mut state)).collect();
    let looked = Instant::now();
    let hits = absent.iter().filter(|id| cache.contains(id)).count();
    let lookup_ns = looked.elapsed().as_nanos() / absent.len() as u128;
    assert_eq!(hits, 0);

    // A day of accepts at the cache's own rate, then the prune a day on.
    for _ in 0..ids / 180 {
        cache.insert(&random_id(&mut state), NOW + 10);
    }
    let pruned = Instant::now();
    let dropped = cache.prune(NOW + DAY, window);
    let prune_us = pruned.elapsed().as_micros();
    println!(
        "BENCH_PROCESSED repr=buckets bucket_days={bucket_days} ids={ids} build_ms={build_ms} first_seal_ms={seal_ms} \
         rss_bytes={rss} rss_per_id={} census_bytes={census} lookup_ns={lookup_ns} \
         daily_prune_us={prune_us} dropped={dropped} kept={}",
        rss / ids as u64,
        cache.len()
    );
}

/// [`PropagationStore`] over directory entries alone, for an eviction pass
/// over millions of messages without their bodies.
struct MetaStore {
    walks: std::cell::Cell<usize>,
    index: BTreeMap<TransientId, StoredMessage>,
    used: u64,
    capacity: u64,
    sequence: u64,
}

impl PropagationStore for MetaStore {
    fn append(
        &mut self,
        transient_id: &TransientId,
        received_at: u64,
        stamp_value: u8,
        body: &[u8],
    ) -> Result<(), StorageError> {
        if self.used + body.len() as u64 > self.capacity {
            return Err(StorageError::Full);
        }
        self.sequence += 1;
        let mut destination_hash = [0u8; 16];
        destination_hash.copy_from_slice(&body[..16]);
        let meta = StoredMessage {
            transient_id: *transient_id,
            destination_hash,
            size: body.len() as u32,
            received_at,
            stamp_value,
            sequence: self.sequence,
        };
        if let Some(previous) = self.index.insert(*transient_id, meta) {
            self.used -= previous.size as u64;
        }
        self.used += body.len() as u64;
        Ok(())
    }
    fn for_each(&self, visit: &mut dyn FnMut(&StoredMessage)) -> Result<(), StorageError> {
        self.walks.set(self.walks.get() + 1);
        self.index.values().for_each(visit);
        Ok(())
    }
    fn purge(&mut self, transient_id: &TransientId) -> Result<bool, StorageError> {
        Ok(match self.index.remove(transient_id) {
            Some(meta) => {
                self.used -= meta.size as u64;
                true
            }
            None => false,
        })
    }
    fn free_space(&self) -> u64 {
        self.capacity - self.used
    }
    fn capacity(&self) -> u64 {
        self.capacity
    }
    fn contains(&self, transient_id: &TransientId) -> Result<bool, StorageError> {
        Ok(self.index.contains_key(transient_id))
    }
    fn count(&self) -> Result<usize, StorageError> {
        Ok(self.index.len())
    }
}

/// A full store of `capacity` bytes at the field's mean size (330 bytes,
/// 200 to 460), received over the 30 days before [`NOW`], then uploads that
/// each find it full: the first `make_room` and the ones after it.
fn price_make_room(capacity: u64, uploads: usize) {
    let mut state = 0x1319_8a2e_0370_7344 ^ capacity;
    let mut store = MetaStore {
        walks: std::cell::Cell::new(0),
        index: BTreeMap::new(),
        used: 0,
        capacity,
        sequence: 0,
    };
    let filled = Instant::now();
    loop {
        let len = 200 + (next_random(&mut state) % 261) as usize;
        let mut body = vec![0x5a; len];
        body[..16].copy_from_slice(&random_id(&mut state)[..16]);
        let received_at = NOW - next_random(&mut state) % (30 * DAY);
        if store
            .append(&random_id(&mut state), received_at, 13, &body)
            .is_err()
        {
            break;
        }
    }
    let messages = store.index.len();
    let fill_ms = filled.elapsed().as_millis();
    let config = PropagationNodeConfig {
        stamp_cost: 0,
        ..PropagationNodeConfig::default()
    };
    let mut node = PropagationNode::new(store, config);
    let mut micros = Vec::new();
    let mut evicted = Vec::new();
    for upload in 0..uploads {
        let mut lxmf = vec![7u8; 16];
        lxmf.extend_from_slice(&random_id(&mut state));
        lxmf.resize(330 - STAMP_SIZE, upload as u8);
        let envelope = PropagationUpload::single(NOW as f64, lxmf, [0xEE; STAMP_SIZE]).encode();
        let started = Instant::now();
        let outcome = node.handle_upload(&envelope, NOW, |_, _| Some(0));
        micros.push(started.elapsed().as_micros());
        match outcome {
            UploadOutcome::Accepted {
                duplicate: false,
                evicted: displaced,
                ..
            } => evicted.push(displaced.len()),
            other => panic!("upload {upload}: {other:?}"),
        }
    }
    let walks = evicted.iter().filter(|count| **count > 0).count();
    println!(
        "BENCH_MAKE_ROOM capacity={capacity} messages={messages} fill_ms={fill_ms} \
         uploads={uploads} first_us={} median_us={} max_us={} uploads_that_evicted={walks} \
         evicted_total={} store_walks={}",
        micros[0],
        {
            let mut sorted = micros.clone();
            sorted.sort_unstable();
            sorted[sorted.len() / 2]
        },
        micros.iter().max().copied().unwrap_or(0),
        evicted.iter().sum::<usize>(),
        node.store().walks.get()
    );
}

/// Order 1370, leviculum#478: the processed cache's bytes per id, its daily
/// prune and its lookup at `LNPND_BENCH_IDS` ids (1 M and 10 M), and
/// `make_room` over a full `LNPND_BENCH_STORE_BYTES` store (2 GB). A
/// measurement; it asserts nothing about the numbers. Run alone, so the
/// resident-set deltas are this test's:
///
/// ```text
/// cargo test --release -p lnpnd --lib processed_and_eviction_cost -- --ignored --nocapture --test-threads=1
/// ```
#[test]
#[ignore = "a measurement over ten million ids and a 2 GB store"]
fn processed_and_eviction_cost_at_field_scale() {
    for ids in sizes_from_env("LNPND_BENCH_IDS", &[1_000_000, 10_000_000]) {
        price_processed_btree(ids);
        for bucket_days in [1, 7] {
            price_processed_days(ids, bucket_days);
        }
    }
    for capacity in sizes_from_env("LNPND_BENCH_STORE_BYTES", &[2_000_000_000]) {
        price_make_room(capacity as u64, 200);
    }
}
