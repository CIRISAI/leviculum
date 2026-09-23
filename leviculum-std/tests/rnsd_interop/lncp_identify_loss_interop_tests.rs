//! A lost LINKIDENTIFY turns into a rejected transfer, and lncp gives up.
//!
//! `lora_lncp_auth_to_python` went RED in the full hardware run of
//! 2026-09-23 06:18. Alpha (our `lnsd`, the sender) put three frames on the
//! wire inside 130 ms — LRRTT, LINKIDENTIFY, RESOURCE_ADV — and 2.3 s later
//! got a 115 B answer that was an `RCL` where the green runs carry a
//! `RESOURCE_REQ`. Python 1.5.2 answers an advertisement with `RCL` in exactly
//! one place: `receive_resource_callback` returns False
//! (`reference/Reticulum/RNS/Utilities/rncp.py:254-266`) because
//! `resource.link.get_remote_identity()` is None, and `Link.py:1091-1097` then
//! calls `RNS.Resource.reject`. The allowed hash was alpha's, so the identity
//! was absent: the identify frame never reached Python's link.
//!
//! The link stayed up the whole time. `Resource.reject` sends the RCL and
//! nothing else. The sender had a live link and a peer that had simply not
//! heard who it was, and it quit.
//!
//! This test **models** that loss; it does not reproduce the air. Whether the
//! 06:18 frame died on air or in the receiving modem's turnaround is a
//! third-listener measurement on the rig, owed separately. Here a frame-aware
//! TCP proxy sits between our node and a Python `rncp -l -a <our hash>` and
//! drops exactly the first packet whose context byte is `LINKIDENTIFY` (0xFB),
//! passing every other frame in both directions untouched.
//!
//! The proxy itself lives in [`crate::identify_loss_proxy`], because the
//! receiver half of this defect needs the same loss in the other direction;
//! here the upstream peer is the Python listener and our sender is downstream.
//!
//! The assertion is the user-visible outcome: `lncp` delivers the file over a
//! link whose identify was lost once. On the code of 2026-09-23 it does not —
//! the failure message carries the observed chain (ADV sent, RCL received,
//! `ResourceError::Cancelled`) so the red is readable without a log dig.
//!
//! The sender is `leviculum_cli::cp::run_send`, i.e. the push loop the `lncp`
//! binary runs, not a copy of it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leviculum_core::Identity;
use leviculum_std::driver::ReticulumNodeBuilder;
use rand_core::OsRng;

use crate::harness::find_available_ports;
use crate::identify_loss_proxy::{
    prepare_rncp_sender, python_rns_available, run_rncp_sender, spawn_identify_dropping_proxy,
    spawn_rncp_listener, ProxyLog, RncpListener,
};

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

/// One push of a 1 KiB file from `lncp`'s own send loop to a Python
/// `rncp -l -a <our hash>`, through the proxy, with `identify_drop_budget`
/// LINKIDENTIFY frames swallowed on the way.
///
/// Everything both tests share lives here, so the only difference between them
/// is the number the proxy is given.
struct PushOutcome {
    /// What `lncp` would have printed and exited with.
    result: Result<(), String>,
    /// The file as `rncp` would have saved it, if it did.
    received: PathBuf,
    payload: Vec<u8>,
    /// Frames carried and swallowed, quoted by every assertion below so a red
    /// run names the chain instead of pointing at a log.
    evidence: String,
    identify_drops: usize,
    rncp_log: String,
    /// Kept alive for the caller: dropping it kills `rncp` and deletes the
    /// directory the received file is in.
    _rncp: RncpListener,
    _tmp: tempfile::TempDir,
}

async fn push_with_identify_drops(identify_drop_budget: usize) -> PushOutcome {
    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = crate::common::temp_storage("lncp_identify_loss", &format!("run{test_id}"));

    let sender_identity = Identity::generate(&mut OsRng);
    let sender_hash_hex = hex::encode(sender_identity.hash());

    let (ports, _alloc) = find_available_ports::<2>().await.expect("allocate ports");
    let (python_port, proxy_port) = (ports[0], ports[1]);

    let (rncp, dest_hash) = spawn_rncp_listener(python_port, &sender_hash_hex, 5, tmp.path());
    let dest_hash_hex = hex::encode(dest_hash.as_bytes());

    let log = Arc::new(Mutex::new(ProxyLog::default()));
    spawn_identify_dropping_proxy(
        proxy_port,
        python_port,
        identify_drop_budget,
        Arc::clone(&log),
    )
    .await;

    let proxy_addr: std::net::SocketAddr = format!("127.0.0.1:{proxy_port}")
        .parse()
        .expect("proxy addr");
    let storage = tmp.path().join("sender-storage");
    std::fs::create_dir_all(&storage).expect("create sender storage");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(proxy_addr)
        .storage_path(storage)
        .build()
        .await
        .expect("build sender node");
    let mut events = node.take_event_receiver().expect("event receiver");
    node.start().await.expect("start sender node");

    // rncp announces every 5 s (-b 5); the path and the peer's public key both
    // arrive with the announce, and there is no transport node to answer a
    // path request, so waiting for the announce is the only readiness signal.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !node.has_path(&dest_hash) {
        assert!(
            Instant::now() < deadline,
            "no announce from rncp within 60 s; rncp log: {}",
            rncp.log()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let payload: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
    let file_path = tmp.path().join("identify-loss.bin");
    std::fs::write(&file_path, &payload).expect("write payload");

    let result = leviculum_cli::cp::run_send(
        &node,
        &mut events,
        file_path.to_str().expect("utf-8 path"),
        &dest_hash_hex,
        Some(90.0),
        0,
        // Not quiet: with --nocapture the recovery line is the evidence that
        // the retry ran, and the final message is the one a user would read.
        false,
        true,
        Some(&sender_identity),
        false,
    )
    .await
    .map_err(|e| e.to_string());

    let (evidence, identify_drops) = {
        let log = log.lock().expect("proxy log");
        (log.evidence(), log.dropped_identify)
    };

    PushOutcome {
        result,
        received: rncp.save_dir.join("identify-loss.bin"),
        payload,
        evidence,
        identify_drops,
        rncp_log: rncp.log(),
        _rncp: rncp,
        _tmp: tmp,
    }
}

/// A single lost identify must not cost the transfer: `lncp` has a live link
/// and a peer that never heard who it was, and one more identify plus one more
/// advertisement is all the protocol needs.
///
/// Red before the remote-rejection fix: the transfer concluded with
/// `ResourceError::Cancelled` and `lncp` returned "The transfer failed:
/// Cancelled".
#[tokio::test]
async fn lncp_recovers_from_a_dropped_link_identify() {
    if !python_rns_available() {
        eprintln!("skipping lncp identify-loss interop: python3 + vendored RNS unavailable");
        return;
    }
    crate::common::init_tracing();

    let outcome = push_with_identify_drops(1).await;

    assert!(
        outcome.result.is_ok(),
        "lncp must deliver the file after one lost identify, got {:?}. {}",
        outcome.result.as_ref().err(),
        outcome.evidence,
    );
    assert_eq!(
        outcome.identify_drops, 1,
        "the modelled loss must have fired exactly once. {}",
        outcome.evidence
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    while !outcome.received.is_file() {
        assert!(
            Instant::now() < deadline,
            "rncp never wrote the file. {}\nrncp log: {}",
            outcome.evidence,
            outcome.rncp_log,
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(
        std::fs::read(&outcome.received).expect("read received file"),
        outcome.payload,
        "rncp must reassemble the payload byte for byte. {}",
        outcome.evidence
    );
}

/// The recovery is one retry, not a loop. With every identify swallowed the
/// peer rejects twice, and `lncp` has to stop and say what the peer did rather
/// than keep re-advertising or blame a cancel nobody local made.
#[tokio::test]
async fn lncp_gives_up_after_one_retry_and_names_the_rejection() {
    if !python_rns_available() {
        eprintln!("skipping lncp identify-loss interop: python3 + vendored RNS unavailable");
        return;
    }
    crate::common::init_tracing();

    let outcome = push_with_identify_drops(usize::MAX).await;

    let message = outcome
        .result
        .as_ref()
        .err()
        .cloned()
        .unwrap_or_else(|| panic!("lncp must not report success. {}", outcome.evidence));
    assert_eq!(
        message, "The transfer failed: the remote rejected the transfer",
        "the final message names what the peer did. {}",
        outcome.evidence
    );
    assert_eq!(
        outcome.identify_drops, 2,
        "exactly one retry: the first identify and the retry's, and no third. {}",
        outcome.evidence
    );
    assert!(
        !outcome.received.is_file(),
        "rncp must not have received anything. {}",
        outcome.evidence
    );
}

/// Reference arm, measured not read: Python's `rncp` as the sender through the
/// **same** proxy, so the only thing that differs between the arms is the stack
/// under test.
///
/// `rncp.py:714` calls `link.identify(identity)` and `rncp.py:717` constructs
/// the `RNS.Resource` with no wait and no acknowledgement in between, because
/// the protocol has no identify acknowledgement. A Python sender that loses
/// that frame therefore fails exactly the way ours did on 2026-09-23 06:18.
/// That is the reference's limit, not a behaviour a peer depends on — which is
/// what makes the recovery in `cp.rs` a deviation the rule permits rather than
/// an incompatibility.
///
/// Ignored by default: it is a measurement, not a gate. Run it with
///
/// ```sh
/// cargo test -p leviculum-std --test rnsd_interop \
///     python_rncp_sender_fails_on_the_same_dropped_identify -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "reference measurement: Python rncp as the sender through the same proxy"]
async fn python_rncp_sender_fails_on_the_same_dropped_identify() {
    if !python_rns_available() {
        eprintln!("skipping reference arm: python3 + vendored RNS unavailable");
        return;
    }

    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = crate::common::temp_storage("rncp_identify_loss", &format!("run{test_id}"));

    // The sender's identity is seeded here so the listener's -a can name it
    // before either process starts, exactly as in the Rust arm.
    let sender_identity = Identity::generate(&mut OsRng);
    let sender_hash_hex = hex::encode(sender_identity.hash());

    let (ports, _alloc) = find_available_ports::<2>().await.expect("allocate ports");
    let (python_port, proxy_port) = (ports[0], ports[1]);

    let (rncp, dest_hash) = spawn_rncp_listener(python_port, &sender_hash_hex, 5, tmp.path());
    let dest_hash_hex = hex::encode(dest_hash.as_bytes());

    let log = Arc::new(Mutex::new(ProxyLog::default()));
    spawn_identify_dropping_proxy(proxy_port, python_port, 1, Arc::clone(&log)).await;

    let (sender_config_dir, sender_identity_path) =
        prepare_rncp_sender(tmp.path(), &sender_identity, proxy_port);

    let payload: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
    let file_path = tmp.path().join("identify-loss.bin");
    std::fs::write(&file_path, &payload).expect("write payload");

    // The listener announces every 5 s and there is no transport node, so the
    // sender's own path wait (rncp.py:658-664) is what covers the gap. 150 s
    // is well past the 90 s -w budget it is given.
    let output = run_rncp_sender(
        sender_config_dir,
        sender_identity_path,
        file_path,
        dest_hash_hex,
        90,
        150,
    )
    .await;

    let stdout = String::from_utf8_lossy(&output.stdout).replace('\n', " | ");
    let evidence = log.lock().expect("proxy log").evidence();
    eprintln!(
        "REFERENCE rncp exit={:?} stdout=\"{stdout}\" {evidence}",
        output.status.code()
    );

    assert!(
        !output.status.success(),
        "the reference sender recovered where ours does not; stop and re-read the \
         hypothesis before changing lncp. exit={:?} stdout=\"{stdout}\" {evidence}\n\
         rncp listener log: {}",
        output.status.code(),
        rncp.log(),
    );
    assert!(
        !rncp.save_dir.join("identify-loss.bin").is_file(),
        "the reference sender delivered the file; stop and re-read the hypothesis. {evidence}"
    );
}
