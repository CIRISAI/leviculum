//! Hardened rnstatus/lnstatus drop-in parity suite (Codeberg #86 client render,
//! #67 daemon stats, #87 ingress hold, #88 blackhole).
//!
//! ## The 2x2 matrix
//!
//! Two REAL daemons are spawned as subprocesses from the SAME Python-style
//! config template (only instance name and TCP port differ), each with a
//! pre-seeded transport identity so the shared-instance RPC authkey is known
//! up front:
//!
//! ```text
//!             |  lnsd (our binary)      |  rnsd (vendor Python)
//!   ----------+-------------------------+------------------------
//!   lnstatus  |  full Rust stack (sane) |  drop-in client vs rnsd
//!   rnstatus  |  drop-in client vs lnsd |  reference baseline
//! ```
//!
//! Both daemons receive BYTE-IDENTICAL controlled traffic (the frames are
//! built once and replayed into both TCP servers with identical pacing), so
//! comparing their stats afterwards is a valid A/B:
//!
//!   * lnstatus vs rnstatus against the SAME daemon = CLIENT render parity
//!     (#86): the two clients read the same frozen daemon state, so their
//!     -j JSON must be structurally identical and their text output byte
//!     identical after normalizing only the documented volatile parts.
//!   * rnstatus against lnsd vs rnstatus against rnsd = DAEMON stats parity
//!     (#67): the same reference client reads both daemons after identical
//!     traffic; the per-interface stats of the traffic-bearing interface
//!     must agree field by field.
//!
//! ## Controlled traffic script (identical for both daemons)
//!
//!   1. STEADY: 6 announces of distinct identities, paced 1.5 s apart (well
//!      below the 3 Hz new-interface ingress threshold) -> 6 paths, real
//!      incoming_announce_frequency.
//!   2. PATH REQUESTS: 4 path requests (3 known + 1 unknown destination),
//!      paced 1.5 s -> real incoming_pr_frequency, outgoing path responses.
//!   3. BURST: after a >10 s quiet gap (so earlier samples are past the
//!      AR_FREQ_DECAY window, see PRE_BURST_DECAY_GAP), 20 fresh-identity
//!      announces at 25 Hz -> ingress burst activates on both stacks
//!      (Interface.py IC_BURST_FREQ_NEW = 3 Hz, IC_BURST_MIN_SAMPLES = 6),
//!      excess announces are HELD. Sampled DURING the burst (inside the 15 s
//!      IC_BURST_PENALTY window, before the first release) and again AFTER
//!      the drain (held back to 0). Covers #87.
//!   4. BLACKHOLE: the victim identity is blackholed via the REAL vendor
//!      rnpath.py -B against each daemon's own RPC; its re-announce is
//!      dropped on both stacks while a sentinel announce passes; -U lifts
//!      the blackhole and the victim announces normally again. Covers #88.
//!   5. FREEZE: traffic quiesced, then both daemons are polled until every
//!      volatile stat has settled (see below) before the comparison matrix.
//!
//! ## Volume guard (A/B discipline)
//!
//! Before ANY cross-daemon comparison the suite counts event volumes on both
//! daemons and fails loudly on mismatch:
//!
//!   * learned path SET is compared EXACTLY (same destination hashes on both
//!     daemons, equal to the script's expectation), not "within a few
//!     percent" - the traffic is byte-identical, so any missing path is a
//!     real processing divergence;
//!   * per-interface rxb/txb are compared within 5% across daemons: the
//!     frames are identical, but the two stacks count at different framing
//!     layers (lnsd counts on-wire HDLC bytes, Python counts de-framed
//!     payload bytes; measured ~3% delta), so exact equality is impossible
//!     by design and the tolerance covers exactly that layer difference.
//!
//! ## Freeze discipline (why the frozen comparisons cannot flake)
//!
//! Python's announce/PR frequency read (Interface.incoming_announce_frequency)
//! returns n/span over a timestamp deque and POPS one decayed sample per call
//! once the span exceeds 10 s; when the deque is down to 2 samples it returns
//! exactly 0. Our implementation mirrors that. The freeze loop polls
//! interface_stats (1 s interval) until, on BOTH daemons, every frequency
//! reads exactly 0, rxs/txs read exactly 0, held_announces is 0 and no burst
//! flag is set. The polling itself drains the deques, so the loop always
//! converges, and after it the ONLY values that can differ between two reads
//! of the same daemon are transport_uptime and (on rnsd) the process rss.
//! Every other field is bit-stable, which is what makes EXACT comparison
//! possible instead of "almost".
//!
//! ## Known, pinned cross-stack divergences (asserted, not ignored)
//!
//! The daemon parity comparison does not silently normalize structural gaps;
//! it asserts them EXACTLY so any drift (vendor bump, our refactor) fails:
//!
//!   * interface list shape: rnsd lists Shared Instance + TCP listener +
//!     spawned connection (+ one LocalClientInterface while rnstatus itself
//!     is connected); lnsd lists only the spawned TCP connection;
//!   * per-interface key set: ours adds announce_queue/peers, Python adds
//!     autoconnect_source (+ parent_interface_name/hash on spawned entries);
//!   * top-level rss: Python reports the daemon process RSS, ours reports
//!     null;
//!   * interface identity fields (name/short_name/hash/type string) encode
//!     per-daemon naming and are compared per daemon, never across.
//!
//! Marked #[ignore]: spawns Python daemons and tooling. Run with:
//!
//! ```sh
//! cargo build -p leviculum-cli --bin lnsd --bin lnstatus
//! cargo test --package leviculum-std --test rnsd_interop \
//!     status_parity -- --include-ignored --test-threads=1 --nocapture
//! ```

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use rand_core::OsRng;
use regex::Regex;
use serde_json::Value;
use tokio::net::TcpStream;

use leviculum_core::constants::{MTU, TRUNCATED_HASHBYTES};
use leviculum_core::identity::Identity;
use leviculum_core::{Destination, DestinationHash, DestinationType, Direction};

use crate::common::{build_path_request_raw_with_tag, init_tracing, now_ms, send_framed};
use crate::harness::find_available_ports;
use crate::rpc_interop_tests::{run_python_tool, RNPATH_PY};

const RNSD_PY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../reference/Reticulum/RNS/Utilities/rnsd.py"
);
const RNSTATUS_PY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../reference/Reticulum/RNS/Utilities/rnstatus.py"
);

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

// =========================================================================
// Binary discovery (real lnsd / lnstatus binaries, freshness-checked)
// =========================================================================

/// Directory that holds the workspace binaries for the active target: the
/// test executable lives in `<target>/<triple>/debug/deps/`, the CLI
/// binaries one level up.
fn bin_dir() -> PathBuf {
    let mut dir = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("deps dir")
        .to_path_buf();
    dir.pop();
    dir
}

/// Newest mtime of any .rs file under `dir` (recursive). Used to reject
/// stale CLI binaries: a binary older than the newest source would silently
/// test yesterday's code (the #53 stale-binary lesson).
fn newest_source_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let m = if path.is_dir() {
            newest_source_mtime(&path)
        } else if path.extension().is_some_and(|e| e == "rs") {
            entry.metadata().ok().and_then(|m| m.modified().ok())
        } else {
            None
        };
        if let Some(m) = m {
            if newest.map(|n| m > n).unwrap_or(true) {
                newest = Some(m);
            }
        }
    }
    newest
}

/// Resolve a prebuilt CLI binary, panicking with a build instruction when it
/// is missing or older than the CLI crate's sources.
fn cli_bin(name: &str) -> PathBuf {
    let bin = bin_dir().join(name);
    let build_hint = format!(
        "run `cargo build -p leviculum-cli --bin lnsd --bin lnstatus` first \
         (expected at {})",
        bin.display()
    );
    let meta = match std::fs::metadata(&bin) {
        Ok(m) => m,
        Err(_) => panic!("{name} binary not found; {build_hint}"),
    };
    let cli_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../leviculum-cli/src");
    if let (Ok(bin_mtime), Some(src_mtime)) = (meta.modified(), newest_source_mtime(&cli_src)) {
        assert!(
            bin_mtime >= src_mtime,
            "{name} binary is STALE (older than leviculum-cli sources); {build_hint}"
        );
    }
    bin
}

// =========================================================================
// Daemon under test
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stack {
    Lnsd,
    Rnsd,
}

impl Stack {
    fn label(self) -> &'static str {
        match self {
            Stack::Lnsd => "lnsd",
            Stack::Rnsd => "rnsd",
        }
    }
}

/// A real daemon subprocess (our lnsd binary or the vendor Python rnsd),
/// started from a shared config template with a pre-seeded transport
/// identity. Both stacks parse the same Python-style INI config, listen on
/// one TCPServerInterface and expose the shared-instance RPC socket
/// `\0rns/<instance>/rpc` with authkey SHA256(transport_identity bytes).
struct ParityDaemon {
    stack: Stack,
    config_dir: PathBuf,
    instance_name: String,
    authkey: [u8; 32],
    /// Listen ports of the configured TCP server interfaces. `tcp_port` is the
    /// first; the multi-interface parity test uses all of them.
    tcp_ports: Vec<u16>,
    tcp_port: u16,
    child: Child,
}

impl ParityDaemon {
    async fn start(stack: Stack) -> ParityDaemon {
        Self::start_n(stack, 1).await
    }

    /// Start a daemon with `n_interfaces` TCP server interfaces (1 or 2). The
    /// single-interface form is what the 2x2 matrix uses; the two-interface
    /// form backs the multi-interface sort/`-a` parity test, where more than
    /// one visible interface is required for ordering to be observable.
    async fn start_n(stack: Stack, n_interfaces: usize) -> ParityDaemon {
        assert!(
            (1..=2).contains(&n_interfaces),
            "parity daemon supports 1 or 2 interfaces"
        );
        let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let instance_name = format!(
            "parity_{}_{}_{}",
            stack.label(),
            std::process::id(),
            test_id
        );
        // The harness allocator hands out a minimum of two ports; the
        // single-interface form uses only the first.
        let (ports, _alloc) = find_available_ports::<2>().await.expect("allocate port");
        let tcp_ports: Vec<u16> = ports[..n_interfaces].to_vec();
        let tcp_port = tcp_ports[0];

        let config_dir = std::env::temp_dir().join(format!("status_parity_{instance_name}"));
        let _ = std::fs::remove_dir_all(&config_dir);
        std::fs::create_dir_all(config_dir.join("storage")).expect("create config dir");

        // Pre-seed the transport identity so the RPC authkey
        // (SHA256 of the 64 private key bytes) is known before startup.
        let identity = Identity::generate(&mut OsRng);
        let identity_bytes = identity.private_key_bytes().expect("private key bytes");
        std::fs::write(
            config_dir.join("storage").join("transport_identity"),
            identity_bytes,
        )
        .expect("write transport_identity");
        use sha2::Digest;
        let mut authkey = [0u8; 32];
        authkey.copy_from_slice(&sha2::Sha256::digest(identity_bytes));

        // One config template for BOTH stacks: transport node, shared
        // instance, one or two TCP servers. No `mode` override so both stacks
        // report the default MODE_FULL.
        let mut iface_blocks = String::new();
        for (idx, port) in tcp_ports.iter().enumerate() {
            iface_blocks.push_str(&format!(
                "\x20 [[Parity TCP Server {idx}]]\n\
                 \x20   type = TCPServerInterface\n\
                 \x20   enabled = yes\n\
                 \x20   listen_ip = 127.0.0.1\n\
                 \x20   listen_port = {port}\n"
            ));
        }
        let config = format!(
            "[reticulum]\n\
             \x20 enable_transport = yes\n\
             \x20 share_instance = yes\n\
             \x20 instance_name = {instance_name}\n\
             \x20 panic_on_interface_error = no\n\
             \n\
             [logging]\n\
             \x20 loglevel = 3\n\
             \n\
             [interfaces]\n\
             {iface_blocks}"
        );
        std::fs::write(config_dir.join("config"), config).expect("write config");

        let log = std::fs::File::create(config_dir.join("daemon.log")).expect("create log");
        let log_err = log.try_clone().expect("clone log handle");
        let child = match stack {
            Stack::Lnsd => Command::new(cli_bin("lnsd"))
                .arg("--config")
                .arg(&config_dir)
                .stdout(Stdio::from(log))
                .stderr(Stdio::from(log_err))
                .spawn()
                .expect("spawn lnsd"),
            Stack::Rnsd => Command::new("python3")
                .arg(RNSD_PY)
                .arg("--config")
                .arg(&config_dir)
                .env(
                    "PYTHONPATH",
                    concat!(env!("CARGO_MANIFEST_DIR"), "/../reference/Reticulum"),
                )
                .stdout(Stdio::from(log))
                .stderr(Stdio::from(log_err))
                .spawn()
                .expect("spawn rnsd"),
        };

        let daemon = ParityDaemon {
            stack,
            config_dir,
            instance_name,
            authkey,
            tcp_ports,
            tcp_port,
            child,
        };

        // Readiness: the RPC socket answering interface_stats is the real
        // "daemon is up" condition for every consumer this suite uses (both
        // clients and the introspection polls all go through it).
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if daemon.try_stats().await.is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{} did not become RPC-ready within 30 s (log: {})",
                daemon.stack.label(),
                daemon.config_dir.join("daemon.log").display()
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        daemon
    }

    async fn try_stats(&self) -> Option<Value> {
        leviculum_std::rpc_query(&self.instance_name, &self.authkey, "interface_stats")
            .await
            .ok()
    }

    /// interface_stats via the same shared-instance RPC the clients use.
    async fn stats(&self) -> Value {
        self.try_stats()
            .await
            .unwrap_or_else(|| panic!("interface_stats RPC failed on {}", self.stack.label()))
    }

    /// Set of learned destination hashes (hex) from the path_table RPC.
    async fn path_hashes(&self) -> BTreeSet<String> {
        let table = leviculum_std::rpc_query(&self.instance_name, &self.authkey, "path_table")
            .await
            .unwrap_or_else(|e| panic!("path_table RPC failed on {}: {e}", self.stack.label()));
        table
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| r.get("hash").and_then(|h| h.as_str()))
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn blackholed(&self) -> Value {
        leviculum_std::rpc_query(&self.instance_name, &self.authkey, "blackholed_identities")
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "blackholed_identities RPC failed on {}: {e}",
                    self.stack.label()
                )
            })
    }
}

impl Drop for ParityDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// =========================================================================
// Status clients (the actual matrix axes)
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusClient {
    Rnstatus,
    Lnstatus,
}

impl StatusClient {
    fn label(self) -> &'static str {
        match self {
            StatusClient::Rnstatus => "rnstatus",
            StatusClient::Lnstatus => "lnstatus",
        }
    }
}

/// Run one status client against one daemon (drop-in style: the client gets
/// the DAEMON's own config directory, exactly like an operator would) and
/// return stdout. Panics with full stderr on a non-zero exit.
async fn run_status(client: StatusClient, daemon: &ParityDaemon, args: &[&str]) -> String {
    let output = match client {
        StatusClient::Rnstatus => run_python_tool(RNSTATUS_PY, args, &daemon.config_dir).await,
        StatusClient::Lnstatus => tokio::process::Command::new(cli_bin("lnstatus"))
            .arg("--config")
            .arg(&daemon.config_dir)
            .args(args)
            .output()
            .await
            .expect("spawn lnstatus"),
    };
    assert!(
        output.status.success(),
        "{} {:?} against {} exited with {:?}\nstdout:\n{}\nstderr:\n{}",
        client.label(),
        args,
        daemon.stack.label(),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// rnstatus connects to the daemon as a full shared-instance client, so on
/// rnsd its own LocalClientInterface appears in the stats while it runs
/// (observer footprint). Wait until that footprint is gone before the next
/// sample, so sequential samples always read the same client-free state.
async fn wait_observer_gone(daemon: &ParityDaemon) {
    if daemon.stack != Stack::Rnsd {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let stats = daemon.stats().await;
        let leftovers = ifaces(&stats)
            .iter()
            .filter(|i| i["type"] == "LocalClientInterface")
            .count();
        if leftovers == 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "rnsd still lists {leftovers} LocalClientInterface entries 15 s after the client exited"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// =========================================================================
// Controlled traffic script
// =========================================================================

/// Steady-phase announce count. 6 paced announces both create paths and fill
/// the announce deque past IC_DEQUE_MIN_SAMPLE so the frequency reads real.
const STEADY_N: usize = 6;
/// Burst announce count: comfortably past the ~6 announces the limiter needs
/// to detect the burst (IC_BURST_MIN_SAMPLES plus decayed-sample pops), far
/// below the 256 held cap, and small enough that the post-penalty drain (15 s
/// + 5 s per held announce) finishes inside the drain deadline.
const BURST_N: usize = 20;
/// Pacing that keeps a phase below the 3 Hz new-interface ingress threshold.
const SLOW_PACE: Duration = Duration::from_millis(1500);
/// Burst pacing: ~25 Hz, far above the 3 Hz threshold.
const BURST_PACE: Duration = Duration::from_millis(40);
/// Quiet gap before the burst, sized past the 10 s frequency decay window
/// (Interface.py AR_FREQ_DECAY). Both stacks compute the announce frequency
/// as n/(now - oldest) over a 48-sample deque and pop only ONE decayed
/// sample per read once the span exceeds 10 s. Without the gap the
/// steady-phase samples pin the span at ~9 s (not yet decay-eligible) and
/// [`BURST_N`] frames only reach ~2.3 Hz - NEITHER stack activates during
/// the burst. After the gap every pre-burst sample is decay-eligible, the
/// burst's per-frame frequency reads pop them one per frame, and the
/// threshold is crossed at (nearly) the same frame on both stacks.
const PRE_BURST_DECAY_GAP: Duration = Duration::from_secs(12);

/// Build a signed announce frame for a caller-owned identity. Every call
/// yields a fresh packet hash (random announce blob), so re-announces are
/// not deduplicated; the same returned bytes are replayed into BOTH daemons.
fn build_announce_for(identity: &Identity, aspect: &str) -> (Vec<u8>, DestinationHash) {
    let mut dest = Destination::new(
        Some(identity.clone()),
        Direction::In,
        DestinationType::Single,
        "status_parity",
        &[aspect],
    )
    .expect("create destination");
    let packet = dest
        .announce(Some(b"parity"), &mut OsRng, now_ms())
        .expect("create announce");
    let mut raw = [0u8; MTU];
    let size = packet.pack(&mut raw).expect("pack announce");
    (raw[..size].to_vec(), *dest.hash())
}

/// The full pre-built traffic script. All frames are generated ONCE so both
/// daemons receive byte-identical traffic (a hard prerequisite for the
/// volume guard and every cross-daemon comparison).
struct TrafficScript {
    steady: Vec<(Vec<u8>, DestinationHash)>,
    path_requests: Vec<Vec<u8>>,
    burst: Vec<(Vec<u8>, DestinationHash)>,
    victim_identity_hex: String,
    victim_hash: DestinationHash,
    victim_reannounce: Vec<u8>,
    victim_after_unblackhole: Vec<u8>,
    sentinel: (Vec<u8>, DestinationHash),
}

fn build_script() -> TrafficScript {
    let steady_ids: Vec<Identity> = (0..STEADY_N)
        .map(|_| Identity::generate(&mut OsRng))
        .collect();
    let steady: Vec<_> = steady_ids
        .iter()
        .enumerate()
        .map(|(i, id)| build_announce_for(id, &format!("s{i}")))
        .collect();

    // Path requests with fixed tags: byte-identical frames on both daemons.
    // Three for known destinations (the daemon answers with a path response)
    // and one for an unknown destination (ignored, still counts as an
    // incoming path request).
    let mut path_requests = Vec::new();
    for (i, target) in [
        steady[1].1,
        steady[2].1,
        DestinationHash::new([0xEEu8; TRUNCATED_HASHBYTES]),
        steady[3].1,
    ]
    .iter()
    .enumerate()
    {
        let tag = [i as u8 + 1; TRUNCATED_HASHBYTES];
        path_requests.push(build_path_request_raw_with_tag(target.as_bytes(), &tag));
    }

    let burst: Vec<_> = (0..BURST_N)
        .map(|i| {
            let id = Identity::generate(&mut OsRng);
            build_announce_for(&id, &format!("b{i}"))
        })
        .collect();

    let victim = &steady_ids[0];
    let victim_hash = steady[0].1;
    let (victim_reannounce, vh2) = build_announce_for(victim, "s0");
    let (victim_after_unblackhole, vh3) = build_announce_for(victim, "s0");
    assert_eq!(victim_hash, vh2);
    assert_eq!(victim_hash, vh3);

    let sentinel_id = Identity::generate(&mut OsRng);
    let sentinel = build_announce_for(&sentinel_id, "sentinel");

    TrafficScript {
        steady,
        path_requests,
        burst,
        victim_identity_hex: hex::encode(victim.hash()),
        victim_hash,
        victim_reannounce,
        victim_after_unblackhole,
        sentinel,
    }
}

/// One injector per daemon: a single raw RNS/TCP connection, kept open for
/// the whole scenario so each daemon sees exactly one stable traffic
/// interface whose ingress state is never reset by reconnects.
struct Injectors {
    lnsd: TcpStream,
    rnsd: TcpStream,
}

impl Injectors {
    /// Send the same frame to both daemons back to back. Loopback TCP writes
    /// complete in microseconds, so both daemons observe the same pacing.
    async fn send_both(&mut self, frame: &[u8]) {
        send_framed(&mut self.lnsd, frame).await;
        send_framed(&mut self.rnsd, frame).await;
    }

    /// Send a paced sequence of frames to both daemons: `pace` BEFORE each
    /// frame keeps every phase entry below the ingress threshold as well.
    async fn send_paced(&mut self, frames: &[&[u8]], pace: Duration) {
        for frame in frames {
            tokio::time::sleep(pace).await;
            self.send_both(frame).await;
        }
    }
}

// =========================================================================
// Stats JSON accessors and freeze predicate
// =========================================================================

fn ifaces(stats: &Value) -> Vec<Value> {
    stats
        .get("interfaces")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

fn num(v: &Value, key: &str) -> f64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn boolean(v: &Value, key: &str) -> bool {
    v.get(key).and_then(|b| b.as_bool()).unwrap_or(false)
}

fn held_total(stats: &Value) -> u64 {
    ifaces(stats)
        .iter()
        .map(|i| num(i, "held_announces") as u64)
        .sum()
}

fn any_burst(stats: &Value) -> bool {
    ifaces(stats).iter().any(|i| boolean(i, "burst_active"))
}

/// The traffic-bearing interface: on lnsd the only entry, on rnsd the
/// spawned per-connection entry. Selected as the non-local entry with the
/// highest rxb, which is unambiguous because exactly one interface ever
/// receives the injected frames.
fn traffic_iface(stats: &Value) -> Value {
    ifaces(stats)
        .into_iter()
        .filter(|i| {
            let t = i.get("type").and_then(|t| t.as_str()).unwrap_or("");
            t != "LocalServerInterface" && t != "LocalClientInterface"
        })
        .max_by_key(|i| num(i, "rxb") as u64)
        .expect("no traffic interface in stats")
}

/// True once every volatile per-interface stat has settled to its exact
/// frozen value (see the module doc for why polling guarantees convergence).
fn is_quiet(stats: &Value) -> bool {
    let ifs = ifaces(stats);
    let iface_quiet = ifs.iter().all(|i| {
        num(i, "rxs") == 0.0
            && num(i, "txs") == 0.0
            && num(i, "incoming_announce_frequency") == 0.0
            && num(i, "outgoing_announce_frequency") == 0.0
            && num(i, "incoming_pr_frequency") == 0.0
            && num(i, "outgoing_pr_frequency") == 0.0
            && num(i, "held_announces") == 0.0
            && !boolean(i, "burst_active")
            && !boolean(i, "pr_burst_active")
    });
    iface_quiet && num(stats, "rxs") == 0.0 && num(stats, "txs") == 0.0
}

/// Poll `daemon` until `pred` holds; panic with the final snapshot when the
/// deadline passes. Eventual-consistency polling: no assertion in this suite
/// couples to a wall-clock instant, only to a state being reached.
async fn wait_stats<F: Fn(&Value) -> bool>(
    daemon: &ParityDaemon,
    what: &str,
    deadline: Duration,
    interval: Duration,
    pred: F,
) -> Value {
    let start = Instant::now();
    loop {
        let stats = daemon.stats().await;
        if pred(&stats) {
            return stats;
        }
        assert!(
            start.elapsed() < deadline,
            "{}: {} not reached within {:?}; last stats:\n{}",
            daemon.stack.label(),
            what,
            deadline,
            serde_json::to_string_pretty(&stats).unwrap_or_default()
        );
        tokio::time::sleep(interval).await;
    }
}

/// Poll until the daemon's learned path set satisfies `pred`.
async fn wait_paths<F: Fn(&BTreeSet<String>) -> bool>(
    daemon: &ParityDaemon,
    what: &str,
    deadline: Duration,
    pred: F,
) -> BTreeSet<String> {
    let start = Instant::now();
    loop {
        let paths = daemon.path_hashes().await;
        if pred(&paths) {
            return paths;
        }
        assert!(
            start.elapsed() < deadline,
            "{}: {} not reached within {:?}; current path set ({}): {:?}",
            daemon.stack.label(),
            what,
            deadline,
            paths.len(),
            paths
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// =========================================================================
// JSON comparison (the -j exact anchor)
// =========================================================================

/// Canonical value for structural comparison: all numbers collapse to f64 so
/// Python's `0` compares equal to our `0.0` (both stacks serialize the same
/// semantic value with different int/float types).
#[derive(Debug, PartialEq)]
enum Canon {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Canon>),
    Obj(std::collections::BTreeMap<String, Canon>),
}

fn canon(v: &Value) -> Canon {
    match v {
        Value::Null => Canon::Null,
        Value::Bool(b) => Canon::Bool(*b),
        Value::Number(n) => Canon::Num(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => Canon::Str(s.clone()),
        Value::Array(a) => Canon::Arr(a.iter().map(canon).collect()),
        Value::Object(o) => Canon::Obj(o.iter().map(|(k, v)| (k.clone(), canon(v))).collect()),
    }
}

/// Collect human-readable differences between two canon trees.
fn canon_diff(a: &Canon, b: &Canon, path: &str, out: &mut Vec<String>) {
    match (a, b) {
        (Canon::Obj(ma), Canon::Obj(mb)) => {
            for k in ma.keys().chain(mb.keys()).collect::<BTreeSet<_>>() {
                match (ma.get(k), mb.get(k)) {
                    (Some(va), Some(vb)) => canon_diff(va, vb, &format!("{path}.{k}"), out),
                    (Some(_), None) => out.push(format!("{path}.{k}: only in left")),
                    (None, Some(_)) => out.push(format!("{path}.{k}: only in right")),
                    (None, None) => unreachable!(),
                }
            }
        }
        (Canon::Arr(aa), Canon::Arr(ab)) => {
            if aa.len() != ab.len() {
                out.push(format!(
                    "{path}: array lengths {} vs {}",
                    aa.len(),
                    ab.len()
                ));
            } else {
                for (i, (va, vb)) in aa.iter().zip(ab).enumerate() {
                    canon_diff(va, vb, &format!("{path}[{i}]"), out);
                }
            }
        }
        (x, y) => {
            if x != y {
                out.push(format!("{path}: {x:?} vs {y:?}"));
            }
        }
    }
}

/// Scrub a client's -j sample for SAME-DAEMON comparison. Everything the
/// scrub touches is asserted first, so the normalization cannot hide a
/// divergence:
///
///   * transport_uptime: monotonic daemon clock, necessarily different for
///     two invocations; asserted positive and plausible, then blanked;
///   * rss: rnsd reports its (constantly moving) process RSS, lnsd reports
///     null; asserted to match the stack, then blanked;
///   * observer footprint (rnsd only): rnstatus connects as a shared
///     instance client and therefore sees ITSELF: exactly one extra
///     LocalClientInterface entry, and clients=1 on the Shared Instance
///     entry. lnstatus talks RPC-only and must see zero/0. The scrub asserts
///     exactly that expectation and removes the footprint so the remaining
///     structures are comparable.
fn scrub_client_sample(stats: &mut Value, client: StatusClient, stack: Stack) {
    let uptime = num(stats, "transport_uptime");
    assert!(
        uptime > 0.0 && uptime < 3600.0,
        "{} on {}: implausible transport_uptime {uptime}",
        client.label(),
        stack.label()
    );
    stats["transport_uptime"] = Value::Null;

    match stack {
        Stack::Lnsd => assert!(
            stats.get("rss").is_some_and(|v| v.is_null()),
            "lnsd must report rss=null, got {:?}",
            stats.get("rss")
        ),
        Stack::Rnsd => assert!(
            num(stats, "rss") > 0.0,
            "rnsd must report a positive process rss, got {:?}",
            stats.get("rss")
        ),
    }
    stats["rss"] = Value::Null;

    let expect_self = matches!((stack, client), (Stack::Rnsd, StatusClient::Rnstatus));
    if let Some(list) = stats.get_mut("interfaces").and_then(|v| v.as_array_mut()) {
        let before = list.len();
        list.retain(|i| i["type"] != "LocalClientInterface");
        let removed = before - list.len();
        assert_eq!(
            removed,
            usize::from(expect_self),
            "{} on {}: unexpected LocalClientInterface observer count",
            client.label(),
            stack.label()
        );
        for iface in list.iter_mut() {
            if iface["type"] == "LocalServerInterface" {
                let clients = num(iface, "clients") as i64;
                assert_eq!(
                    clients,
                    i64::from(expect_self),
                    "{} on {}: Shared Instance clients must count exactly the observer",
                    client.label(),
                    stack.label()
                );
                iface["clients"] = Value::from(0);
            }
            // burst_activated / pr_burst_activated are absolute wall-clock
            // epoch timestamps (Interface.py ic_burst_activated /
            // ic_pr_burst_activated; rnstatus renders `now - activated` as the
            // burst duration, rnstatus.py:566). The daemon derives them from a
            // fresh SystemTime::now() epoch base on every query, so two
            // sequential samples of the same live daemon differ in the low
            // digits (e.g. 1783071384.7079797 vs 1783071384.70798) and the
            // exact-float structural compare flakes. Assert the KEY is emitted
            // by both stacks (the parity guarantee), then normalise the
            // volatile VALUE to a fixed sentinel. Mirrors `clients` above; an
            // idle interface already reads the int 0, which this leaves intact.
            for key in ["burst_activated", "pr_burst_activated"] {
                assert!(
                    iface.get(key).is_some(),
                    "{} on {}: interface -j must emit `{key}`",
                    client.label(),
                    stack.label()
                );
                iface[key] = Value::from(0);
            }
        }
    }
}

/// Assert two scrubbed samples are structurally IDENTICAL.
fn assert_json_identical(left: &Value, right: &Value, what: &str) {
    let mut diffs = Vec::new();
    canon_diff(&canon(left), &canon(right), "$", &mut diffs);
    assert!(
        diffs.is_empty(),
        "{what}: -j structures differ:\n  {}\nleft:\n{}\nright:\n{}",
        diffs.join("\n  "),
        serde_json::to_string_pretty(left).unwrap_or_default(),
        serde_json::to_string_pretty(right).unwrap_or_default(),
    );
}

// =========================================================================
// Text normalization (per-flag render parity)
// =========================================================================

/// Normalize the documented volatile parts of a status text sample. In the
/// FROZEN state the only volatile line is the uptime (every frequency and
/// speed reads exactly 0 by the freeze predicate, so their renderings are
/// already identical); during the burst the elapsed-time suffix, decaying
/// frequencies, speeds and the held count are additionally normalized, each
/// backed by a separate numeric assertion so the normalization never hides a
/// real divergence. Whitespace is collapsed only on lines that were touched,
/// because value width changes shift rnstatus's column padding.
///
/// Three artifacts of SEQUENTIAL sampling on a live daemon are normalized
/// structurally (their substance is asserted separately, see the during-burst
/// section):
///
///   * the `<HZ>/c` (or `/p`) per-client rate suffix: rnstatus samples with
///     itself connected as a shared-instance client (clients=1 on the Shared
///     Instance entry), lnstatus samples client-free (clients=0), so the
///     suffix presence differs by observer footprint exactly like the
///     `clients` field the -j scrub pins;
///   * the ` burst for <T>` suffix: on rnsd the parent listener's burst flag
///     is advanced by the 5 s jobs cadence and can flip between the two
///     samples; presence on the daemon under test is asserted explicitly
///     before normalization;
///   * padding between a frequency and its ↑/↓ arrow: rnstatus pads the
///     shorter of the two frequency strings for column alignment, and the
///     two clients sample different (decaying) values with different widths.
fn normalize_status_text(text: &str) -> String {
    let re_time = Regex::new(r"\d+(?:\.\d+)?(?:d|h|m|s)\b").unwrap();
    // Frequencies render sub-Hz multipliers (mHz/µHz) once the deque decay
    // sets in, so the multiplier class covers them as well.
    let re_hz = Regex::new(r"\d+(?:\.\d+)?\s[munµKMGTPEZY]?Hz").unwrap();
    let re_bps = Regex::new(r"\d+(?:\.\d+)?\s[kKMGTPEZY]?bps").unwrap();
    let re_held = Regex::new(r"\d+ announces?\b").unwrap();
    let re_per_client = Regex::new(r"\s*<HZ>/[cp]").unwrap();
    let re_burst_for = Regex::new(r"\s*burst for <T>(?:, <T>)*(?: and <T>)?").unwrap();

    text.lines()
        .map(|line| {
            let mut l = line.to_string();
            let mut touched = false;
            if l.contains("Uptime is") || l.contains("burst for") {
                // The announce-rate suffix (t:1h/p:0s/g:5) is stable config,
                // shielded from the time regex by masking it first.
                let shielded = l.replace("(t:1h/p:0s/g:5)", "(AR)");
                let replaced = re_time.replace_all(&shielded, "<T>").into_owned();
                l = replaced.replace("(AR)", "(t:1h/p:0s/g:5)");
                touched = true;
            }
            if re_hz.is_match(&l) {
                l = re_hz.replace_all(&l, "<HZ>").into_owned();
                touched = true;
            }
            if !l.contains("Rate ") && re_bps.is_match(&l) {
                l = re_bps.replace_all(&l, "<BPS>").into_owned();
                touched = true;
            }
            if l.contains("Held") && re_held.is_match(&l) {
                l = re_held.replace_all(&l, "<N> announces").into_owned();
                touched = true;
            }
            if touched {
                // Structural artifacts of sequential sampling (see above).
                l = re_per_client.replace_all(&l, "").into_owned();
                l = re_burst_for.replace_all(&l, "").into_owned();
                l = l.split_whitespace().collect::<Vec<_>>().join(" ");
                l = l.replace("<HZ> ↑", "<HZ>↑").replace("<HZ> ↓", "<HZ>↓");
            }
            l
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract the held-announce count a client rendered (from its -A output).
fn parse_held(text: &str) -> Option<u64> {
    let re = Regex::new(r"Held\s+: (\d+) announce").unwrap();
    re.captures(text)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

// =========================================================================
// The scenario
// =========================================================================

/// Full 2x2 matrix under one controlled traffic script. Single test on
/// purpose: both daemons must live through the exact same scenario at the
/// same wall-clock time for the A/B to be valid, and the expensive phases
/// (burst drain, freeze) are shared by every comparison.
#[tokio::test]
#[ignore = "spawns Python daemons and tooling; run via --include-ignored after the tier3"]
async fn status_parity_matrix_2x2() {
    init_tracing();

    let script = build_script();

    // ---- Spawn both daemons and connect one injector to each. ----
    let lnsd = ParityDaemon::start(Stack::Lnsd).await;
    let rnsd = ParityDaemon::start(Stack::Rnsd).await;

    let mut injectors = Injectors {
        lnsd: connect_injector(&lnsd).await,
        rnsd: connect_injector(&rnsd).await,
    };

    // ---- Phase 1: STEADY announces. ----
    // Anti-flake: paced below the ingress threshold, then the path sets are
    // POLLED to the exact expected value on both daemons (count based, no
    // fixed post-send sleep).
    let steady_frames: Vec<&[u8]> = script.steady.iter().map(|(f, _)| f.as_slice()).collect();
    injectors.send_paced(&steady_frames, SLOW_PACE).await;

    let steady_set: BTreeSet<String> = script
        .steady
        .iter()
        .map(|(_, h)| hex::encode(h.as_bytes()))
        .collect();
    for daemon in [&lnsd, &rnsd] {
        let paths = wait_paths(
            daemon,
            "steady paths learned",
            Duration::from_secs(30),
            |p| p == &steady_set,
        )
        .await;
        assert_eq!(paths, steady_set);
    }

    // ---- Phase 2: PATH REQUESTS. ----
    let pr_frames: Vec<&[u8]> = script.path_requests.iter().map(|f| f.as_slice()).collect();
    injectors.send_paced(&pr_frames, SLOW_PACE).await;

    // Volume evidence for #67 pr_frequency: the traffic interface on BOTH
    // daemons must show a real (non-zero) incoming PR frequency and must
    // have SENT bytes (the three known-destination requests are answered
    // with path responses). Polled, not slept; the frequency needs more
    // than IC_DEQUE_MIN_SAMPLE=2 deque entries, which 4 requests guarantee.
    for daemon in [&lnsd, &rnsd] {
        wait_stats(
            daemon,
            "incoming_pr_frequency > 0 and path responses sent",
            Duration::from_secs(20),
            Duration::from_millis(500),
            |stats| {
                let t = traffic_iface(stats);
                num(&t, "incoming_pr_frequency") > 0.0
                    && num(&t, "outgoing_announce_frequency") > 0.0
                    && num(&t, "txb") > 0.0
            },
        )
        .await;
    }

    // ---- Phase 3: BURST (Codeberg #87). ----
    // Quiet gap first so the steady/PR-phase samples are decay-eligible and
    // the burst crosses the ingress threshold on both stacks (see
    // PRE_BURST_DECAY_GAP). This is a traffic-script property (when frames
    // are sent), not an assertion timing.
    tokio::time::sleep(PRE_BURST_DECAY_GAP).await;
    let burst_frames: Vec<&[u8]> = script.burst.iter().map(|(f, _)| f.as_slice()).collect();
    injectors.send_paced(&burst_frames, BURST_PACE).await;
    let burst_end = Instant::now();

    // The limiter must have engaged on both stacks: burst_active reported
    // and announces held. Polled immediately after the blast; the reads sit
    // deep inside the 15 s IC_BURST_PENALTY window (no release can have
    // happened yet), so held counts are STABLE while we sample.
    let lnsd_burst = wait_stats(
        &lnsd,
        "burst engaged",
        Duration::from_secs(5),
        Duration::from_millis(200),
        |s| any_burst(s) && held_total(s) > 0,
    )
    .await;
    let rnsd_burst = wait_stats(
        &rnsd,
        "burst engaged",
        Duration::from_secs(5),
        Duration::from_millis(200),
        |s| any_burst(s) && held_total(s) > 0,
    )
    .await;

    // Cross-daemon held-volume guard. The activation index depends on the
    // per-stack frequency arithmetic hitting the 3 Hz threshold one or two
    // frames apart (float timing on a 25 Hz stream), so an exact match is
    // not a valid expectation; a small window plus a hard lower bound is.
    let (held_l, held_r) = (held_total(&lnsd_burst), held_total(&rnsd_burst));
    assert!(
        held_l >= 5 && held_r >= 5,
        "both stacks must hold a substantial part of the burst (lnsd={held_l}, rnsd={held_r})"
    );
    assert!(
        held_l.abs_diff(held_r) <= 4,
        "held-announce volumes diverge beyond activation jitter: lnsd={held_l} rnsd={held_r}"
    );

    // DURING-BURST client sampling (#87 visibility through the real tools).
    // Per daemon the two clients run SEQUENTIALLY (observer determinism) but
    // the two daemons are sampled in parallel so everything lands inside the
    // penalty window. The held count each client rendered is asserted
    // against the RPC snapshot before the text normalization blanks it.
    let (lnsd_texts, rnsd_texts) = tokio::join!(
        async {
            let rn = run_status(StatusClient::Rnstatus, &lnsd, &["-A"]).await;
            wait_observer_gone(&lnsd).await;
            let ln = run_status(StatusClient::Lnstatus, &lnsd, &["-A"]).await;
            let lb = run_status(StatusClient::Lnstatus, &lnsd, &["-B"]).await;
            (rn, ln, lb)
        },
        async {
            let rn = run_status(StatusClient::Rnstatus, &rnsd, &["-A"]).await;
            wait_observer_gone(&rnsd).await;
            let ln = run_status(StatusClient::Lnstatus, &rnsd, &["-A"]).await;
            (rn, ln)
        }
    );

    // lnsd shows the traffic interface directly: both clients must render
    // the held count and the burst suffix. Tolerance of 2 on the parsed
    // count vs the RPC snapshot covers a release schedule that starts while
    // a client is still sampling under extreme load; the burst itself
    // cannot clear that fast (frequency decay plus 15 s hold).
    let (rn_l, ln_l, lnsd_burst_filter) = &lnsd_texts;
    for (who, text) in [("rnstatus", rn_l), ("lnstatus", ln_l)] {
        let held = parse_held(text).unwrap_or_else(|| {
            panic!("{who} on lnsd must render a Held line during the burst, got:\n{text}")
        });
        assert!(
            held > 0 && held_l.saturating_sub(held) <= 2,
            "{who} on lnsd rendered held={held}, RPC snapshot said {held_l}"
        );
        assert!(
            text.contains("burst for"),
            "{who} on lnsd must render the burst suffix during the burst, got:\n{text}"
        );
    }
    assert_eq!(
        normalize_status_text(rn_l),
        normalize_status_text(ln_l),
        "during-burst -A render parity on lnsd"
    );
    // -B (burst filter) must show the bursting interface on lnsd.
    assert!(
        lnsd_burst_filter.contains("tcp_server"),
        "lnstatus -B on lnsd must list the bursting interface, got:\n{lnsd_burst_filter}"
    );

    // On rnsd the burst lives on the spawned connection entry, which
    // rnstatus HIDES by default (TCPInterface[Client prefix), so the held
    // substance is asserted via RPC above; the -A text parity between the
    // clients still holds on the visible entries. The per-client announce
    // rate (`.../c`) must be rendered by BOTH clients for the TCP listener
    // (clients=1, the injector connection) - asserted here because the
    // normalization strips the observer-dependent occurrence on the Shared
    // Instance entry (rnstatus counts itself, lnstatus is RPC-only).
    let (rn_r, ln_r) = &rnsd_texts;
    for (who, text) in [("rnstatus", rn_r), ("lnstatus", ln_r)] {
        assert!(
            text.contains("/c"),
            "{who} on rnsd must render the per-client announce rate, got:\n{text}"
        );
    }
    assert_eq!(
        normalize_status_text(rn_r),
        normalize_status_text(ln_r),
        "during-burst -A render parity on rnsd"
    );

    // ---- Phase 3b: DRAIN (hold-and-release, not drop). ----
    // Every burst announce must eventually reach the path table (release
    // schedule: 15 s penalty, then one per 5 s), and the held queues must
    // drain to zero with the burst flag cleared. Deadline sized to the
    // schedule (15 + 20*5 = 115 s) plus slack, never to a guessed latency.
    let full_set: BTreeSet<String> = steady_set
        .iter()
        .cloned()
        .chain(script.burst.iter().map(|(_, h)| hex::encode(h.as_bytes())))
        .collect();
    for daemon in [&lnsd, &rnsd] {
        let paths = wait_paths(
            daemon,
            "all burst announces released into the path table",
            Duration::from_secs(200),
            |p| p == &full_set,
        )
        .await;
        assert_eq!(paths, full_set, "hold-and-release must lose nothing");
        wait_stats(
            daemon,
            "held queue drained and burst cleared",
            Duration::from_secs(60),
            Duration::from_secs(1),
            |s| held_total(s) == 0 && !any_burst(s),
        )
        .await;
    }
    tracing::info!(
        "burst drained on both daemons after {:?} (held peaks lnsd={held_l} rnsd={held_r})",
        burst_end.elapsed()
    );

    // ---- Phase 4: BLACKHOLE (Codeberg #88), via the real operator tool. ----
    for daemon in [&lnsd, &rnsd] {
        let out = run_python_tool(
            RNPATH_PY,
            &["-B", &script.victim_identity_hex],
            &daemon.config_dir,
        )
        .await;
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("Blackholed identity"),
            "rnpath -B against {} failed: status {:?} stdout {:?} stderr {:?}",
            daemon.stack.label(),
            out.status.code(),
            stdout,
            String::from_utf8_lossy(&out.stderr)
        );
        wait_observer_gone(daemon).await;
    }

    let victim_hex = hex::encode(script.victim_hash.as_bytes());
    // Blackholing removes the already-learned path row on both stacks.
    for daemon in [&lnsd, &rnsd] {
        wait_paths(
            daemon,
            "victim path removed by blackhole",
            Duration::from_secs(15),
            |p| !p.contains(&victim_hex),
        )
        .await;
    }

    // The blackhole set itself must read back identically through the RPC
    // both daemons expose (this is the is_blackholed surface rnpath uses).
    for daemon in [&lnsd, &rnsd] {
        let bh = daemon.blackholed().await;
        let entry = bh.get(&script.victim_identity_hex).unwrap_or_else(|| {
            panic!(
                "{}: blackholed_identities must contain the victim, got {bh}",
                daemon.stack.label()
            )
        });
        assert!(
            entry.get("until").is_some_and(|u| u.is_null()),
            "{}: rnpath -B sets no expiry, until must be null, got {entry}",
            daemon.stack.label()
        );
    }

    // Victim re-announce is DROPPED; the sentinel that follows on the same
    // ordered TCP stream is processed. Waiting for the sentinel path proves
    // the victim frame was consumed before we assert its absence (ordering
    // based, no sleep-and-hope).
    injectors
        .send_paced(&[&script.victim_reannounce, &script.sentinel.0], SLOW_PACE)
        .await;
    let sentinel_hex = hex::encode(script.sentinel.1.as_bytes());
    for daemon in [&lnsd, &rnsd] {
        wait_paths(
            daemon,
            "sentinel announce processed",
            Duration::from_secs(15),
            |p| p.contains(&sentinel_hex),
        )
        .await;
        let paths = daemon.path_hashes().await;
        assert!(
            !paths.contains(&victim_hex),
            "{}: the blackholed identity's announce must not create a path",
            daemon.stack.label()
        );
    }

    // Lift the blackhole; the victim announces normally again on both.
    for daemon in [&lnsd, &rnsd] {
        let out = run_python_tool(
            RNPATH_PY,
            &["-U", &script.victim_identity_hex],
            &daemon.config_dir,
        )
        .await;
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("Lifted blackhole"),
            "rnpath -U against {} failed: status {:?} stdout {:?} stderr {:?}",
            daemon.stack.label(),
            out.status.code(),
            stdout,
            String::from_utf8_lossy(&out.stderr)
        );
        wait_observer_gone(daemon).await;
    }
    injectors
        .send_paced(&[&script.victim_after_unblackhole], SLOW_PACE)
        .await;
    for daemon in [&lnsd, &rnsd] {
        wait_paths(
            daemon,
            "victim path restored after unblackhole",
            Duration::from_secs(15),
            |p| p.contains(&victim_hex),
        )
        .await;
    }

    // ---- Phase 5: FREEZE. ----
    // Poll both daemons until every volatile stat reads its exact frozen
    // value. The 1 s polling itself drains the frequency deques on both
    // stacks (identical n/span-with-decay semantics), so this converges
    // deterministically; the deadline is sized to the deque length (~30
    // samples), not guessed.
    for daemon in [&lnsd, &rnsd] {
        wait_stats(
            daemon,
            "all volatile stats settled (freeze)",
            Duration::from_secs(240),
            Duration::from_secs(1),
            is_quiet,
        )
        .await;
    }

    // ---- FINAL VOLUME GUARD (fail loudly before any comparison). ----
    let expected_final: BTreeSet<String> = full_set
        .iter()
        .cloned()
        .chain([sentinel_hex.clone()])
        .collect();
    let paths_l = lnsd.path_hashes().await;
    let paths_r = rnsd.path_hashes().await;
    assert_eq!(
        paths_l, expected_final,
        "VOLUME GUARD: lnsd path set diverges from the script expectation"
    );
    assert_eq!(
        paths_r, expected_final,
        "VOLUME GUARD: rnsd path set diverges from the script expectation"
    );

    let stats_l = lnsd.stats().await;
    let stats_r = rnsd.stats().await;
    let (traffic_l, traffic_r) = (traffic_iface(&stats_l), traffic_iface(&stats_r));
    for key in ["rxb", "txb"] {
        let (a, b) = (num(&traffic_l, key), num(&traffic_r, key));
        assert!(
            a > 0.0 && b > 0.0,
            "VOLUME GUARD: {key} must be non-zero on both traffic interfaces"
        );
        let rel = (a - b).abs() / a.max(b);
        assert!(
            rel <= 0.05,
            "VOLUME GUARD: traffic interface {key} diverges {:.1}% (lnsd={a}, rnsd={b}); \
             identical frames were injected, comparison would be invalid",
            rel * 100.0
        );
    }

    // ---- Frozen 2x2 matrix: the -j JSON anchor. ----
    // Client parity: after the documented scrub the two clients' -j output
    // against the SAME daemon must be structurally IDENTICAL: identical
    // interface set, identical key sets, every stable field bit-equal.
    for daemon in [&lnsd, &rnsd] {
        let rn_raw = run_status(StatusClient::Rnstatus, daemon, &["-j"]).await;
        wait_observer_gone(daemon).await;
        let ln_raw = run_status(StatusClient::Lnstatus, daemon, &["-j"]).await;

        let mut rn: Value = serde_json::from_str(rn_raw.trim())
            .unwrap_or_else(|e| panic!("rnstatus -j on {} not JSON: {e}", daemon.stack.label()));
        let mut ln: Value = serde_json::from_str(ln_raw.trim())
            .unwrap_or_else(|e| panic!("lnstatus -j on {} not JSON: {e}", daemon.stack.label()));
        scrub_client_sample(&mut rn, StatusClient::Rnstatus, daemon.stack);
        scrub_client_sample(&mut ln, StatusClient::Lnstatus, daemon.stack);
        assert_json_identical(
            &rn,
            &ln,
            &format!("client -j parity on {}", daemon.stack.label()),
        );
    }

    // Daemon parity (#67): the reference client reads both daemons; compare
    // via rnstatus -j so the daemon side is the only variable.
    let rn_on_l: Value = serde_json::from_str(
        run_status(StatusClient::Rnstatus, &lnsd, &["-j"])
            .await
            .trim(),
    )
    .expect("rnstatus -j on lnsd JSON");
    wait_observer_gone(&lnsd).await;
    let rn_on_r: Value = serde_json::from_str(
        run_status(StatusClient::Rnstatus, &rnsd, &["-j"])
            .await
            .trim(),
    )
    .expect("rnstatus -j on rnsd JSON");
    wait_observer_gone(&rnsd).await;
    assert_daemon_stats_parity(&rn_on_l, &rn_on_r);

    // ---- Frozen per-flag render parity. ----
    // In the frozen state every frequency and speed renders "0 Hz"/"0 bps"
    // on both clients (asserted by the freeze predicate), so the only
    // normalized line is the uptime. Everything else must match byte for
    // byte. Flags are run sequentially per daemon so each pair reads the
    // same observer-free state.
    let flag_sets: &[&[&str]] = &[
        &[],
        &["-A"],
        &["-P"],
        &["-A", "-P"],
        &["-B"],
        &["-l"],
        &["-t"],
        &["-s", "traffic"],
        &["tcp"],
    ];
    for daemon in [&lnsd, &rnsd] {
        for flags in flag_sets {
            let rn = run_status(StatusClient::Rnstatus, daemon, flags).await;
            wait_observer_gone(daemon).await;
            let ln = run_status(StatusClient::Lnstatus, daemon, flags).await;
            assert_eq!(
                normalize_status_text(&rn),
                normalize_status_text(&ln),
                "render parity for flags {:?} on {}\nrnstatus:\n{}\nlnstatus:\n{}",
                flags,
                daemon.stack.label(),
                rn,
                ln
            );
        }
    }

    // ---- lnstatus on lnsd (full Rust stack): concrete sanity ranges. ----
    let ln_on_l: Value = serde_json::from_str(
        run_status(StatusClient::Lnstatus, &lnsd, &["-j"])
            .await
            .trim(),
    )
    .expect("lnstatus -j on lnsd JSON");
    assert_full_stack_sane(&ln_on_l);

    // Cleanup (only reached on success; failures keep the dirs + daemon
    // logs for diagnosis).
    drop(injectors);
    let (l_dir, r_dir) = (lnsd.config_dir.clone(), rnsd.config_dir.clone());
    drop(lnsd);
    drop(rnsd);
    let _ = std::fs::remove_dir_all(l_dir);
    let _ = std::fs::remove_dir_all(r_dir);
}

async fn connect_injector(daemon: &ParityDaemon) -> TcpStream {
    connect_injector_port(daemon, daemon.tcp_port).await
}

async fn connect_injector_port(daemon: &ParityDaemon, port: u16) -> TcpStream {
    let addr = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match TcpStream::connect(&addr).await {
            Ok(s) => return s,
            Err(e) => assert!(
                Instant::now() < deadline,
                "cannot connect injector to {}: {e}",
                daemon.stack.label()
            ),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// =========================================================================
// Multi-interface sort / -a parity (complements the single-interface 2x2)
// =========================================================================

/// Non-local visible interface entries (the spawned per-connection ones).
fn visible_ifaces(stats: &Value) -> Vec<Value> {
    ifaces(stats)
        .into_iter()
        .filter(|i| {
            let t = i.get("type").and_then(|t| t.as_str()).unwrap_or("");
            t != "LocalServerInterface" && t != "LocalClientInterface"
        })
        .collect()
}

/// Drop the volatile uptime value so two sequential reads of the same frozen
/// daemon are byte-comparable. Every other line is stable in the idle state
/// (speeds render `0 bps`, byte counters are pinned by the readiness poll),
/// so the uptime trailer is the only thing that can differ between the two
/// clients' invocations.
fn normalize_idle_text(text: &str) -> String {
    let re_uptime = Regex::new(r"Uptime is .*").unwrap();
    text.lines()
        .map(|l| {
            if l.contains("Uptime is") {
                re_uptime.replace(l, "Uptime is <T>").into_owned()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// lnstatus vs rnstatus (the drop-in A/B) against ONE lnsd carrying TWO
/// traffic interfaces with unequal received-byte counts. This pins the parity
/// surface the single-interface 2x2 cannot exercise: the default multi-entry
/// ordering, every `-s <field>` sort direction, `-r` reverse, and the `-a`
/// spawned-interface block. Both clients read the SAME frozen daemon state, so
/// after blanking only the uptime line their output must be byte-identical for
/// a user or script switching between the tools.
///
/// Determinism: the two injectors send different byte volumes on one raw TCP
/// connection each, so the spawned interfaces have distinct rxb; the test then
/// polls interface_stats until exactly two visible interfaces exist with
/// unequal, non-zero rxb AND every speed has decayed to 0 (frozen), so sort
/// order is well-defined and no live rate can differ between the two reads.
#[tokio::test]
#[ignore = "spawns lnsd + python rnstatus; run via --include-ignored after the tier3"]
async fn lnstatus_rnstatus_multi_interface_sort_parity() {
    init_tracing();

    let lnsd = ParityDaemon::start_n(Stack::Lnsd, 2).await;

    // One raw connection per TCP server; send more bytes on the first so the
    // two spawned interfaces end with clearly unequal rxb (deterministic sort
    // order). The connections stay open for the whole test so the interface
    // set never changes underneath the comparison.
    let mut inj_a = connect_injector_port(&lnsd, lnsd.tcp_ports[0]).await;
    let mut inj_b = connect_injector_port(&lnsd, lnsd.tcp_ports[1]).await;
    use tokio::io::AsyncWriteExt as _;
    // HDLC flag-delimited noise; the exact contents are irrelevant, only that
    // interface A receives strictly more on-wire bytes than interface B.
    inj_a
        .write_all(&[0x7e, 0x00, 0x11, 0x22, 0x33, 0x7e])
        .await
        .expect("write inj_a");
    inj_b.write_all(&[0x7e, 0x7e]).await.expect("write inj_b");
    inj_a.flush().await.expect("flush a");
    inj_b.flush().await.expect("flush b");

    // Readiness + freeze: exactly two visible interfaces, unequal non-zero
    // rxb, and all speeds decayed to 0 so nothing live can differ between the
    // two sequential client reads.
    wait_stats(
        &lnsd,
        "two frozen interfaces with unequal rxb",
        Duration::from_secs(30),
        Duration::from_millis(500),
        |stats| {
            let vis = visible_ifaces(stats);
            if vis.len() != 2 {
                return false;
            }
            let (r0, r1) = (num(&vis[0], "rxb"), num(&vis[1], "rxb"));
            r0 > 0.0 && r1 > 0.0 && r0 != r1 && is_quiet(stats)
        },
    )
    .await;

    // The A/B matrix: same daemon, two clients, byte-identical output after
    // blanking only the uptime. `-a` is included on every set because the
    // spawned TCP connections are hidden by default (the `TCPInterface[Client`
    // name prefix) and the sort/order surface is only visible once shown.
    let flag_sets: &[&[&str]] = &[
        &["-a"],
        &["-a", "-s", "rx"],
        &["-a", "-s", "rx", "-r"],
        &["-a", "-s", "tx"],
        &["-a", "-s", "traffic"],
        &["-a", "-s", "rate"],
        &["-a", "-s", "traffic", "-r"],
        &["-a", "-t"],
        &["-a", "-l"],
        &["-a", "-A", "-P"],
        &["-a", "tcp"],
    ];
    for flags in flag_sets {
        let rn = run_status(StatusClient::Rnstatus, &lnsd, flags).await;
        let ln = run_status(StatusClient::Lnstatus, &lnsd, flags).await;
        assert_eq!(
            normalize_idle_text(&rn),
            normalize_idle_text(&ln),
            "multi-interface render parity for flags {flags:?} on lnsd\nrnstatus:\n{rn}\nlnstatus:\n{ln}"
        );
    }

    // The -j top-level key set must match structurally as well (the JSON both
    // clients read is the same daemon dict; key order/separators are the only
    // allowed difference, per the module doc).
    let rn_j: Value = serde_json::from_str(
        run_status(StatusClient::Rnstatus, &lnsd, &["-j"])
            .await
            .trim(),
    )
    .expect("rnstatus -j JSON");
    let ln_j: Value = serde_json::from_str(
        run_status(StatusClient::Lnstatus, &lnsd, &["-j"])
            .await
            .trim(),
    )
    .expect("lnstatus -j JSON");
    let top_keys = |v: &Value| -> BTreeSet<String> {
        v.as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };
    assert_eq!(
        top_keys(&rn_j),
        top_keys(&ln_j),
        "multi-interface -j top-level key sets must match across clients"
    );

    drop(inj_a);
    drop(inj_b);
    let dir = lnsd.config_dir.clone();
    drop(lnsd);
    let _ = std::fs::remove_dir_all(dir);
}

/// Field-by-field daemon stats parity (#67) on the frozen state, rnstatus
/// being the shared measuring instrument. Cross-daemon identity fields
/// (name/short_name/hash/type) encode per-daemon naming and are asserted
/// per daemon instead; the known structural divergences are pinned exactly
/// so any drift fails the test.
fn assert_daemon_stats_parity(on_lnsd: &Value, on_rnsd: &Value) {
    // Top-level key sets must be identical.
    let keys = |v: &Value| -> BTreeSet<String> {
        v.as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };
    assert_eq!(
        keys(on_lnsd),
        keys(on_rnsd),
        "top-level interface_stats key sets must match across daemons"
    );

    // Pinned interface-list shapes (see module doc). rnstatus is connected
    // while it samples, so on rnsd its own LocalClientInterface entry is
    // part of the expected shape.
    let type_counts = |v: &Value| -> Vec<String> {
        let mut t: Vec<String> = ifaces(v)
            .iter()
            .map(|i| i["type"].as_str().unwrap_or("?").to_string())
            .collect();
        t.sort();
        t
    };
    assert_eq!(
        type_counts(on_lnsd),
        vec!["TCPServerInterface"],
        "lnsd must expose exactly the spawned traffic connection"
    );
    assert_eq!(
        type_counts(on_rnsd),
        vec![
            "LocalClientInterface",
            "LocalServerInterface",
            "TCPClientInterface",
            "TCPServerInterface"
        ],
        "rnsd must expose shared instance + listener + spawned connection + the sampling client"
    );

    let (tl, tr) = (traffic_iface(on_lnsd), traffic_iface(on_rnsd));

    // Pinned per-interface key-set divergence. `announce_queue` is always
    // reported by lnsd but only LAZILY by Python: the key appears once the
    // interface's announce-cap queue has ever held an entry, which depends
    // on rebroadcast timing under the burst load. Both shapes are pinned.
    let (kl, kr) = (keys(&tl), keys(&tr));
    let ours_only: BTreeSet<String> = kl.difference(&kr).cloned().collect();
    let python_only: BTreeSet<String> = kr.difference(&kl).cloned().collect();
    let ours_only_full: BTreeSet<String> = ["announce_queue", "peers"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let ours_only_queued: BTreeSet<String> = ["peers"].iter().map(|s| s.to_string()).collect();
    assert!(
        ours_only == ours_only_full || ours_only == ours_only_queued,
        "unexpected lnsd-only interface_stats keys: {ours_only:?}"
    );
    assert_eq!(
        python_only,
        [
            "autoconnect_source",
            "parent_interface_name",
            "parent_interface_hash"
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        "unexpected rnsd-only interface_stats keys"
    );

    // Exact-equal stats on the traffic interface. All of these are frozen
    // (freeze predicate) or static config, so numeric equality is exact.
    for key in [
        "status",
        "mode",
        "bitrate",
        "clients",
        "ifac_signature",
        "ifac_size",
        "ifac_netname",
        "announce_rate_target",
        "announce_rate_penalty",
        "announce_rate_grace",
        "held_announces",
        "burst_active",
        "pr_burst_active",
        "pr_burst_activated",
        "incoming_announce_frequency",
        "outgoing_announce_frequency",
        "incoming_pr_frequency",
        "outgoing_pr_frequency",
        "rxs",
        "txs",
    ] {
        let (a, b) = (canon(&tl[key]), canon(&tr[key]));
        assert_eq!(
            a, b,
            "daemon parity: traffic interface field {key} differs (lnsd vs rnsd)"
        );
    }

    // Both stacks record that an announce burst was activated at some point
    // of the scenario. The raw activation timestamps use different epochs,
    // so only the "it happened" fact is comparable.
    assert!(
        num(&tl, "burst_activated") != 0.0 && num(&tr, "burst_activated") != 0.0,
        "both daemons must remember the burst activation (lnsd={:?}, rnsd={:?})",
        tl.get("burst_activated"),
        tr.get("burst_activated")
    );

    // Frozen totals: speeds are exactly zero on both.
    for key in ["rxs", "txs"] {
        assert_eq!(
            num(on_lnsd, key),
            0.0,
            "lnsd total {key} must be frozen to 0"
        );
        assert_eq!(
            num(on_rnsd, key),
            0.0,
            "rnsd total {key} must be frozen to 0"
        );
    }

    // Transport identity fields: per-daemon values, shared shape.
    for (label, v) in [("lnsd", on_lnsd), ("rnsd", on_rnsd)] {
        let tid = v["transport_id"].as_str().unwrap_or_default();
        assert_eq!(tid.len(), 32, "{label}: transport_id must be 16 bytes hex");
        assert!(
            v["network_id"].is_null(),
            "{label}: network_id must be null"
        );
        assert!(
            v["probe_responder"].is_null(),
            "{label}: probe responder is off"
        );
        let up = num(v, "transport_uptime");
        assert!(up > 0.0 && up < 3600.0, "{label}: implausible uptime {up}");
    }
}

/// Concrete value-range assertions for the full Rust stack (lnstatus reading
/// lnsd), the "our stack is sane" cell of the matrix. Every range is
/// defined, not "looks sane".
fn assert_full_stack_sane(stats: &Value) {
    let ifs = ifaces(stats);
    assert_eq!(ifs.len(), 1, "one traffic interface expected");
    let t = &ifs[0];
    assert_eq!(t["type"], "TCPServerInterface");
    assert_eq!(t["status"], Value::Bool(true), "interface must be up");
    assert_eq!(num(t, "mode"), 1.0, "MODE_FULL");
    assert_eq!(num(t, "bitrate"), 10_000_000.0, "TCP bitrate guess");
    assert_eq!(
        t["hash"].as_str().map(|h| h.len()),
        Some(32),
        "interface hash must be 16 bytes hex"
    );
    assert!(num(t, "rxb") > 0.0, "traffic was received");
    assert!(num(t, "txb") > 0.0, "path responses were sent");
    assert_eq!(num(t, "announce_rate_target"), 3600.0);
    assert_eq!(num(t, "announce_rate_penalty"), 0.0);
    assert_eq!(num(t, "announce_rate_grace"), 5.0);
    assert_eq!(num(t, "held_announces"), 0.0, "drained after the burst");
    assert_eq!(t["burst_active"], Value::Bool(false));
    for key in [
        "incoming_announce_frequency",
        "outgoing_announce_frequency",
        "incoming_pr_frequency",
        "outgoing_pr_frequency",
    ] {
        assert_eq!(num(t, key), 0.0, "{key} frozen to exactly 0");
    }
    assert_eq!(
        stats["transport_id"].as_str().map(|h| h.len()),
        Some(32),
        "transport_id must be 16 bytes hex"
    );
    let up = num(stats, "transport_uptime");
    assert!(up > 0.0 && up < 3600.0, "implausible uptime {up}");
    assert_eq!(
        num(stats, "rxb"),
        num(t, "rxb"),
        "totals equal the single interface"
    );
    assert_eq!(
        num(stats, "txb"),
        num(t, "txb"),
        "totals equal the single interface"
    );
}
