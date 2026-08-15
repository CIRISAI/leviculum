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
//! # mode 5 — the consumer's link dance (leviculum#46):
//! DANCE_SIZES="8 16 32 48 64" ROUNDS=5 RESOURCE_KIB=512 \
//!   ESTABLISH_TIMEOUT=15 TRANSFER_TIMEOUT=60 STALL_SECS=15 LOAD_DEADLINE=240 \
//!   cargo test -p leviculum-std --test transport_fanout_bench \
//!     link_dance_sweep -- --ignored --nocapture
//! ```

use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use leviculum_core::resource::ResourceStrategy;
use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::{EventReceiver, NodeEvent};

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

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Nearest-rank percentile over a pre-sorted slice (0 if empty).
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        0.0
    } else {
        sorted[((sorted.len() - 1) as f64 * q) as usize]
    }
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
                    ..
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
    let (p50, p95, max) = (
        percentile(&lat_ms, 0.5),
        percentile(&lat_ms, 0.95),
        percentile(&lat_ms, 1.0),
    );
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

// ---------------------------------------------------------------------------
// leviculum#29 stages 2-3 — the EXPENSIVE inbound crypto classes. Link data is
// HMAC+AES (~1µs release); the classes below are the real lock long-poles:
// single-destination datagrams cost X25519 ECDH + HKDF per packet, announces
// cost an Ed25519 verify each. These modes measure the serve node's aggregate
// throughput for each class under N-client fan-out.
// ---------------------------------------------------------------------------

/// Spawn an event drain on `node` counting events matching `pred`.
fn count_events<F>(
    node: &mut ReticulumNode,
    pred: F,
) -> (Arc<AtomicUsize>, tokio::task::JoinHandle<()>)
where
    F: Fn(&NodeEvent) -> bool + Send + 'static,
{
    let counter = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&counter);
    let mut rx = node.take_event_receiver().expect("event rx");
    let h = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if pred(&ev) {
                c.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    (counter, h)
}

/// Mode 3 — single-destination datagram flood: every packet costs the serve
/// node an ECDH decrypt (ratcheted Single destination, the sealed-envelope
/// field shape), all currently under the one node lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load benchmark; run explicitly with --ignored --nocapture"]
async fn single_dest_datagram_flood() {
    let n = env_usize("FLOOD_N", 20);
    let packets = env_usize("PACKETS", 200);
    let payload = env_usize("PAYLOAD", 64);

    // Serve node with a ratcheted Single destination.
    let addr: SocketAddr = format!("127.0.0.1:{}", next_port()).parse().unwrap();
    let storage = tempfile::tempdir().expect("serve tempdir");
    let mut serve = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server(addr)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build serve node");
    std::mem::forget(storage);
    serve.start().await.expect("start serve node");

    let identity = Identity::generate(&mut rand_core::OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "bench",
        &["datagram"],
    )
    .expect("serve destination");
    dest.enable_ratchets(&mut rand_core::OsRng, 1)
        .expect("ratchets");
    let hash = *dest.hash();
    serve.register_destination(dest);

    let (received, _drain) = count_events(&mut serve, |ev| {
        matches!(ev, NodeEvent::PacketReceived { .. })
    });

    // Clients: TCP up, install path from one announce, then flood datagrams.
    let mut nodes = Vec::new();
    for _ in 0..n {
        if let Some(c) = bring_up_client_tcp_only(addr).await {
            nodes.push(c);
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    serve
        .announce_destination(&hash, Some(b"bench"))
        .await
        .expect("announce");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut clients = Vec::new();
    for node in nodes {
        while Instant::now() < deadline && !node.has_path(&hash) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if node.has_path(&hash) {
            clients.push(node);
        }
    }
    let established = clients.len();
    eprintln!("[bench] datagram flood: {established}/{n} clients have the path");
    assert!(established > 0);

    let target_total = established * packets;
    received.store(0, Ordering::Relaxed);
    let start = Instant::now();
    let mut senders = Vec::new();
    for node in clients {
        let data = vec![0xABu8; payload];
        senders.push(tokio::spawn(async move {
            for _ in 0..packets {
                loop {
                    match node.send_single_packet(&hash, &data).await {
                        Ok(_) => break,
                        Err(e) => {
                            if std::env::var("BENCH_DEBUG").is_ok() {
                                eprintln!("[send-err] {e:?}");
                            }
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                    }
                }
            }
        }));
    }
    // Datagrams are fire-and-forget: a small fraction can drop under burst, so
    // waiting for ALL would let the deadline dominate the math. Stop when the
    // count stalls and time to the LAST arrival.
    let load_deadline = start + Duration::from_secs(180);
    let mut last_count = 0usize;
    let mut last_arrival = start;
    loop {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let now_count = received.load(Ordering::Relaxed);
        if now_count > last_count {
            last_count = now_count;
            last_arrival = Instant::now();
        }
        if now_count >= target_total
            || Instant::now() > load_deadline
            || last_arrival.elapsed() > Duration::from_secs(5)
        {
            break;
        }
    }
    let elapsed = (last_arrival - start).as_secs_f64().max(0.001);
    for s in senders {
        s.abort();
    }
    let got = received.load(Ordering::Relaxed);
    println!();
    println!("single-dest datagram flood — leviculum#29 expensive class (ECDH/packet)");
    println!(
        "clients={established} received={got}/{target_total} elapsed={elapsed:.2}s throughput={:.0} pkts/s",
        got as f64 / elapsed
    );
    if let Ok(path) = std::env::var("BENCH_JSON_OUT_SINGLEDEST") {
        let commit = env_or("GIT_COMMIT", "unknown");
        let date = env_or("BENCH_DATE", "unknown");
        let runner = env_or(
            "BENCH_RUNNER",
            &format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        );
        let json = format!(
            "{{\n  \"schema\": \"leviculum/bench-results/1\",\n  \"benchmark\": \"single_dest_datagram_flood\",\n  \"issue\": \"leviculum#29\",\n  \"commit\": \"{commit}\",\n  \"date\": \"{date}\",\n  \"runner\": \"{runner}\",\n  \"params\": {{\"clients\": {established}, \"packets_per_client\": {packets}, \"payload_bytes\": {payload}}},\n  \"received\": {got},\n  \"elapsed_s\": {elapsed:.3},\n  \"throughput_pkts_s\": {:.1}\n}}\n",
            got as f64 / elapsed
        );
        std::fs::write(&path, json).expect("write json");
        eprintln!("[bench] wrote {path}");
    }
}

/// Mode 4 — announce flood: every announce costs the serve node an Ed25519
/// signature verify, all currently under the one node lock. Each client
/// announces FRESH destinations so no per-destination rate limit engages.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load benchmark; run explicitly with --ignored --nocapture"]
async fn announce_verify_flood() {
    let n = env_usize("FLOOD_N", 10);
    let announces = env_usize("ANNOUNCES", 100);

    let addr: SocketAddr = format!("127.0.0.1:{}", next_port()).parse().unwrap();
    let storage = tempfile::tempdir().expect("serve tempdir");
    // ingress_control defaults ON for listeners (#189) and quarantines exactly
    // the burst this mode measures; the field case (#29) is announce churn from
    // established peers, so measure the verify path with the limiter off.
    let mut cfg = leviculum_std::config::Config::default();
    cfg.interfaces.insert(
        "bench-listener".to_string(),
        leviculum_std::config::InterfaceConfig {
            name: "bench-listener".to_string(),
            interface_type: "TCPServerInterface".to_string(),
            listen_ip: Some(addr.ip().to_string()),
            listen_port: Some(addr.port()),
            ingress_control: Some(false),
            ..Default::default()
        },
    );
    let mut serve = ReticulumNodeBuilder::new()
        .config(cfg)
        .enable_transport(false)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build serve node");
    std::mem::forget(storage);
    serve.start().await.expect("start serve node");

    let (received, _drain) = count_events(&mut serve, |ev| {
        matches!(ev, NodeEvent::AnnounceReceived { .. })
    });

    let mut clients = Vec::new();
    for _ in 0..n {
        if let Some(c) = bring_up_client_tcp_only(addr).await {
            clients.push(c);
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let live = clients.len();
    eprintln!("[bench] announce flood: {live}/{n} clients connected");
    assert!(live > 0);

    let target_total = live * announces;
    received.store(0, Ordering::Relaxed);
    let start = Instant::now();
    let mut senders = Vec::new();
    for c in clients {
        senders.push(tokio::spawn(async move {
            for _ in 0..announces {
                // Fresh identity+destination per announce: unique dest, so the
                // serve node's per-destination announce policies never engage.
                let identity = Identity::generate(&mut rand_core::OsRng);
                let dest = Destination::new(
                    Some(identity),
                    Direction::In,
                    DestinationType::Single,
                    "bench",
                    &["annflood"],
                )
                .expect("dest");
                let hash = *dest.hash();
                c.register_destination(dest);
                let _ = c.announce_destination(&hash, None).await;
            }
        }));
    }
    // Same stall-stop math as the datagram mode: a dropped announce must not
    // let the deadline dominate the rate.
    let load_deadline = start + Duration::from_secs(180);
    let mut last_count = 0usize;
    let mut last_arrival = start;
    loop {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let now_count = received.load(Ordering::Relaxed);
        if now_count > last_count {
            last_count = now_count;
            last_arrival = Instant::now();
        }
        if now_count >= target_total
            || Instant::now() > load_deadline
            || last_arrival.elapsed() > Duration::from_secs(5)
        {
            break;
        }
    }
    let elapsed = (last_arrival - start).as_secs_f64().max(0.001);
    for s in senders {
        s.abort();
    }
    let got = received.load(Ordering::Relaxed);
    println!();
    println!("announce flood — leviculum#29 expensive class (Ed25519 verify/announce)");
    println!(
        "clients={live} received={got}/{target_total} elapsed={elapsed:.2}s throughput={:.0} ann/s",
        got as f64 / elapsed
    );
    if let Ok(path) = std::env::var("BENCH_JSON_OUT_ANNOUNCE") {
        let commit = env_or("GIT_COMMIT", "unknown");
        let date = env_or("BENCH_DATE", "unknown");
        let runner = env_or(
            "BENCH_RUNNER",
            &format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        );
        let json = format!(
            "{{\n  \"schema\": \"leviculum/bench-results/1\",\n  \"benchmark\": \"announce_verify_flood\",\n  \"issue\": \"leviculum#29\",\n  \"commit\": \"{commit}\",\n  \"date\": \"{date}\",\n  \"runner\": \"{runner}\",\n  \"params\": {{\"clients\": {live}, \"announces_per_client\": {announces}}},\n  \"received\": {got},\n  \"elapsed_s\": {elapsed:.3},\n  \"throughput_ann_s\": {:.1}\n}}\n",
            got as f64 / elapsed
        );
        std::fs::write(&path, json).expect("write json");
        eprintln!("[bench] wrote {path}");
    }
}

// ---------------------------------------------------------------------------
// Mode 5 — the "link dance" (leviculum#46): the consumer's real unit of work.
// connect -> LinkEstablished -> identify_link -> serve-side ready ping ->
// send_resource(512 KiB, incompressible, no auto-compress) -> sender-side
// ResourceCompleted -> close_link -> LinkClosed, repeated R rounds per client.
//
// Provenance: this is edge's per-peer round shape (CIRISEdge#482), which
// documents collapse past ~40 peers. This mode is the leviculum-native
// before/after instrument for #42 (completion futures) and #43 (streamed
// resources).
//
// Methodology:
//
// (i)   Shared CI runners swing absolute throughput ±2x with machine phases.
//       Published numbers are TRENDS; a change's before/after comparison
//       should use per-item lock-hold deltas, not two throughput runs.
// (ii)  Stall-stop measurement: elapsed is the time of the LAST completed
//       dance and a level breaks on a no-progress window, so a tail of
//       timeouts (or the hard per-level deadline) never dilutes the rate — a
//       deadline-quotient bench measures the deadline, not the system.
// (iii) Every row derives service demand (us/dance = elapsed/completed at
//       saturation), the workload-x-cost matrix input. The model already
//       fits: ~85-100 us/packet of single-lock loop machinery predicted the
//       measured ~11.5k pkts/s mode-1 ceiling.
// (iv)  Upstream Lew_Palm/leviculum#208 (hub delivery ceiling; their bar: lab
//       PDR < 100% is a bug, not variance) requires separating HOST-OVERLOAD
//       from PROTOCOL LOSS — hence failure classes reported per N and never
//       aggregated. Establish/TransferTimeout scale with N under overload;
//       ResourceFailed while the node still completes other dances is
//       protocol loss.
// ---------------------------------------------------------------------------

/// Outcome of one dance attempt, reported by a client task to the level
/// watcher over the progress channel. Completions are the stall-stop
/// progress signal; failures are counted but do not extend the window.
enum DanceOutcome {
    Completed { establish_ms: f64, transfer_ms: f64 },
    Failed(DanceFail),
}

/// Failure classes stay separate per N so host-overload (the timeouts,
/// which scale with N) never aggregates with protocol loss
/// (ResourceFailed) — the upstream Lew_Palm/leviculum#208 requirement.
#[derive(Clone, Copy)]
enum DanceFail {
    /// connect/identify/send_resource returned Err (driver/API failure).
    Api,
    /// LinkEstablished or the responder ready ping missed the deadline.
    EstablishTimeout,
    /// Neither ResourceCompleted nor ResourceFailed arrived in time.
    TransferTimeout,
    /// The transfer explicitly failed — protocol loss, not overload.
    Resource,
}

/// Level-invariant dance parameters, shared by every client task.
struct DanceCfg {
    hash: leviculum_core::DestinationHash,
    signing_key: [u8; 32],
    rounds: usize,
    /// Incompressible (xorshift-filled) blob, sent with auto_compress=false.
    blob: Vec<u8>,
    establish_timeout: Duration,
    transfer_timeout: Duration,
    /// Cooperative end-of-phase flag for the time-boxed overload mode; a
    /// client checks it between dances, so a raised flag drains in at most
    /// one dance's worth of timeouts. Mode 5 never raises it.
    stop: Arc<std::sync::atomic::AtomicBool>,
}

/// Sweep-level knobs parsed once from env in `link_dance_sweep`.
struct SweepKnobs {
    rounds: usize,
    resource_kib: usize,
    establish_timeout: Duration,
    transfer_timeout: Duration,
    stall: Duration,
    /// Hard per-level cap; stall-stop normally ends the level first.
    level_deadline: Duration,
}

struct DanceLevelResult {
    n: usize,
    /// Clients that finished path install and entered the round loop
    /// (JSON field `clients_ready` — NOT mode 1's per-link `established`:
    /// each client here establishes R links, so that word is ambiguous).
    clients_ready: usize,
    target: usize,
    completed: usize,
    fail_api: usize,
    fail_establish: usize,
    fail_transfer: usize,
    fail_resource: usize,
    /// start -> last completed dance (stall-stop; min 1 ms guard).
    elapsed: Duration,
    /// One entry per completed dance, sorted ascending (percentile-ready).
    establish_ms: Vec<f64>,
    transfer_ms: Vec<f64>,
}

impl DanceLevelResult {
    fn dances_per_s(&self) -> f64 {
        if self.completed == 0 {
            0.0
        } else {
            self.completed as f64 / self.elapsed.as_secs_f64()
        }
    }

    fn service_demand_us(&self) -> f64 {
        if self.completed == 0 {
            0.0
        } else {
            1e6 * self.elapsed.as_secs_f64() / self.completed as f64
        }
    }
}

/// Serve node for the dance: TCP server, one Single destination
/// ("bench"/["dance"]), event drain that flips each responder link to
/// AcceptAll and answers with a 1-byte ready ping. The node is Arc'd because
/// the drain needs `&ReticulumNode` after `take_event_receiver` (which needs
/// `&mut self`) — receiver first, then wrap. Abort the drain handle before
/// dropping the level's Arc so the node actually drops.
async fn build_dance_serve_node() -> (
    Arc<ReticulumNode>,
    SocketAddr,
    leviculum_core::DestinationHash,
    [u8; 32],
    tokio::task::JoinHandle<()>,
    Arc<AtomicUsize>,
) {
    let addr: SocketAddr = format!("127.0.0.1:{}", next_port()).parse().unwrap();
    let storage = tempfile::tempdir().expect("serve tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server(addr)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build dance serve node");
    std::mem::forget(storage);
    node.start().await.expect("start dance serve node");

    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "bench",
        &["dance"],
    )
    .expect("dance serve destination");
    let hash = *dest.hash();
    node.register_destination(dest);

    let mut rx = node.take_event_receiver().expect("dance serve event rx");
    let node = Arc::new(node);
    let serve = Arc::clone(&node);
    // Visible-shedding probe for the overload mode: every control-plane
    // drop on the serve node surfaces as a ControlPlaneOverflow marker, and
    // the drain counts them — silent loss is a bug, counted loss is a
    // measured degradation datum.
    let overflow = Arc::new(AtomicUsize::new(0));
    let overflow_in_drain = Arc::clone(&overflow);
    let drain = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let NodeEvent::ControlPlaneOverflow { dropped_count } = ev {
                overflow_in_drain.fetch_add(dropped_count as usize, Ordering::Relaxed);
                continue;
            }
            if let NodeEvent::LinkEstablished {
                link_id,
                is_initiator: false,
                ..
            } = ev
            {
                // Per-link resource strategy defaults to AcceptNone and is
                // only settable AFTER establishment, so a client ADV racing
                // this drain would be silently rejected — the sender times
                // out, a harness artifact that would masquerade as protocol
                // loss in the #208 numbers. Flip the link, then answer with a
                // 1-byte ready ping the client awaits before sending. Cost is
                // 1 packet against ~1,130 resource parts per dance.
                //
                // Per-link task, NOT inline: at high N the establishment
                // burst arrives while earlier links may already be dead
                // (client gave up), and an inline bounded retry (up to 1 s)
                // would head-of-line every later link's strategy flip — the
                // sender then times out on a link whose ADV was rejected,
                // which masquerades as protocol loss. The drain's only job
                // is to keep receiving. Err is ignored: the link may
                // already be gone.
                let pinger = Arc::clone(&serve);
                tokio::spawn(async move {
                    let _ = pinger.set_resource_strategy(&link_id, ResourceStrategy::AcceptAll);
                    let handle = pinger.link_handle(&link_id);
                    for _ in 0..100 {
                        if handle.try_send(b"go").await.is_ok() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                });
            }
        }
    });

    (node, addr, hash, signing_key, drain, overflow)
}

/// One dance round. Every wait is a `timeout_at` around `rx.recv()`; while
/// awaiting a specific event, everything else is drained and discarded (the
/// round loop is the receiver's sole consumer, so the channel never backs up
/// — including the ~1,130 per-part `ResourceProgress` ticks per transfer and
/// any `ControlPlaneOverflow` marker; the completion/failure events are
/// control-plane, lossless, so matching on them stays safe).
async fn dance_once(
    node: &ReticulumNode,
    rx: &mut EventReceiver,
    cfg: &DanceCfg,
    identity: &Identity,
) -> DanceOutcome {
    let t0 = Instant::now();
    let handle = match node.connect(&cfg.hash, &cfg.signing_key).await {
        Ok(h) => h,
        Err(_) => return DanceOutcome::Failed(DanceFail::Api),
    };
    let link_id = *handle.link_id();
    let dl = tokio::time::Instant::now() + cfg.establish_timeout;
    loop {
        match tokio::time::timeout_at(dl, rx.recv()).await {
            Ok(Some(NodeEvent::LinkEstablished {
                link_id: lid,
                is_initiator: true,
                ..
            })) if lid == link_id => break,
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => {
                let _ = node.close_link(&link_id).await;
                return DanceOutcome::Failed(DanceFail::EstablishTimeout);
            }
        }
    }
    // Pure protocol establish — the ready-ping wait below shares this round's
    // establish deadline but lands in NEITHER histogram, keeping both clean
    // for #42/#43 before/after comparison (it shows up only in dances/s).
    let establish_ms = t0.elapsed().as_secs_f64() * 1000.0;

    if node.identify_link(&link_id, identity).await.is_err() {
        let _ = node.close_link(&link_id).await;
        return DanceOutcome::Failed(DanceFail::Api);
    }

    // The ready ping arrives on the client's DATA plane (droppable), which is
    // safe only because this receiver is idle at this moment — match both
    // framings in case the serve side's send surfaces as either.
    loop {
        match tokio::time::timeout_at(dl, rx.recv()).await {
            Ok(Some(NodeEvent::MessageReceived { link_id: lid, .. }))
            | Ok(Some(NodeEvent::LinkDataReceived { link_id: lid, .. }))
                if lid == link_id =>
            {
                break;
            }
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => {
                let _ = node.close_link(&link_id).await;
                return DanceOutcome::Failed(DanceFail::EstablishTimeout);
            }
        }
    }

    let t1 = Instant::now();
    // send_resource_awaited (leviculum#42) rather than hand-matching
    // ResourceCompleted events: the sender's final event of a
    // multi-segment transfer (RESOURCE_KIB > 1024 crosses
    // RESOURCE_MAX_EFFICIENT_SIZE) carries the FINAL segment's hash, not
    // the one send_resource returns, so an exact-hash event match reports
    // a false transfer timeout for every split transfer. The completion
    // future resolves at the dispatch layer — correct across segments,
    // failure-typed, immune to data-plane event drops — and the bench
    // doubles as the API's under-load integration test.
    let (_rhash, sent) = match node
        .send_resource_awaited(&link_id, &cfg.blob, None, false)
        .await
    {
        Ok(pair) => pair,
        Err(_) => {
            let _ = node.close_link(&link_id).await;
            return DanceOutcome::Failed(DanceFail::Api);
        }
    };
    let transfer_ms = match tokio::time::timeout(cfg.transfer_timeout, sent).await {
        Ok(Ok(_info)) => t1.elapsed().as_secs_f64() * 1000.0,
        Ok(Err(_)) => {
            let _ = node.close_link(&link_id).await;
            return DanceOutcome::Failed(DanceFail::Resource);
        }
        Err(_) => {
            let _ = node.close_link(&link_id).await;
            return DanceOutcome::Failed(DanceFail::TransferTimeout);
        }
    };

    let _ = node.close_link(&link_id).await;
    // Bookkeeping only: bound the wait so a lost close never stalls the loop.
    let close_dl = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match tokio::time::timeout_at(close_dl, rx.recv()).await {
            Ok(Some(NodeEvent::LinkClosed { link_id: lid, .. })) if lid == link_id => break,
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => break,
        }
    }

    DanceOutcome::Completed {
        establish_ms,
        transfer_ms,
    }
}

/// One client's whole life: await PathFound (30 s deadline), bump `ready`,
/// then `cfg.rounds` dances. Sole consumer of its own `EventReceiver`; every
/// wait is a tokio timeout around `recv()` — no sleep-polling.
async fn dance_client(
    node: ReticulumNode,
    mut rx: EventReceiver,
    cfg: Arc<DanceCfg>,
    report: tokio::sync::mpsc::Sender<DanceOutcome>,
    ready: Arc<AtomicUsize>,
) {
    // PathFound is control-plane (lossless), and the level's one announce may
    // already sit buffered from before this task started — recv() sees it
    // either way. A miss leaves this client out of the level's clients_ready.
    let path_dl = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        match tokio::time::timeout_at(path_dl, rx.recv()).await {
            Ok(Some(NodeEvent::PathFound {
                destination_hash, ..
            })) if destination_hash == cfg.hash => break,
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return,
        }
    }
    ready.fetch_add(1, Ordering::Relaxed);

    // One identity per client, stable across rounds — edge's shape.
    let identity = Identity::generate(&mut rand_core::OsRng);
    for _ in 0..cfg.rounds {
        if cfg.stop.load(Ordering::Relaxed) {
            return; // phase over (overload mode)
        }
        let outcome = dance_once(&node, &mut rx, &cfg, &identity).await;
        if report.send(outcome).await.is_err() {
            return; // watcher gone — the level already ended
        }
    }
}

async fn run_dance_level(n: usize, knobs: &SweepKnobs) -> DanceLevelResult {
    let (serve, serve_addr, hash, signing_key, serve_drain, _overflow) =
        build_dance_serve_node().await;

    // TCP first, then one announce every connected client installs the path
    // from (the same one-announce pattern as bring_up_fleet).
    let mut nodes = Vec::with_capacity(n);
    for _ in 0..n {
        if let Some(c) = bring_up_client_tcp_only(serve_addr).await {
            nodes.push(c);
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    serve
        .announce_destination(&hash, Some(b"bench"))
        .await
        .expect("dance serve announce");

    // Incompressible blob (mode 2's xorshift fill) with auto_compress=false:
    // models sealed-envelope field payloads and keeps per-dance cost
    // deterministic (no bz2 variance).
    let mut blob = vec![0u8; knobs.resource_kib * 1024];
    let mut x: u64 = 0x9E3779B97F4A7C15;
    for c in blob.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *c = x as u8;
    }
    let cfg = Arc::new(DanceCfg {
        hash,
        signing_key,
        rounds: knobs.rounds,
        blob,
        establish_timeout: knobs.establish_timeout,
        transfer_timeout: knobs.transfer_timeout,
        stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });

    let ready = Arc::new(AtomicUsize::new(0));
    let (report_tx, mut report_rx) = tokio::sync::mpsc::channel(64);
    let mut tasks = Vec::with_capacity(nodes.len());
    for mut node in nodes {
        let Some(rx) = node.take_event_receiver() else {
            continue;
        };
        tasks.push(tokio::spawn(dance_client(
            node,
            rx,
            Arc::clone(&cfg),
            report_tx.clone(),
            Arc::clone(&ready),
        )));
    }
    // Natural completion = the channel closing once every client task returns;
    // the level's own sender must go first for that to ever happen.
    drop(report_tx);

    let start = tokio::time::Instant::now();
    let hard = start + knobs.level_deadline;
    // First-completion grace is one full unit-of-work budget (not the stall
    // window): at N=64 the first cohort of concurrent 512 KiB transfers
    // legitimately outlasts any reasonable stall window. The budget includes
    // establish_timeout because a client's transfer clock starts only after
    // establish + ping — grace of transfer_timeout alone expires BEFORE any
    // client's transfer deadline can fire, so a fully-collapsed level would
    // always report zero counted failures.
    let mut stall_dl = start + knobs.establish_timeout + knobs.transfer_timeout;
    let mut last_completion = start;
    let mut completed = 0usize;
    let (mut fail_api, mut fail_establish, mut fail_transfer, mut fail_resource) = (0, 0, 0, 0);
    let mut establish_ms: Vec<f64> = Vec::new();
    let mut transfer_ms: Vec<f64> = Vec::new();
    loop {
        match tokio::time::timeout_at(stall_dl.min(hard), report_rx.recv()).await {
            Ok(Some(DanceOutcome::Completed {
                establish_ms: e,
                transfer_ms: t,
            })) => {
                establish_ms.push(e);
                transfer_ms.push(t);
                completed += 1;
                last_completion = tokio::time::Instant::now();
                stall_dl = last_completion + knobs.stall;
            }
            Ok(Some(DanceOutcome::Failed(f))) => match f {
                DanceFail::Api => fail_api += 1,
                DanceFail::EstablishTimeout => fail_establish += 1,
                DanceFail::TransferTimeout => fail_transfer += 1,
                DanceFail::Resource => fail_resource += 1,
            },
            Ok(None) => break, // every client task finished its rounds
            Err(_) => break,   // stall window or hard deadline expired
        }
    }
    // Stall-stop anchor: elapsed ends at the LAST completed dance, so a tail
    // of timeouts (or the hard deadline) never dilutes the rate.
    let elapsed = (last_completion - start).max(Duration::from_millis(1));

    for t in &tasks {
        t.abort();
    }
    serve_drain.abort();
    drop(serve);
    tokio::time::sleep(Duration::from_millis(200)).await;

    establish_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    transfer_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let clients_ready = ready.load(Ordering::Relaxed);
    DanceLevelResult {
        n,
        clients_ready,
        target: clients_ready * knobs.rounds,
        completed,
        fail_api,
        fail_establish,
        fail_transfer,
        fail_resource,
        elapsed,
        establish_ms,
        transfer_ms,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load benchmark; run explicitly with --ignored --nocapture"]
async fn link_dance_sweep() {
    let sizes: Vec<usize> = std::env::var("DANCE_SIZES")
        .ok()
        .map(|v| {
            v.split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![8, 16, 32, 48, 64]);
    let knobs = SweepKnobs {
        rounds: env_usize("ROUNDS", 5),
        resource_kib: env_usize("RESOURCE_KIB", 512),
        establish_timeout: Duration::from_secs(env_usize("ESTABLISH_TIMEOUT", 15) as u64),
        transfer_timeout: Duration::from_secs(env_usize("TRANSFER_TIMEOUT", 60) as u64),
        stall: Duration::from_secs(env_usize("STALL_SECS", 15) as u64),
        level_deadline: Duration::from_secs(env_usize("LOAD_DEADLINE", 240) as u64),
    };

    println!();
    println!("link dance — leviculum#46 (connect/identify/resource/close per round)");
    println!(
        "rounds/client={} resource={} KiB establish_to={}s transfer_to={}s stall={}s",
        knobs.rounds,
        knobs.resource_kib,
        knobs.establish_timeout.as_secs(),
        knobs.transfer_timeout.as_secs(),
        knobs.stall.as_secs(),
    );
    println!(
        "{:>5} | {:>5} | {:>9} | {:>12} | {:>9} | {:>8} | {:>13} | {:>15} | {:>10}",
        "N",
        "ready",
        "dances",
        "fail a/e/x/r",
        "elapsed_s",
        "dances/s",
        "est p50/p95",
        "xfer p50/p95",
        "us/dance"
    );
    println!(
        "{:-<5}-+-{:-<5}-+-{:-<9}-+-{:-<12}-+-{:-<9}-+-{:-<8}-+-{:-<13}-+-{:-<15}-+-{:-<10}",
        "", "", "", "", "", "", "", "", ""
    );

    let mut results = Vec::new();
    for n in sizes {
        let r = run_dance_level(n, &knobs).await;
        println!(
            "{:>5} | {:>5} | {:>9} | {:>12} | {:>9.2} | {:>8.2} | {:>13} | {:>15} | {:>10.0}",
            r.n,
            r.clients_ready,
            format!("{}/{}", r.completed, r.target),
            format!(
                "{}/{}/{}/{}",
                r.fail_api, r.fail_establish, r.fail_transfer, r.fail_resource
            ),
            r.elapsed.as_secs_f64(),
            r.dances_per_s(),
            format!(
                "{:.1}/{:.1}",
                percentile(&r.establish_ms, 0.5),
                percentile(&r.establish_ms, 0.95)
            ),
            format!(
                "{:.0}/{:.0}",
                percentile(&r.transfer_ms, 0.5),
                percentile(&r.transfer_ms, 0.95)
            ),
            r.service_demand_us(),
        );
        results.push(r);
    }
    println!();

    if let Ok(path) = std::env::var("BENCH_JSON_OUT_LINKDANCE") {
        write_linkdance_json(&path, &knobs, &results);
        eprintln!("[bench] wrote {path}");
    }
}

/// Hand-write the link-dance JSON (dependency-free, same discipline as
/// `write_bench_json`): `leviculum/bench-results/1` shape with a `sweep`
/// array carrying the per-N rows, failure classes broken out per #208.
fn write_linkdance_json(path: &str, knobs: &SweepKnobs, results: &[DanceLevelResult]) {
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
            "\n    {{\"n\": {}, \"clients_ready\": {}, \"target_dances\": {}, \"completed\": {}, \"failures\": {{\"api\": {}, \"establish_timeout\": {}, \"transfer_timeout\": {}, \"resource_failed\": {}}}, \"elapsed_s\": {:.3}, \"dances_per_s\": {:.2}, \"establish_ms\": {{\"p50\": {:.1}, \"p95\": {:.1}}}, \"transfer_ms\": {{\"p50\": {:.1}, \"p95\": {:.1}}}, \"service_demand_us_per_dance\": {:.1}}}",
            r.n,
            r.clients_ready,
            r.target,
            r.completed,
            r.fail_api,
            r.fail_establish,
            r.fail_transfer,
            r.fail_resource,
            r.elapsed.as_secs_f64(),
            r.dances_per_s(),
            percentile(&r.establish_ms, 0.5),
            percentile(&r.establish_ms, 0.95),
            percentile(&r.transfer_ms, 0.5),
            percentile(&r.transfer_ms, 0.95),
            r.service_demand_us(),
        ));
    }

    let json = format!(
        "{{\n  \"schema\": \"leviculum/bench-results/1\",\n  \"benchmark\": \"link_dance\",\n  \"issue\": \"leviculum#46\",\n  \"commit\": \"{commit}\",\n  \"date\": \"{date}\",\n  \"runner\": \"{runner}\",\n  \"params\": {{\"rounds_per_client\": {}, \"resource_kib\": {}, \"establish_timeout_s\": {}, \"transfer_timeout_s\": {}, \"stall_secs\": {}}},\n  \"sweep\": [{sweep}\n  ]\n}}\n",
        knobs.rounds,
        knobs.resource_kib,
        knobs.establish_timeout.as_secs(),
        knobs.transfer_timeout.as_secs(),
        knobs.stall.as_secs(),
    );
    std::fs::write(path, json).expect("write linkdance bench json");
}

// ---------------------------------------------------------------------------
// Mode 6 — leviculum#46: the degradation envelope. Mode 5 finds the knee;
// this mode drives PAST it on purpose and characterizes what the node does
// there, so saturation is a measured pattern instead of a guess. Each phase
// is time-boxed (clients loop dances until the flag flips), against ONE
// serve node that lives through every phase — recovery is only meaningful
// on the node that just took the beating.
//
// Invariants asserted by measurement, not assumption:
//   liveness — an outside caller's node-lock acquire (has_path on a probe
//     hash) is sampled at 100 ms through every phase; its percentiles ARE
//     the "holds stay flat under overload" hypothesis (#29).
//   bounded memory — VmRSS is read per phase; a growing queue shows here.
//   visible shedding — control-plane drops surface as ControlPlaneOverflow
//     markers and are counted; silent loss is a bug.
//   typed failure — the a/e/x/r taxonomy separates clean rejection from
//     stall from protocol loss (#208's host-overload vs protocol-loss rule).
//   recovery — after the last phase: solo dances on a fresh client, until
//     one matches the unloaded baseline (time-to-recover, first-class).
// ---------------------------------------------------------------------------

fn read_rss_mib() -> f64 {
    // VmRSS covers the whole test process — serve node, client fleets, and
    // harness. Phase-over-phase DELTA is the signal; the absolute value is
    // context.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| {
                l.split_whitespace()
                    .nth(1)
                    .and_then(|kb| kb.parse::<f64>().ok())
            })
        })
        .map(|kb| kb / 1024.0)
        .unwrap_or(f64::NAN)
}

struct OverloadPhase {
    n: usize,
    clients_ready: usize,
    /// Completions inside the phase window — goodput's numerator.
    completed: usize,
    /// Completions that landed during the post-flag drain (still real work,
    /// not offered-window goodput).
    drained: usize,
    fail_api: usize,
    fail_establish: usize,
    fail_transfer: usize,
    fail_resource: usize,
    window_secs: f64,
    establish_ms: Vec<f64>,
    transfer_ms: Vec<f64>,
    /// Node-lock acquire latency samples (ms) from the outside probe.
    probe_ms: Vec<f64>,
    overflow_markers: usize,
    rss_mib: f64,
}

impl OverloadPhase {
    fn goodput(&self) -> f64 {
        self.completed as f64 / self.window_secs.max(0.001)
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_overload_phase(
    serve: &Arc<ReticulumNode>,
    hash: leviculum_core::DestinationHash,
    signing_key: [u8; 32],
    serve_addr: SocketAddr,
    overflow: &Arc<AtomicUsize>,
    n: usize,
    phase: Duration,
    knobs: &SweepKnobs,
) -> OverloadPhase {
    let mut nodes = Vec::with_capacity(n);
    for _ in 0..n {
        if let Some(c) = bring_up_client_tcp_only(serve_addr).await {
            nodes.push(c);
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    serve
        .announce_destination(&hash, Some(b"bench"))
        .await
        .expect("overload announce");

    let mut blob = vec![0u8; knobs.resource_kib * 1024];
    let mut x: u64 = 0x9E3779B97F4A7C15;
    for c in blob.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *c = x as u8;
    }
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cfg = Arc::new(DanceCfg {
        hash,
        signing_key,
        rounds: usize::MAX, // time-boxed: the stop flag ends the phase
        blob,
        establish_timeout: knobs.establish_timeout,
        transfer_timeout: knobs.transfer_timeout,
        stop: Arc::clone(&stop),
    });

    let ready = Arc::new(AtomicUsize::new(0));
    let (report_tx, mut report_rx) = tokio::sync::mpsc::channel(64);
    let mut tasks = Vec::with_capacity(nodes.len());
    for mut node in nodes {
        let Some(rx) = node.take_event_receiver() else {
            continue;
        };
        tasks.push(tokio::spawn(dance_client(
            node,
            rx,
            Arc::clone(&cfg),
            report_tx.clone(),
            Arc::clone(&ready),
        )));
    }
    drop(report_tx);

    // The liveness probe: how long an OUTSIDE caller waits for the node
    // lock while the phase load runs. has_path on an unknown hash is the
    // cheapest lock-taking call the driver exposes.
    let probe_serve = Arc::clone(serve);
    let probe_stop = Arc::clone(&stop);
    let probe = tokio::spawn(async move {
        let mut samples: Vec<f64> = Vec::new();
        let unknown = leviculum_core::DestinationHash::new([0u8; 16]);
        while !probe_stop.load(Ordering::Relaxed) {
            let t = Instant::now();
            let _ = probe_serve.has_path(&unknown);
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        samples
    });

    let overflow_before = overflow.load(Ordering::Relaxed);
    let window_start = Instant::now();
    let deadline = tokio::time::Instant::now() + phase;
    let mut completed = 0usize;
    let mut drained = 0usize;
    let (mut fail_api, mut fail_establish, mut fail_transfer, mut fail_resource) = (0, 0, 0, 0);
    let mut establish_ms: Vec<f64> = Vec::new();
    let mut transfer_ms: Vec<f64> = Vec::new();
    let mut in_window = true;
    // Post-flag drain budget: one dance's worth of timeouts, so an
    // in-flight dance can finish or fail on its own terms.
    let drain_dl =
        deadline + knobs.establish_timeout + knobs.transfer_timeout + Duration::from_secs(5);
    loop {
        let dl = if in_window { deadline } else { drain_dl };
        match tokio::time::timeout_at(dl, report_rx.recv()).await {
            Ok(Some(DanceOutcome::Completed {
                establish_ms: e,
                transfer_ms: t,
            })) => {
                establish_ms.push(e);
                transfer_ms.push(t);
                if in_window {
                    completed += 1;
                } else {
                    drained += 1;
                }
            }
            Ok(Some(DanceOutcome::Failed(f))) => match f {
                DanceFail::Api => fail_api += 1,
                DanceFail::EstablishTimeout => fail_establish += 1,
                DanceFail::TransferTimeout => fail_transfer += 1,
                DanceFail::Resource => fail_resource += 1,
            },
            Ok(None) => break, // every client exited
            Err(_) if in_window => {
                // Window over: raise the flag, keep collecting the drain.
                stop.store(true, Ordering::Relaxed);
                in_window = false;
            }
            Err(_) => break, // drain budget exhausted; abort stragglers below
        }
    }
    stop.store(true, Ordering::Relaxed);
    let window_secs = phase
        .as_secs_f64()
        .min(window_start.elapsed().as_secs_f64());
    for t in &tasks {
        t.abort();
    }
    let mut probe_ms = probe.await.unwrap_or_default();
    probe_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    establish_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    transfer_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());

    OverloadPhase {
        n,
        clients_ready: ready.load(Ordering::Relaxed),
        completed,
        drained,
        fail_api,
        fail_establish,
        fail_transfer,
        fail_resource,
        window_secs,
        establish_ms,
        transfer_ms,
        probe_ms,
        overflow_markers: overflow.load(Ordering::Relaxed) - overflow_before,
        rss_mib: read_rss_mib(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "overload benchmark; run explicitly with --ignored --nocapture"]
async fn link_dance_overload() {
    let sizes: Vec<usize> = std::env::var("OVERLOAD_SIZES")
        .ok()
        .map(|v| {
            v.split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![8, 32, 64, 96, 128]);
    let phase = Duration::from_secs(env_usize("PHASE_SECS", 20) as u64);
    let knobs = SweepKnobs {
        rounds: usize::MAX,
        resource_kib: env_usize("RESOURCE_KIB", 512),
        establish_timeout: Duration::from_secs(env_usize("ESTABLISH_TIMEOUT", 15) as u64),
        transfer_timeout: Duration::from_secs(env_usize("TRANSFER_TIMEOUT", 60) as u64),
        stall: Duration::from_secs(0),  // unused: phases are time-boxed
        level_deadline: Duration::ZERO, // unused: phases are time-boxed
    };

    let (serve, serve_addr, hash, signing_key, serve_drain, overflow) =
        build_dance_serve_node().await;

    println!();
    println!(
        "link dance OVERLOAD — leviculum#46 (degradation envelope, one serve node throughout)"
    );
    println!(
        "phase={}s resource={} KiB establish_to={}s transfer_to={}s",
        phase.as_secs(),
        knobs.resource_kib,
        knobs.establish_timeout.as_secs(),
        knobs.transfer_timeout.as_secs(),
    );
    println!(
        "{:>5} | {:>5} | {:>7} | {:>6} | {:>12} | {:>8} | {:>15} | {:>17} | {:>8} | {:>8}",
        "N",
        "ready",
        "in-win",
        "drain",
        "fail a/e/x/r",
        "good/s",
        "xfer p50/p95",
        "probe p50/p95/max",
        "overflow",
        "rss MiB"
    );

    let mut phases: Vec<OverloadPhase> = Vec::new();
    for n in sizes {
        let r = run_overload_phase(
            &serve,
            hash,
            signing_key,
            serve_addr,
            &overflow,
            n,
            phase,
            &knobs,
        )
        .await;
        println!(
            "{:>5} | {:>5} | {:>7} | {:>6} | {:>12} | {:>8.2} | {:>15} | {:>17} | {:>8} | {:>8.1}",
            r.n,
            r.clients_ready,
            r.completed,
            r.drained,
            format!(
                "{}/{}/{}/{}",
                r.fail_api, r.fail_establish, r.fail_transfer, r.fail_resource
            ),
            r.goodput(),
            format!(
                "{:.0}/{:.0}",
                percentile(&r.transfer_ms, 0.5),
                percentile(&r.transfer_ms, 0.95)
            ),
            format!(
                "{:.2}/{:.2}/{:.2}",
                percentile(&r.probe_ms, 0.5),
                percentile(&r.probe_ms, 0.95),
                r.probe_ms.last().copied().unwrap_or(0.0)
            ),
            r.overflow_markers,
            r.rss_mib,
        );
        phases.push(r);
    }

    // Recovery: the serve node just absorbed the worst phase. A fresh solo
    // client dances until its transfer latency is within 3x of the lightest
    // phase's p50 (or the attempt cap runs out) — time to that dance is the
    // time-to-recover.
    let baseline_p50 = percentile(&phases[0].transfer_ms, 0.5);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut recovery_ms: Vec<f64> = Vec::new();
    let mut recovered_after_s = f64::NAN;
    let recovery_start = Instant::now();
    if let Some(mut solo) = bring_up_client_tcp_only(serve_addr).await {
        if let Some(mut rx) = solo.take_event_receiver() {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = serve.announce_destination(&hash, Some(b"bench")).await;
            // Wait for the path like dance_client does.
            let path_dl = tokio::time::Instant::now() + Duration::from_secs(30);
            let mut have_path = false;
            while let Ok(Some(ev)) = tokio::time::timeout_at(path_dl, rx.recv()).await {
                if matches!(ev, NodeEvent::PathFound { destination_hash, .. } if destination_hash == hash)
                {
                    have_path = true;
                    break;
                }
            }
            if have_path {
                let mut blob = vec![0u8; knobs.resource_kib * 1024];
                let mut x: u64 = 0x2545F4914F6CDD1D;
                for c in blob.iter_mut() {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    *c = x as u8;
                }
                let cfg = DanceCfg {
                    hash,
                    signing_key,
                    rounds: 1,
                    blob,
                    establish_timeout: knobs.establish_timeout,
                    transfer_timeout: knobs.transfer_timeout,
                    stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                };
                let identity = Identity::generate(&mut rand_core::OsRng);
                for _ in 0..10 {
                    match dance_once(&solo, &mut rx, &cfg, &identity).await {
                        DanceOutcome::Completed { transfer_ms: t, .. } => {
                            recovery_ms.push(t);
                            if t <= baseline_p50 * 3.0 && recovered_after_s.is_nan() {
                                recovered_after_s = recovery_start.elapsed().as_secs_f64();
                                break;
                            }
                        }
                        DanceOutcome::Failed(_) => recovery_ms.push(f64::NAN),
                    }
                }
            }
        }
    }
    println!(
        "recovery: baseline p50 {:.0} ms, solo dances {:?} ms, recovered_after {:.1}s",
        baseline_p50, recovery_ms, recovered_after_s
    );

    serve_drain.abort();

    if let Ok(path) = std::env::var("BENCH_JSON_OUT_OVERLOAD") {
        let commit = env_or("GIT_COMMIT", "unknown");
        let date = env_or("BENCH_DATE", "unknown");
        let runner = env_or(
            "BENCH_RUNNER",
            &format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        );
        let mut rows = String::new();
        for (i, r) in phases.iter().enumerate() {
            if i > 0 {
                rows.push(',');
            }
            rows.push_str(&format!(
                "\n    {{\"n\": {}, \"clients_ready\": {}, \"completed_in_window\": {}, \"drained\": {}, \"failures\": {{\"api\": {}, \"establish\": {}, \"transfer\": {}, \"resource\": {}}}, \"window_secs\": {:.1}, \"goodput_dances_s\": {:.2}, \"establish_ms\": {{\"p50\": {:.1}, \"p95\": {:.1}}}, \"transfer_ms\": {{\"p50\": {:.1}, \"p95\": {:.1}}}, \"lock_probe_ms\": {{\"p50\": {:.3}, \"p95\": {:.3}, \"max\": {:.3}}}, \"overflow_markers\": {}, \"rss_mib\": {:.1}}}",
                r.n,
                r.clients_ready,
                r.completed,
                r.drained,
                r.fail_api,
                r.fail_establish,
                r.fail_transfer,
                r.fail_resource,
                r.window_secs,
                r.goodput(),
                percentile(&r.establish_ms, 0.5),
                percentile(&r.establish_ms, 0.95),
                percentile(&r.transfer_ms, 0.5),
                percentile(&r.transfer_ms, 0.95),
                percentile(&r.probe_ms, 0.5),
                percentile(&r.probe_ms, 0.95),
                r.probe_ms.last().copied().unwrap_or(0.0),
                r.overflow_markers,
                r.rss_mib,
            ));
        }
        let solo = recovery_ms
            .iter()
            .map(|v| {
                if v.is_nan() {
                    "null".to_string()
                } else {
                    format!("{v:.0}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let recovered = if recovered_after_s.is_nan() {
            "null".to_string()
        } else {
            format!("{recovered_after_s:.1}")
        };
        let json = format!(
            "{{\n  \"schema\": \"leviculum/bench-results/1\",\n  \"benchmark\": \"link_dance_overload\",\n  \"issue\": \"leviculum#46\",\n  \"commit\": \"{commit}\",\n  \"date\": \"{date}\",\n  \"runner\": \"{runner}\",\n  \"params\": {{\"phase_secs\": {}, \"resource_kib\": {}, \"establish_timeout_s\": {}, \"transfer_timeout_s\": {}}},\n  \"phases\": [{rows}\n  ],\n  \"recovery\": {{\"baseline_transfer_p50_ms\": {:.0}, \"solo_transfer_ms\": [{solo}], \"recovered_after_s\": {recovered}}}\n}}\n",
            phase.as_secs(),
            knobs.resource_kib,
            knobs.establish_timeout.as_secs(),
            knobs.transfer_timeout.as_secs(),
            baseline_p50,
        );
        std::fs::write(path, json).expect("write overload bench json");
    }
}
