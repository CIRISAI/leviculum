//! RPC interop tests: run actual Python CLI tools against the Rust daemon.
//!
//! These tests verify that `rnstatus`, `rnpath`, and other Python utilities
//! can query a running Rust daemon via the `multiprocessing.connection` RPC
//! channel. This catches pickle format mismatches, HMAC handshake
//! incompatibilities, missing dict keys, and wire-level differences.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use leviculum_core::Identity;
use leviculum_std::driver::ReticulumNodeBuilder;

use crate::common::init_tracing;
use crate::harness::find_available_ports;

/// Unique counter to avoid collisions between parallel tests.
static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Path to vendor Python utilities.
const RNSTATUS_PY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../vendor/Reticulum/RNS/Utilities/rnstatus.py"
);
pub(crate) const RNPATH_PY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../vendor/Reticulum/RNS/Utilities/rnpath.py"
);

/// Path to vendor Reticulum package (for PYTHONPATH).
const VENDOR_RNS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../vendor/Reticulum");

/// Start a Rust daemon with shared instance + RPC and return the node,
/// instance name, TCP address, and the identity's private key bytes
/// (needed to write the transport_identity file for Python tools).
pub(crate) async fn start_rust_daemon_with_rpc() -> (
    leviculum_std::ReticulumNode,
    String,
    SocketAddr,
    [u8; 64],
    tempfile::TempDir,
) {
    let (ports, _port_alloc) = find_available_ports::<2>()
        .await
        .expect("failed to allocate ports");
    let tcp_port = ports[0];
    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let instance_name = format!("rpcinterop_{}_{}", std::process::id(), test_id);
    let tcp_addr: SocketAddr = format!("127.0.0.1:{}", tcp_port).parse().unwrap();

    // Generate identity up front so we can extract private key bytes
    // before handing ownership to the builder.
    let identity = Identity::generate(&mut rand_core::OsRng);
    let identity_bytes = identity
        .private_key_bytes()
        .expect("generated identity must have private keys");

    let storage = crate::common::temp_storage("start_rust_daemon_with_rpc", "node");
    let mut node = ReticulumNodeBuilder::new()
        .identity(identity)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.clone())
        .add_tcp_server(tcp_addr)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("Failed to build Rust daemon node");

    node.start().await.expect("Failed to start Rust node");

    // Wait for sockets to be ready
    tokio::time::sleep(Duration::from_millis(500)).await;

    (node, instance_name, tcp_addr, identity_bytes, storage)
}

/// Create a temp config directory with a Python-compatible config file and
/// transport_identity file so that Python tools derive the same RPC auth key.
///
/// Returns the path to the temp directory.
fn create_python_config_dir(instance_name: &str, identity_bytes: &[u8; 64]) -> PathBuf {
    let tempdir = std::env::temp_dir().join(format!("rpc_interop_test_{}", instance_name));

    // Clean up from any previous run
    let _ = std::fs::remove_dir_all(&tempdir);

    // Create directory structure
    let storage_dir = tempdir.join("storage");
    std::fs::create_dir_all(&storage_dir).expect("create storage dir");

    // Write the transport_identity file (64 bytes: X25519 prv + Ed25519 prv)
    // Must match exactly what the Rust daemon uses, so Python derives the same
    // RPC auth key = SHA-256(private_key_bytes).
    std::fs::write(storage_dir.join("transport_identity"), identity_bytes)
        .expect("write transport_identity");

    // Write minimal Python Reticulum config (INI format).
    // share_instance = Yes makes Python connect as a client to the existing daemon.
    // instance_name must match so it finds the right Unix socket.
    let config_content = format!(
        "[reticulum]\n\
         \x20 enable_transport = no\n\
         \x20 share_instance = Yes\n\
         \x20 instance_name = {instance_name}\n\
         \n\
         [logging]\n\
         \x20 loglevel = 4\n\
         \n\
         [interfaces]\n"
    );
    std::fs::write(tempdir.join("config"), config_content).expect("write config");

    tempdir
}

/// Run a Python utility and return its output.
pub(crate) async fn run_python_tool(script: &str, args: &[&str], config_dir: &Path) -> Output {
    let config_str = config_dir.to_str().expect("config dir must be valid UTF-8");

    let output = tokio::process::Command::new("python3")
        .arg(script)
        .arg("--config")
        .arg(config_str)
        .args(args)
        .env("PYTHONPATH", VENDOR_RNS_ROOT)
        .output()
        .await
        .expect("failed to spawn python3");

    output
}

/// Poll `rnstatus` against the daemon until it actually answers (RPC server up
/// and producing output), or panic after `deadline`.
///
/// Hardens against the rnstatus-empty-output flake: the daemon's
/// shared-instance socket starts accepting connections before the RPC handler
/// is guaranteed to produce a response, and under CI contention a fixed
/// post-start sleep can elapse while the daemon still answers an empty body
/// (the client connects, gets EOF, and exits 0 with empty stdout). Waiting on
/// the real readiness condition — a non-empty `rnstatus` response — makes every
/// RPC interop test deterministic regardless of load. The first successful
/// response only gates *readiness*; each test still asserts on its own fresh
/// query afterwards.
async fn wait_for_rpc_ready(config_dir: &Path, deadline: Duration) {
    let start = tokio::time::Instant::now();
    let mut attempts = 0u32;
    loop {
        let output = run_python_tool(RNSTATUS_PY, &[], config_dir).await;
        attempts += 1;
        if output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty() {
            return;
        }
        if start.elapsed() >= deadline {
            panic!(
                "daemon RPC not ready after {:?} ({} rnstatus attempts); \
                 last status={:?}, stdout_len={}, stderr={}",
                deadline,
                attempts,
                output.status.code(),
                output.stdout.len(),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Create the Python client config and wait until the daemon's RPC is actually
/// answering before returning. Use this in place of a bare
/// `create_python_config_dir` + fixed sleep so the subsequent tool invocation
/// runs against a provably-ready daemon.
pub(crate) async fn python_client_ready(instance_name: &str, identity_bytes: &[u8; 64]) -> PathBuf {
    let config_dir = create_python_config_dir(instance_name, identity_bytes);
    wait_for_rpc_ready(&config_dir, Duration::from_secs(20)).await;
    config_dir
}

use crate::common::cleanup_config_dir;

// rnstatus tests
/// Test that `rnstatus --config <tempdir>` succeeds against the Rust daemon.
///
/// This is the definitive interop test: Python parses our pickle output,
/// verifies our HMAC handshake, and formats our interface_stats dict.
#[tokio::test]
async fn test_rnstatus_against_rust_daemon() {
    init_tracing();

    let (_node, instance_name, _tcp_addr, identity_bytes, _storage) =
        start_rust_daemon_with_rpc().await;
    let config_dir = python_client_ready(&instance_name, &identity_bytes).await;

    let output = run_python_tool(RNSTATUS_PY, &[], &config_dir).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        eprintln!("=== rnstatus STDOUT ===\n{}", stdout);
        eprintln!("=== rnstatus STDERR ===\n{}", stderr);
        panic!("rnstatus exited with code {:?}", output.status.code());
    }

    // rnstatus should show transport instance info.
    // The daemon has transport enabled, so the output includes the transport hash
    // and uptime. Interface status lines appear only if connected peers exist.
    assert!(
        !stdout.trim().is_empty(),
        "rnstatus should produce non-empty output"
    );
    assert!(
        stdout.contains("Transport Instance"),
        "rnstatus output should show transport instance, got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("Uptime"),
        "rnstatus output should show uptime, got:\n{}",
        stdout
    );

    cleanup_config_dir(&config_dir);
}

/// Test `rnstatus --json` returns valid JSON with expected keys.
#[tokio::test]
async fn test_rnstatus_json_against_rust_daemon() {
    init_tracing();

    let (_node, instance_name, _tcp_addr, identity_bytes, _storage) =
        start_rust_daemon_with_rpc().await;
    let config_dir = python_client_ready(&instance_name, &identity_bytes).await;

    let output = run_python_tool(RNSTATUS_PY, &["--json"], &config_dir).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        eprintln!("=== rnstatus --json STDOUT ===\n{}", stdout);
        eprintln!("=== rnstatus --json STDERR ===\n{}", stderr);
        panic!(
            "rnstatus --json exited with code {:?}",
            output.status.code()
        );
    }

    // Parse as JSON and verify structure
    let json: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("rnstatus --json should return valid JSON");

    assert!(
        json.get("interfaces").is_some(),
        "JSON should have 'interfaces' key"
    );
    assert!(
        json.get("transport_id").is_some(),
        "JSON should have 'transport_id' key (transport enabled)"
    );
    assert!(
        json.get("transport_uptime").is_some(),
        "JSON should have 'transport_uptime' key"
    );

    // transport_uptime should be a positive number
    let uptime = json["transport_uptime"]
        .as_f64()
        .expect("uptime should be float");
    assert!(uptime >= 0.0, "uptime should be non-negative");

    cleanup_config_dir(&config_dir);
}

// rnpath tests
/// Test `rnpath -t` (show path table) against the Rust daemon.
/// Should return empty but not crash.
#[tokio::test]
async fn test_rnpath_table_against_rust_daemon() {
    init_tracing();

    let (_node, instance_name, _tcp_addr, identity_bytes, _storage) =
        start_rust_daemon_with_rpc().await;
    let config_dir = python_client_ready(&instance_name, &identity_bytes).await;

    let output = run_python_tool(RNPATH_PY, &["-t"], &config_dir).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        eprintln!("=== rnpath -t STDOUT ===\n{}", stdout);
        eprintln!("=== rnpath -t STDERR ===\n{}", stderr);
        panic!("rnpath -t exited with code {:?}", output.status.code());
    }

    // Empty path table, rnpath should still exit successfully.
    // It may print a header or "No paths" message, or nothing at all.

    cleanup_config_dir(&config_dir);
}

/// Test `rnpath -r` (show rate table) against the Rust daemon.
#[tokio::test]
async fn test_rnpath_rate_table_against_rust_daemon() {
    init_tracing();

    let (_node, instance_name, _tcp_addr, identity_bytes, _storage) =
        start_rust_daemon_with_rpc().await;
    let config_dir = python_client_ready(&instance_name, &identity_bytes).await;

    let output = run_python_tool(RNPATH_PY, &["-r"], &config_dir).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        eprintln!("=== rnpath -r STDOUT ===\n{}", stdout);
        eprintln!("=== rnpath -r STDERR ===\n{}", stderr);
        panic!("rnpath -r exited with code {:?}", output.status.code());
    }

    cleanup_config_dir(&config_dir);
}

/// Test `rnstatus -l` (link stats) against the Rust daemon.
/// Exercises the link_count RPC command.
#[tokio::test]
async fn test_rnstatus_link_stats_against_rust_daemon() {
    init_tracing();

    let (_node, instance_name, _tcp_addr, identity_bytes, _storage) =
        start_rust_daemon_with_rpc().await;
    let config_dir = python_client_ready(&instance_name, &identity_bytes).await;

    let output = run_python_tool(RNSTATUS_PY, &["-l"], &config_dir).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        eprintln!("=== rnstatus -l STDOUT ===\n{}", stdout);
        eprintln!("=== rnstatus -l STDERR ===\n{}", stderr);
        panic!("rnstatus -l exited with code {:?}", output.status.code());
    }

    cleanup_config_dir(&config_dir);
}
