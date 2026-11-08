//! The growth soak for LV-1000 (order 1515): which of lnpnd's tables keeps
//! growing once its message store is full.
//!
//! lnpnd reached 398 MB anonymous RSS on miauhaus (1.9 GB host) and the OOM
//! killer took it twice in four days. Its store there held 326 023 messages,
//! and 14 peers synced with it all day (`analyze-lnpnd.py`, 2026-11-07).
//! This soak is that load on one host, with the release daemon as the node
//! under test:
//!
//! ```text
//!   lnpnd (release, its own process)   store prefilled to the field's size,
//!        │  \0rns/<name>               limit just above it
//!   shared instance (in process, transport on)
//!        │  \0rns/<name>
//!   14 propagation engines (in process)  each holding fresh messages for the
//!                                        node, peered with it alone (a
//!                                        static peer, no autopeering)
//! ```
//!
//! The node offers its store to every peer, round after round, as miauhaus
//! forwards 1.4 M messages in eight hours, and the peers offer it theirs, so
//! it fills its store and evicts. Every minute the soak sends SIGUSR1, reads
//! the `PN_CENSUS` line the node writes into its event log, and at the end
//! prints the slope of every table over the samples taken with the store
//! full:
//!
//! ```text
//! PN_GROWTH table=store_index kb_per_min=0.0 first_kb=... last_kb=...
//! ```
//!
//! **Red** when a table's slope over the full-store window is above
//! [`FLAT_KB_PER_MIN`], or the store never filled (then the soak proved
//! nothing). Full is full in bytes: eviction takes the heaviest messages
//! first, so the count may still rise at the limit, and `store_index` may
//! rise by what those messages cost, the window's count slope times the
//! index's bytes a message. `dup` is judged against the growth it has by design: every
//! accepted id is remembered for the reference's 180-day window
//! (`leviculum-lxmf/src/processed_ids.rs`), [`DUP_BYTES_PER_ACCEPT`] bytes an
//! id at most, every synced one's origin while the message is held,
//! [`DUP_BYTES_PER_SYNCED_ORIGIN`], and every evicted one for the message
//! expiry, [`DUP_BYTES_PER_EVICTION`]. `rss_kb` is printed beside the tables, not judged: the
//! allocator keeps what it once had, and what to fix is the table.
//!
//! Run it with `just pn-growth-soak` (release, 20 minutes, 300 000
//! messages). Knobs: `LNPND_SOAK_MESSAGES`, `LNPND_SOAK_PEERS`,
//! `LNPND_SOAK_SECS`, `LNPND_SOAK_SAMPLE_SECS`, `LNPND_SOAK_PEER_MESSAGES`,
//! `LNPND_SOAK_DIR` (a directory on a real disk, the target dir's tmp by
//! default; never `/tmp`, a tmpfs every lane shares).

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leviculum_core::{Destination, Identity};
use leviculum_lxmf::constants::{LXMF_OVERHEAD, STAMP_SIZE};
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::{
    MemoryPeerStore, MemoryPropagationStore, PeeringConfig, PropagationNodeConfig, PropagationStore,
};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::durable::Fsync;
use leviculum_std::process::spawn_supervised;
use leviculum_std::{FilePropagationStore, ReticulumNode};
use lnpnd::engine::{Engine, EngineConfig};

/// A table that grows slower than this over the full-store window is flat:
/// one kilobyte a minute is 1.4 MB a day, below anything that ends in an
/// OOM on a node, and above the jitter of a table that is filled and
/// emptied by the rounds in flight at the sample.
const FLAT_KB_PER_MIN: f64 = 1.0;

/// The duplicate list's designed growth per accepted id: 16 bytes of key in
/// a sealed bucket, about 27 in an open one by the census's B-tree model.
const DUP_BYTES_PER_ACCEPT: f64 = 32.0;

/// One peer's store: room for its fresh messages (10 000 at most 600 bytes)
/// and for what the node forwards it, which it evicts as it goes.
const PEER_STORE_BYTES: u64 = 8_000_000;

/// The departure the duplicate list remembers for every message that left
/// the store, for the message expiry (`Departures`,
/// `leviculum-lxmf/src/propagation_node.rs`): a 32-byte id and its time
/// and reason in a B-tree, about 78 bytes by the census's model.
const DUP_BYTES_PER_EVICTION: f64 = 80.0;

/// The origin the peering runtime keeps for every message a peer's sync
/// stored, so the offer toward that peer leaves it out (`Origins`,
/// `lnpnd/src/peering.rs`, LV-1003): a 16-byte id prefix, the peer and the
/// receive time in a hash table, about 47 bytes by the census's model,
/// counted in `dup`. Every accept in this soak arrives by a peer's sync. It
/// goes when its message is evicted, so it is bounded by the store's count,
/// but the prefilled messages the node evicts first carry none: until they
/// are gone the column grows by this much an accept.
const DUP_BYTES_PER_SYNCED_ORIGIN: f64 = 48.0;

/// The line lnpnd prints once its propagation destination is up.
const READY: &str = "lnpnd: propagation node ready";

fn knob(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

struct NoFsync;

impl Fsync for NoFsync {
    fn sync(&self, _handle: &std::fs::File) -> std::io::Result<()> {
        Ok(())
    }
}

fn next_random(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// One stored body, `lxmf_data || stamp`, and its transient id: the size
/// around the field's mean of 330 bytes, unique by `seed`, the id the node
/// will compute for it (`SHA-256(lxmf_data)`).
fn message(seed: u64, state: &mut u64) -> ([u8; 32], Vec<u8>) {
    let len = LXMF_OVERHEAD + 160 + (next_random(state) % 260) as usize;
    let mut body = vec![0x5au8; len];
    body[..8].copy_from_slice(&next_random(state).to_le_bytes());
    body[16..24].copy_from_slice(&seed.to_be_bytes());
    let transient_id = leviculum_core::crypto::full_hash(&body);
    body.extend_from_slice(&[0x5b; STAMP_SIZE]);
    (transient_id, body)
}

/// The node's store, filled to `messages` as a node that has run a while
/// holds it, and its size in bytes. A reopen tops an earlier fill up.
fn prefill(dir: &Path, messages: usize) -> u64 {
    let fsync: Arc<dyn Fsync> = Arc::new(NoFsync);
    let mut store = FilePropagationStore::open_with(dir, u64::MAX / 2, fsync).expect("store dir");
    let mut state = 0x6a09_e667_f3bc_c908 ^ store.len() as u64;
    let now = unix_secs();
    let mut seed = store.len() as u64;
    while store.len() < messages {
        let (transient_id, body) = message(seed, &mut state);
        seed += 1;
        // Received over the last twenty days, so nothing expires mid-soak.
        let received_at = now - next_random(&mut state) % (20 * 24 * 3600);
        store
            .append(&transient_id, received_at, 13, &body)
            .expect("the prefill has room");
    }
    store.capacity().saturating_sub(store.free_space())
}

fn propagation_hash_of(identity: &Identity) -> [u8; 16] {
    let name_hash = Destination::compute_name_hash(APP_NAME, &["propagation"]);
    *Destination::compute_destination_hash(&name_hash, identity.hash()).as_bytes()
}

/// The release lnpnd beside this test binary, or the one cargo built for it.
fn lnpnd_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lnpnd"))
}

struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One parsed `PN_CENSUS` line: column to value, plus the minute it was
/// read at.
#[derive(Debug, Clone)]
struct Sample {
    minute: f64,
    values: BTreeMap<String, f64>,
}

impl Sample {
    fn get(&self, key: &str) -> f64 {
        self.values.get(key).copied().unwrap_or(0.0)
    }
}

/// The `key=value` pairs of the last `PN_CENSUS` line in `log` past byte
/// `from`, and where the log ended.
fn last_census(log: &Path, from: usize) -> (Option<BTreeMap<String, f64>>, usize) {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let tail = text.get(from..).unwrap_or("");
    let line = tail
        .lines()
        .rev()
        .find(|line| line.starts_with("PN_CENSUS "));
    let values = line.map(|line| {
        line.split_whitespace()
            .filter_map(|field| {
                let (key, value) = field.split_once('=')?;
                Some((key.to_string(), value.trim_matches('"').parse().ok()?))
            })
            .collect()
    });
    (values, text.len())
}

/// Least-squares slope of `key` over `samples`, per minute.
fn slope(samples: &[Sample], key: &str) -> f64 {
    let n = samples.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let mean_x = samples.iter().map(|s| s.minute).sum::<f64>() / n;
    let mean_y = samples.iter().map(|s| s.get(key)).sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for sample in samples {
        num += (sample.minute - mean_x) * (sample.get(key) - mean_y);
        den += (sample.minute - mean_x).powi(2);
    }
    if den == 0.0 {
        0.0
    } else {
        num / den
    }
}

/// The in-process peers: each a propagation engine on the shared instance,
/// its store holding `fresh` messages the node has not seen, peered with
/// the node alone.
async fn start_peers(
    instance: &str,
    node_hash: [u8; 16],
    count: usize,
    fresh: usize,
    base: &Path,
) -> Vec<ReticulumNode> {
    let mut peers = Vec::new();
    let mut state = 0x243f_6a88_85a3_08d3u64;
    for index in 0..count {
        let mut store = MemoryPropagationStore::new(PEER_STORE_BYTES);
        let now = unix_secs();
        for n in 0..fresh {
            let seed = 1_000_000_000 + (index * fresh + n) as u64;
            let (transient_id, body) = message(seed, &mut state);
            store
                .append(&transient_id, now - 60, 0, &body)
                .expect("the peer's store has room");
        }
        let (engine, _events) = Engine::new(EngineConfig {
            identity: leviculum_std::generate_identity(),
            node_config: PropagationNodeConfig {
                stamp_cost: 0,
                sync_limit_kb: 256,
                ..PropagationNodeConfig::default()
            },
            store,
            announce_interval_secs: 600,
            announce_delay_secs: 0,
            announce_at_start: true,
            // The node as the one static peer and no autopeering: the
            // peers do not peer with each other, and still announce
            // themselves open, which a static-only node does not, so the
            // node peers with them.
            peering: PeeringConfig {
                static_peers: vec![node_hash],
                autopeer: false,
                ..PeeringConfig::default()
            },
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: PEER_STORE_BYTES,
            delivery_limit_kb: 1000,
        });
        let engine = engine.with_sync_interval(Duration::from_secs(5));
        let storage = base.join(format!("peer-{index}"));
        std::fs::create_dir_all(&storage).expect("peer storage");
        let mut node = lnpnd::node_builder(engine, storage)
            .connect_to_shared_instance(instance)
            .build()
            .await
            .expect("build a peer");
        node.start().await.expect("start a peer");
        peers.push(node);
    }
    peers
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a 20-minute soak (order 1515): run with `just pn-growth-soak`"]
async fn pn_tables_stay_flat_once_the_store_is_full() {
    let messages = knob("LNPND_SOAK_MESSAGES", 300_000) as usize;
    let peer_count = knob("LNPND_SOAK_PEERS", 14) as usize;
    let soak_secs = knob("LNPND_SOAK_SECS", 1200);
    let sample_secs = knob("LNPND_SOAK_SAMPLE_SECS", 60).max(1);
    let fresh = knob("LNPND_SOAK_PEER_MESSAGES", 10_000) as usize;
    let base = std::env::var_os("LNPND_SOAK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pn-growth-soak"));
    std::fs::create_dir_all(&base).expect("soak directory");
    let run = tempfile::tempdir_in(&base).expect("a run directory");
    eprintln!(
        "PN_SOAK dir={} messages={messages} peers={peer_count} secs={soak_secs} \
         sample_secs={sample_secs} fresh_per_peer={fresh}",
        run.path().display()
    );

    // The node's store, kept across runs under `base` so a rerun skips the
    // fill: lnpnd opens it from its index file in seconds.
    let store_cache = base.join(format!("store-{messages}"));
    let filled = Instant::now();
    let store_bytes = prefill(&store_cache, messages);
    eprintln!(
        "PN_SOAK store {messages} messages, {store_bytes} B, ready in {} s",
        filled.elapsed().as_secs()
    );
    let config_dir = run.path().join("node");
    let store_dir = config_dir.join("storage/lxmf/messagestore");
    std::fs::create_dir_all(store_dir.parent().expect("parent")).expect("store parent");
    let copied = Command::new("cp")
        .arg("-a")
        .arg(&store_cache)
        .arg(&store_dir)
        .status()
        .expect("cp runs");
    assert!(copied.success(), "copying the prefilled store failed");
    // The limit sits one megabyte above the prefill, so the first peers'
    // messages fill it and every later one evicts.
    let limit_mb = store_bytes / 1_000_000 + 1;
    std::fs::write(
        config_dir.join("config"),
        format!(
            "[propagation]\nenable_node = yes\nmessage_storage_limit = {limit_mb}\n\
             propagation_stamp_cost_target = 0\nautopeer = yes\nmax_peers = 20\n\
             [lxmf]\n[logging]\nloglevel = 4\n"
        ),
    )
    .expect("node config");
    let (identity, _) =
        lnpnd::identity::load_or_create(&config_dir.join("identity")).expect("node identity");
    let node_hash = propagation_hash_of(&identity);

    // The shared instance.
    let instance = format!("lnpnd-soak-{}", std::process::id());
    let mut daemon = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .storage_path(run.path().join("daemon"))
        .build()
        .await
        .expect("build the shared instance");
    daemon.start().await.expect("start the shared instance");

    // The node under test.
    let event_log = run.path().join("events.log");
    let mut cmd = Command::new(lnpnd_bin());
    cmd.arg("--config")
        .arg(&config_dir)
        .arg("--instance")
        .arg(&instance)
        .env("LEVICULUM_EVENT_LOG", &event_log)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = Reaped(spawn_supervised(cmd).expect("lnpnd runs"));
    let mut stderr = child.0.stderr.take().expect("piped stderr");
    let said = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&said);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut text = sink.lock().expect("stderr lock");
            // Only the head is kept: the ready line and the open profile.
            if text.len() < 64 * 1024 {
                text.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(120);
    while !said.lock().expect("stderr lock").contains(READY) {
        assert!(
            child.0.try_wait().expect("waitable").is_none(),
            "lnpnd exited before it was ready:\n{}",
            said.lock().expect("stderr lock")
        );
        assert!(
            Instant::now() < deadline,
            "lnpnd not ready in 120 s:\n{}",
            said.lock().expect("stderr lock")
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let _peers = start_peers(&instance, node_hash, peer_count, fresh, run.path()).await;

    // Every sample: SIGUSR1, then the census the next tick writes.
    let started = Instant::now();
    let mut samples = Vec::new();
    let mut read_from = 0usize;
    while started.elapsed() < Duration::from_secs(soak_secs) {
        tokio::time::sleep(Duration::from_secs(sample_secs)).await;
        assert!(
            child.0.try_wait().expect("waitable").is_none(),
            "lnpnd died during the soak"
        );
        let signalled = Command::new("kill")
            .arg("-USR1")
            .arg(child.0.id().to_string())
            .status()
            .expect("kill runs");
        assert!(signalled.success());
        let asked = Instant::now();
        loop {
            let (values, end) = last_census(&event_log, read_from);
            if let Some(values) = values {
                read_from = end;
                let sample = Sample {
                    minute: started.elapsed().as_secs_f64() / 60.0,
                    values,
                };
                eprintln!(
                    "PN_SOAK_SAMPLE minute={:.1} stored={} tables_kb={:.0} rss_kb={}",
                    sample.minute,
                    sample.get("stored"),
                    sample.get("tables") / 1000.0,
                    sample.get("rss_kb"),
                );
                samples.push(sample);
                break;
            }
            assert!(
                asked.elapsed() < Duration::from_secs(30),
                "no PN_CENSUS within 30 s of SIGUSR1"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    let log = std::fs::read_to_string(&event_log).unwrap_or_default();
    let count = |name: &str| log.lines().filter(|l| l.starts_with(name)).count();
    let accepted = log
        .lines()
        .filter(|l| l.starts_with("PN_ACCEPT ") && l.contains("dup=false"))
        .count();
    let syncs = |dir: &str| {
        log.lines()
            .filter(|l| l.starts_with("PN_SYNC ") && l.contains(&format!(" dir={dir} ")))
            .count()
    };
    eprintln!(
        "PN_SOAK events PN_ACCEPT(new)={accepted} PN_EVICT={} PN_SYNC(out)={} PN_SYNC(in)={} \
         PN_OFFER={} PN_PEER={}",
        count("PN_EVICT "),
        syncs("out"),
        syncs("in"),
        count("PN_OFFER "),
        count("PN_PEER "),
    );
    let mut peer_reasons: BTreeMap<String, usize> = BTreeMap::new();
    for line in log.lines().filter(|l| l.starts_with("PN_PEER ")) {
        let field = |key: &str| {
            line.split_whitespace()
                .find_map(|f| f.strip_prefix(key))
                .unwrap_or("?")
                .trim_matches('"')
                .to_string()
        };
        *peer_reasons
            .entry(format!("{}/{}", field("action="), field("reason=")))
            .or_default() += 1;
    }
    eprintln!("PN_SOAK peer table changes {peer_reasons:?}");
    drop(child);
    daemon.stop().await.ok();

    assert!(samples.len() >= 4, "too few samples to fit a slope");
    let first = samples.first().expect("a sample").clone();
    let last = samples.last().expect("a sample").clone();
    eprintln!("PN_SOAK first {}", census_line(&first));
    eprintln!("PN_SOAK last  {}", census_line(&last));

    // The full-store window: from the first census after the store's first
    // eviction, the second half of the run at the earliest. Full means full
    // in bytes: eviction takes the heaviest messages first, so the count can
    // still rise a little while the bytes stand at the limit, and the index,
    // which costs per message, rises with it. That growth is bounded by the
    // limit and is allowed for below; anything past it is not.
    let first_evict_t = log
        .lines()
        .find(|l| l.starts_with("PN_EVICT "))
        .and_then(|l| l.split_whitespace().find_map(|f| f.strip_prefix("t=")))
        .and_then(|t| t.parse::<f64>().ok());
    let evicted = count("PN_EVICT ");
    let full_from = first_evict_t
        .and_then(|t| samples.iter().position(|s| s.get("t") >= t))
        .unwrap_or(samples.len())
        .max(samples.len() / 2)
        .min(samples.len());
    let window = &samples[full_from..];
    assert!(
        evicted > 0 && window.len() >= 3,
        "the store never evicted, or too late to leave three samples ({} of them): \
         a flat table over a filling store proves nothing",
        window.len()
    );

    let span = (last.minute - first.minute).max(1e-9);
    let accepts_per_min = accepted as f64 / span;
    let stored_per_min = slope(window, "stored").max(0.0);
    let index_per_message = last.get("store_index") / last.get("stored").max(1.0);
    let index_allowance = stored_per_min * index_per_message / 1000.0;
    let evictions_per_min = evicted as f64 / span;
    let dup_allowance = (accepts_per_min * (DUP_BYTES_PER_ACCEPT + DUP_BYTES_PER_SYNCED_ORIGIN)
        + evictions_per_min * DUP_BYTES_PER_EVICTION)
        / 1000.0;
    let mut red = Vec::new();
    for table in lnpnd::census::COLUMNS.iter().chain(&["tables"]) {
        let kb_per_min = slope(window, table) / 1000.0;
        let allowed = FLAT_KB_PER_MIN
            + match *table {
                "dup" => dup_allowance,
                "store_index" => index_allowance,
                "tables" => dup_allowance + index_allowance,
                _ => 0.0,
            };
        println!(
            "PN_GROWTH table={table} kb_per_min={kb_per_min:.2} allowed={allowed:.2} \
             first_kb={:.1} last_kb={:.1}",
            first.get(table) / 1000.0,
            last.get(table) / 1000.0
        );
        if kb_per_min > allowed {
            red.push(format!("{table} {kb_per_min:.2} kB/min"));
        }
    }
    println!(
        "PN_GROWTH table=rss kb_per_min={:.2} first_kb={} last_kb={} (printed, not judged)",
        slope(window, "rss_kb"),
        first.get("rss_kb"),
        last.get("rss_kb")
    );
    println!(
        "PN_GROWTH window_from_minute={:.1} samples={} evicted={evicted} accepted={accepted} \
         stored_per_min={stored_per_min:.1} index_bytes_per_message={index_per_message:.0}",
        window[0].minute,
        window.len()
    );
    assert!(
        red.is_empty(),
        "tables still growing with the store full: {red:?}"
    );
}

fn census_line(sample: &Sample) -> String {
    let mut line = String::from("PN_CENSUS");
    for (key, value) in &sample.values {
        line.push_str(&format!(" {key}={value}"));
    }
    line
}
