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
//! **The verdict** (order 1558) judges a table by its design bound where
//! it has one, and by its slope only where it has none:
//!
//! - `dup` by count at every sample: the processed ids, departures and
//!   origins it holds against `dup_cap_ids`, the bound the role and the
//!   peering runtime hold them to (`leviculum_lxmf::dup_caps`, two
//!   processed ids and one departure per stored message, one origin per
//!   stored message). Below the bound it grows by design, and a soak of
//!   minutes does not reach it, so its slope is printed, not judged.
//! - `sync_in`, the sync batches in flight, as a bounded transient: its
//!   peak against [`SYNC_LIMIT_BYTES`] per peer, and whether it drained
//!   back to its floor at least once in the full-store window. A slope
//!   over samples that land inside or between batches says nothing.
//! - every other column by its least-squares slope over the full-store
//!   window, red above [`FLAT_KB_PER_MIN`]; `store_index` may rise by what
//!   the count still rising at the byte limit costs (eviction takes the
//!   heaviest messages first), the window's count slope times the index's
//!   bytes a message.
//! - `tables`, their sum, through its columns; a jump that is the origins'
//!   hash table doubling is named (`PN_GROWTH resize`).
//! - `transport`, the node's own transport tables (order 1586), by their
//!   ceilings at every sample: red when one has none
//!   (`transport_unbounded`) or holds more than its own
//!   (`transport_over`). An announcer on the shared instance sends
//!   [`ANNOUNCES_PER_MIN`] fresh destinations a minute, the public mesh
//!   miauhaus relays to its clients, so the tables reach their ceilings
//!   within the run; the last `PN_TRANSPORT` line, every table with its
//!   fill and ceiling, is printed at the end.
//!
//! - the allocator's retention (order 1592): the slope of `mem_kb`, the
//!   process's anonymous memory wherever it lies (`anon_rss_kb`, resident,
//!   plus `swap_kb`, VmSwap), less the slope of `tables`, what the census
//!   says the tables grew by design, over the full-store window against
//!   [`MEM_FLAT_KB_PER_MIN`]. Not the resident set: it carries file pages
//!   and drops when the kernel reclaims them, and on a host under memory
//!   pressure the kernel swaps the cold store index out and pages it back
//!   in, so its slope is the host's, not the node's (the order's first
//!   libc run sat at 10 MB resident under a 97 MB index). The
//!   tables can all be flat while the allocator keeps what it once had:
//!   the lnpnd on miauhaus (902bc129) grew about 70 MB an hour in fresh
//!   anonymous mappings no table owned, on musl's mallocng. Beside it every
//!   sample carries the anonymous writable mappings of `/proc/<pid>/maps`,
//!   the field census's view: `anon_maps` regions, `anon_kb` their bytes,
//!   `anon_big_maps` the regions above 8 MB; and `anon_rss_kb`, the
//!   resident anonymous memory of `/proc/<pid>/smaps_rollup`. The mapped
//!   bytes are address space: mimalloc reserves a 1 GiB arena up front, so
//!   under it `anon_kb` stands at about 1 GB from the first sample and only
//!   the resident figure moves.
//!
//! Red too when the store never filled: then the soak proved nothing.
//! Every sample's whole census is printed (`PN_SOAK_SAMPLE`), and on red
//! the node's event log is kept beside the store cache, named in the
//! failure.
//!
//! `just pn-growth-soak` runs the default build, mimalloc; `just
//! pn-growth-soak 1200 300000 14 system` the libc allocator, for the
//! comparison.
//!
//! Run it with `just pn-growth-soak` (release, 20 minutes, 300 000
//! messages). Knobs: `LNPND_SOAK_MESSAGES`, `LNPND_SOAK_PEERS`,
//! `LNPND_SOAK_ANNOUNCES_PER_MIN`,
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

/// The allocator's retention allowed over the full-store window: anonymous
/// memory's slope less the tables' (order 1592), in kB a minute. The
/// field's 70 MB an hour is 1 170 kB a minute; what the soak measured on
/// both allocators is in the order's report.
const MEM_FLAT_KB_PER_MIN: f64 = 300.0;

/// A mapping above this is one the field census lists by size.
const BIG_MAP_BYTES: u64 = 8 * 1024 * 1024;

/// One peer's store: room for its fresh messages (10 000 at most 600 bytes)
/// and for what the node forwards it, which it evicts as it goes.
const PEER_STORE_BYTES: u64 = 8_000_000;

/// The node's per-sync limit, lnpnd's default (`DEFAULT_SYNC_LIMIT_KB`,
/// lxmd's `SYNC_LIMIT`, `lnpnd/src/main.rs`): what one peer's sync batch
/// may hold, the `sync_in` bound per peer.
const SYNC_LIMIT_BYTES: f64 = 256.0 * 40.0 * 1000.0;

/// Fresh destinations the announcer sends a minute (order 1586): 20
/// minutes of it is 8 000, four times the local client profile's 2 048
/// identities and destinations (order 1585).
const ANNOUNCES_PER_MIN: u64 = 400;

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
/// The anonymous writable mappings of process `pid`, the field census's
/// columns: region count, their bytes in kB, and the regions above
/// [`BIG_MAP_BYTES`].
fn anon_maps(pid: u32) -> [(String, f64); 5] {
    let text = std::fs::read_to_string(format!("/proc/{pid}/maps")).unwrap_or_default();
    let regions = leviculum_std::proc_memory::parse_anon_writable_maps(&text);
    let bytes: u64 = regions.iter().map(|r| r.size()).sum();
    let big = regions.iter().filter(|r| r.size() > BIG_MAP_BYTES).count();
    // `Anonymous:` in kB, the resident anonymous pages of every mapping.
    let rollup = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).unwrap_or_default();
    let anon_rss_kb = rollup
        .lines()
        .find_map(|line| line.strip_prefix("Anonymous:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<f64>().ok())
        .unwrap_or(0.0);
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let swap_kb = leviculum_std::proc_memory::parse_status(&status)
        .swap_bytes
        .map_or(0.0, |bytes| (bytes / 1024) as f64);
    [
        ("swap_kb".to_string(), swap_kb),
        ("anon_maps".to_string(), regions.len() as f64),
        ("anon_kb".to_string(), (bytes / 1024) as f64),
        ("anon_big_maps".to_string(), big as f64),
        ("anon_rss_kb".to_string(), anon_rss_kb),
    ]
}

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

/// A client on the shared instance announcing `per_min` fresh destinations
/// a minute, each under a new identity, until `stop` is set: the mesh's
/// announces as the instance relays them to its local clients (order
/// 1586). Returns how many it announced.
fn start_announcer(
    instance: &str,
    storage: PathBuf,
    per_min: u64,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> tokio::task::JoinHandle<usize> {
    let instance = instance.to_string();
    tokio::spawn(async move {
        use std::sync::atomic::Ordering;
        std::fs::create_dir_all(&storage).expect("announcer storage");
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(false)
            .connect_to_shared_instance(&instance)
            .storage_path(storage)
            .without_events()
            .build()
            .await
            .expect("build the announcer");
        node.start().await.expect("start the announcer");
        let mut sent = 0usize;
        if per_min == 0 {
            node.stop().await.ok();
            return sent;
        }
        let spacing = Duration::from_millis(60_000 / per_min);
        while !stop.load(Ordering::Relaxed) {
            let destination = Destination::new(
                Some(leviculum_std::generate_identity()),
                leviculum_core::Direction::In,
                leviculum_core::DestinationType::Single,
                "soak",
                &["announcer"],
            )
            .expect("a destination");
            let hash = *destination.hash();
            node.register_destination(destination);
            if node.announce_destination(&hash, None).await.is_ok() {
                sent += 1;
            }
            tokio::time::sleep(spacing).await;
        }
        node.stop().await.ok();
        sent
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a 20-minute soak (order 1515): run with `just pn-growth-soak`"]
async fn pn_tables_stay_flat_once_the_store_is_full() {
    let messages = knob("LNPND_SOAK_MESSAGES", 300_000) as usize;
    let peer_count = knob("LNPND_SOAK_PEERS", 14) as usize;
    let soak_secs = knob("LNPND_SOAK_SECS", 1200);
    let sample_secs = knob("LNPND_SOAK_SAMPLE_SECS", 60).max(1);
    let fresh = knob("LNPND_SOAK_PEER_MESSAGES", 10_000) as usize;
    let announces_per_min = knob("LNPND_SOAK_ANNOUNCES_PER_MIN", ANNOUNCES_PER_MIN);
    let base = std::env::var_os("LNPND_SOAK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pn-growth-soak"));
    std::fs::create_dir_all(&base).expect("soak directory");
    let run = tempfile::tempdir_in(&base).expect("a run directory");
    eprintln!(
        "PN_SOAK dir={} messages={messages} peers={peer_count} secs={soak_secs} \
         sample_secs={sample_secs} fresh_per_peer={fresh} announces_per_min={announces_per_min}",
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
    let stop_announcer = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let announcer = start_announcer(
        &instance,
        run.path().join("announcer"),
        announces_per_min,
        Arc::clone(&stop_announcer),
    );

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
            if let Some(mut values) = values {
                read_from = end;
                values.extend(anon_maps(child.0.id()));
                let mem_kb = values.get("anon_rss_kb").copied().unwrap_or(0.0)
                    + values.get("swap_kb").copied().unwrap_or(0.0);
                values.insert("mem_kb".to_string(), mem_kb);
                let sample = Sample {
                    minute: started.elapsed().as_secs_f64() / 60.0,
                    values,
                };
                // The whole census, every column, so a red soak can be
                // read sample by sample after the fact (order 1558).
                eprintln!(
                    "PN_SOAK_SAMPLE minute={:.1} {}",
                    sample.minute,
                    census_line(&sample)
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
    stop_announcer.store(true, std::sync::atomic::Ordering::Relaxed);
    let announced = announcer.await.unwrap_or(0);
    let last_transport = log
        .lines()
        .rev()
        .find(|l| l.starts_with("PN_TRANSPORT "))
        .unwrap_or("PN_TRANSPORT none")
        .to_string();
    eprintln!("PN_SOAK announced={announced} last {last_transport}");
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
    if evicted == 0 || window.len() < 3 {
        panic!(
            "the store never evicted, or too late to leave three samples ({} of them): \
             a flat table over a filling store proves nothing; events.log {}",
            window.len(),
            keep_event_log(&event_log, &base)
        );
    }

    let span = (last.minute - first.minute).max(1e-9);
    let stored_per_min = slope(window, "stored").max(0.0);
    let index_per_message = last.get("store_index") / last.get("stored").max(1.0);
    let index_allowance = stored_per_min * index_per_message / 1000.0;
    let mut red = Vec::new();

    // A table's hash map doubling shows as one jump between two samples;
    // name it, so the jump is not read as growth.
    for pair in samples.windows(2) {
        let (before, after) = (pair[0].get("origin_slots"), pair[1].get("origin_slots"));
        if after > before && before > 0.0 {
            println!(
                "PN_GROWTH resize table=origins minute={:.1} slots={before}->{after} \
                 dup_jump_kb={:.1}",
                pair[1].minute,
                (pair[1].get("dup") - pair[0].get("dup")) / 1000.0
            );
        }
    }

    // `dup` is judged by its design bound, in ids, at every sample
    // (`leviculum_lxmf::dup_caps`, order 1558), not by a slope: below the
    // bound it grows by design, and a soak of minutes never reaches it.
    let dup_held =
        |s: &Sample| s.get("processed_ids") + s.get("departed_ids") + s.get("origin_ids");
    let fullest = samples
        .iter()
        .max_by(|a, b| {
            let fill = |s: &Sample| dup_held(s) / s.get("dup_cap_ids").max(1.0);
            fill(a).total_cmp(&fill(b))
        })
        .expect("a sample");
    for sample in &samples {
        let held =
            sample.get("processed_ids") + sample.get("departed_ids") + sample.get("origin_ids");
        if held > sample.get("dup_cap_ids") {
            red.push(format!(
                "dup {held} ids over its bound {} at minute {:.1}",
                sample.get("dup_cap_ids"),
                sample.minute
            ));
            break;
        }
    }
    println!(
        "PN_GROWTH table=dup judged=bound fullest_held_ids={} cap_ids={} at_minute={:.1} \
         processed={} departed={} origins={} first_kb={:.1} last_kb={:.1} kb_per_min={:.2}",
        dup_held(fullest),
        fullest.get("dup_cap_ids"),
        fullest.minute,
        last.get("processed_ids"),
        last.get("departed_ids"),
        last.get("origin_ids"),
        first.get("dup") / 1000.0,
        last.get("dup") / 1000.0,
        slope(window, "dup") / 1000.0,
    );

    // The node's transport tables (order 1586), by their ceilings at every
    // sample: none may lack one, none may hold more than its own. Below
    // the ceilings they grow by design, with every destination announced.
    let transport_breach = samples
        .iter()
        .find(|s| s.get("transport_unbounded") > 0.0 || s.get("transport_over") > 0.0);
    let at_ceiling = |name: &str| {
        last_transport
            .split_whitespace()
            .find_map(|f| f.strip_prefix("tables="))
            .and_then(|tables| {
                tables
                    .split(',')
                    .find_map(|t| t.strip_prefix(&format!("{name}:")).map(str::to_string))
            })
            .unwrap_or_else(|| "?".to_string())
    };
    println!(
        "PN_GROWTH table=transport judged=bound announced={announced} first_kb={:.1} \
         last_kb={:.1} kb_per_min={:.2} entries={} unbounded={} over={} \
         known_identities={} path_table={} announce_cache={} known_dest_entries={}",
        first.get("transport") / 1000.0,
        last.get("transport") / 1000.0,
        slope(window, "transport") / 1000.0,
        last.get("transport_entries"),
        last.get("transport_unbounded"),
        last.get("transport_over"),
        at_ceiling("known_identities"),
        at_ceiling("path_table"),
        at_ceiling("announce_cache"),
        at_ceiling("known_dest_entries"),
    );
    if let Some(sample) = transport_breach {
        red.push(format!(
            "transport: {} table(s) without a ceiling, {} over theirs at minute {:.1}",
            sample.get("transport_unbounded"),
            sample.get("transport_over"),
            sample.minute
        ));
    }

    // `sync_in` holds the sync batches in flight, each at most the node's
    // per-sync limit, one per peer at most: a bounded transient. It is
    // judged by its peak against that bound and by its return to the floor
    // at the end, not by a slope over samples that land in or between
    // batches.
    let sync_in_bound = SYNC_LIMIT_BYTES * peer_count as f64;
    let sync_in_peak = samples.iter().map(|s| s.get("sync_in")).fold(0.0, f64::max);
    let sync_in_floor = samples
        .iter()
        .map(|s| s.get("sync_in"))
        .fold(f64::MAX, f64::min);
    // Back to the floor: the queue drained at least once in the full-store
    // window. A queue that grows never does; the last sample alone may
    // land inside a batch.
    let sync_in_drained = window
        .iter()
        .any(|s| s.get("sync_in") <= sync_in_floor + FLAT_KB_PER_MIN * 1000.0);
    println!(
        "PN_GROWTH table=sync_in judged=transient peak_kb={:.1} bound_kb={:.1} floor_kb={:.1} \
         last_kb={:.1} drained={sync_in_drained} kb_per_min={:.2}",
        sync_in_peak / 1000.0,
        sync_in_bound / 1000.0,
        sync_in_floor / 1000.0,
        last.get("sync_in") / 1000.0,
        slope(window, "sync_in") / 1000.0,
    );
    if sync_in_peak > sync_in_bound {
        red.push(format!(
            "sync_in peak {:.1} kB over its bound {:.1} kB",
            sync_in_peak / 1000.0,
            sync_in_bound / 1000.0
        ));
    }
    if !sync_in_drained {
        red.push(format!(
            "sync_in never back at its floor {:.1} kB in the full-store window",
            sync_in_floor / 1000.0
        ));
    }

    // Every other column has no bound but time, and is judged by its slope
    // over the full-store window; `store_index` may rise by what the
    // count still rising at the byte limit costs.
    for table in lnpnd::census::COLUMNS {
        if matches!(*table, "dup" | "sync_in") {
            continue;
        }
        let kb_per_min = slope(window, table) / 1000.0;
        let allowed = FLAT_KB_PER_MIN
            + if *table == "store_index" {
                index_allowance
            } else {
                0.0
            };
        println!(
            "PN_GROWTH table={table} judged=slope kb_per_min={kb_per_min:.2} allowed={allowed:.2} \
             first_kb={:.1} last_kb={:.1}",
            first.get(table) / 1000.0,
            last.get(table) / 1000.0
        );
        if kb_per_min > allowed {
            red.push(format!("{table} {kb_per_min:.2} kB/min"));
        }
    }
    println!(
        "PN_GROWTH table=tables judged=by_column kb_per_min={:.2} first_kb={:.1} last_kb={:.1}",
        slope(window, "tables") / 1000.0,
        first.get("tables") / 1000.0,
        last.get("tables") / 1000.0
    );
    // Resident plus swap by its slope (order 1592): flat tables under a
    // growing process are the allocator keeping what it had.
    let mem_per_min = slope(window, "mem_kb");
    let tables_per_min = slope(window, "tables") / 1000.0;
    let retained_per_min = mem_per_min - tables_per_min;
    println!(
        "PN_GROWTH table=allocator judged=slope retained_kb_per_min={retained_per_min:.2} \
         allowed={MEM_FLAT_KB_PER_MIN:.2} anon_plus_swap_kb_per_min={mem_per_min:.2} \
         tables_kb_per_min={tables_per_min:.2} first_kb={} last_kb={} window_first_kb={} \
         rss_kb={}->{} rss_kb_per_min={:.2} swap_kb={}->{} \
         anon_maps={}->{} anon_kb={}->{} anon_kb_per_min={:.2} anon_big_maps={} \
         anon_rss_kb={}->{} anon_rss_kb_per_min={:.2}",
        first.get("mem_kb"),
        last.get("mem_kb"),
        window[0].get("mem_kb"),
        first.get("rss_kb"),
        last.get("rss_kb"),
        slope(window, "rss_kb"),
        first.get("swap_kb"),
        last.get("swap_kb"),
        first.get("anon_maps"),
        last.get("anon_maps"),
        first.get("anon_kb"),
        last.get("anon_kb"),
        slope(window, "anon_kb"),
        last.get("anon_big_maps"),
        first.get("anon_rss_kb"),
        last.get("anon_rss_kb"),
        slope(window, "anon_rss_kb"),
    );
    if retained_per_min > MEM_FLAT_KB_PER_MIN {
        red.push(format!(
            "allocator retains {retained_per_min:.2} kB/min beyond the tables"
        ));
    }
    println!(
        "PN_GROWTH window_from_minute={:.1} samples={} evicted={evicted} accepted={accepted} \
         accepts_per_min={:.0} stored_per_min={stored_per_min:.1} \
         index_bytes_per_message={index_per_message:.0}",
        window[0].minute,
        window.len(),
        accepted as f64 / span,
    );
    if !red.is_empty() {
        panic!(
            "tables still growing with the store full: {red:?}; events.log {}",
            keep_event_log(&event_log, &base)
        );
    }
}

/// Keep the node's event log beside the store cache for the reading after
/// the fact: the run directory goes with the test. Says where it went.
fn keep_event_log(event_log: &Path, base: &Path) -> String {
    let kept = base.join(format!("events-red-{}.log", unix_secs()));
    match std::fs::copy(event_log, &kept) {
        Ok(_) => kept.display().to_string(),
        Err(error) => format!("could not be kept ({error})"),
    }
}

fn census_line(sample: &Sample) -> String {
    let mut line = String::from("PN_CENSUS");
    for (key, value) in &sample.values {
        line.push_str(&format!(" {key}={value}"));
    }
    line
}
