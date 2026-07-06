//! Codeberg #38: a Python client link times out through our relay on hop
//! asymmetry, while a leviculum-std client on the identical topology establishes.
//!
//! ## Mechanism (reference-confirmed against vendored Python)
//!
//! The Python **initiator** matches a returning LRPROOF to its pending link ONLY
//! when `packet.hops == link.expected_hops` (`RNS/Transport.py:2226`, unless
//! `expected_hops == PATHFINDER_M`, i.e. the path was unknown at link creation).
//! `expected_hops` is taken from the initiator's path table at `create_link`
//! time. If that path is UNDERCOUNTED (optimistic, fewer hops than the live
//! round trip), the proof returns with `packet.hops > expected_hops`, no pending
//! link matches, `validate_proof` is never called, and `create_link` times out.
//!
//! Our relay `transport.rs:3724` deviates from Python's strict relay
//! (`Transport.py:2176`, which DROPS on hop mismatch): on mismatch it logs
//! `LRPROOF hop asymmetry, forwarding anyway (remaining_hops)` and forwards the
//! proof UNCHANGED — it does NOT rewrite `packet.hops` down to the frozen
//! `remaining_hops` (#38 STEP-0 Q1). So the asymmetric proof reaches the client.
//! Our own leviculum-std initiator accepts it (lenient, Priority 1: max
//! delivery); a strict Python initiator rejects it and times out. Same relay,
//! same page, only the client differs — exactly the field symptom.
//!
//! ## Topology
//!
//! ```text
//!   initiator ──tcp──> relay-1 ──tcp──> [UNDERCOUNT SHIM] ──tcp──> relay-2 ──tcp──> Python responder
//!   (Python A, or       (our lnsd,                                  (our lnsd,        (TestDaemon B,
//!    Rust control)       transport on)                              transport on)     link-accepting)
//! ```
//!
//! The UNDERCOUNT SHIM is an in-test TCP forwarder. It copies bytes verbatim in
//! the initiator→responder direction; in the responder→initiator direction it
//! DECREMENTS the hop-count byte of every ANNOUNCE frame by one (the hop byte is
//! packet byte[1], outside the Ed25519-signed announce payload — #38 STEP-0 Q4),
//! leaving the signature valid. Effect: relay-1 (and the initiator behind it)
//! learn the responder one hop too close. When the initiator links to the
//! responder, relay-1 freezes `remaining_hops` at the undercounted value; the
//! LRPROOF returns over the true (longer) path; relay-1 logs the asymmetry and
//! forwards anyway; the Python initiator's `create_link` times out, while a Rust
//! initiator on the SAME running relays establishes.
//!
//! ## Running
//!
//! ```sh
//! cargo test -p leviculum-std --test rnsd_interop \
//!   lrproof_hop_undercount -- --nocapture --test-threads=1
//! ```

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::DestinationHash;
use leviculum_std::driver::ReticulumNodeBuilder;

use crate::common::{
    temp_storage, wait_for_link_established, wait_for_path_on_node, wait_for_path_reannounce,
    wait_for_path_reannounce_on_daemon,
};
use crate::harness::{pick_free_tcp_port, TestDaemon};

// ----------------------------------------------------------------------------
// Process-global WARN capture (so relay-1's asymmetry warning is observable).
// ----------------------------------------------------------------------------

#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

static LOG_BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

/// Install a global WARN-level tracing subscriber writing into a shared buffer,
/// exactly once per process. Returns the buffer. Because this test is meant to
/// be run by name (effectively in isolation), the global default is ours.
fn captured_logs() -> Arc<Mutex<Vec<u8>>> {
    LOG_BUF
        .get_or_init(|| {
            let buf = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::fmt()
                .with_writer(CaptureWriter(buf.clone()))
                .with_max_level(tracing::Level::WARN)
                .with_ansi(false)
                .with_target(true)
                .finish();
            // Ignore the error if some other subscriber is already global: the
            // divergence assertions (a)+(c) still stand; only assertion (b)
            // depends on this capture.
            let _ = tracing::subscriber::set_global_default(subscriber);
            buf
        })
        .clone()
}

fn log_snapshot(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buf.lock().unwrap()).into_owned()
}

// ----------------------------------------------------------------------------
// The undercount TCP shim.
// ----------------------------------------------------------------------------

/// A packet is an ANNOUNCE with no IFAC signature when byte[0]'s low two bits are
/// `0b01` (PacketType::Announce) and the IFAC bit (0x80) is clear.
fn is_plain_announce(pkt: &[u8]) -> bool {
    pkt.len() >= 2 && (pkt[0] & 0x80) == 0 && (pkt[0] & 0x03) == 0x01
}

/// Copy `reader` → `writer` verbatim (initiator → responder direction).
async fn copy_verbatim<R, W>(mut reader: R, mut writer: W)
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if writer.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = writer.shutdown().await;
}

/// Copy `reader` → `writer` (responder → initiator direction), HDLC-deframing the
/// stream and decrementing the hop byte of each ANNOUNCE frame before re-framing.
async fn copy_undercount_announces<R, W>(mut reader: R, mut writer: W, mangled: Arc<AtomicU64>)
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut deframer = Deframer::new();
    let mut chunk = [0u8; 4096];
    let mut out = Vec::with_capacity(600);
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for result in deframer.process(&chunk[..n]) {
            let DeframeResult::Frame(mut pkt) = result else {
                continue;
            };
            if is_plain_announce(&pkt) && pkt[1] > 0 {
                pkt[1] -= 1; // hop byte, outside the announce signature
                mangled.fetch_add(1, Ordering::Relaxed);
            }
            frame(&pkt, &mut out);
            if writer.write_all(&out).await.is_err() {
                let _ = writer.shutdown().await;
                return;
            }
        }
    }
    let _ = writer.shutdown().await;
}

/// Spawn the undercount shim. Returns its listen address and the counter of
/// mangled announces. relay-1 dials the shim; the shim dials relay-2.
async fn spawn_undercount_shim(relay2_addr: SocketAddr) -> (SocketAddr, Arc<AtomicU64>) {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind shim");
    let shim_addr = listener.local_addr().expect("shim local addr");
    let mangled = Arc::new(AtomicU64::new(0));
    let mangled_ret = mangled.clone();

    tokio::spawn(async move {
        loop {
            let (inbound, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => break,
            };
            let upstream = match TcpStream::connect(relay2_addr).await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let (in_r, in_w) = inbound.into_split();
            let (up_r, up_w) = upstream.into_split();
            let mangled = mangled.clone();
            // initiator(relay-1) → responder(relay-2): verbatim.
            tokio::spawn(copy_verbatim(in_r, up_w));
            // responder(relay-2) → initiator(relay-1): undercount announces.
            tokio::spawn(copy_undercount_announces(up_r, in_w, mangled));
        }
    });

    (shim_addr, mangled_ret)
}

// ----------------------------------------------------------------------------
// The reproduction.
// ----------------------------------------------------------------------------

/// Full #38 reproduction: Python initiator times out, Rust initiator establishes,
/// on the identical hop-undercount relay chain.
#[tokio::test]
async fn lrproof_hop_undercount_python_client_times_out_rust_client_establishes() {
    let log_buf = captured_logs();
    log_buf.lock().unwrap().clear();

    // --- Ports for the two Rust relay servers (the shim binds :0 itself). ---
    let relay1_port = pick_free_tcp_port().expect("relay-1 port");
    let relay2_port = pick_free_tcp_port().expect("relay-2 port");
    let relay1_addr: SocketAddr = format!("127.0.0.1:{relay1_port}").parse().unwrap();
    let relay2_addr: SocketAddr = format!("127.0.0.1:{relay2_port}").parse().unwrap();

    // --- Python responder B (link-accepting destination). ---
    let responder = TestDaemon::start().await.expect("start Python responder B");
    let dest_b = responder
        .register_destination("mvr38", &["responder"])
        .await
        .expect("register responder dest");
    responder
        .set_proof_strategy(&dest_b.hash, "PROVE_ALL")
        .await
        .expect("responder PROVE_ALL");

    let dest_b_hash_bytes: [u8; 16] = hex::decode(&dest_b.hash).unwrap().try_into().unwrap();
    let dest_b_hash = DestinationHash::new(dest_b_hash_bytes);
    // Ed25519 signing (verifying) key = second half of the 64-byte public key.
    let dest_b_key_bytes = hex::decode(&dest_b.public_key).unwrap();
    let mut dest_b_signing_key = [0u8; 32];
    dest_b_signing_key.copy_from_slice(&dest_b_key_bytes[32..64]);

    // --- relay-2: transport on, TCP server for the shim, TCP client to B. ---
    let _relay2_storage = temp_storage("lrproof_hop_undercount", "relay2");
    let mut relay2 = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .add_tcp_server(relay2_addr)
        .add_tcp_client(responder.rns_addr())
        .storage_path(_relay2_storage.path().to_path_buf())
        .build()
        .await
        .expect("build relay-2");
    relay2.start().await.expect("start relay-2");

    // --- The undercount shim between relay-1 and relay-2. ---
    let (shim_addr, mangled) = spawn_undercount_shim(relay2_addr).await;

    // --- relay-1 ("our lnsd"): transport on, TCP server for the initiator,
    //     TCP client to the shim. ---
    let _relay1_storage = temp_storage("lrproof_hop_undercount", "relay1");
    let mut relay1 = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .add_tcp_server(relay1_addr)
        .add_tcp_client(shim_addr)
        .storage_path(_relay1_storage.path().to_path_buf())
        .build()
        .await
        .expect("build relay-1");
    relay1.start().await.expect("start relay-1");

    // Let the relay chain's TCP links come up.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Drive B's announce down the chain; relay-1 must learn B UNDERCOUNTED.
    responder
        .announce_destination(&dest_b.hash, b"mvr38-responder")
        .await
        .expect("announce B");
    let relay1_has_b = wait_for_path_on_node(&relay1, &dest_b_hash, Duration::from_secs(30)).await;
    assert!(
        relay1_has_b,
        "relay-1 must learn a path to the responder within 30s"
    );

    // Sanity: the shim actually mangled ≥1 announce, and relay-1's learned path
    // is the undercounted value (fewer hops than the true 2-hop chain).
    assert!(
        mangled.load(Ordering::Relaxed) >= 1,
        "the shim must have decremented at least one announce hop byte"
    );
    let relay1_hops_to_b = relay1
        .path_table_entries()
        .into_iter()
        .find(|e| e.hash == dest_b_hash_bytes)
        .map(|e| e.hops);
    // True chain initiator..B is 2 transport hops; the undercount makes relay-1
    // store 1. (We assert < 2 rather than == 1 to stay robust to re-announce
    // ordering, while still proving the undercount.)
    assert!(
        matches!(relay1_hops_to_b, Some(h) if h < 2),
        "relay-1's path to B must be undercounted (< 2 hops); got {relay1_hops_to_b:?}"
    );

    // ========================================================================
    // (A) PYTHON initiator: create_link must TIME OUT.
    // ========================================================================
    let initiator_py = TestDaemon::start().await.expect("start Python initiator A");
    initiator_py
        .add_client_interface("127.0.0.1", relay1_port, Some("to-relay1"))
        .await
        .expect("Python A dials relay-1");

    // A must learn a (likewise undercounted) path to B, so create_link freezes a
    // concrete expected_hops (not PATHFINDER_M, which would bypass the check).
    let py_has_b = wait_for_path_reannounce_on_daemon(
        &initiator_py,
        &dest_b_hash,
        &responder,
        &dest_b.hash,
        b"mvr38-responder",
        Duration::from_secs(30),
    )
    .await;
    assert!(
        py_has_b,
        "Python initiator A must learn a path to B before create_link"
    );

    let py_link = initiator_py
        .create_link(&dest_b.hash, &dest_b.public_key, 15)
        .await;
    let logs = log_snapshot(&log_buf);
    assert!(
        py_link.is_err(),
        "REPRODUCTION (a): the Python initiator's create_link must TIME OUT on \
         the hop asymmetry, but it returned {py_link:?}.\n--- relay logs ---\n{logs}"
    );

    // ========================================================================
    // (B) our relay logged the asymmetry-forward.
    // ========================================================================
    assert!(
        logs.contains("LRPROOF hop asymmetry, forwarding anyway"),
        "REPRODUCTION (b): relay-1 must log the LRPROOF hop asymmetry \
         (forwarding anyway).\n--- relay logs ---\n{logs}"
    );

    // ========================================================================
    // (C) CONTROL: a leviculum-std initiator on the SAME running relays DOES
    //     establish the link — the client-side divergence.
    // ========================================================================
    let _initiator_rs_storage = temp_storage("lrproof_hop_undercount", "initiator_rs");
    let mut initiator_rs = ReticulumNodeBuilder::new()
        .add_tcp_client(relay1_addr)
        .storage_path(_initiator_rs_storage.path().to_path_buf())
        .build()
        .await
        .expect("build Rust initiator");
    let mut events_rs = initiator_rs.take_event_receiver().unwrap();
    initiator_rs.start().await.expect("start Rust initiator");
    tokio::time::sleep(Duration::from_secs(1)).await;

    let rs_has_b = wait_for_path_reannounce(
        || initiator_rs.has_path(&dest_b_hash),
        &responder,
        &dest_b.hash,
        b"mvr38-responder",
        Duration::from_secs(30),
    )
    .await;
    assert!(
        rs_has_b,
        "Rust control initiator must learn a path to B before connect"
    );

    let handle = initiator_rs
        .connect(&dest_b_hash, &dest_b_signing_key)
        .await
        .expect("Rust initiator connect");
    let established =
        wait_for_link_established(&mut events_rs, handle.link_id(), Duration::from_secs(20)).await;
    let logs = log_snapshot(&log_buf);
    assert!(
        established,
        "REPRODUCTION (c/CONTROL): the leviculum-std initiator must ESTABLISH the \
         link on the identical hop-undercount chain where the Python initiator \
         timed out. This is the #38 divergence: same relay, only the client \
         differs.\n--- relay logs ---\n{logs}"
    );

    // Tidy up async nodes.
    let _ = initiator_rs.stop().await;
    let _ = relay1.stop().await;
    let _ = relay2.stop().await;
}
