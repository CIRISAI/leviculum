//! TCP load test: our `lnsd` as an internet-facing transport hub under heavy
//! traffic (delivery + leak + stability). Codeberg #101.
//!
//! Topology (identical generator for both stacks under test; only the HUB swaps):
//!
//! ```text
//!   N raw TCP clients ─┐            ┌─ sink (TestDaemon, Single dest, TCP client)
//!   churn connections ─┼─▶  HUB  ──▶┘
//!                      ┘   (lnsd / rnsd, transport-enabled)
//! ```
//!
//! * HUB is a real, separate process: either our `lnsd` binary built from a
//!   `type = BackboneInterface` server config (exercises #89 and the high-
//!   connection role Backbone exists for), or — for the A/B validation — a
//!   Python `rnsd`-equivalent (`TestDaemon`, `enable_transport = yes`). Being a
//!   separate process is what makes `/proc/<pid>` RSS + fd sampling meaningful.
//! * The SINK is a `TestDaemon` that registers a Single destination, connects to
//!   the hub as a TCP client, and announces — so the hub learns a 1-hop path and
//!   forwards each client packet to it (unicast, no broadcast amplification).
//! * Each client sends sequence-numbered, encrypted single packets to the sink.
//!
//! ## Delivery accounting (deterministic, unambiguous)
//!
//! Each client `i` sends single packets whose *plaintext* payload is
//! `b"LT" + u32_le(i) + u32_le(seq)`, `seq = 0..sent_i`, encrypted to the sink's
//! Single-destination identity (fresh ephemeral key per packet). The hub relays;
//! the sink decrypts, extracts `(i, seq)`, and folds `seq` into a per-source set
//! (`test_daemon.py::_on_single_packet` / `get_loadtest_stats`). Delivery is 100%
//! iff, for every client `i`: `distinct == sent_i`, `min == 0`, `max == sent_i-1`
//! (contiguous, no gaps, no duplicates). `sent_i` is known exactly on the
//! generator side. TCP is lossless, so any shortfall is a real hub bug.
//!
//! ## Load parameters (env-tunable)
//!
//! | env | default (soak) | meaning |
//! |-----|----------------|---------|
//! | `LOADTEST_CONNS`         | 200 | steady concurrent TCP client connections |
//! | `LOADTEST_SECS`          | 60  | steady + churn duration (seconds) |
//! | `LOADTEST_PKT_MS`        | 50  | per-connection inter-packet interval (ms) |
//! | `LOADTEST_CHURN_WORKERS` | 16  | connections repeatedly opened/closed |
//! | `LOADTEST_CHURN_PKTS`    | 4   | packets per churn connection before close |
//! | `LOADTEST_MAX_RSS_GROWTH_PCT` | 40 | max steady-phase RSS growth over warm-up |
//! | `LOADTEST_SAMPLE_MS`     | 250 | RSS/fd/CPU sampler cadence (ms) |
//! | `LOADTEST_LNSD_BIN`      | auto | path to the `lnsd` binary |
//!
//! ## The delivery record (Codeberg #208)
//!
//! Every run prints, and with `LEVICULUM_DELIVERY_LOG=<file>` also appends, one
//! line — green runs and red runs alike, emitted before the assertions:
//!
//! ```text
//! DELIVERY test=lnsd_soak sent=377285 recv=375570 pct=99.5454 ci95=99.5234-99.5665 //!   conns=128 pkt_ms=15 secs=20 churn_workers=16 churn_conns=42 cores=4 //!   hub_cpu_pct=82.4 gen_cpu_pct=210.5 host_busy_pct=96.1
//! ```
//!
//! The assertion here is binary (100 % or bust); the line is the distribution
//! behind it. #208 recorded one 99.5454 % run at 128 connections / 15 ms and
//! could not say whether the hub or the four-core host it shared with the
//! harness was the cause, because nobody had ever swept delivery against
//! connection count and rate. So each line carries its cell coordinates — two
//! runs at different `conns`/`pkt_ms` offered different volumes and cannot be
//! pooled — and the CPU occupancy of the hub, of the harness driving it
//! (generator plus sampler, the cost #208 could not account for), and of the
//! machine. That is what makes a cell interpretable: delivery below 100 % while
//! nothing is saturated is a defect in the hub, delivery below 100 % past
//! saturation is a load ceiling the gate should name.
//!
//! `scripts/sweep-tcp-hub.sh` drives the sweep and reads the matrix back out of
//! the log.
//!
//! ## Running
//!
//! ```sh
//! CARGO_TARGET_DIR=/home/lew/.cache/leviculum-dev-target \
//!   cargo build -p leviculum-cli --bin lnsd --release
//! # smoke (small, ~15 s):
//! CARGO_TARGET_DIR=... cargo test -p leviculum-std --test rnsd_interop \
//!   loadtest_tcp_hub_smoke -- --ignored --nocapture
//! # soak (heavy, env-tunable):
//! CARGO_TARGET_DIR=... cargo test -p leviculum-std --test rnsd_interop \
//!   loadtest_tcp_hub_soak -- --ignored --nocapture
//! # A/B vs real rnsd:
//! CARGO_TARGET_DIR=... cargo test -p leviculum-std --test rnsd_interop \
//!   loadtest_tcp_hub_ab_vs_rnsd -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use leviculum_core::constants::MTU;
use leviculum_core::identity::Identity;
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::{Destination, DestinationType, Direction};
use leviculum_std::interfaces::hdlc::frame;
use leviculum_std::process::spawn_supervised;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::common::init_tracing;
use crate::harness::{pick_free_tcp_port, TestDaemon};

// =========================================================================
// Load parameters
// =========================================================================

#[derive(Clone, Debug)]
struct LoadParams {
    conns: usize,
    secs: u64,
    pkt_ms: u64,
    churn_workers: usize,
    churn_pkts: u32,
    max_rss_growth_pct: u64,
    /// Absolute RSS ceiling above warm-up baseline (runaway backstop), MiB.
    max_rss_abs_mib: u64,
    /// Sampler cadence for RSS/fd time series, ms.
    sample_ms: u64,
    /// How long to wait, after traffic stops, for the sink to reach 100%.
    drain_secs: u64,
}

impl LoadParams {
    /// Soak defaults, each overridable by env.
    fn soak() -> Self {
        Self {
            conns: env_usize("LOADTEST_CONNS", 200),
            secs: env_u64("LOADTEST_SECS", 60),
            pkt_ms: env_u64("LOADTEST_PKT_MS", 50),
            churn_workers: env_usize("LOADTEST_CHURN_WORKERS", 16),
            churn_pkts: env_u64("LOADTEST_CHURN_PKTS", 4) as u32,
            max_rss_growth_pct: env_u64("LOADTEST_MAX_RSS_GROWTH_PCT", 40),
            max_rss_abs_mib: env_u64("LOADTEST_MAX_RSS_ABS_MIB", 300),
            sample_ms: env_u64("LOADTEST_SAMPLE_MS", 250),
            drain_secs: env_u64("LOADTEST_DRAIN_SECS", 20),
        }
    }

    /// Small, fast variant for the on-demand smoke test (~15 s).
    fn smoke() -> Self {
        Self {
            conns: env_usize("LOADTEST_CONNS", 24),
            secs: env_u64("LOADTEST_SECS", 5),
            pkt_ms: env_u64("LOADTEST_PKT_MS", 50),
            churn_workers: env_usize("LOADTEST_CHURN_WORKERS", 6),
            churn_pkts: env_u64("LOADTEST_CHURN_PKTS", 4) as u32,
            max_rss_growth_pct: env_u64("LOADTEST_MAX_RSS_GROWTH_PCT", 60),
            max_rss_abs_mib: env_u64("LOADTEST_MAX_RSS_ABS_MIB", 200),
            sample_ms: env_u64("LOADTEST_SAMPLE_MS", 200),
            drain_secs: env_u64("LOADTEST_DRAIN_SECS", 15),
        }
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// =========================================================================
// Phase labels for the RSS/fd time series
// =========================================================================

const PHASE_WARMUP: u8 = 0;
const PHASE_STEADY: u8 = 1;
const PHASE_DRAIN: u8 = 2;

fn phase_name(p: u8) -> &'static str {
    match p {
        PHASE_WARMUP => "warmup",
        PHASE_STEADY => "steady+churn",
        PHASE_DRAIN => "drain",
        u8::MAX => "final",
        _ => "?",
    }
}

/// Warm-up / probe client_id, excluded from delivery assertions because packets
/// sent before the sink's path is installed on the hub are legitimately lost.
const WARMUP_CLIENT_ID: u32 = u32::MAX;
/// Churn connections draw ids from this base upward so they never collide with
/// the steady clients `0..conns`.
const CHURN_ID_BASE: u32 = 1_000_000;

// =========================================================================
// Hub abstraction: the process under test (lnsd) or the A/B reference (rnsd)
// =========================================================================

/// Our `lnsd` binary running as a separate process from a BackboneInterface config.
struct LnsdHub {
    child: Child,
    port: u16,
    pid: u32,
    /// The hub's transport identity hash (16 bytes), parsed from its startup log.
    /// Clients address relayed packets to this as `transport_id`.
    transport_id: [u8; 16],
    log_path: PathBuf,
    _config_dir: tempfile::TempDir,
}

impl Drop for LnsdHub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Locate the `lnsd` binary: `LOADTEST_LNSD_BIN`, else `$CARGO_TARGET_DIR` (or
/// `<manifest>/../target`) `release/lnsd` then `debug/lnsd`.
fn locate_lnsd() -> PathBuf {
    if let Ok(p) = std::env::var("LOADTEST_LNSD_BIN") {
        let p = PathBuf::from(p);
        assert!(
            p.exists(),
            "LOADTEST_LNSD_BIN points at a missing file: {p:?}"
        );
        return p;
    }
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../target"));

    // Candidate roots: the plain target dir plus any per-triple subdir (the
    // default host target here is x86_64-unknown-linux-musl, so binaries land
    // under <target>/<triple>/<profile>/lnsd, not <target>/<profile>/lnsd).
    let mut roots = vec![target.clone()];
    if let Ok(entries) = std::fs::read_dir(&target) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                // Only consider per-triple subdirs for the HOST architecture. A
                // machine that has cross-built (e.g. aarch64-unknown-linux-gnu on
                // an x86_64 host) leaves a wrong-arch `lnsd` here; picking it would
                // spawn with "Exec format error". The host arch string appears in
                // its own triple dir name (x86_64-*, aarch64-*), never in another's.
                let name = e.file_name();
                if name.to_string_lossy().contains(std::env::consts::ARCH) {
                    roots.push(e.path());
                }
            }
        }
    }
    for root in &roots {
        for profile in ["release", "debug"] {
            let cand = root.join(profile).join("lnsd");
            if cand.exists() {
                return cand;
            }
        }
    }
    panic!(
        "lnsd binary not found under {target:?}. Build it first, e.g.:\n  \
         CARGO_TARGET_DIR={} cargo build -p leviculum-cli --bin lnsd --release\n\
         or set LOADTEST_LNSD_BIN=<path>.",
        target.display()
    );
}

/// Spawn `lnsd` from a `type = BackboneInterface` server config on `port`,
/// with transport enabled, capturing stderr+stdout to a log file.
fn spawn_lnsd_hub(port: u16) -> LnsdHub {
    let bin = locate_lnsd();
    let config_dir = tempfile::Builder::new()
        .prefix("loadtest_lnsd_")
        .tempdir()
        .expect("config tempdir");

    // Backbone listen-only config: Config::load normalizes it onto our TCP
    // server exactly as Python does; enable_transport makes the hub relay.
    let config = format!(
        "[reticulum]\n\
         \x20 enable_transport = True\n\
         \n\
         [interfaces]\n\
         \x20 [[Backbone Load Hub]]\n\
         \x20   type = BackboneInterface\n\
         \x20   listen_on = 127.0.0.1\n\
         \x20   port = {port}\n"
    );
    std::fs::write(config_dir.path().join("config"), config).expect("write lnsd config");

    let log_path = config_dir.path().join("lnsd.log");
    let log = std::fs::File::create(&log_path).expect("create lnsd log");
    let log_err = log.try_clone().expect("clone log handle");

    let mut cmd = Command::new(&bin);
    cmd.arg("--config")
        .arg(config_dir.path())
        .env("NO_COLOR", "1")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    let child = spawn_supervised(cmd).unwrap_or_else(|e| panic!("spawn lnsd ({bin:?}): {e}"));
    let pid = child.id();
    let transport_id = wait_for_lnsd_transport_id(&log_path);

    LnsdHub {
        child,
        port,
        pid,
        transport_id,
        log_path,
        _config_dir: config_dir,
    }
}

/// Poll the lnsd startup log until the transport identity appears, returning its
/// 16-byte hash. lnsd logs `event="IDENTITY" node=<32 hex>` from the driver
/// builder at startup.
fn wait_for_lnsd_transport_id(log_path: &Path) -> [u8; 16] {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if let Ok(log) = std::fs::read_to_string(log_path) {
            for raw in log.lines() {
                let line = strip_ansi(raw);
                if !line.contains("event=\"IDENTITY\"") {
                    continue;
                }
                if let Some(pos) = line.find("node=") {
                    let hex: String = line[pos + 5..]
                        .chars()
                        .take_while(|c| c.is_ascii_hexdigit())
                        .collect();
                    if hex.len() >= 32 {
                        if let Ok(bytes) = hex::decode(&hex[..32]) {
                            return bytes.try_into().expect("16 bytes");
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("lnsd transport identity never appeared in log {log_path:?}");
}

/// Remove ANSI SGR escape sequences (`\x1b[...m`) so tracing's colorized output
/// can be matched with plain substring searches.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip until the terminating 'm' (SGR) or any letter.
            for e in chars.by_ref() {
                if e.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Decode a 16-byte hex hash.
fn parse_hash16(hex_str: &str) -> [u8; 16] {
    hex::decode(hex_str)
        .expect("hash hex")
        .try_into()
        .expect("16-byte hash")
}

// =========================================================================
// /proc sampling
// =========================================================================

fn read_vmrss_kb(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            // "VmRSS:\t   12345 kB"
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb);
        }
    }
    None
}

fn count_open_fds(pid: u32) -> Option<usize> {
    Some(std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?.count())
}

/// Clock ticks per second as `/proc` reports them. Both `/proc/<pid>/stat` and
/// `/proc/stat` are exported in USER_HZ, which Linux fixes at 100 whatever
/// CONFIG_HZ is, so ticks become seconds without a libc dependency.
const USER_HZ: f64 = 100.0;

/// `utime + stime` of `pid` in USER_HZ ticks, i.e. the CPU time that process
/// has burned since it started.
fn read_proc_cpu_ticks(pid: u32) -> Option<u64> {
    proc_cpu_ticks_from_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// Fields 14 (`utime`) and 15 (`stime`) of a `/proc/<pid>/stat` line.
///
/// Parsing starts after the LAST `)`, not at the second whitespace: field 2 is
/// the parenthesised comm and a comm may itself contain spaces and parentheses
/// (`(lnsd (hub))` is a legal name), which shifts every later field for a
/// naive splitter. Past the last `)` the remainder starts at field 3, so
/// `utime` is the 12th token there.
fn proc_cpu_ticks_from_stat(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

/// Machine-wide `(busy, total)` CPU ticks from `/proc/stat`'s aggregate line.
fn read_host_cpu_ticks() -> Option<(u64, u64)> {
    host_cpu_ticks_from_stat(&std::fs::read_to_string("/proc/stat").ok()?)
}

/// `(busy, total)` from the `cpu ` aggregate line. Idle AND iowait count as
/// not-busy: a hub waiting on a disk is not competing for a core.
fn host_cpu_ticks_from_stat(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().next()?;
    let mut it = line.split_whitespace();
    if it.next()? != "cpu" {
        return None;
    }
    // user nice system idle iowait irq softirq steal ...
    let v: Vec<u64> = it.take(8).filter_map(|f| f.parse::<u64>().ok()).collect();
    if v.len() < 5 {
        return None;
    }
    let total: u64 = v.iter().sum();
    let idle = v[3] + v[4];
    Some((total.saturating_sub(idle), total))
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    elapsed_ms: u128,
    phase: u8,
    rss_kb: u64,
    fds: usize,
    /// Hub process CPU ticks (cumulative).
    hub_cpu_ticks: u64,
    /// This test process's CPU ticks (cumulative) — the load generator and the
    /// sampler itself. #208's 99.5454 % run had the harness competing with the
    /// hub for the same four cores, so the harness's own cost is measured here
    /// rather than assumed small.
    gen_cpu_ticks: u64,
    /// Machine-wide busy / total CPU ticks (cumulative).
    host_busy_ticks: u64,
    host_total_ticks: u64,
}

/// CPU occupancy over the steady phase, the discriminator #208 asks for: a
/// delivery shortfall while nothing is saturated is a hub defect, a shortfall
/// past saturation is a load ceiling the gate should name.
#[derive(Clone, Copy, Debug)]
struct CpuLoad {
    /// Hub CPU, in percent of ONE core (may exceed 100 on a threaded hub).
    hub_pct: f64,
    /// Generator + sampler CPU, same unit.
    gen_pct: f64,
    /// Machine-wide busy time, in percent of all cores (0..100).
    host_busy_pct: f64,
    /// Span the rates were computed over, and how many samples it spanned.
    span_ms: u128,
    samples: usize,
}

/// Differentiate the cumulative counters across the steady phase. Needs two
/// steady samples at least a sampler interval apart; `None` when a run was too
/// short to have any (a rate from one sample is a division by zero, not a
/// small number).
fn steady_cpu(samples: &[Sample]) -> Option<CpuLoad> {
    let steady: Vec<&Sample> = samples.iter().filter(|s| s.phase == PHASE_STEADY).collect();
    let (first, last) = (steady.first()?, steady.last()?);
    let span_ms = last.elapsed_ms.checked_sub(first.elapsed_ms)?;
    if span_ms == 0 {
        return None;
    }
    let span_s = span_ms as f64 / 1000.0;
    let per_core = |ticks: u64| 100.0 * (ticks as f64 / USER_HZ) / span_s;
    let d_total = last.host_total_ticks.saturating_sub(first.host_total_ticks);
    Some(CpuLoad {
        hub_pct: per_core(last.hub_cpu_ticks.saturating_sub(first.hub_cpu_ticks)),
        gen_pct: per_core(last.gen_cpu_ticks.saturating_sub(first.gen_cpu_ticks)),
        host_busy_pct: if d_total == 0 {
            0.0
        } else {
            100.0 * last.host_busy_ticks.saturating_sub(first.host_busy_ticks) as f64
                / d_total as f64
        },
        span_ms,
        samples: steady.len(),
    })
}

// =========================================================================
// Client packet construction
// =========================================================================

/// Build the OUT Single destination for the sink from its announced public key.
/// Its hash must equal the sink's registered hash (sanity-checked by the caller).
fn sink_out_destination(pubkey_hex: &str) -> Destination {
    let pubkey = hex::decode(pubkey_hex).expect("sink pubkey hex");
    let identity = Identity::from_public_key_bytes(&pubkey).expect("sink identity from pubkey");
    Destination::new(
        Some(identity),
        Direction::Out,
        DestinationType::Single,
        "loadtest",
        &["sink"],
    )
    .expect("build sink OUT destination")
}

/// HDLC-framed, encrypted single Data packet carrying `b"LT" + id + seq`.
///
/// Emitted as a REAL RNS transport-addressed packet: HEADER_2 with
/// `transport_id = hub_transport_id` and `transport_type = TRANSPORT`, exactly
/// what a Python sender emits when its path to the sink runs through the hub as
/// the next hop. The hub (`transport_id == self`, `remaining_hops == 1`) strips
/// the transport header and forwards a HEADER_1 packet to the sink
/// (`Transport.py:1571`). Sending `transport_id = None` instead would be relayed
/// by our permissive lnsd but silently DROPPED by Python — see the A/B notes.
fn build_lt_frame(
    dest: &Destination,
    sink_hash: [u8; 16],
    hub_transport_id: [u8; 16],
    client_id: u32,
    seq: u32,
    rng: &mut rand_core::OsRng,
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(10);
    payload.extend_from_slice(b"LT");
    payload.extend_from_slice(&client_id.to_le_bytes());
    payload.extend_from_slice(&seq.to_le_bytes());

    let ciphertext = dest
        .encrypt(&payload, None, rng)
        .expect("encrypt load-test payload");

    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type2,
            context_flag: false,
            transport_type: TransportType::Transport,
            dest_type: DestinationType::Single,
            packet_type: PacketType::Data,
        },
        hops: 0,
        transport_id: Some(hub_transport_id),
        destination_hash: sink_hash,
        context: PacketContext::None,
        data: PacketData::Owned(ciphertext),
    };

    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).expect("pack load-test packet");
    let mut framed = Vec::new();
    frame(&buf[..len], &mut framed);
    framed
}

// =========================================================================
// Log-failure scanning (lnsd stderr)
// =========================================================================

/// Benign lines allowed even though they mention a disconnect/close. Client
/// churn *intentionally* tears down connections, so these are expected.
const LOG_ALLOW: &[&str] = &[
    "connection reset",
    "broken pipe",
    "closed by peer",
    "reset by peer",
    "client disconnected",
    "peer disconnected",
    "connection closed",
    "eof",
    "disconnected",
    "unexpected end of file",
];

/// Fatal patterns. A line matching any of these (and none of `LOG_ALLOW`) fails
/// the run. `panic`/`error` catch crashes and error-level logs; the rest are the
/// specific resource-exhaustion / overflow WARNs a struggling hub emits.
const LOG_DENY: &[&str] = &[
    "panic",
    "error", // tracing ERROR level or "error" text
    "too many open files",
    "os error 24", // EMFILE
    "os error 23", // ENFILE
    "leak",
    "buffer full",
    "queue full",
    "overflow",
    "dropping packet",
    "dropped packet",
    "capacity exceeded",
    "backtrace",
];

/// Returns the offending lines (empty = clean).
fn scan_log_for_failures(log: &str) -> Vec<String> {
    let mut bad = Vec::new();
    for raw in log.lines() {
        let line = strip_ansi(raw);
        let lc = line.to_ascii_lowercase();
        if LOG_ALLOW.iter().any(|a| lc.contains(a)) {
            continue;
        }
        if LOG_DENY.iter().any(|d| lc.contains(d)) {
            bad.push(line.to_string());
        }
    }
    bad
}

// =========================================================================
// Core run
// =========================================================================

struct LoadResult {
    conns: usize,
    churn_connections: u64,
    total_sent: u64,
    total_delivered: u64,
    losers: Vec<(u32, u32, u32)>, // (client_id, sent, delivered)
    samples: Vec<Sample>,
    baseline_rss_kb: u64,
    peak_rss_kb: u64,
    end_rss_kb: u64,
    baseline_fds: usize,
    peak_fds: usize,
    end_fds: usize,
    /// Steady-phase CPU occupancy of hub, generator and machine; `None` when
    /// the run was too short for two steady samples.
    cpu: Option<CpuLoad>,
    /// Cores the run had available, so a hub percentage can be read against the
    /// machine it was measured on.
    cores: usize,
}

impl LoadResult {
    fn delivery_pct(&self) -> f64 {
        if self.total_sent == 0 {
            0.0
        } else {
            100.0 * self.total_delivered as f64 / self.total_sent as f64
        }
    }
}

/// Drive `params` worth of load through a hub reachable at `hub_addr` whose
/// process is `hub_pid`, delivering to `sink`. Returns measured results; does
/// NOT assert (callers assert so the A/B path can compare instead).
async fn run_load(
    hub_addr: std::net::SocketAddr,
    hub_pid: u32,
    hub_transport_id: [u8; 16],
    sink: &TestDaemon,
    params: &LoadParams,
    hub_log: Option<&Path>,
) -> LoadResult {
    // --- Sink destination: register, connect to hub, announce. ---
    let dest_info = sink
        .register_destination("loadtest", &["sink"])
        .await
        .expect("register sink destination");
    let sink_hash: [u8; 16] = hex::decode(&dest_info.hash)
        .expect("sink hash hex")
        .try_into()
        .expect("sink hash 16 bytes");

    let out_dest = Arc::new(sink_out_destination(&dest_info.public_key));
    assert_eq!(
        out_dest.hash().as_bytes(),
        &sink_hash,
        "reconstructed sink OUT destination hash must match the registered hash"
    );

    sink.add_client_interface("127.0.0.1", hub_addr.port(), Some("SinkToHub"))
        .await
        .expect("sink connects to hub as TCP client");
    // NOTE: the announce is (re-)issued inside warm_up_path, not once here. A
    // single announce can race the sink's just-initiated client connection to
    // the hub; if it lands before the interface is up it is lost and — since a
    // Single destination only announces on demand — the hub would never learn
    // the path. Re-announcing until a probe packet is delivered makes the path
    // install deterministic (a real internet peer re-announces periodically).

    // --- Sampler: RSS + fd time series with phase labels. ---
    let phase = Arc::new(AtomicU8::new(PHASE_WARMUP));
    let sampler_phase = phase.clone();
    let sample_ms = params.sample_ms;
    let start = tokio::time::Instant::now();
    let sampler = tokio::spawn(async move {
        let mut samples = Vec::new();
        let self_pid = std::process::id();
        loop {
            let rss = read_vmrss_kb(hub_pid);
            let fds = count_open_fds(hub_pid);
            let hub_cpu = read_proc_cpu_ticks(hub_pid);
            match (rss, fds, hub_cpu) {
                (Some(rss_kb), Some(fds), Some(hub_cpu_ticks)) => {
                    let (host_busy_ticks, host_total_ticks) =
                        read_host_cpu_ticks().unwrap_or((0, 0));
                    samples.push(Sample {
                        elapsed_ms: start.elapsed().as_millis(),
                        phase: sampler_phase.load(Ordering::Relaxed),
                        rss_kb,
                        fds,
                        hub_cpu_ticks,
                        gen_cpu_ticks: read_proc_cpu_ticks(self_pid).unwrap_or(0),
                        host_busy_ticks,
                        host_total_ticks,
                    })
                }
                _ => break, // process gone
            }
            if sampler_phase.load(Ordering::Relaxed) == u8::MAX {
                break; // stop signal
            }
            tokio::time::sleep(Duration::from_millis(sample_ms)).await;
        }
        samples
    });

    // --- Warm-up: prove the client -> hub -> sink path is live before load. ---
    let warmup_ok = warm_up_path(
        hub_addr,
        &out_dest,
        sink_hash,
        hub_transport_id,
        sink,
        &dest_info.hash,
    )
    .await;
    if !warmup_ok {
        if let Some(p) = hub_log {
            let log = std::fs::read_to_string(p).unwrap_or_default();
            eprintln!("---- hub log (warm-up failure) ----\n{log}\n---- end hub log ----");
        }
    }
    assert!(
        warmup_ok,
        "warm-up failed: no packet reached the sink through the hub within the warm-up window \
         (path never installed)"
    );
    let baseline_rss_kb = read_vmrss_kb(hub_pid).expect("baseline rss");
    let baseline_fds = count_open_fds(hub_pid).expect("baseline fds");

    // --- Steady + churn phase. ---
    phase.store(PHASE_STEADY, Ordering::Relaxed);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(params.secs);

    let mut steady_tasks = Vec::with_capacity(params.conns);
    for id in 0..params.conns as u32 {
        let dest = out_dest.clone();
        let addr = hub_addr;
        let pkt_ms = params.pkt_ms;
        steady_tasks.push(tokio::spawn(async move {
            steady_client(
                addr,
                dest,
                sink_hash,
                hub_transport_id,
                id,
                pkt_ms,
                deadline,
            )
            .await
        }));
    }

    let churn_counter = Arc::new(AtomicU32::new(CHURN_ID_BASE));
    let churn_conn_count = Arc::new(AtomicU32::new(0));
    let mut churn_tasks = Vec::with_capacity(params.churn_workers);
    for _ in 0..params.churn_workers {
        let dest = out_dest.clone();
        let addr = hub_addr;
        let counter = churn_counter.clone();
        let conns = churn_conn_count.clone();
        let churn_pkts = params.churn_pkts;
        churn_tasks.push(tokio::spawn(async move {
            churn_worker(
                addr,
                dest,
                sink_hash,
                hub_transport_id,
                counter,
                conns,
                churn_pkts,
                deadline,
            )
            .await
        }));
    }

    // Collect exact per-client sent counts.
    let mut expected: HashMap<u32, u32> = HashMap::new();
    for t in steady_tasks {
        let (id, sent) = t.await.expect("steady task join");
        expected.insert(id, sent);
    }
    for t in churn_tasks {
        let per_conn: Vec<(u32, u32)> = t.await.expect("churn task join");
        for (id, sent) in per_conn {
            expected.insert(id, sent);
        }
    }
    let total_sent: u64 = expected.values().map(|&v| v as u64).sum();
    let churn_connections = churn_conn_count.load(Ordering::Relaxed) as u64;

    // --- Drain: wait for the sink to reach 100% (or plateau below = loss). ---
    phase.store(PHASE_DRAIN, Ordering::Relaxed);
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(params.drain_secs);
    let mut last_delivered = 0u64;
    let mut stable_polls = 0u32;
    loop {
        let (per_source, _total) = sink.get_loadtest_stats().await.expect("loadtest stats");
        let delivered: u64 = per_source
            .iter()
            .filter(|(id, _)| **id != WARMUP_CLIENT_ID)
            .map(|(_, (distinct, _, _))| *distinct as u64)
            .sum();
        if delivered >= total_sent {
            break;
        }
        if delivered == last_delivered {
            stable_polls += 1;
        } else {
            stable_polls = 0;
            last_delivered = delivered;
        }
        // 8 consecutive unchanged polls (~2s) after the deadline = genuine loss.
        if tokio::time::Instant::now() >= drain_deadline && stable_polls >= 8 {
            break;
        }
        if tokio::time::Instant::now() >= drain_deadline + Duration::from_secs(10) {
            break; // hard cap
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // --- Final tally + per-client verification. ---
    let (per_source, _total) = sink
        .get_loadtest_stats()
        .await
        .expect("final loadtest stats");
    let mut total_delivered = 0u64;
    let mut losers = Vec::new();
    for (&id, &sent) in &expected {
        let (distinct, min, max) = per_source.get(&id).copied().unwrap_or((0, None, None));
        total_delivered += distinct as u64;
        let contiguous = distinct == sent
            && (sent == 0 || (min == Some(0) && max == Some(sent.saturating_sub(1))));
        if !contiguous {
            losers.push((id, sent, distinct));
        }
    }
    losers.sort_unstable();

    // --- Stop sampler, read RSS/fd endpoints. ---
    phase.store(u8::MAX, Ordering::Relaxed);
    let samples = sampler.await.unwrap_or_default();
    let peak_rss_kb = samples
        .iter()
        .map(|s| s.rss_kb)
        .max()
        .unwrap_or(baseline_rss_kb);
    let peak_fds = samples.iter().map(|s| s.fds).max().unwrap_or(baseline_fds);
    let end_rss_kb = read_vmrss_kb(hub_pid).unwrap_or(baseline_rss_kb);
    let end_fds = count_open_fds(hub_pid).unwrap_or(baseline_fds);

    let cpu = steady_cpu(&samples);
    LoadResult {
        conns: params.conns,
        churn_connections,
        total_sent,
        total_delivered,
        losers,
        samples,
        baseline_rss_kb,
        peak_rss_kb,
        end_rss_kb,
        baseline_fds,
        peak_fds,
        end_fds,
        cpu,
        cores: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
    }
}

/// Repeatedly send one probe packet until the sink records it (path live) or the
/// window expires. Uses `WARMUP_CLIENT_ID`, excluded from delivery assertions.
async fn warm_up_path(
    hub_addr: std::net::SocketAddr,
    dest: &Destination,
    sink_hash: [u8; 16],
    hub_transport_id: [u8; 16],
    sink: &TestDaemon,
    sink_dest_hex: &str,
) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut rng = rand_core::OsRng;
    let mut seq = 0u32;
    let mut connect_errs = 0u32;
    while tokio::time::Instant::now() < deadline {
        // Re-announce every iteration so the hub installs the path even if the
        // first announce raced the sink's client-interface establishment.
        let _ = sink
            .announce_destination(sink_dest_hex, b"loadtest-sink")
            .await;
        match TcpStream::connect(hub_addr).await {
            Ok(mut stream) => {
                let framed = build_lt_frame(
                    dest,
                    sink_hash,
                    hub_transport_id,
                    WARMUP_CLIENT_ID,
                    seq,
                    &mut rng,
                );
                seq += 1;
                let _ = stream.write_all(&framed).await;
                let _ = stream.flush().await;
                // Keep it open briefly so the hub processes before FIN.
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            Err(e) => {
                connect_errs += 1;
                if connect_errs <= 3 {
                    eprintln!("warm-up: connect to hub {hub_addr} failed: {e}");
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        if let Ok((per_source, _)) = sink.get_loadtest_stats().await {
            if per_source.contains_key(&WARMUP_CLIENT_ID) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    eprintln!(
        "warm-up: gave up after {seq} probe packets, {connect_errs} connect errors to {hub_addr}"
    );
    false
}

/// One steady client: connect once, drain reads, send `seq++` every `pkt_ms`
/// until `deadline`. Returns `(client_id, packets_sent)`.
async fn steady_client(
    hub_addr: std::net::SocketAddr,
    dest: Arc<Destination>,
    sink_hash: [u8; 16],
    hub_transport_id: [u8; 16],
    client_id: u32,
    pkt_ms: u64,
    deadline: tokio::time::Instant,
) -> (u32, u32) {
    let stream = match TcpStream::connect(hub_addr).await {
        Ok(s) => s,
        Err(_) => return (client_id, 0),
    };
    let (mut rd, mut wr) = stream.into_split();
    // Drain anything the hub sends (sink announce, path requests) so its write
    // side to us never backs up.
    let drain = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        while let Ok(n) = rd.read(&mut buf).await {
            if n == 0 {
                break;
            }
        }
    });

    let mut rng = rand_core::OsRng;
    let mut seq = 0u32;
    let mut interval = tokio::time::interval(Duration::from_millis(pkt_ms.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let framed = build_lt_frame(&dest, sink_hash, hub_transport_id, client_id, seq, &mut rng);
        if wr.write_all(&framed).await.is_err() {
            break;
        }
        seq += 1;
    }
    let _ = wr.flush().await;
    // Graceful FIN after buffered bytes drain into the kernel.
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(wr);
    drain.abort();
    (client_id, seq)
}

/// One churn worker: until `deadline`, repeatedly open a connection with a fresh
/// client_id, send `churn_pkts` packets, flush, briefly settle, close. Returns
/// the `(client_id, sent)` for every connection it opened.
#[allow(clippy::too_many_arguments)]
async fn churn_worker(
    hub_addr: std::net::SocketAddr,
    dest: Arc<Destination>,
    sink_hash: [u8; 16],
    hub_transport_id: [u8; 16],
    counter: Arc<AtomicU32>,
    conn_count: Arc<AtomicU32>,
    churn_pkts: u32,
    deadline: tokio::time::Instant,
) -> Vec<(u32, u32)> {
    let mut rng = rand_core::OsRng;
    let mut out = Vec::new();
    while tokio::time::Instant::now() < deadline {
        let client_id = counter.fetch_add(1, Ordering::Relaxed);
        let mut stream = match TcpStream::connect(hub_addr).await {
            Ok(s) => s,
            Err(_) => {
                // Connection refused under load is a real failure signal; record
                // it as a zero-delivery connection so accounting catches it.
                out.push((client_id, churn_pkts));
                conn_count.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
        };
        conn_count.fetch_add(1, Ordering::Relaxed);
        let mut sent = 0u32;
        for seq in 0..churn_pkts {
            let framed =
                build_lt_frame(&dest, sink_hash, hub_transport_id, client_id, seq, &mut rng);
            if stream.write_all(&framed).await.is_err() {
                break;
            }
            sent += 1;
        }
        let _ = stream.flush().await;
        // Let the buffered bytes drain into the kernel and the hub process them
        // before the FIN, so a closing connection never loses in-flight packets.
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(stream);
        out.push((client_id, sent));
    }
    out
}

// =========================================================================
// Delivery record: one DELIVERY line per run, green and red (#208)
// =========================================================================

/// Two-sided 95% standard normal quantile, `Φ⁻¹(0.975)`.
const Z_95: f64 = 1.959_963_984_540_053_6;

/// 95% Wilson score interval for `recv` of `sent`, in percent.
///
/// Wilson (JASA 22(158), 1927), not the normal approximation, for the reason
/// periculum's `ratio.rs` gives: at `recv == sent` the normal interval collapses
/// to zero width and claims certainty, which is exactly where a 100 %-delivery
/// run sits. `sent == 0` measured nothing and gets `[0, 100]`.
fn wilson_95_pct(recv: u64, sent: u64) -> (f64, f64) {
    if sent == 0 {
        return (0.0, 100.0);
    }
    let n = sent as f64;
    let p = recv.min(sent) as f64 / n;
    let z2 = Z_95 * Z_95;
    let denom = 1.0 + z2 / n;
    let centre = (p + z2 / (2.0 * n)) / denom;
    let half = (Z_95 / denom) * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    (
        ((centre - half) * 100.0).clamp(0.0, 100.0),
        ((centre + half) * 100.0).clamp(0.0, 100.0),
    )
}

/// Drop decimals a number does not need: `99.5000` -> `99.5`, `100.0000` -> `100`.
fn trim_zeros(text: &str) -> String {
    if !text.contains('.') {
        return text.to_string();
    }
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The interval as `low-high` percentages without the `%`, each end rounded
/// OUTWARD so the printed interval always contains the computed one.
///
/// Four decimals, not periculum's two significant digits: a hub run offers
/// hundreds of thousands of packets, where `99.5-100` would erase the very
/// shortfall this line exists to record (#208's run was 99.5454 %).
fn ci95_text(recv: u64, sent: u64) -> String {
    if sent == 0 {
        return "n/a".to_string();
    }
    let (low, high) = wilson_95_pct(recv, sent);
    let scale = 10_000.0;
    format!(
        "{}-{}",
        trim_zeros(&format!("{:.4}", (low * scale).floor() / scale)),
        trim_zeros(&format!("{:.4}", (high * scale).ceil() / scale))
    )
}

/// The point estimate for the `pct=` field.
///
/// Four decimals normally, six when four would round a shortfall up onto a flat
/// `100.0000`: a line that spells a lossy run as 100 % is worse than no line,
/// and this log is read by grepping for what is not 100.
fn pct_text(recv: u64, sent: u64) -> String {
    if sent == 0 {
        return "n/a".to_string();
    }
    let pct = 100.0 * recv.min(sent) as f64 / sent as f64;
    let four = format!("{pct:.4}");
    if recv < sent && four == "100.0000" {
        return format!("{pct:.6}");
    }
    four
}

/// One `DELIVERY` line for a completed run.
///
/// Field names follow the project's `LEVICULUM_DELIVERY_LOG` convention
/// (`test= sent= recv= pct= ci95=`, periculum's `executor.rs`) and add the cell
/// coordinates and the CPU occupancy. The coordinates are not optional: the same
/// rule that puts `interval_ms=` on a benchmark line applies here — two runs at
/// different connection counts or pacings offered different volumes, so a line
/// that does not name its cell cannot be pooled with another. The CPU fields are
/// what makes the sweep interpretable at all: delivery below 100 % while nothing
/// is saturated is a hub defect, below 100 % past saturation is a load ceiling.
fn delivery_line(test: &str, res: &LoadResult, params: &LoadParams) -> String {
    let sent = res.total_sent;
    let recv = res.total_delivered;
    let cpu = res
        .cpu
        .map(|c| {
            format!(
                "hub_cpu_pct={:.1} gen_cpu_pct={:.1} host_busy_pct={:.1}",
                c.hub_pct, c.gen_pct, c.host_busy_pct
            )
        })
        .unwrap_or_else(|| "hub_cpu_pct=n/a gen_cpu_pct=n/a host_busy_pct=n/a".to_string());
    format!(
        "DELIVERY test={test} sent={sent} recv={recv} pct={} ci95={} \
         conns={} pkt_ms={} secs={} churn_workers={} churn_conns={} cores={} {cpu}",
        pct_text(recv, sent),
        ci95_text(recv, sent),
        params.conns,
        params.pkt_ms,
        params.secs,
        params.churn_workers,
        res.churn_connections,
        res.cores,
    )
}

/// Append one line to the delivery log, creating it if absent.
///
/// Failures are reported and not fatal — a missing distribution must not turn a
/// green run red — but they are LOUD, because a sweep that silently logs nothing
/// looks exactly like a sweep that found nothing.
fn append_delivery_line(path: &Path, line: &str) {
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{line}") {
                eprintln!(
                    "WARNING: LEVICULUM_DELIVERY_LOG write to {} failed: {e}",
                    path.display()
                );
            }
        }
        Err(e) => eprintln!(
            "WARNING: LEVICULUM_DELIVERY_LOG open {} failed: {e}",
            path.display()
        ),
    }
}

/// Print the run's `DELIVERY` line and, when `LEVICULUM_DELIVERY_LOG` names a
/// file, append it there too — for red runs as well as green, which is the whole
/// point: a sweep reads the distribution, and the cells that interest it are the
/// ones that failed.
fn record_delivery(test: &str, res: &LoadResult, params: &LoadParams) {
    let line = delivery_line(test, res, params);
    println!("{line}");
    if let Ok(path) = std::env::var("LEVICULUM_DELIVERY_LOG") {
        append_delivery_line(Path::new(&path), &line);
    }
}

// =========================================================================
// Assertions + reporting
// =========================================================================

fn report_and_assert(label: &str, res: &LoadResult, params: &LoadParams, log_failures: &[String]) {
    println!("\n===== TCP load test: {label} =====");
    println!(
        "params: conns={} secs={} pkt_ms={} churn_workers={} churn_pkts={}",
        params.conns, params.secs, params.pkt_ms, params.churn_workers, params.churn_pkts
    );
    println!(
        "connections: {} steady + {} churn = {} total",
        res.conns,
        res.churn_connections,
        res.conns as u64 + res.churn_connections
    );
    println!(
        "delivery: sent={} delivered={} ({:.4}%)",
        res.total_sent,
        res.total_delivered,
        res.delivery_pct()
    );
    println!(
        "RSS kB: baseline={} peak={} end={} (growth {:.1}% over baseline)",
        res.baseline_rss_kb,
        res.peak_rss_kb,
        res.end_rss_kb,
        100.0 * (res.peak_rss_kb as f64 - res.baseline_rss_kb as f64) / res.baseline_rss_kb as f64
    );
    println!(
        "fds: baseline={} peak={} end={}",
        res.baseline_fds, res.peak_fds, res.end_fds
    );
    print_cpu(res);
    print_rss_series(&res.samples);

    // The delivery record goes out BEFORE the assertions, so a run that fails
    // one of them still contributes its cell to the distribution (#208).
    record_delivery(label, res, params);

    // --- 1. Delivery: exactly 100%, every client contiguous. ---
    assert!(
        res.losers.is_empty(),
        "{label}: {} client(s) lost packets (TCP is lossless -> real hub bug). \
         First offenders (id, sent, delivered): {:?}",
        res.losers.len(),
        &res.losers[..res.losers.len().min(10)]
    );
    assert_eq!(
        res.total_delivered, res.total_sent,
        "{label}: aggregate delivery must be exactly sent (100%)"
    );

    // --- 2. RSS: must PLATEAU under sustained load + churn, not climb. ---
    // A steady population of `conns` connections legitimately costs memory, so
    // "RSS grew from idle baseline to steady" is expected, not a leak. The leak
    // signal is a CONTINUOUS climb across the steady+churn phase (thousands of
    // connections churned): per gross-vs-net, a per-connection allocation not
    // freed on disconnect keeps rising. We compare the first vs second half of
    // the steady-phase RSS samples. A floor (LOADTEST_RSS_GROWTH_FLOOR_MIB)
    // suppresses false positives from small absolute noise on tiny baselines.
    let steady: Vec<u64> = res
        .samples
        .iter()
        .filter(|s| s.phase == PHASE_STEADY)
        .map(|s| s.rss_kb)
        .collect();
    let floor_kb = env_u64("LOADTEST_RSS_GROWTH_FLOOR_MIB", 24) * 1024;
    if steady.len() >= 4 {
        let mid = steady.len() / 2;
        let early_avg = steady[..mid].iter().sum::<u64>() / mid as u64;
        let late_avg = steady[mid..].iter().sum::<u64>() / (steady.len() - mid) as u64;
        let climb_kb = late_avg.saturating_sub(early_avg);
        let climb_pct = 100 * climb_kb / early_avg.max(1);
        println!(
            "rss plateau: early_avg={early_avg}kB late_avg={late_avg}kB climb={climb_kb}kB ({climb_pct}%)"
        );
        // Fail only when BOTH the proportional AND absolute climb are exceeded,
        // so a plateau with small jitter never trips.
        assert!(
            climb_pct <= params.max_rss_growth_pct || climb_kb <= floor_kb,
            "{label}: RSS climbed {climb_kb}kB ({climb_pct}%) from early to late steady phase \
             (early {early_avg} -> late {late_avg} kB), exceeds both {}% and {}kB floor \
             (per-connection leak suspected — RSS not plateauing under churn)",
            params.max_rss_growth_pct,
            floor_kb
        );
    }
    // Runaway backstop: peak must stay under an absolute ceiling over baseline.
    let abs_ceiling_kb = res.baseline_rss_kb + params.max_rss_abs_mib * 1024;
    assert!(
        res.peak_rss_kb <= abs_ceiling_kb,
        "{label}: peak RSS {} kB exceeds absolute ceiling {} kB (baseline + {} MiB)",
        res.peak_rss_kb,
        abs_ceiling_kb,
        params.max_rss_abs_mib
    );

    // --- 3. fds: bounded under churn, released afterward. ---
    // Legitimate peak = baseline (listener + sink conn) + one fd per steady
    // connection + churn concurrency (each worker holds one at a time) + margin.
    // A per-connection fd leak under churn would blow far past this AND leave
    // end_fds elevated (checked below).
    let fd_ceiling = res.baseline_fds + res.conns + params.churn_workers + 32;
    assert!(
        res.peak_fds <= fd_ceiling,
        "{label}: peak fd count {} exceeds ceiling {} (baseline {} + conns {} + churn {} + 32); \
         fd leak under churn",
        res.peak_fds,
        fd_ceiling,
        res.baseline_fds,
        res.conns,
        params.churn_workers
    );
    // After steady clients close + drain, fds must fall back near baseline.
    assert!(
        res.end_fds <= res.baseline_fds + 16,
        "{label}: end fd count {} did not return near baseline {} (+16) after teardown; \
         per-connection fds not released",
        res.end_fds,
        res.baseline_fds
    );

    // --- 4. Log-failure patterns. ---
    assert!(
        log_failures.is_empty(),
        "{label}: hub log contains {} fatal/bad line(s): {:#?}",
        log_failures.len(),
        &log_failures[..log_failures.len().min(20)]
    );

    println!("PASS: {label}");
}

/// Steady-phase CPU occupancy: the hub, the harness driving it, and the machine.
///
/// The harness's own figure is on the line because #208's shortfall was measured
/// with the harness competing for the same four cores; a reader of a sweep cell
/// has to be able to see whether the generator, not the hub, ate the machine.
fn print_cpu(res: &LoadResult) {
    match res.cpu {
        Some(c) => println!(
            "cpu (steady, {} samples over {} ms, {} cores): hub={:.1}% of one core \
             ({:.1}% of machine) generator+sampler={:.1}% of one core host_busy={:.1}%",
            c.samples,
            c.span_ms,
            res.cores,
            c.hub_pct,
            c.hub_pct / res.cores as f64,
            c.gen_pct,
            c.host_busy_pct
        ),
        None => println!(
            "cpu (steady): <not measurable: fewer than two steady samples; \
             raise LOADTEST_SECS or lower LOADTEST_SAMPLE_MS>"
        ),
    }
}

/// Compact RSS/fd series: one line per phase transition + phase min/max.
fn print_rss_series(samples: &[Sample]) {
    if samples.is_empty() {
        println!("rss/fd series: <no samples>");
        return;
    }
    println!("rss/fd time series ({} samples):", samples.len());
    let mut cur_phase = u8::MAX;
    for s in samples {
        if s.phase != cur_phase {
            cur_phase = s.phase;
            println!("  -- phase: {} --", phase_name(s.phase));
        }
        println!(
            "  t={:>6}ms rss={:>8}kB fd={}",
            s.elapsed_ms, s.rss_kb, s.fds
        );
    }
}

fn read_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

// =========================================================================
// Tests
// =========================================================================

/// On-demand smoke variant (~15 s): small N/T, real `lnsd` process, full
/// assertions. `#[ignore]` because it spawns the `lnsd` binary, which the
/// leviculum-std test build does not produce (build it first, see module docs).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns lnsd binary; run on demand"]
async fn loadtest_tcp_hub_smoke() {
    init_tracing();
    let params = LoadParams::smoke();
    let port = pick_free_tcp_port().expect("hub port");
    let hub = spawn_lnsd_hub(port);
    // Give the listener time to bind.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let sink = TestDaemon::start().await.expect("start sink daemon");

    let res = run_load(
        ([127, 0, 0, 1], hub.port).into(),
        hub.pid,
        hub.transport_id,
        &sink,
        &params,
        Some(&hub.log_path),
    )
    .await;

    // Read the hub log AFTER the run but BEFORE dropping the hub (whose Drop
    // deletes the config tempdir holding the log).
    let log = read_log(&hub.log_path);
    let log_failures = scan_log_for_failures(&log);
    report_and_assert("lnsd_smoke", &res, &params, &log_failures);
}

/// Heavy soak: env-tunable, defaults 200 conns / 60 s, real `lnsd` process.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "heavy on-demand / nightly load test"]
async fn loadtest_tcp_hub_soak() {
    init_tracing();
    let params = LoadParams::soak();
    let port = pick_free_tcp_port().expect("hub port");
    let hub = spawn_lnsd_hub(port);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let sink = TestDaemon::start().await.expect("start sink daemon");

    let res = run_load(
        ([127, 0, 0, 1], hub.port).into(),
        hub.pid,
        hub.transport_id,
        &sink,
        &params,
        Some(&hub.log_path),
    )
    .await;

    let log = read_log(&hub.log_path);
    let log_failures = scan_log_for_failures(&log);
    report_and_assert("lnsd_soak", &res, &params, &log_failures);
}

/// A/B: the SAME generator drives our `lnsd` and a real Python `rnsd`
/// (`TestDaemon`, transport hub). lnsd must be at least as good as rnsd on
/// delivery + RSS trend. If rnsd also loses packets, the generator is wrong
/// (not lnsd) — which validates the test itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "A/B load test vs real rnsd; run on demand"]
async fn loadtest_tcp_hub_ab_vs_rnsd() {
    init_tracing();
    // Smaller than full soak so the A/B pair completes promptly, but identical
    // for both stacks.
    let mut params = LoadParams::soak();
    params.conns = env_usize("LOADTEST_CONNS", 100);
    params.secs = env_u64("LOADTEST_SECS", 30);

    // ---- A: our lnsd hub. ----
    let a_port = pick_free_tcp_port().expect("lnsd hub port");
    let a_hub = spawn_lnsd_hub(a_port);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let a_sink = TestDaemon::start().await.expect("start sink daemon (A)");
    let a_res = run_load(
        ([127, 0, 0, 1], a_hub.port).into(),
        a_hub.pid,
        a_hub.transport_id,
        &a_sink,
        &params,
        Some(&a_hub.log_path),
    )
    .await;
    let a_log = read_log(&a_hub.log_path);
    let a_failures = scan_log_for_failures(&a_log);
    drop(a_sink);
    drop(a_hub);

    // ---- B: real Python rnsd hub (TestDaemon, transport-enabled TCP server). ----
    let b_hub = TestDaemon::start().await.expect("start rnsd hub");
    let b_pid = b_hub.pid();
    let b_addr: std::net::SocketAddr = ([127, 0, 0, 1], b_hub.rns_port()).into();
    let b_transport_id = parse_hash16(
        &b_hub
            .get_transport_status()
            .await
            .expect("rnsd transport status")
            .identity_hash
            .expect("rnsd transport identity hash"),
    );
    let b_sink = TestDaemon::start().await.expect("start sink daemon (B)");
    let b_res = run_load(b_addr, b_pid, b_transport_id, &b_sink, &params, None).await;
    // The reference arm is not asserted, so it records its own cell: an A/B pair
    // in one distribution is exactly what the #198 measurement produced, and the
    // reference's delivery under the same load is worth keeping beside ours.
    record_delivery("rnsd_ab_arm", &b_res, &params);

    // ---- Report both, then assert lnsd >= rnsd. ----
    println!("\n########## A/B: lnsd vs rnsd ##########");
    println!(
        "lnsd : delivery {:.4}% ({}/{}), RSS {}->{} kB, fds {}->{}",
        a_res.delivery_pct(),
        a_res.total_delivered,
        a_res.total_sent,
        a_res.baseline_rss_kb,
        a_res.peak_rss_kb,
        a_res.baseline_fds,
        a_res.peak_fds
    );
    println!(
        "rnsd : delivery {:.4}% ({}/{}), RSS {}->{} kB, fds {}->{}",
        b_res.delivery_pct(),
        b_res.total_delivered,
        b_res.total_sent,
        b_res.baseline_rss_kb,
        b_res.peak_rss_kb,
        b_res.baseline_fds,
        b_res.peak_fds
    );

    // Generator-validity: the SAME generator drives both stacks. If lnsd hits a
    // clean 100% (asserted below via report_and_assert), the generator provably
    // emits deliverable RNS traffic — so any rnsd shortfall is a real difference
    // in the reference stack, NOT a broken generator. (The "if the reference
    // also loses, blame the generator" heuristic only bites when lnsd ALSO
    // loses; here lnsd is the witness that 100% is achievable.)
    if b_res.total_delivered < b_res.total_sent {
        println!(
            "NOTE: rnsd delivered {}/{} ({:.4}%) under identical sustained load + churn while \
             lnsd delivered 100% ({}/{}). Same generator -> generator is validated by lnsd \
             (100% is provably achievable), so rnsd's shortfall is a real difference in the \
             Python reference, not a broken generator. rnsd is the reference (not under test); \
             its loss mechanism is not further diagnosed here.",
            b_res.total_delivered,
            b_res.total_sent,
            b_res.delivery_pct(),
            a_res.total_delivered,
            a_res.total_sent,
        );
    }

    // lnsd must be at least as good as rnsd on delivery.
    assert!(
        a_res.delivery_pct() >= b_res.delivery_pct() - 1e-9,
        "lnsd delivery {:.4}% < rnsd {:.4}% (lnsd must be at least as good as the reference)",
        a_res.delivery_pct(),
        b_res.delivery_pct()
    );
    // lnsd's own hard bar: exactly 100% delivery, bounded RSS/fds, clean logs.
    report_and_assert("lnsd_ab_arm", &a_res, &params, &a_failures);
}

// =========================================================================
// Unit tests for the delivery record and the /proc arithmetic (#208)
//
// Pure, so they run in the normal `cargo test -p leviculum-std --test
// rnsd_interop` pass without an lnsd binary or a Python daemon. They exist
// because the sweep the issue asks for is read off these lines: a `pct=` that
// rounds a lossy run onto 100, or a cell that forgets its coordinates, turns a
// measurement into a number nobody can interpret.
// =========================================================================

#[cfg(test)]
mod delivery_record_tests {
    use super::*;

    /// The run #208 recorded: 1715 short of 377,285. `pct=` must spell it, and
    /// the interval must exclude 100 %, which is the whole claim of the line.
    #[test]
    fn records_the_208_shortfall_at_full_precision() {
        let (recv, sent) = (377_285u64 - 1715, 377_285u64);
        assert_eq!(pct_text(recv, sent), "99.5454");
        assert_eq!(ci95_text(recv, sent), "99.5234-99.5665");
    }

    /// A shortfall too small for four decimals must not be spelled `100.0000`:
    /// this log is read by grepping for what is not 100.
    #[test]
    fn a_shortfall_never_renders_as_a_flat_hundred() {
        assert_eq!(pct_text(9_999_999, 10_000_000), "99.999990");
        assert_eq!(pct_text(10_000_000, 10_000_000), "100.0000");
    }

    /// A clean run gets 100 % as a point estimate and an interval that still has
    /// a lower end — which is why Wilson and not the normal approximation, whose
    /// interval here would be `[100, 100]`.
    #[test]
    fn a_clean_run_keeps_an_interval() {
        assert_eq!(pct_text(1000, 1000), "100.0000");
        assert_eq!(ci95_text(1000, 1000), "99.6173-100");
    }

    /// The textbook case from periculum's `ratio.rs`: 1 of 7 is consistent with
    /// a hub delivering 2.5 % and with one delivering 51 %.
    #[test]
    fn wilson_matches_the_worked_example() {
        assert_eq!(ci95_text(1, 7), "2.5679-51.3128");
        let (low, high) = wilson_95_pct(1, 7);
        assert!((low - 2.567_962).abs() < 1e-5, "low was {low}");
        assert!((high - 51.312_783).abs() < 1e-5, "high was {high}");
    }

    /// Nothing offered is not 0 % delivery; it is no measurement.
    #[test]
    fn zero_trials_claim_nothing() {
        assert_eq!(pct_text(0, 0), "n/a");
        assert_eq!(ci95_text(0, 0), "n/a");
        assert_eq!(wilson_95_pct(0, 0), (0.0, 100.0));
    }

    fn sample(elapsed_ms: u128, phase: u8, hub: u64, gen: u64, busy: u64, total: u64) -> Sample {
        Sample {
            elapsed_ms,
            phase,
            rss_kb: 1024,
            fds: 10,
            hub_cpu_ticks: hub,
            gen_cpu_ticks: gen,
            host_busy_ticks: busy,
            host_total_ticks: total,
        }
    }

    fn result_for(sent: u64, recv: u64, samples: Vec<Sample>) -> LoadResult {
        let cpu = steady_cpu(&samples);
        LoadResult {
            conns: 128,
            churn_connections: 42,
            total_sent: sent,
            total_delivered: recv,
            losers: Vec::new(),
            samples,
            baseline_rss_kb: 1024,
            peak_rss_kb: 2048,
            end_rss_kb: 1024,
            baseline_fds: 10,
            peak_fds: 140,
            end_fds: 11,
            cpu,
            cores: 4,
        }
    }

    /// Cumulative tick counters become rates: 50 ticks of hub time over one
    /// second is half a core; the warm-up and drain samples must not dilute it.
    #[test]
    fn steady_cpu_differentiates_only_the_steady_phase() {
        let samples = vec![
            sample(0, PHASE_WARMUP, 0, 0, 0, 0),
            sample(1_000, PHASE_STEADY, 100, 400, 1_000, 1_000),
            sample(2_000, PHASE_STEADY, 150, 600, 1_300, 1_400),
            sample(3_000, PHASE_DRAIN, 999, 999, 9_999, 9_999),
        ];
        let cpu = steady_cpu(&samples).expect("two steady samples");
        assert!((cpu.hub_pct - 50.0).abs() < 1e-6, "hub {}", cpu.hub_pct);
        assert!((cpu.gen_pct - 200.0).abs() < 1e-6, "gen {}", cpu.gen_pct);
        assert!(
            (cpu.host_busy_pct - 75.0).abs() < 1e-6,
            "host {}",
            cpu.host_busy_pct
        );
        assert_eq!(cpu.span_ms, 1_000);
        assert_eq!(cpu.samples, 2);
    }

    /// One steady sample is no rate. Reporting 0 % CPU for a run too short to
    /// measure would read as an idle hub.
    #[test]
    fn a_single_steady_sample_yields_no_rate() {
        let samples = vec![
            sample(0, PHASE_WARMUP, 0, 0, 0, 0),
            sample(1_000, PHASE_STEADY, 100, 400, 1_000, 1_000),
        ];
        assert!(steady_cpu(&samples).is_none());
    }

    /// Every field a sweep needs to place a cell, on one line.
    #[test]
    fn the_delivery_line_names_its_cell_and_its_cpu() {
        let samples = vec![
            sample(1_000, PHASE_STEADY, 100, 400, 1_000, 1_000),
            sample(2_000, PHASE_STEADY, 150, 600, 1_300, 1_400),
        ];
        let res = result_for(377_285, 377_285 - 1715, samples);
        let mut params = LoadParams::soak();
        params.conns = 128;
        params.pkt_ms = 15;
        params.secs = 20;
        params.churn_workers = 16;
        let line = delivery_line("lnsd_soak", &res, &params);
        assert_eq!(
            line,
            "DELIVERY test=lnsd_soak sent=377285 recv=375570 pct=99.5454 \
             ci95=99.5234-99.5665 conns=128 pkt_ms=15 secs=20 churn_workers=16 \
             churn_conns=42 cores=4 hub_cpu_pct=50.0 gen_cpu_pct=200.0 host_busy_pct=75.0"
        );
    }

    /// A run whose CPU could not be measured says so on the line instead of
    /// claiming zero occupancy.
    #[test]
    fn an_unmeasured_cell_says_so() {
        let res = result_for(100, 100, Vec::new());
        let line = delivery_line("lnsd_smoke", &res, &LoadParams::smoke());
        assert!(
            line.contains("hub_cpu_pct=n/a gen_cpu_pct=n/a host_busy_pct=n/a"),
            "line was {line}"
        );
    }

    /// A comm with spaces and parentheses shifts every field for a naive
    /// splitter; parsing past the last `)` is what makes utime/stime right.
    #[test]
    fn proc_stat_parsing_survives_a_hostile_comm() {
        let stat = "1234 (lnsd (hub)) S 1 1234 1234 0 -1 4194304 100 0 0 0 4321 1234 0 0 \
                    20 0 8 0 100 0 0";
        assert_eq!(proc_cpu_ticks_from_stat(stat), Some(4321 + 1234));
    }

    /// iowait counts as not-busy: a hub blocked on a disk is not eating a core.
    #[test]
    fn host_cpu_excludes_idle_and_iowait() {
        let stat = "cpu  100 10 40 700 50 5 5 0 0 0\ncpu0 1 2 3 4 5 6 7 0 0 0\n";
        let (busy, total) = host_cpu_ticks_from_stat(stat).expect("aggregate line");
        // user + nice + system + idle + iowait + irq + softirq (+ steal, 0 here)
        assert_eq!(total, 100 + 10 + 40 + 700 + 50 + 5 + 5);
        assert_eq!(busy, total - 700 - 50);
    }

    /// Cells accumulate: a sweep of N runs must leave N lines in one file, in
    /// order, which is what makes the log a distribution rather than a last
    /// result. (The env var itself is not set here — mutating the process
    /// environment while the other tests in this binary run in parallel threads
    /// is not worth the coverage.)
    #[test]
    fn the_log_accumulates_one_line_per_run() {
        let dir = tempfile::Builder::new()
            .prefix("delivery_log_")
            .tempdir()
            .expect("tempdir");
        let path = dir.path().join("delivery.log");
        let params = LoadParams::smoke();
        let first = delivery_line("lnsd_smoke", &result_for(10, 10, Vec::new()), &params);
        let second = delivery_line("lnsd_smoke", &result_for(10, 9, Vec::new()), &params);

        append_delivery_line(&path, &first);
        append_delivery_line(&path, &second);

        let logged = std::fs::read_to_string(&path).expect("delivery log written");
        assert_eq!(
            logged.lines().collect::<Vec<_>>(),
            vec![first.as_str(), second.as_str()]
        );
    }

    /// An unwritable path costs the distribution, not the run.
    #[test]
    fn an_unwritable_log_does_not_kill_the_run() {
        append_delivery_line(
            Path::new("/proc/definitely/not/writable"),
            "DELIVERY test=x",
        );
    }
}
