//! Windows daemon ↔ Python-RNS RPC interop over TCP loopback.
//!
//! On Unix this surface is covered by the `rnsd_interop` crate's
//! `rpc_interop_tests` (Python tools vs the Rust daemon over an AF_UNIX
//! abstract socket). On Windows the shared-instance RPC speaks the *same*
//! pickle/umsgpack + HMAC wire format but over **TCP loopback**
//! (`127.0.0.1:37429`), mirroring Python-RNS's AF_INET fallback. This test
//! proves that path end-to-end by running the real `rnstatus` against an
//! in-process Rust daemon — catching any Windows-specific framing, auth, or
//! socket-setup regression that the pure build/lib-test jobs cannot.
//!
//! It uses the **default** instance (ports 37428/37429 — the Python-compatible
//! values), so it is a single serial test function: two instances would
//! collide on those fixed ports, exactly as they would for two Windows `rnsd`s.
#![cfg(windows)]

use std::time::Duration;

use reticulum_core::Identity;
use reticulum_std::driver::ReticulumNodeBuilder;

/// `rnstatus --config <dir>` must succeed against a Windows Rust daemon whose
/// shared-instance RPC is served over TCP loopback.
#[tokio::test]
async fn rnstatus_against_windows_rust_daemon_over_tcp() {
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

    // On Windows the interpreter is `python` (pip installs rns into it in CI).
    let output = tokio::process::Command::new("python")
        .args(["-m", "RNS.Utilities.rnstatus", "--config"])
        .arg(&cfg)
        .output()
        .await
        .expect("spawn python rnstatus");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let _ = std::fs::remove_dir_all(&cfg);

    // Always surface rnstatus output: an exit-0-but-empty run means it fell
    // back to spawning its own instance instead of reaching our daemon, which
    // is exactly the failure we need to see.
    eprintln!("=== rnstatus exit {:?} ===", output.status.code());
    eprintln!("=== rnstatus STDOUT ===\n{stdout}");
    eprintln!("=== rnstatus STDERR ===\n{stderr}");

    assert!(
        output.status.success(),
        "rnstatus exited with code {:?}",
        output.status.code()
    );

    // Python parsed our pickle/HMAC RPC reply and formatted the transport
    // instance — the definitive cross-stack interop assertion.
    assert!(
        stdout.contains("Transport Instance"),
        "rnstatus should show the transport instance, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Uptime"),
        "rnstatus should show uptime, got:\n{stdout}"
    );
}
