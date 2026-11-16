//! What the store maintenance tick costs the core hook, against store size
//! (order 1295, leviculum#478).
//!
//! The tick's expiry sweep walked every stored message under the core lock
//! every [`STORE_MAINTENANCE_SECS`](crate::engine), 24.6 ms on miauhaus's
//! Odroid at about 300 000 messages (order 1171's field profile) whether
//! anything had expired or not. Over the file store's receive-time index it
//! reads only what expired. The suite test pins that property; the ignored
//! one prices it over a field-sized store.

use std::cell::Cell;
use std::ops::ControlFlow;
use std::path::Path;

use leviculum_lxmf::propagation_store::StoredMessage;
use leviculum_lxmf::storage::StorageError;
use leviculum_lxmf::{
    PropagationNode, PropagationNodeConfig, PropagationStore, TransientId, MAX_EXPIRED_PER_TICK,
    MESSAGE_EXPIRY_SECS,
};
use leviculum_std::FilePropagationStore;

use crate::test_support::spread_store;

/// A synthetic "now" the stores are filled around.
const NOW: u64 = 1_790_000_000;
const DAY: u64 = 24 * 60 * 60;

/// The production store, its whole-store walks counted. `indexed` false
/// hides the receive-time index, so the sweep takes the trait's default
/// walk, as it did over `FilePropagationStore` before this order.
struct Counted {
    store: FilePropagationStore,
    indexed: Cell<bool>,
    walks: Cell<usize>,
}

impl PropagationStore for Counted {
    fn append(
        &mut self,
        transient_id: &TransientId,
        received_at: u64,
        stamp_value: u8,
        body: &[u8],
    ) -> Result<(), StorageError> {
        self.store
            .append(transient_id, received_at, stamp_value, body)
    }
    fn for_each(&self, visit: &mut dyn FnMut(&StoredMessage)) -> Result<(), StorageError> {
        self.walks.set(self.walks.get() + 1);
        self.store.for_each(visit)
    }
    fn for_each_received_before(
        &self,
        before: u64,
        visit: &mut dyn FnMut(&StoredMessage) -> ControlFlow<()>,
    ) -> Result<(), StorageError> {
        if self.indexed.get() {
            return self.store.for_each_received_before(before, visit);
        }
        let mut done = false;
        self.for_each(&mut |meta| {
            if !done && meta.received_at < before {
                done = visit(meta).is_break();
            }
        })
    }
    fn purge(&mut self, transient_id: &TransientId) -> Result<bool, StorageError> {
        self.store.purge(transient_id)
    }
    fn free_space(&self) -> u64 {
        self.store.free_space()
    }
    fn capacity(&self) -> u64 {
        self.store.capacity()
    }
    fn contains(&self, transient_id: &TransientId) -> Result<bool, StorageError> {
        self.store.contains(transient_id)
    }
    fn count(&self) -> Result<usize, StorageError> {
        self.store.count()
    }
}

fn node_over(store: FilePropagationStore, indexed: bool) -> PropagationNode<Counted> {
    PropagationNode::new(
        Counted {
            store,
            indexed: Cell::new(indexed),
            walks: Cell::new(0),
        },
        PropagationNodeConfig::default(),
    )
}

fn older_than_expiry(store: &FilePropagationStore, now: u64) -> Vec<TransientId> {
    let mut ids = Vec::new();
    store
        .for_each(&mut |meta| {
            if now.saturating_sub(meta.received_at) > MESSAGE_EXPIRY_SECS {
                ids.push(meta.transient_id);
            }
        })
        .expect("walk");
    ids.sort_unstable();
    ids
}

/// leviculum#478, order 1295: a maintenance tick over the production store
/// evicts exactly the messages older than the 30-day expiry, and walks
/// none of the store to find them; the next tick finds nothing. The
/// positive control hides the index and counts the walk the sweep made
/// before, over a twin store, and must evict the same messages.
#[test]
fn an_expiry_tick_reads_what_expired_not_the_store() {
    for indexed in [true, false] {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = spread_store(dir.path(), 400, 40, NOW);
        let due = older_than_expiry(&store, NOW);
        assert!(
            due.len() > 40 && due.len() < 200,
            "a quarter of 400 messages over 40 days is due, {} are",
            due.len()
        );
        let mut node = node_over(store, indexed);

        let mut evicted: Vec<TransientId> = node
            .tick(NOW)
            .into_iter()
            .map(|eviction| eviction.transient_id)
            .collect();
        evicted.sort_unstable();
        assert_eq!(
            evicted, due,
            "indexed {indexed}: the tick evicts what expired"
        );
        assert_eq!(node.store().count(), Ok(400 - due.len()));
        assert!(node.tick(NOW + 1).is_empty(), "nothing is left to expire");

        let walks = node.store().walks.get();
        if indexed {
            assert_eq!(walks, 0, "the sweep walked the production store");
        } else {
            assert_eq!(walks, 2, "positive control: one walk a tick");
        }
    }
}

fn median_and_max(mut micros: Vec<u128>) -> (u128, u128) {
    micros.sort_unstable();
    (micros[micros.len() / 2], micros[micros.len() - 1])
}

/// leviculum#478, order 1295: what one maintenance tick costs the core hook
/// over a store of `LNPND_BENCH_STORED` messages (300 000, miauhaus's count
/// on 2026-11-02 was 320 666) received over the 40 days before a synthetic
/// now, the sweep as it was (a whole-store walk) against the sweep as it is
/// (the receive-time index):
///
/// - `idle`: a tick at which nothing has expired yet, miauhaus's state
///   since 2026-10-30;
/// - `first_slice`, `first_rest`: the tick that finds ten days' backlog
///   expired at once, the slice lnpnd's hook takes and the rest in one;
/// - `steady`: one tick a maintenance period later, which finds what
///   expired in those 480 s.
///
/// A measurement: it asserts only that both sweeps evict what expired. The
/// store lives in `LNPND_BENCH_DIR` and is kept and topped up, so a second
/// run fills only what the first run's sweep removed:
///
/// ```text
/// LNPND_BENCH_DIR=/home/lew/scratch/1295 \
///   cargo test --release -p lnpnd --lib expiry_tick_cost -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a measurement over a store of hundreds of thousands of files"]
fn expiry_tick_cost_over_a_field_sized_store() {
    use std::time::Instant;

    let dir = std::env::var("LNPND_BENCH_DIR").expect("LNPND_BENCH_DIR names the store");
    let messages: usize = std::env::var("LNPND_BENCH_STORED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(300_000);
    let filled = Instant::now();
    let store = spread_store(Path::new(&dir), messages, 40, NOW);
    println!(
        "BENCH_STORE dir={dir} messages={} open_ms={}",
        store.len(),
        filled.elapsed().as_millis()
    );
    let backlog = older_than_expiry(&store, NOW).len();
    let mut node = node_over(store, false);
    let period = 480;

    // Idle: ten days before NOW nothing is 30 days old yet.
    let idle_now = NOW - 10 * DAY;
    for indexed in [false, true] {
        node.store().indexed.set(indexed);
        let mut ticks = Vec::new();
        for _ in 0..15 {
            let tick = Instant::now();
            let evicted = node.tick(idle_now);
            ticks.push(tick.elapsed().as_micros());
            assert!(evicted.is_empty());
        }
        let (median, max) = median_and_max(ticks);
        println!(
            "BENCH_EXPIRY phase=idle sweep={} messages={} tick_us_median={median} tick_us_max={max}",
            if indexed { "indexed" } else { "walk" },
            messages
        );
    }

    // First: the backlog, over the index: one slice as lnpnd's hook takes
    // it, then the rest in one unbounded tick.
    node.store().indexed.set(true);
    let tick = Instant::now();
    let slice = node.tick_at_most(NOW, MAX_EXPIRED_PER_TICK);
    println!(
        "BENCH_EXPIRY phase=first_slice sweep=indexed messages={messages} evicted={} tick_us={}",
        slice.len(),
        tick.elapsed().as_micros()
    );
    let tick = Instant::now();
    let evicted = node.tick(NOW);
    println!(
        "BENCH_EXPIRY phase=first_rest sweep=indexed messages={} evicted={} tick_us={}",
        messages - slice.len(),
        evicted.len(),
        tick.elapsed().as_micros()
    );
    assert_eq!(slice.len() + evicted.len(), backlog);

    // Steady: one period later each, alternating the two sweeps.
    let mut now = NOW;
    for indexed in [false, true] {
        node.store().indexed.set(indexed);
        let (mut ticks, mut total) = (Vec::new(), 0);
        for _ in 0..15 {
            now += period;
            let due = older_than_expiry(&node.store().store, now).len();
            let tick = Instant::now();
            let evicted = node.tick(now);
            ticks.push(tick.elapsed().as_micros());
            assert_eq!(evicted.len(), due);
            total += evicted.len();
        }
        let (median, max) = median_and_max(ticks);
        println!(
            "BENCH_EXPIRY phase=steady sweep={} messages={} evicted_per_tick={} \
             tick_us_median={median} tick_us_max={max}",
            if indexed { "indexed" } else { "walk" },
            node.store().count().unwrap_or(0),
            total / 15
        );
    }
}
