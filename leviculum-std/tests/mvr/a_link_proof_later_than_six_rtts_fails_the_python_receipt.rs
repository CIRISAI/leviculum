//! mvr for order 1607: our proof of a Python link packet is valid, and the
//! Python receipt still reads FAILED when the proof arrives later than the
//! receipt's own deadline, which on a loopback link is a few milliseconds.
//!
//! Field shape: `rnsd_interop`'s
//! `link_packet_interop_tests::test_python_raw_link_packet_to_rust_prove_all_receipt_delivered`
//! went red once in 380 under the parallel load of `cargo test --workspace
//! --no-fail-fast` (1579): FAILED instead of DELIVERED, green alone.
//!
//! The reference half: `RNS.Packet(link, data).send()` gives the receipt
//! `timeout = max(link.rtt * link.traffic_timeout_factor, 5 ms)` with a
//! factor of 6 (`reference/Reticulum/RNS/Packet.py:431`,
//! `RNS/Link.py:81-82`). `check_timeout` turns a receipt still SENT past
//! `sent_at + timeout` into FAILED (`Packet.py:557-565`); Python calls it
//! from its job loop once a second (`RNS/Transport.py:556-567`) and the test
//! daemon calls it on every `get_receipt_status`. Measured on the coder host
//! on an idle loopback link: RTT 0.7 to 1.3 ms, deadline 5.0 to 7.6 ms, our
//! proof back after 0.4 to 0.6 ms. Beside `stress-ng --cpu 30` one run's
//! proof came back after 23.9 ms against a 23.6 ms deadline and stayed
//! DELIVERED only because no check fell into the gap.
//!
//! The Rust half: our proof leaves in the same core step that decrypts the
//! data (`leviculum-core/src/node/link_management.rs`, the
//! `ProofStrategy::All` arm of the plain link-data handler), before
//! `LinkDataReceived` is even queued. There is no work on that path a load
//! stall could be blamed on; the proof is late only when a process is not
//! scheduled. So the race is the harness's, not the product's: which of the
//! two events, the proof reaching Python's `validate_link_proof`
//! (`Packet.py:450`) or a `check_timeout` past the deadline, comes first.
//!
//! Here the stall is made deterministic instead of drawn from the host: a
//! relay between our node and the Python daemon passes everything until the
//! link is up, then holds every byte from us for [`PROOF_HOLD`]. The first
//! test reads the receipt once its deadline has passed and before the held
//! proof is let through: FAILED, the field red. The second sends the same
//! packet with the deadline the interop test now declares
//! (`TestDaemon::send_on_link_tracked`'s `receipt_timeout`): DELIVERED.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use leviculum_core::link::LinkId;
use leviculum_core::node::NodeEvent;
use leviculum_core::{Destination, DestinationType, Direction, Identity, ProofStrategy};
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::EventReceiver;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::harness::TestDaemon;

/// How long the relay holds our bytes once armed: far above any loopback
/// receipt deadline (6 x an RTT of a few ms) and far below the declared one.
const PROOF_HOLD: Duration = Duration::from_millis(1500);

/// A TCP relay that, once `armed`, holds every chunk from our node for
/// [`PROOF_HOLD`] before passing it on. The other direction is never held.
async fn spawn_holding_relay(
    upstream: std::net::SocketAddr,
) -> (std::net::SocketAddr, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind relay");
    let addr = listener.local_addr().expect("relay addr");
    let armed = Arc::new(AtomicBool::new(false));
    let armed_task = armed.clone();
    tokio::spawn(async move {
        let Ok((ours, _)) = listener.accept().await else {
            return;
        };
        let Ok(theirs) = TcpStream::connect(upstream).await else {
            return;
        };
        let (mut ours_rx, mut ours_tx) = ours.into_split();
        let (mut theirs_rx, mut theirs_tx) = theirs.into_split();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            while let Ok(n) = theirs_rx.read(&mut buf).await {
                if n == 0 || ours_tx.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
        });
        let mut buf = vec![0u8; 4096];
        while let Ok(n) = ours_rx.read(&mut buf).await {
            if n == 0 {
                break;
            }
            if armed_task.load(Ordering::SeqCst) {
                tokio::time::sleep(PROOF_HOLD).await;
            }
            if theirs_tx.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    });
    (addr, armed)
}

async fn wait_for<T>(
    events: &mut EventReceiver,
    within: Duration,
    mut pick: impl FnMut(NodeEvent) -> Option<T>,
) -> Option<T> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, events.recv()).await {
            Ok(Some(event)) => {
                if let Some(found) = pick(event) {
                    return Some(found);
                }
            }
            _ => return None,
        }
    }
}

/// A Python-initiated link to a PROVE_ALL destination on our node, through
/// the relay. Returns the relay's arm switch with everything the test needs.
struct Rig {
    daemon: TestDaemon,
    node: ReticulumNode,
    events: EventReceiver,
    link_id: LinkId,
    py_link: String,
    armed: Arc<AtomicBool>,
    _storage: tempfile::TempDir,
}

async fn rig() -> Rig {
    let daemon = TestDaemon::start().await.expect("start daemon");
    let (relay, armed) = spawn_holding_relay(daemon.rns_addr()).await;

    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(relay)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    let mut events = node.take_event_receiver().expect("event receiver");
    node.start().await.expect("start node");
    node.wait_for_interface_ready(0, Duration::from_secs(5))
        .await
        .expect("the TCP client connects through the relay");

    let identity = Identity::generate(&mut rand_core::OsRng);
    let public_key = hex::encode(identity.public_key_bytes());
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvr",
        &["1607", "prover"],
    )
    .expect("create destination");
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    node.register_destination(dest);
    node.announce_destination(&dest_hash, None)
        .await
        .expect("announce");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !daemon.has_path(dest_hash.as_bytes()).await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "Python never learned the prover's path"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let py_link = daemon
        .create_link(&hex::encode(dest_hash.as_bytes()), &public_key, 10)
        .await
        .expect("Python establishes the link");
    let link_id = wait_for(&mut events, Duration::from_secs(5), |e| match e {
        NodeEvent::LinkEstablished {
            link_id,
            is_initiator: false,
            ..
        } => Some(link_id),
        _ => None,
    })
    .await
    .expect("our side sees the incoming link established");

    Rig {
        daemon,
        node,
        events,
        link_id,
        py_link,
        armed,
        _storage: storage,
    }
}

async fn wait_for_data(rig: &mut Rig) {
    let link_id = rig.link_id;
    wait_for(&mut rig.events, Duration::from_secs(5), |e| match e {
        NodeEvent::LinkDataReceived { link_id: id, .. } if id == link_id => Some(()),
        _ => None,
    })
    .await
    .expect("our node receives the Python packet (the proof leaves in the same step)");
}

fn field<'a>(report: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    report.get(key).unwrap_or(&serde_json::Value::Null)
}

/// The field red, made deterministic: the proof is held past the reference's
/// own deadline, and a status read in that gap concludes FAILED although the
/// proof that follows is valid.
#[tokio::test]
async fn a_link_proof_later_than_six_rtts_fails_the_python_receipt() {
    let mut rig = rig().await;
    rig.armed.store(true, Ordering::SeqCst);

    let (receipt, _) = rig
        .daemon
        .send_on_link_tracked(&rig.py_link, b"held proof", None)
        .await
        .expect("send_on_link_tracked");
    wait_for_data(&mut rig).await;

    // Read once the receipt's own deadline has passed, well inside the hold.
    let report = loop {
        let report = rig
            .daemon
            .get_receipt_report(&receipt)
            .await
            .expect("get_receipt_report");
        let timeout = field(&report, "timeout").as_f64().expect("timeout");
        let age = field(&report, "age").as_f64().expect("age");
        assert!(
            timeout < PROOF_HOLD.as_secs_f64() / 2.0,
            "the reference deadline {timeout} s is not a loopback one: {report}"
        );
        if age > timeout || field(&report, "status") != "SENT" {
            break report;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    assert!(
        field(&report, "age").as_f64().unwrap_or(f64::MAX) < PROOF_HOLD.as_secs_f64(),
        "the read came after the hold, the proof may have arrived: {report}"
    );
    assert_eq!(
        field(&report, "status"),
        "FAILED",
        "a proof held {PROOF_HOLD:?} must miss Python's receipt deadline \
         max(6 x rtt, 5 ms) (Packet.py:431): {report}"
    );

    rig.node.stop().await.expect("stop node");
}

/// The same held proof under the deadline the interop test declares: the
/// receipt waits for it and reads DELIVERED, so what the interop test asks,
/// whether our proof validates at Python, no longer depends on the scheduler.
#[tokio::test]
async fn the_same_held_proof_is_delivered_under_a_declared_receipt_deadline() {
    let mut rig = rig().await;
    rig.armed.store(true, Ordering::SeqCst);

    let (receipt, _) = rig
        .daemon
        .send_on_link_tracked(&rig.py_link, b"held proof", Some(10.0))
        .await
        .expect("send_on_link_tracked");
    wait_for_data(&mut rig).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    let report = loop {
        let report = rig
            .daemon
            .get_receipt_report(&receipt)
            .await
            .expect("get_receipt_report");
        if field(&report, "status") != "SENT" || tokio::time::Instant::now() > deadline {
            break report;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        field(&report, "status"),
        "DELIVERED",
        "the held proof must validate under a 10 s receipt deadline: {report}"
    );
    let concluded = field(&report, "concluded_after").as_f64().unwrap_or(0.0);
    assert!(
        concluded >= PROOF_HOLD.as_secs_f64() * 0.9,
        "the proof concluded after {concluded} s, before the hold ended: the \
         relay did not hold it, so this test proved nothing: {report}"
    );

    rig.node.stop().await.expect("stop node");
}
