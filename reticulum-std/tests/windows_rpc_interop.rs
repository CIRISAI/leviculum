//! Windows daemon ↔ Python-RNS RPC interop over TCP loopback.
//!
//! On Unix this surface is covered by the `rnsd_interop` crate's
//! `rpc_interop_tests` (Python tools vs the Rust daemon over an AF_UNIX
//! abstract socket). On Windows the shared-instance RPC speaks the *same*
//! umsgpack/pickle + HMAC wire format but over **TCP loopback**
//! (`127.0.0.1:37429`), mirroring Python-RNS's AF_INET fallback. This test
//! proves that path end-to-end by driving real Python-RNS
//! (`RNS.Reticulum().get_interface_stats()`) against an in-process Rust daemon
//! — catching any Windows-specific multiprocessing AF_INET framing, auth, or
//! socket-setup regression that the pure build/lib-test jobs cannot.
//!
//! It probes the RPC directly rather than scraping `rnstatus` text: `rnstatus`
//! filters the shared-instance interface out of its display, so with no other
//! interfaces it prints nothing even when the RPC succeeds. `get_interface_stats`
//! returns the daemon's `transport_id`, which is the unambiguous interop signal.
//!
//! It uses the **default** instance (ports 37428/37429 — the Python-compatible
//! values), so it is a single serial test function: two instances would
//! collide on those fixed ports, exactly as they would for two Windows `rnsd`s.
#![cfg(windows)]

use std::time::Duration;

use reticulum_core::Identity;
use reticulum_std::driver::ReticulumNodeBuilder;

/// Real Python-RNS must complete a `get_interface_stats()` RPC against a
/// Windows Rust daemon whose shared-instance RPC is served over TCP loopback —
/// proving the multiprocessing AF_INET auth + framing interoperate, returning
/// our `transport_id`.
#[tokio::test]
async fn python_rns_rpc_interop_over_tcp_loopback() {
    // Generate the identity up front so we can write the matching
    // `transport_identity` for Python: the RPC authkey is SHA-256(prv), so both
    // sides must hold the same 64 private-key bytes to complete the handshake.
    // Surface the daemon's own tracing (RPC/local bind success or failure) on
    // stderr so a failing run is diagnosable without a second iteration.
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_test_writer()
        .try_init();

    let identity = Identity::generate(&mut rand_core::OsRng);
    let identity_bytes = identity
        .private_key_bytes()
        .expect("generated identity must have private keys");

    let storage = tempfile::tempdir().expect("create node storage tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .identity(identity)
        .enable_transport(true)
        // Default instance → "rns/default" / "rns/default/rpc" → TCP 37428 /
        // 37429 on Windows (loopback_addr), the ports a Windows rnsd uses.
        .share_instance(true)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build Rust daemon node");
    node.start().await.expect("start Rust node");

    // Let the local + RPC listeners bind before the client connects.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Python config dir. `share_instance = Yes` makes rnstatus try to bind the
    // shared instance, fail (our daemon holds the port), and fall back to
    // connecting as a client to 127.0.0.1:37429. The transport_identity gives
    // it the same RPC authkey.
    let cfg = std::env::temp_dir().join("leviculum_win_rpc_interop");
    let _ = std::fs::remove_dir_all(&cfg);
    std::fs::create_dir_all(cfg.join("storage")).expect("create python storage dir");
    std::fs::write(
        cfg.join("storage").join("transport_identity"),
        identity_bytes,
    )
    .expect("write transport_identity");
    std::fs::write(
        cfg.join("config"),
        "[reticulum]\n\
         \x20 enable_transport = no\n\
         \x20 share_instance = Yes\n\
         \x20 shared_instance_type = tcp\n\
         \n\
         [logging]\n\
         \x20 loglevel = 7\n\
         \n\
         [interfaces]\n",
    )
    .expect("write python config");

    // Drive the RPC the same way rnstatus does — RNS.Reticulum() connects to the
    // shared instance, then get_interface_stats() does the multiprocessing RPC to
    // 127.0.0.1:37429 — but WITHOUT rnstatus's bare `try:` that swallows the
    // exception. On failure this prints the full traceback (ConnectionReset vs
    // AuthenticationError vs framing), which is the root-cause signal. On success
    // it prints the stats keys; STATS_OK requires the transport_id our daemon
    // serves, proving the TCP-loopback RPC speaks Python's wire format.
    let probe = cfg.join("probe.py");
    std::fs::write(
        &probe,
        r#"import sys, traceback, RNS
cfg = sys.argv[1]
try:
    r = RNS.Reticulum(configdir=cfg, loglevel=7)
    print("CONNECTED_SHARED", r.is_connected_to_shared_instance, flush=True)
    stats = r.get_interface_stats()
    keys = sorted(stats.keys()) if isinstance(stats, dict) else None
    print("STATS_KEYS", keys, flush=True)
    if isinstance(stats, dict) and "transport_id" in stats:
        print("STATS_OK", flush=True)
    else:
        print("STATS_MISSING_TRANSPORT_ID", flush=True)
        sys.exit(4)
except Exception:
    traceback.print_exc()
    sys.exit(3)
"#,
    )
    .expect("write probe.py");

    // On Windows the interpreter is `python` (pip installs rns into it in CI).
    let output = tokio::process::Command::new("python")
        .arg(&probe)
        .arg(&cfg)
        .output()
        .await
        .expect("spawn python probe");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("=== probe exit {:?} ===", output.status.code());
    eprintln!("=== probe STDOUT ===\n{stdout}");
    eprintln!("=== probe STDERR ===\n{stderr}");

    let _ = std::fs::remove_dir_all(&cfg);

    assert!(
        stdout.contains("STATS_OK"),
        "Python get_interface_stats() over the TCP-loopback RPC must return our \
         daemon's transport_id (exit {:?})",
        output.status.code()
    );
}
