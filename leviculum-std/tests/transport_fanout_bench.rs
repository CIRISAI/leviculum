//! Transport concurrency-ceiling benchmark — leviculum#29.
//!
//! One TCP-**server** "serve" node plus N TCP-**client** nodes. Each client
//! establishes a link to the serve node's destination and then pumps a fixed
//! number of link-data packets at it. We measure the serve node's aggregate
//! **inbound** throughput — link payloads decrypted + routed per second,
//! counted via link message events (`MessageReceived`/`LinkDataReceived`) — as
//! N scales.
//!
//! The serve node runs its whole transport (every inbound packet's
//! decrypt/route) in one event-loop task behind one synchronous
//! `Mutex<StdNodeCore>`, so aggregate throughput does not scale with N: it
//! plateaus (and, once establishment/handshake churn dominates the single
//! loop's time, cliffs). This harness is the leviculum-native before/after
//! instrument for any change that widens that ceiling — it needs no downstream
//! ciris-server wheels or Docker, unlike CIRISServer's `run_load_repro.sh`.
//!
//! Ignored by default (it spins up many nodes and takes tens of seconds). Run:
//!
//! ```text
//! cargo test -p leviculum-std --test transport_fanout_bench -- --ignored --nocapture
//! # tune the sweep and load:
//! SIZES="1 20 40 60" PACKETS=200 PAYLOAD=32 \
//!   cargo test -p leviculum-std --test transport_fanout_bench -- --ignored --nocapture
//! ```

use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::NodeEvent;

/// Port band chosen to avoid collisions with the mvr/interop suites in a shared
/// `cargo test` invocation. This bench is `--ignored` so it normally runs alone.
static PORT_COUNTER: AtomicU16 = AtomicU16::new(61000);

fn next_port() -> u16 {
    loop {
        let candidate = PORT_COUNTER.fetch_add(1, Ordering::Relaxed);
        if candidate >= 62500 {
            PORT_COUNTER.store(61000, Ordering::Relaxed);
            continue;
        }
        if StdTcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return candidate;
        }
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct LevelResult {
    n: usize,
    established: usize,
    /// Total link-data packets the serve node received.
    received: usize,
    /// Packets each client attempted to send.
    target: usize,
    elapsed: Duration,
}

impl LevelResult {
    fn throughput(&self) -> f64 {
        if self.elapsed.as_secs_f64() > 0.0 {
            self.received as f64 / self.elapsed.as_secs_f64()
        } else {
            0.0
        }
    }
}

/// A live client node holding an established link to the serve node.
struct Client {
    node: ReticulumNode,
    link_id: leviculum_core::link::LinkId,
    /// Kept so the client's own event stream drains (prevents unbounded growth).
    _drain: tokio::task::JoinHandle<()>,
}

/// Spin up the serve node: TCP server, one registered destination, an event
/// drain that counts inbound `LinkDataReceived`. Returns the node, its dest
/// hash + signing key, the shared inbound counter, the responder-side link ids
/// (as clients establish), and the drain handle.
async fn build_serve_node() -> (
    ReticulumNode,
    SocketAddr,
    leviculum_core::DestinationHash,
    [u8; 32],
    Arc<AtomicUsize>,
    Arc<std::sync::Mutex<Vec<leviculum_core::link::LinkId>>>,
    tokio::task::JoinHandle<()>,
) {
    let addr: SocketAddr = format!("127.0.0.1:{}", next_port()).parse().unwrap();
    let storage = tempfile::tempdir().expect("serve tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server(addr)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build serve node");
    // Leak the tempdir for the process lifetime of the bench (kept simple).
    std::mem::forget(storage);
    node.start().await.expect("start serve node");

    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "bench",
        &["fanout"],
    )
    .expect("serve destination");
    let hash = *dest.hash();
    node.register_destination(dest);

    let received = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&received);
    let responder_links = Arc::new(std::sync::Mutex::new(Vec::new()));
    let links_sink = Arc::clone(&responder_links);
    let mut rx = node.take_event_receiver().expect("serve event rx");
    let drain = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            // `try_send`/`send_on_link` payloads surface as channel messages
            // (`MessageReceived`); `LinkDataReceived` is the raw non-channel
            // variant. Count both so the metric tracks decrypted+routed link
            // payloads regardless of framing.
            match ev {
                NodeEvent::LinkDataReceived { .. } | NodeEvent::MessageReceived { .. } => {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                NodeEvent::LinkEstablished {
                    link_id,
                    is_initiator: false,
                } => {
                    links_sink.lock().unwrap().push(link_id);
                }
                _ => {}
            }
        }
    });

    (
        node,
        addr,
        hash,
        signing_key,
        received,
        responder_links,
        drain,
    )
}

async fn run_level(n: usize, packets: usize, payload: usize) -> LevelResult {
    let (serve, serve_addr, hash, signing_key, received, _links, _serve_drain) =
        build_serve_node().await;

    let clients = bring_up_fleet(&serve, serve_addr, hash, signing_key, n).await;
    let established = clients.len();
    eprintln!("[bench] N={n}: established {established}/{n} links");

    // Load phase: every client pumps `packets` link-data packets as fast as the
    // serve node will take them. Time from first send to the serve node having
    // decrypted+routed all of them (or a bounded deadline).
    let payload_bytes = vec![0xABu8; payload];
    received.store(0, Ordering::Relaxed);
    let target_total = established * packets;

    let start = Instant::now();
    let mut senders = Vec::with_capacity(established);
    for c in &clients {
        let handle = c.node.link_handle(&c.link_id);
        let data = payload_bytes.clone();
        senders.push(tokio::spawn(async move {
            for _ in 0..packets {
                // Retry on transient Busy/pacing; a dropped packet would skew
                // the received-count target, so keep trying briefly.
                loop {
                    match handle.try_send(&data).await {
                        Ok(()) => break,
                        Err(_) => tokio::time::sleep(Duration::from_millis(1)).await,
                    }
                }
            }
        }));
    }

    // Wait for the serve node to drain the whole load, bounded.
    let load_deadline = start
        + Duration::from_secs(
            std::env::var("LOAD_DEADLINE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120),
        );
    while received.load(Ordering::Relaxed) < target_total && Instant::now() < load_deadline {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let elapsed = start.elapsed();
    for s in senders {
        s.abort();
    }

    let result = LevelResult {
        n,
        established,
        received: received.load(Ordering::Relaxed),
        target: packets,
        elapsed,
    };

    // Tear the clients + serve node down before the next level (frees ports).
    drop(clients);
    drop(serve);
    tokio::time::sleep(Duration::from_millis(200)).await;

    result
}

/// Bring up N clients against the serve node: TCP first, one announce, then
/// concurrent path-install + link establishment. Returns the established set.
async fn bring_up_fleet(
    serve: &ReticulumNode,
    serve_addr: SocketAddr,
    hash: leviculum_core::DestinationHash,
    signing_key: [u8; 32],
    n: usize,
) -> Vec<Client> {
    // Bring all clients' TCP connections up first, then announce once so every
    // connected client can install the path from the same announce.
    let mut connecting = Vec::with_capacity(n);
    for _ in 0..n {
        connecting.push(bring_up_client_tcp_only(serve_addr).await);
    }
    // Settle the TCP peerings, then announce.
    tokio::time::sleep(Duration::from_millis(500)).await;
    serve
        .announce_destination(&hash, Some(b"bench"))
        .await
        .expect("serve announce");

    // Finish establishment for each client concurrently.
    let mut tasks = Vec::with_capacity(n);
    for node in connecting.into_iter().flatten() {
        tasks.push(tokio::spawn(finish_client(node, hash, signing_key)));
    }
    let mut clients = Vec::with_capacity(n);
    for t in tasks {
        if let Ok(Some(c)) = t.await {
            clients.push(c);
        }
    }
    clients
}

/// Bring up only the client node + TCP connection (no path/link yet).
async fn bring_up_client_tcp_only(serve_addr: SocketAddr) -> Option<ReticulumNode> {
    let storage = tempfile::tempdir().ok()?;
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(serve_addr)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .ok()?;
    std::mem::forget(storage);
    node.start().await.ok()?;
    Some(node)
}

/// Given a connected client node, install the path and establish the link.
async fn finish_client(
    node: ReticulumNode,
    hash: leviculum_core::DestinationHash,
    signing_key: [u8; 32],
) -> Option<Client> {
    // Drain the client's own events so its channel never backs up.
    let mut node = node;
    let mut rx = node.take_event_receiver()?;
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if node.has_path(&hash) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    if !node.has_path(&hash) {
        drain.abort();
        return None;
    }

    let handle = node.connect(&hash, &signing_key).await.ok()?;
    let link_id = *handle.link_id();
    while Instant::now() < deadline {
        if node.link_is_established(&link_id) {
            return Some(Client {
                node,
                link_id,
                _drain: drain,
            });
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    drain.abort();
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load benchmark; run explicitly with --ignored --nocapture"]
async fn transport_fanout_sweep() {
    let sizes: Vec<usize> = std::env::var("SIZES")
        .ok()
        .map(|v| {
            v.split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![1, 10, 20, 40, 60]);
    let packets = env_usize("PACKETS", 200);
    let payload = env_usize("PAYLOAD", 32);

    println!();
    println!("transport fan-out ceiling — leviculum#29");
    println!("packets/client={packets}  payload={payload}B");
    println!(
        "{:>5} | {:>11} | {:>9} | {:>10} | {:>12}",
        "N", "established", "recv", "elapsed_s", "pkts/s"
    );
    println!(
        "{:-<5}-+-{:-<11}-+-{:-<9}-+-{:-<10}-+-{:-<12}",
        "", "", "", "", ""
    );

    let mut results = Vec::new();
    for n in sizes {
        let r = run_level(n, packets, payload).await;
        println!(
            "{:>5} | {:>11} | {:>9} | {:>10.2} | {:>12.0}",
            r.n,
            format!("{}/{}", r.established, r.n),
            format!("{}/{}", r.received, r.established * r.target),
            r.elapsed.as_secs_f64(),
            r.throughput(),
        );
        results.push(r);
    }
    println!();

    // Emit machine-readable results for the bench page (CIRISServer-style
    // schema): one file, published as an artifact and rendered to GitHub Pages.
    if let Ok(path) = std::env::var("BENCH_JSON_OUT") {
        write_bench_json(&path, packets, payload, &results);
        eprintln!("[bench] wrote {path}");
    }
}

/// Mode 2 — the field symptom (leviculum#29 / CIRISEdge#370): outbound
/// `send_resource` latency while N links flood the node inbound.
///
/// Today `send_resource` runs the whole resource build — bulk encrypt + full
/// hash + per-part map hash (and bz2 when compressing) — INSIDE the one
/// `Mutex<StdNodeCore>` critical section, so each call both stalls behind the
/// inbound flood's lock holds and, worse, blocks ALL inbound decrypt/route for
/// the duration of the build (ms-scale for round-sized payloads). This mode
/// measures both directions of that exclusion: the `send_resource()` call
/// latency distribution, and the inbound throughput dip while sends happen.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load benchmark; run explicitly with --ignored --nocapture"]
async fn outbound_resource_latency_under_flood() {
    let n = env_usize("FLOOD_N", 20);
    let payload = env_usize("PAYLOAD", 64);
    let resource_kib = env_usize("RESOURCE_KIB", 256);
    let sends = env_usize("RESOURCE_SENDS", 20);

    let (serve, serve_addr, hash, signing_key, received, responder_links, _drain) =
        build_serve_node().await;
    let clients = bring_up_fleet(&serve, serve_addr, hash, signing_key, n).await;
    eprintln!("[bench] flood_n={n}: established {}/{n}", clients.len());
    assert!(!clients.is_empty(), "no links established");

    // Continuous inbound flood until stopped.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut senders = Vec::new();
    for c in &clients {
        let handle = c.node.link_handle(&c.link_id);
        let data = vec![0xABu8; payload];
        let stop2 = Arc::clone(&stop);
        senders.push(tokio::spawn(async move {
            while !stop2.load(Ordering::Relaxed) {
                if handle.try_send(&data).await.is_err() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        }));
    }

    // Warm up, then measure the inbound baseline rate with no outbound sends.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let b0 = received.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let baseline_rate = (received.load(Ordering::Relaxed) - b0) as f64 / 3.0;

    // Resource phase: send a round-sized blob on responder links (round-robin,
    // so TransferInProgress from an earlier in-flight transfer is avoided while
    // sends <= links), measuring each send_resource() call's wall latency.
    //
    // auto_compress=false: bz2 of an incompressible blob would dominate the
    // build and then be discarded; the deterministic cost we care about is the
    // bulk encrypt + hashing.
    let rlinks = responder_links.lock().unwrap().clone();
    assert!(!rlinks.is_empty(), "no responder links captured");
    // COMPRESS=1: auto_compress on with an incompressible (pseudo-random)
    // blob — the sealed-envelope field case, where bz2 burns CPU and is then
    // discarded because ciphertext doesn't compress. Default: compressible
    // constant fill with compression off (deterministic encrypt+hash cost).
    let compress = std::env::var("COMPRESS").is_ok_and(|v| v == "1");
    let blob: Vec<u8> = if compress {
        let mut b = vec![0u8; resource_kib * 1024];
        let mut x: u64 = 0x9E3779B97F4A7C15;
        for c in b.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *c = x as u8;
        }
        b
    } else {
        vec![0x5Au8; resource_kib * 1024]
    };
    let mut lat_ms: Vec<f64> = Vec::new();
    let mut send_errs = 0usize;
    let r0 = received.load(Ordering::Relaxed);
    let t_phase = Instant::now();
    for i in 0..sends {
        let lid = rlinks[i % rlinks.len()];
        let t0 = Instant::now();
        match serve.send_resource(&lid, &blob, None, compress).await {
            Ok(_) => lat_ms.push(t0.elapsed().as_secs_f64() * 1000.0),
            Err(_) => send_errs += 1,
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let phase_s = t_phase.elapsed().as_secs_f64();
    let during_rate = (received.load(Ordering::Relaxed) - r0) as f64 / phase_s;
    stop.store(true, Ordering::Relaxed);
    for s in senders {
        s.abort();
    }

    lat_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |f: f64| -> f64 {
        if lat_ms.is_empty() {
            0.0
        } else {
            lat_ms[((lat_ms.len() - 1) as f64 * f) as usize]
        }
    };
    let (p50, p95, max) = (q(0.5), q(0.95), q(1.0));
    let dip_pct = if baseline_rate > 0.0 {
        (1.0 - during_rate / baseline_rate) * 100.0
    } else {
        0.0
    };

    println!();
    println!("outbound send_resource under flood — leviculum#29 mode 2");
    println!(
        "flood: {} links x {payload}B | resource: {resource_kib} KiB x {} sends ({send_errs} errs)",
        clients.len(),
        lat_ms.len(),
    );
    println!("send_resource latency ms: p50={p50:.1} p95={p95:.1} max={max:.1}");
    println!(
        "inbound pkts/s: baseline={baseline_rate:.0} during-sends={during_rate:.0} (dip {dip_pct:.0}%)"
    );
    println!();

    if let Ok(path) = std::env::var("BENCH_JSON_OUT_OUTBOUND") {
        let json = format!(
            "{{\n  \"schema\": \"leviculum/bench-results/1\",\n  \"benchmark\": \"outbound_resource_under_flood\",\n  \"issue\": \"leviculum#29\",\n  \"params\": {{\"flood_links\": {}, \"payload_bytes\": {payload}, \"resource_kib\": {resource_kib}, \"sends\": {}}},\n  \"send_latency_ms\": {{\"p50\": {p50:.2}, \"p95\": {p95:.2}, \"max\": {max:.2}}},\n  \"inbound_pkts_s\": {{\"baseline\": {baseline_rate:.1}, \"during_sends\": {during_rate:.1}, \"dip_pct\": {dip_pct:.1}}}\n}}\n",
            clients.len(),
            lat_ms.len(),
        );
        std::fs::write(&path, json).expect("write outbound bench json");
        eprintln!("[bench] wrote {path}");
    }
}

/// Hand-write the `bench_results.json` (dependency-free — no serde in the test
/// binary). Mirrors CIRISServer's `{schema, commit, date, runner, ...}` shape,
/// with a `sweep` array for the N-vs-throughput curve.
fn write_bench_json(path: &str, packets: usize, payload: usize, results: &[LevelResult]) {
    fn env_or(key: &str, default: &str) -> String {
        std::env::var(key)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    }
    let commit = env_or("GIT_COMMIT", "unknown");
    let date = env_or("BENCH_DATE", "unknown");
    let runner = env_or(
        "BENCH_RUNNER",
        &format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
    );

    let mut sweep = String::new();
    for (i, r) in results.iter().enumerate() {
        if i > 0 {
            sweep.push(',');
        }
        sweep.push_str(&format!(
            "\n    {{\"n\": {}, \"established\": {}, \"received\": {}, \"target_total\": {}, \"elapsed_s\": {:.3}, \"throughput_pkts_s\": {:.1}}}",
            r.n,
            r.established,
            r.received,
            r.established * r.target,
            r.elapsed.as_secs_f64(),
            r.throughput(),
        ));
    }

    let json = format!(
        "{{\n  \"schema\": \"leviculum/bench-results/1\",\n  \"benchmark\": \"transport_fanout\",\n  \"issue\": \"leviculum#29\",\n  \"commit\": \"{commit}\",\n  \"date\": \"{date}\",\n  \"runner\": \"{runner}\",\n  \"params\": {{\"packets_per_client\": {packets}, \"payload_bytes\": {payload}}},\n  \"sweep\": [{sweep}\n  ]\n}}\n"
    );
    std::fs::write(path, json).expect("write bench json");
}
