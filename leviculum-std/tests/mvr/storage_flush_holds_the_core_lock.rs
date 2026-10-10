//! mvr for leviculum#563 — the periodic storage flush holds the core lock
//! for seconds on a node that knows many destinations.
//!
//! The soak node miauhaus wrote one `CORE_STALL` every 3602 s and nothing in
//! between, 1.9 s growing to 2.6 s over 45 hours. The period is the default
//! `flush_interval_secs` (3600 s) plus the stall itself, because the driver
//! re-arms the flush timer only after `begin_flush` (Branch 7 of the event
//! loop, `leviculum-std/src/driver/mod.rs`) returns. The file IO already ran
//! off the lock (leviculum#44); what stayed on it was the capture: one
//! announce parse per runtime identity to fill `app_data` (#420), the fold
//! into the known-destinations table, and a deep copy of that table.
//!
//! # What this reproduces
//!
//! One node with a storage directory and no interfaces, in the field node's
//! shape as its census read on 2026-10-03: 50 000 runtime identities (the
//! `identity_cap`), 87 000 known destinations, 17 000 paths with a cached
//! announce each, and 75 000 packet hashes. The known destinations come off
//! disk the way a node that has run for weeks starts; a [`CoreProcessor`]
//! adds the rest from `on_tick` in slices too short to stall the loop
//! themselves, then keeps the store dirty as announce traffic does. The flush
//! interval is 2 s, and the driver's own stall watchdog
//! (`spawn_core_stall_watchdog`) is the instrument.
//!
//! Each flush reports its own core-lock holds as `FLUSH_LOCK_HOLD`, in CPU
//! time of the holding thread: the work done under the lock, which belongs
//! to this node alone and needs no threshold of the watchdog's to be seen.
//!
//! The processor also asks for a tick every millisecond and prints the
//! longest gap between two ticks while the flushes run. It is printed, not
//! asserted (leviculum#607): a wall-clock gap includes every slice the host
//! scheduler gave to another process, so beside a hundred parallel mvrs it
//! read 67 ms with `core_stalls=0`, and under `stress-ng --cpu 40` 58 to 77
//! ms, with the flush unchanged. A test that is red when the host is busy
//! is flaky by construction.
//!
//! **Acceptance**: across three flushes, no `CORE_STALL` and no flush whose
//! longest lock hold took [`MAX_FLUSH_HOLD_CPU`] or more, and the file the
//! flushes wrote reads back through the production reader
//! (`StdStorage::new`) with every identity and every `app_data` in it.
//!
//! The capture sees every node in this process, so a stall from another
//! mvr running in parallel would also land here. A healthy node emits
//! `CORE_STALL` never (`CORE_STALL_REPORT_MS`), so such a line is a defect
//! in its own right and not noise to filter out.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use leviculum_core::node::{NodeCoreBuilder, NodeEvent};
use leviculum_core::storage_types::PathEntry;
use leviculum_core::traits::Storage as _;
use leviculum_core::transport::TickOutput;
use leviculum_core::{Destination, DestinationHash, DestinationType, Direction, Identity};
use leviculum_std::driver::{
    CoreProcessor, ReticulumNodeBuilder, StdClock, StdNodeCore, StdStorage,
};
use leviculum_std::test_support::event_log::init_event_log;

/// `known_dest_entries: 87059 entries` in the field node's census.
const KNOWN_DESTINATIONS: usize = 87_000;

/// The runtime identities the field node holds: `known_identities: 50000`,
/// which is the cap. A node loads every known destination it has on disk,
/// so the cap keeps the last 50 000 in hash order.
const IDENTITY_CAP: usize = 50_000;

/// The field node's path table and announce cache (`PATH_TABLE` last=16815,
/// `announce_cache: 16922`), rounded up.
const PATHS: usize = 17_000;

/// Both generations of the field node's packet-hash list (`packet_cache`
/// 25 527 + `packet_cache_prev` 50 001).
const PACKET_HASHES: usize = 75_000;

/// Entries loaded per `on_tick`. Small enough that loading never trips the
/// watchdog itself, so every stall the test sees belongs to the flush.
const LOAD_SLICE: usize = 500;

const FLUSH_INTERVAL_SECS: u64 = 2;
const FLUSHES: usize = 3;

/// The longest single core-lock hold, in CPU time, a flush may take.
///
/// Before the #563 fix the field shape held the loop for the whole capture,
/// and each flush showed it as one wall-clock tick gap of that length: 93,
/// 94 and 100 ms in three runs on the coder host, debug build. The watchdog
/// stayed silent in all three, which is the host being faster than the
/// field node (1.9 to 2.6 s there in a release build, 20 to 28 times the
/// host's debug hold), not the hold being harmless. After the fix the
/// longest hold is one identity slice: 2.5 to 2.6 ms of CPU time on a quiet
/// host, 2.6 to 3.7 ms under `stress-ng --cpu 40`. The bound sits
/// between the two, a fifth of the watchdog's 250 ms, and CPU time keeps
/// it where it is on a loaded host (#607).
const MAX_FLUSH_HOLD_CPU: Duration = Duration::from_millis(50);

/// The `key=value` field of a structured event line, as a number.
fn field(line: &str, key: &str) -> Option<u64> {
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
        .and_then(|v| v.parse().ok())
}

fn destination(i: usize) -> [u8; 16] {
    let mut dest = [0x63; 16];
    dest[..8].copy_from_slice(&(i as u64).to_be_bytes());
    dest
}

/// Adds the field node's paths and announces, then keeps the store dirty
/// and measures how long the loop goes between ticks.
struct FieldShape {
    /// The destinations still to give a path and a cached announce.
    paths: std::ops::Range<usize>,
    announce: Vec<u8>,
    identity: Identity,
    hashes_left: usize,
    next_hash: u64,
    loaded: Arc<AtomicBool>,
    /// Set by the test while the flushes it counts run.
    measuring: Arc<AtomicBool>,
    longest_gap_us: Arc<AtomicU64>,
    last_tick: Option<Instant>,
}

impl FieldShape {
    fn packet_hash(&mut self) -> [u8; 32] {
        self.next_hash += 1;
        let mut hash = [0u8; 32];
        hash[..8].copy_from_slice(&self.next_hash.to_be_bytes());
        hash
    }
}

impl CoreProcessor for FieldShape {
    fn on_event(&mut self, _core: &mut StdNodeCore, _event: &NodeEvent) -> TickOutput {
        TickOutput::empty()
    }

    fn on_tick(&mut self, core: &mut StdNodeCore, now_ms: u64) -> TickOutput {
        let mut output = TickOutput::empty();
        // Come back at once, rather than in a second.
        output.next_deadline_ms = Some(now_ms);

        if self.loaded.load(Ordering::Relaxed) {
            let now = Instant::now();
            if let Some(last) = self.last_tick {
                if self.measuring.load(Ordering::Relaxed) {
                    let gap = now.duration_since(last).as_micros() as u64;
                    self.longest_gap_us.fetch_max(gap, Ordering::Relaxed);
                }
            }
            self.last_tick = Some(now);
            // The field node hears announces all the time: a re-learned
            // identity and a new packet hash keep every flush a full one.
            let hash = self.packet_hash();
            let storage = core.storage_mut();
            storage.add_packet_hash(hash);
            storage.set_identity(destination(KNOWN_DESTINATIONS - 1), self.identity.clone());
            return output;
        }

        for i in self.paths.by_ref().take(LOAD_SLICE) {
            let dest = destination(i);
            let storage = core.storage_mut();
            storage.set_announce_cache(dest, self.announce.clone());
            storage.set_path(
                dest,
                PathEntry {
                    hops: 3,
                    expires_ms: now_ms + 7 * 24 * 3_600_000,
                    interface_index: 0,
                    random_blobs: vec![[dest[0]; 10]],
                    next_hop: Some([dest[1]; 16]),
                    via_peer: None,
                },
            );
        }
        for _ in 0..self.hashes_left.min(LOAD_SLICE * 3) {
            let hash = self.packet_hash();
            core.storage_mut().add_packet_hash(hash);
            self.hashes_left -= 1;
        }
        if self.paths.is_empty() && self.hashes_left == 0 {
            self.loaded.store(true, Ordering::Relaxed);
        }
        output
    }
}

/// A raw announce with a display-name-sized `app_data`, the shape most of a
/// public mesh's announces have. One is cached under every destination: the
/// flush parses the bytes, it never checks they belong to the key.
fn field_announce() -> (Vec<u8>, Vec<u8>) {
    let mut destination = Destination::new(
        Some(Identity::generate(&mut rand_core::OsRng)),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .expect("a delivery destination");
    let app_data = b"\x92\xc4\x0fField node peer\xc0".to_vec();
    let now_secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let packet = destination
        .announce(Some(&app_data), &mut rand_core::OsRng, 0, now_secs)
        .expect("announce packet");
    let mut buf = vec![0u8; 600];
    let len = packet.pack(&mut buf).expect("pack the announce");
    buf.truncate(len);
    (buf, app_data)
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hourly_flush_leaves_the_core_lock_free() {
    let storage = tempfile::tempdir().expect("tempdir");
    let identity = Identity::generate(&mut rand_core::OsRng);

    // What weeks of uptime leave on disk: the known destinations of every
    // announce the node ever validated.
    {
        let mut disk = StdStorage::new(storage.path()).expect("storage");
        // Flushed in batches under the cap, so none is evicted unwritten.
        for i in 0..KNOWN_DESTINATIONS {
            disk.set_identity(destination(i), identity.clone());
            if (i + 1) % (IDENTITY_CAP / 2) == 0 {
                disk.flush();
            }
        }
        disk.flush();
    }

    let (announce, app_data) = field_announce();
    let loaded = Arc::new(AtomicBool::new(false));
    let measuring = Arc::new(AtomicBool::new(false));
    let longest_gap_us = Arc::new(AtomicU64::new(0));
    // The paths go to destinations the identity cap keeps, as on the field
    // node: a path is to someone whose announce was just validated.
    let paths = KNOWN_DESTINATIONS - PATHS..KNOWN_DESTINATIONS;

    let evlog = init_event_log();
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .storage_path(storage.path().to_path_buf())
        .flush_interval_secs(FLUSH_INTERVAL_SECS)
        .core_processor(FieldShape {
            paths: paths.clone(),
            announce,
            identity: identity.clone(),
            hashes_left: PACKET_HASHES,
            next_hash: 0,
            loaded: Arc::clone(&loaded),
            measuring: Arc::clone(&measuring),
            longest_gap_us: Arc::clone(&longest_gap_us),
            last_tick: None,
        })
        .build()
        .await
        .expect("build node");
    node.start().await.expect("start node");

    let deadline = Instant::now() + Duration::from_secs(60);
    while !loaded.load(Ordering::Relaxed) {
        assert!(
            Instant::now() < deadline,
            "the processor did not load the field shape within 60 s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Three flushes after the load, each one seen as a new write of the
    // known-destinations file.
    let file = storage.path().join("known_destinations");
    let mut last = modified(&file);
    measuring.store(true, Ordering::Relaxed);
    let mut flushes = 0;
    let deadline = Instant::now() + Duration::from_secs(FLUSH_INTERVAL_SECS * 10);
    while flushes < FLUSHES {
        assert!(
            Instant::now() < deadline,
            "only {flushes} of {FLUSHES} flushes wrote {} in {} s",
            file.display(),
            FLUSH_INTERVAL_SECS * 10
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        let now = modified(&file);
        if now != last {
            flushes += 1;
            last = now;
        }
    }
    // The watchdog reports a stall when the wait ends, at most one probe
    // interval after the lock comes free.
    tokio::time::sleep(Duration::from_millis(200)).await;
    measuring.store(false, Ordering::Relaxed);
    let events = evlog.dump();
    let stalls: Vec<&String> = events
        .iter()
        .filter(|line| line.starts_with("CORE_STALL "))
        .collect();
    let flush_holds: Vec<&String> = events
        .iter()
        .filter(|line| line.starts_with("FLUSH_LOCK_HOLD "))
        .collect();
    let longest_hold = flush_holds
        .iter()
        .filter_map(|line| field(line, "longest_cpu_us"))
        .max()
        .map(Duration::from_micros);
    let longest_gap = Duration::from_micros(longest_gap_us.load(Ordering::Relaxed));
    node.stop().await.expect("stop node");
    drop(node);
    eprintln!(
        "FLUSH_HOLD known={KNOWN_DESTINATIONS} identities={IDENTITY_CAP} paths={PATHS} \
         longest_tick_gap_ms={} core_stalls={} flushes={} longest_hold_cpu_us={}",
        longest_gap.as_millis(),
        stalls.len(),
        flush_holds.len(),
        longest_hold.map_or(0, |d| d.as_micros())
    );

    assert!(
        stalls.is_empty(),
        "the storage flush must not hold the core lock past CORE_STALL_REPORT_MS; \
         {} stall(s) across {FLUSHES} flushes:\n{}",
        stalls.len(),
        stalls
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        flush_holds.len() >= FLUSHES,
        "every flush reports its lock holds; {} FLUSH_LOCK_HOLD line(s) for \
         at least {FLUSHES} flushes",
        flush_holds.len()
    );
    let longest_hold = longest_hold.unwrap_or(Duration::MAX);
    assert!(
        longest_hold < MAX_FLUSH_HOLD_CPU,
        "a flush held the core lock for {} us of CPU time, bound {} ms:\n{}",
        longest_hold.as_micros(),
        MAX_FLUSH_HOLD_CPU.as_millis(),
        flush_holds
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );

    // The production reader still reads what the flushes wrote.
    let reader = NodeCoreBuilder::new().enable_transport(false).build(
        rand_core::OsRng,
        StdClock::new(),
        StdStorage::new(storage.path()).expect("storage reopens"),
    );
    let known = reader
        .storage()
        .collection_counts()
        .into_iter()
        .find(|c| c.name == "known_dest_entries")
        .map(|c| c.entries);
    assert_eq!(
        known,
        Some(KNOWN_DESTINATIONS),
        "every known destination must come back from the flushed file"
    );
    let recalled = paths
        .clone()
        .filter(|i| {
            reader
                .recall_app_data(&DestinationHash::new(destination(*i)))
                .as_deref()
                == Some(app_data.as_slice())
        })
        .count();
    assert_eq!(
        recalled, PATHS,
        "every destination with a cached announce must come back with its app_data"
    );
}
