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
    "/../reference/Reticulum/RNS/Utilities/rnstatus.py"
);
pub(crate) const RNPATH_PY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../reference/Reticulum/RNS/Utilities/rnpath.py"
);
const RNPROBE_PY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../reference/Reticulum/RNS/Utilities/rnprobe.py"
);

/// Path to vendor Reticulum package (for PYTHONPATH).
const VENDOR_RNS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../reference/Reticulum");

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
    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let instance_name = format!("rpcinterop_{}_{}", std::process::id(), test_id);

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
        .add_tcp_server(crate::ports::LOOPBACK_ANY_PORT)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("Failed to build Rust daemon node");

    node.start().await.expect("Failed to start Rust node");
    let tcp_addr = crate::ports::bound_tcp_addr(&node.tcp_listen_addrs());

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

    // Codeberg #318: every interface entry carries the tx_queue_drops
    // counter, and it reaches the Python side intact — the reference
    // reader looks fields up by name and never enumerates an entry, so
    // the additive key must ride through rnstatus --json unchanged
    // (this same run also proves rnstatus still parses our dict with
    // the key present).
    let interfaces = json["interfaces"]
        .as_array()
        .expect("interfaces should be a list");
    assert!(!interfaces.is_empty(), "daemon should report interfaces");
    for entry in interfaces {
        let drops = entry.get("tx_queue_drops").unwrap_or_else(|| {
            panic!("every interface entry must carry tx_queue_drops, missing on: {entry}")
        });
        assert!(
            drops.as_u64().is_some(),
            "tx_queue_drops must be a non-negative integer, got {drops} on: {entry}"
        );
    }

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

/// D4 of the shared-instance audit (`docs/src/audits/2026-10-11-shared-instance-rpc.md`):
/// with one announce-rate entry present, `rnpath -r` from the reference tree
/// prints the rate row. It reads `timestamps[0]`
/// (`reference/Reticulum/RNS/Utilities/rnpath.py:344`), so an empty list
/// made it print "Error while processing entry" for every row.
#[tokio::test]
async fn test_rnpath_rate_table_prints_a_row_for_a_rate_entry() {
    init_tracing();

    let (_node, instance_name, tcp_addr, identity_bytes, _storage) =
        start_rust_daemon_with_rpc().await;
    let config_dir = python_client_ready(&instance_name, &identity_bytes).await;

    // A neighbour over TCP: the daemon is transport-enabled, so its TCP
    // interface carries the default announce-rate target and the announce
    // opens a rate entry.
    let peer_storage = crate::common::temp_storage("rnpath_rate_row", "peer");
    let mut peer = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(tcp_addr)
        .storage_path(peer_storage.path().to_path_buf())
        .build()
        .await
        .expect("build peer");
    peer.start().await.expect("start peer");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let dest = leviculum_core::Destination::new(
        Some(Identity::generate(&mut rand_core::OsRng)),
        leviculum_core::Direction::In,
        leviculum_core::DestinationType::Single,
        "ratetable",
        &["row"],
    )
    .expect("create destination");
    let dest_hash = *dest.hash();
    peer.register_destination(dest);
    peer.announce_destination(&dest_hash, None)
        .await
        .expect("announce");

    let hash_hex = hex::encode(dest_hash.as_bytes());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let (stdout, stderr) = loop {
        let output = run_python_tool(RNPATH_PY, &["-r"], &config_dir).await;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "rnpath -r exited with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code()
        );
        if stdout.contains(&hash_hex) || tokio::time::Instant::now() >= deadline {
            break (stdout, stderr);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    eprintln!("=== rnpath -r STDOUT ===\n{stdout}");

    assert!(
        stdout.contains(&hash_hex),
        "rnpath -r never listed the announced destination {hash_hex}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("Error while processing entry"),
        "rnpath -r could not render the rate row:\n{stdout}"
    );
    assert!(
        stdout.contains("announces/hour"),
        "rnpath -r printed no rate line:\n{stdout}"
    );

    cleanup_config_dir(&config_dir);
}

/// D5 and D7 of the shared-instance audit: the reference `rnstatus -A` shows
/// what waits in an announce-cap queue on lnsd ("Queued : 1 announce",
/// `reference/Reticulum/RNS/Utilities/rnstatus.py:542-547`), and `rnpath -D`
/// empties it (`reference/Reticulum/RNS/Utilities/rnpath.py:386-387`), after
/// which `rnstatus -A` prints no queue line. Before, the key was always None,
/// so the line never appeared, and the drop was a constant `true`.
///
/// The queue fills deterministically: the daemon relays one announce onto a
/// TCP client whose configured bitrate (100 bps, 2 bps at the default 2 %
/// cap) holds the next announce back for several minutes. The first emission
/// leaves at once, the PATHFINDER retry a few seconds later has to wait, and
/// a destination holds one slot, so the count is 1 and stays 1.
#[tokio::test]
async fn test_rnstatus_shows_and_rnpath_drops_the_announce_queue() {
    init_tracing();

    let sink_storage = crate::common::temp_storage("announce_queue", "sink");
    let mut sink = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server(crate::ports::LOOPBACK_ANY_PORT)
        .storage_path(sink_storage.path().to_path_buf())
        .build()
        .await
        .expect("build sink");
    sink.start().await.expect("start sink");
    let sink_addr = crate::ports::bound_tcp_addr(&sink.tcp_listen_addrs());

    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let instance_name = format!("annqueue_{}_{}", std::process::id(), test_id);
    let identity = Identity::generate(&mut rand_core::OsRng);
    let identity_bytes = identity
        .private_key_bytes()
        .expect("generated identity must have private keys");
    let storage = crate::common::temp_storage("announce_queue", "node");
    let mut node = ReticulumNodeBuilder::new()
        .identity(identity)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.clone())
        .add_tcp_server(crate::ports::LOOPBACK_ANY_PORT)
        .add_tcp_client_with_bitrate(sink_addr, 100)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build daemon");
    node.start().await.expect("start daemon");
    let tcp_addr = crate::ports::bound_tcp_addr(&node.tcp_listen_addrs());
    let config_dir = python_client_ready(&instance_name, &identity_bytes).await;

    let peer_storage = crate::common::temp_storage("announce_queue", "peer");
    let mut peer = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(tcp_addr)
        .storage_path(peer_storage.path().to_path_buf())
        .build()
        .await
        .expect("build peer");
    peer.start().await.expect("start peer");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let dest = leviculum_core::Destination::new(
        Some(Identity::generate(&mut rand_core::OsRng)),
        leviculum_core::Direction::In,
        leviculum_core::DestinationType::Single,
        "announcequeue",
        &["held"],
    )
    .expect("create destination");
    let dest_hash = *dest.hash();
    peer.register_destination(dest);
    peer.announce_destination(&dest_hash, None)
        .await
        .expect("announce");

    let rnstatus_a = |dir: PathBuf| async move {
        let output = run_python_tool(RNSTATUS_PY, &["-A"], &dir).await;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "rnstatus -A exited with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code()
        );
        stdout
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let queued = loop {
        let stdout = rnstatus_a(config_dir.clone()).await;
        if stdout.contains("Queued") || tokio::time::Instant::now() >= deadline {
            break stdout;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    eprintln!("=== rnstatus -A (queue filled) STDOUT ===\n{queued}");
    let queue_lines: Vec<&str> = queued.lines().filter(|l| l.contains("Queued")).collect();
    assert_eq!(
        queue_lines.len(),
        1,
        "exactly the capped interface reports a queue:\n{queued}"
    );
    assert!(
        queue_lines[0].contains(": 1 announce") && !queue_lines[0].contains("announces"),
        "rnstatus -A must show the one queued announce:\n{queued}"
    );

    let output = run_python_tool(RNPATH_PY, &["-D"], &config_dir).await;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    eprintln!("=== rnpath -D STDOUT ===\n{stdout}\n=== STDERR ===\n{stderr}");
    assert!(
        output.status.success() && stdout.contains("Dropping announce queues on all interfaces"),
        "rnpath -D exited with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code()
    );

    let dropped = rnstatus_a(config_dir.clone()).await;
    eprintln!("=== rnstatus -A (after rnpath -D) STDOUT ===\n{dropped}");
    assert!(
        dropped.contains("Transport Instance"),
        "rnstatus -A must still answer after the drop:\n{dropped}"
    );
    assert!(
        !dropped.contains("Queued"),
        "rnpath -D must leave no queued announce:\n{dropped}"
    );

    cleanup_config_dir(&config_dir);
}

/// D8 of the shared-instance audit: the reference `rnpath -t` names a path's
/// interface as the same daemon's `rnstatus` names it, so a script joining
/// `rnpath -tj` to `rnstatus -j` by name finds the row. Python stringifies
/// the same interface object for both (`reference/Reticulum/RNS/Reticulum.py:1469`
/// and `:1534`). The path here runs over a connection the daemon's TCP server
/// accepted, which `interface_stats` reports as `TCPInterface[Client on ...]`
/// and the path answers used to report by the driver's `tcp_server/<peer>`.
#[tokio::test]
async fn test_rnpath_table_names_the_interface_rnstatus_reports() {
    init_tracing();

    let (_node, instance_name, tcp_addr, identity_bytes, _storage) =
        start_rust_daemon_with_rpc().await;
    let config_dir = python_client_ready(&instance_name, &identity_bytes).await;

    let peer_storage = crate::common::temp_storage("rnpath_iface_name", "peer");
    let mut peer = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(tcp_addr)
        .storage_path(peer_storage.path().to_path_buf())
        .build()
        .await
        .expect("build peer");
    peer.start().await.expect("start peer");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let dest = leviculum_core::Destination::new(
        Some(Identity::generate(&mut rand_core::OsRng)),
        leviculum_core::Direction::In,
        leviculum_core::DestinationType::Single,
        "pathiface",
        &["name"],
    )
    .expect("create destination");
    let dest_hash = *dest.hash();
    peer.register_destination(dest);
    peer.announce_destination(&dest_hash, None)
        .await
        .expect("announce");

    let run_ok = |script: &'static str, args: &'static [&'static str], dir: PathBuf| async move {
        let output = run_python_tool(script, args, &dir).await;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "{script} {args:?} exited with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code()
        );
        stdout
    };

    let hash_hex = hex::encode(dest_hash.as_bytes());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let path_rows = loop {
        let stdout = run_ok(RNPATH_PY, &["-t", "-j"], config_dir.clone()).await;
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(stdout.trim()).expect("rnpath -tj prints a JSON list");
        if rows.iter().any(|r| r["hash"] == hash_hex.as_str())
            || tokio::time::Instant::now() >= deadline
        {
            break rows;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    let row = path_rows
        .iter()
        .find(|r| r["hash"] == hash_hex.as_str())
        .unwrap_or_else(|| panic!("rnpath -tj never listed {hash_hex}: {path_rows:?}"));
    let path_iface = row["interface"]
        .as_str()
        .expect("a path row's interface is a string")
        .to_string();

    let status: serde_json::Value = serde_json::from_str(
        run_ok(RNSTATUS_PY, &["-j"], config_dir.clone())
            .await
            .trim(),
    )
    .expect("rnstatus -j prints JSON");
    let names: Vec<&str> = status["interfaces"]
        .as_array()
        .expect("rnstatus -j lists interfaces")
        .iter()
        .filter_map(|i| i["name"].as_str())
        .collect();
    eprintln!("=== rnpath -tj interface: {path_iface}\n=== rnstatus -j names: {names:?}");
    assert!(
        names.contains(&path_iface.as_str()),
        "joining rnpath -tj to rnstatus -j by name must match: the path names \
         {path_iface:?}, rnstatus names {names:?}"
    );
    assert!(
        path_iface.starts_with("TCPInterface[Client on "),
        "the accepted connection carries the reference display name, got {path_iface:?}"
    );

    let text = run_ok(RNPATH_PY, &["-t"], config_dir.clone()).await;
    eprintln!("=== rnpath -t STDOUT ===\n{text}");
    assert!(
        text.lines()
            .any(|l| l.contains(&format!(" on {path_iface} expires "))),
        "rnpath -t must print the rnstatus name for the path:\n{text}"
    );

    // The configured-client row (647's deviation 3): a second daemon reaches
    // the first through a `[[Upstream Link]]` TCPClientInterface section and
    // learns the same path over it. Both answers name that interface as the
    // reference's `TCPInterface.__str__` renders it,
    // `TCPInterface[<name>/<host>:<port>]`
    // (`reference/Reticulum/RNS/Interfaces/TCPInterface.py:443-449`), where
    // they used to name the driver's `tcp_client_0`.
    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let instance2 = format!("rpcinterop_client_{}_{}", std::process::id(), test_id);
    let identity2 = Identity::generate(&mut rand_core::OsRng);
    let identity2_bytes = identity2
        .private_key_bytes()
        .expect("generated identity must have private keys");
    let storage2 = crate::common::temp_storage("rnpath_iface_name", "client_daemon");
    let ini = storage2.path().join("daemon.ini");
    std::fs::write(
        &ini,
        format!(
            "[reticulum]\n  enable_transport = yes\n\n[interfaces]\n  \
             [[Upstream Link]]\n    type = TCPClientInterface\n    \
             enabled = yes\n    target_host = 127.0.0.1\n    \
             target_port = {}\n",
            tcp_addr.port()
        ),
    )
    .expect("write daemon config");
    let config2 = leviculum_std::config::Config::load(&ini).expect("load daemon config");
    let mut daemon2 = ReticulumNodeBuilder::new()
        .identity(identity2)
        .config(config2)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance2.clone())
        .storage_path(storage2.path().to_path_buf())
        .build()
        .await
        .expect("build client daemon");
    daemon2.start().await.expect("start client daemon");
    let config_dir2 = python_client_ready(&instance2, &identity2_bytes).await;
    // A fresh announce, now that the second daemon is attached.
    peer.announce_destination(&dest_hash, None)
        .await
        .expect("announce");

    let expected = format!("TCPInterface[Upstream Link/127.0.0.1:{}]", tcp_addr.port());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let rows2 = loop {
        let stdout = run_ok(RNPATH_PY, &["-t", "-j"], config_dir2.clone()).await;
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(stdout.trim()).expect("rnpath -tj prints a JSON list");
        if rows.iter().any(|r| r["hash"] == hash_hex.as_str())
            || tokio::time::Instant::now() >= deadline
        {
            break rows;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    let row2 = rows2
        .iter()
        .find(|r| r["hash"] == hash_hex.as_str())
        .unwrap_or_else(|| panic!("the client daemon never listed {hash_hex}: {rows2:?}"));
    let client_iface = row2["interface"].as_str().unwrap_or_default().to_string();
    let status2: serde_json::Value = serde_json::from_str(
        run_ok(RNSTATUS_PY, &["-j"], config_dir2.clone())
            .await
            .trim(),
    )
    .expect("rnstatus -j prints JSON");
    let names2: Vec<String> = status2["interfaces"]
        .as_array()
        .expect("rnstatus -j lists interfaces")
        .iter()
        .filter_map(|i| i["name"].as_str().map(str::to_owned))
        .collect();
    let text2 = run_ok(RNPATH_PY, &["-t"], config_dir2.clone()).await;
    eprintln!(
        "=== configured client: rnpath -tj interface: {client_iface}\n\
         === rnstatus -j names: {names2:?}\n=== rnpath -t STDOUT ===\n{text2}"
    );
    assert_eq!(
        client_iface, expected,
        "the configured client carries the reference display name"
    );
    assert!(
        names2.contains(&expected),
        "rnstatus -j names the configured client the same way: {names2:?}"
    );
    assert!(
        text2
            .lines()
            .any(|l| l.contains(&format!(" on {expected} expires "))),
        "rnpath -t prints the reference name for the configured client:\n{text2}"
    );

    cleanup_config_dir(&config_dir);
    cleanup_config_dir(&config_dir2);
}

/// D9 of the shared-instance audit: with no path at the daemon,
/// `next_hop_if_name` answers the string "None", and the reference
/// `rnprobe -v` prints no " on <interface>" suffix
/// (`reference/Reticulum/RNS/Utilities/rnprobe.py:127`). The probed
/// destination is the daemon's own probe responder: the client learns a path
/// to it, the daemon holds none to itself, which is the no-path answer an
/// rnsd gives in the same place. lnsd answered "unknown", printed as
/// "on unknown".
#[tokio::test]
async fn test_rnprobe_verbose_prints_no_interface_without_a_path() {
    init_tracing();

    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let instance_name = format!("probenone_{}_{}", std::process::id(), test_id);
    let identity = Identity::generate(&mut rand_core::OsRng);
    let identity_bytes = identity
        .private_key_bytes()
        .expect("generated identity must have private keys");
    let probe_hash = *leviculum_core::Destination::new(
        Some(identity.clone()),
        leviculum_core::Direction::In,
        leviculum_core::DestinationType::Single,
        "rnstransport",
        &["probe"],
    )
    .expect("probe destination")
    .hash();
    let mut config = leviculum_std::config::Config::default();
    config.reticulum.respond_to_probes = true;
    let storage = crate::common::temp_storage("rnprobe_no_path", "node");
    let mut node = ReticulumNodeBuilder::new()
        .identity(identity)
        .config(config)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.clone())
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build daemon");
    node.start().await.expect("start daemon");
    let config_dir = python_client_ready(&instance_name, &identity_bytes).await;

    let probe_hex = hex::encode(probe_hash.as_bytes());
    let output = run_python_tool(
        RNPROBE_PY,
        &[
            "-v",
            "-n",
            "1",
            "-t",
            "15",
            "rnstransport.probe",
            &probe_hex,
        ],
        &config_dir,
    )
    .await;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    eprintln!("=== rnprobe -v STDOUT ===\n{stdout}\n=== STDERR ===\n{stderr}");
    let sent: Vec<&str> = stdout
        .split(['\r', '\n'])
        .filter(|l| l.contains("Sent probe"))
        .collect();
    assert!(
        !sent.is_empty(),
        "rnprobe -v never sent its probe:\n{stdout}\n{stderr}"
    );
    for line in sent {
        assert!(
            !line.contains(" on "),
            "with no path at the daemon rnprobe -v must print no interface: {line:?}"
        );
    }

    cleanup_config_dir(&config_dir);
}

/// D16 of the shared-instance audit: a config that raises Python's
/// `default_ar_target` (`reference/Reticulum/RNS/Reticulum.py:622-625`,
/// applied at `:830-833`) changes the announce-rate target every interface
/// without its own reports, and the reference `rnstatus -A` renders it in the
/// `(t:...)` suffix (`reference/Reticulum/RNS/Utilities/rnstatus.py:556-563`).
/// The key used to be tolerated and inert, so lnsd showed `(t:1h/...)` for a
/// config that gave rnsd `(t:2h/...)`. The daemon is built from the same file
/// the client reads; the rows checked are the TCP listener and the connection
/// it accepted, one reported from the driver's inventory and one from the core.
/// The client's own `LocalInterface` row renders none, as the reference sets
/// None on every LocalInterface (`reference/Reticulum/RNS/Interfaces/LocalInterface.py:428-430`);
/// it used to render the fill too.
#[tokio::test]
async fn test_rnstatus_shows_the_configured_default_ar_target() {
    init_tracing();

    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let instance_name = format!("defaultar_{}_{}", std::process::id(), test_id);
    let identity = Identity::generate(&mut rand_core::OsRng);
    let identity_bytes = identity
        .private_key_bytes()
        .expect("generated identity must have private keys");
    let config_dir = create_python_config_dir(&instance_name, &identity_bytes);
    let config_file = config_dir.join("config");
    let content = std::fs::read_to_string(&config_file).expect("read config");
    std::fs::write(
        &config_file,
        content.replacen(
            "[reticulum]\n",
            "[reticulum]\n  default_ar_target = 7200\n",
            1,
        ),
    )
    .expect("write config");
    let daemon_config = leviculum_std::config::Config::load(&config_file).expect("load the config");
    let storage = crate::common::temp_storage("default_ar_target", "node");
    let mut node = ReticulumNodeBuilder::new()
        .identity(identity)
        .config(daemon_config)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.clone())
        .add_tcp_server(crate::ports::LOOPBACK_ANY_PORT)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build daemon");
    node.start().await.expect("start daemon");
    let tcp_addr = crate::ports::bound_tcp_addr(&node.tcp_listen_addrs());
    wait_for_rpc_ready(&config_dir, Duration::from_secs(20)).await;

    let peer_storage = crate::common::temp_storage("default_ar_target", "peer");
    let mut peer = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(tcp_addr)
        .storage_path(peer_storage.path().to_path_buf())
        .build()
        .await
        .expect("build peer");
    peer.start().await.expect("start peer");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let stdout = loop {
        // `-a`: rnstatus hides spawned interfaces without it.
        let output = run_python_tool(RNSTATUS_PY, &["-a", "-A"], &config_dir).await;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "rnstatus -a -A exited with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code()
        );
        if stdout.contains("TCPInterface[Client on ") || tokio::time::Instant::now() >= deadline {
            break stdout;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    eprintln!("=== rnstatus -a -A STDOUT ===\n{stdout}");

    assert!(
        stdout.contains("TCPInterface[Client on "),
        "the peer's connection never appeared:\n{stdout}"
    );
    let suffixes: Vec<&str> = stdout.lines().filter(|l| l.contains("(t:")).collect();
    assert_eq!(
        suffixes.len(),
        2,
        "the listener and its accepted connection each render a target, the \
         LocalInterface row none:\n{stdout}"
    );
    for line in suffixes {
        assert!(
            line.contains("(t:2h/p:0s/g:5)"),
            "default_ar_target = 7200 renders as (t:2h/...), with the reference \
             penalty and grace beside it: {line:?}\n{stdout}"
        );
    }

    cleanup_config_dir(&config_dir);
}

/// D1 of the shared-instance audit: a config that sets `rpc_key` keys both
/// ends of the RPC (`reference/Reticulum/RNS/Reticulum.py:509-515`, applied at
/// `:355-359` and `:1265`). `rnstatus` reading that config must reach lnsd
/// built from it; before, lnsd ignored the key and refused the handshake.
/// The negative control: a client without the key no longer gets in.
#[tokio::test]
async fn test_rnstatus_with_rpc_key_against_rust_daemon() {
    init_tracing();

    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let instance_name = format!("rpckey_{}_{}", std::process::id(), test_id);
    let rpc_key_hex = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";
    let identity = Identity::generate(&mut rand_core::OsRng);
    let identity_bytes = identity
        .private_key_bytes()
        .expect("generated identity must have private keys");

    // One config file, the one the Python client reads, is also the one
    // the daemon is built from: the drop-in contract in its plainest form.
    let keyed_dir = create_python_config_dir(&instance_name, &identity_bytes);
    let config_file = keyed_dir.join("config");
    let content = std::fs::read_to_string(&config_file).expect("read config");
    std::fs::write(
        &config_file,
        content.replacen(
            "[reticulum]\n",
            &format!("[reticulum]\n  rpc_key = {rpc_key_hex}\n"),
            1,
        ),
    )
    .expect("write keyed config");
    let daemon_config =
        leviculum_std::config::Config::load(&config_file).expect("load the keyed config");
    let storage = crate::common::temp_storage("rnstatus_rpc_key", "node");
    let mut node = ReticulumNodeBuilder::new()
        .identity(identity)
        .config(daemon_config)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.clone())
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build daemon");
    node.start().await.expect("start daemon");

    // Waited on the real condition: rnstatus exits 0 with "Could not get RNS
    // status" both while the RPC is still coming up and when the key is
    // refused, so only the rendered status tells the two apart.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let output = loop {
        let output = run_python_tool(RNSTATUS_PY, &[], &keyed_dir).await;
        if String::from_utf8_lossy(&output.stdout).contains("Transport Instance")
            || tokio::time::Instant::now() >= deadline
        {
            break output;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("=== rnstatus (rpc_key) STDOUT ===\n{stdout}\n=== STDERR ===\n{stderr}");
    assert!(
        output.status.success() && stdout.contains("Transport Instance"),
        "rnstatus with the configured rpc_key must reach lnsd:\n{stdout}\n{stderr}"
    );

    // Same daemon, a client config without the key: it derives the key
    // from the transport identity, which no longer opens the listener.
    let unkeyed_dir = std::env::temp_dir().join(format!("rpc_interop_unkeyed_{instance_name}"));
    let _ = std::fs::remove_dir_all(&unkeyed_dir);
    std::fs::create_dir_all(&unkeyed_dir).expect("create unkeyed dir");
    std::fs::create_dir_all(unkeyed_dir.join("storage")).expect("storage dir");
    std::fs::write(
        unkeyed_dir.join("storage").join("transport_identity"),
        identity_bytes,
    )
    .expect("write identity");
    std::fs::write(unkeyed_dir.join("config"), content).expect("write unkeyed config");
    let output = run_python_tool(RNSTATUS_PY, &[], &unkeyed_dir).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!("=== rnstatus (no rpc_key) STDOUT ===\n{stdout}");
    assert!(
        !stdout.contains("Transport Instance"),
        "a client without the configured rpc_key must not get in:\n{stdout}"
    );

    cleanup_config_dir(&keyed_dir);
    cleanup_config_dir(&unkeyed_dir);
}

/// D2 of the shared-instance audit: `shared_instance_type = tcp` moves BOTH
/// shared-instance channels to TCP loopback, on Linux too
/// (`reference/Reticulum/RNS/Reticulum.py:683-694`), so every Python tool
/// reading that config dials 127.0.0.1 at the configured ports. lnsd built
/// from the same file has to listen there: the reference `rnstatus` gets the
/// status, and `ss` shows this process holding both loopback ports. The
/// negative control: the abstract sockets of the instance are not served.
/// Red before: lnsd served the abstract pair regardless and `rnstatus`
/// printed "Could not get RNS status".
#[tokio::test]
async fn test_rnstatus_over_tcp_shared_instance_against_rust_daemon() {
    init_tracing();

    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let instance_name = format!("sitcp_{}_{}", std::process::id(), test_id);
    // port-handoff: the one config file names both ports before either side
    // runs, lnsd binds them and the Python client dials them from it.
    let data_port = crate::harness::pick_free_tcp_port().expect("free data port");
    // port-handoff: as above, the RPC channel's port.
    let control_port = crate::harness::pick_free_tcp_port().expect("free control port");
    let identity = Identity::generate(&mut rand_core::OsRng);
    let identity_bytes = identity
        .private_key_bytes()
        .expect("generated identity must have private keys");

    let config_dir = create_python_config_dir(&instance_name, &identity_bytes);
    let config_file = config_dir.join("config");
    let content = std::fs::read_to_string(&config_file).expect("read config");
    std::fs::write(
        &config_file,
        content.replacen(
            "[reticulum]\n",
            &format!(
                "[reticulum]\n  shared_instance_type = tcp\n  \
                 shared_instance_port = {data_port}\n  \
                 instance_control_port = {control_port}\n"
            ),
            1,
        ),
    )
    .expect("write tcp config");
    let daemon_config =
        leviculum_std::config::Config::load(&config_file).expect("load the tcp config");
    let storage = crate::common::temp_storage("rnstatus_tcp_instance", "node");
    let mut node = ReticulumNodeBuilder::new()
        .identity(identity)
        .config(daemon_config)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.clone())
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build daemon");
    node.start().await.expect("start daemon");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let output = loop {
        let output = run_python_tool(RNSTATUS_PY, &[], &config_dir).await;
        if String::from_utf8_lossy(&output.stdout).contains("Transport Instance")
            || tokio::time::Instant::now() >= deadline
        {
            break output;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!(
        "=== rnstatus (shared_instance_type = tcp) STDOUT ===\n{stdout}\n=== STDERR ===\n{stderr}"
    );
    assert!(
        output.status.success() && stdout.contains("Transport Instance"),
        "rnstatus over the tcp shared instance must reach lnsd:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains(&format!("Shared Instance[{data_port}]")),
        "the shared-instance row carries the reference's TCP name:\n{stdout}"
    );

    // Both ports are held by this process, which is the daemon.
    let ss = std::process::Command::new("ss")
        .args(["-ltnp"])
        .output()
        .expect("ss runs (apt install iproute2)");
    let listeners = String::from_utf8_lossy(&ss.stdout).into_owned();
    let pid = format!("pid={},", std::process::id());
    for port in [data_port, control_port] {
        let line = listeners
            .lines()
            .find(|l| l.contains(&format!("127.0.0.1:{port} ")))
            .unwrap_or_else(|| panic!("nothing listens on 127.0.0.1:{port}:\n{listeners}"));
        eprintln!("=== ss -ltnp: {line}");
        assert!(
            line.contains(&pid),
            "127.0.0.1:{port} must be held by lnsd ({pid}): {line}"
        );
    }

    // Negative control: the abstract pair is not served in this mode.
    let ss = std::process::Command::new("ss")
        .args(["-xl"])
        .output()
        .expect("ss runs (apt install iproute2)");
    let unix_listeners = String::from_utf8_lossy(&ss.stdout);
    assert!(
        !unix_listeners.contains(&format!("@rns/{instance_name}")),
        "no abstract socket of the instance may be served under tcp:\n{unix_listeners}"
    );

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

// --- The drop-in rule against a post-1.3.5 rnstatus (Codeberg #329) -------

/// Environment variable naming a Reticulum source checkout to run the client
/// tools from, instead of the vendored `reference/Reticulum`.
const RNS_ROOT_ENV: &str = "LEVICULUM_RNS_ROOT";

/// [`wait_for_rpc_ready`] with the rnstatus of an arbitrary Reticulum tree.
///
/// The A/B below uses this on BOTH sides rather than the vendored rnstatus:
/// the readiness probe is a client of the daemon like any other, so running a
/// different one per side would put a second variable in a comparison that is
/// supposed to have one.
async fn wait_for_rpc_ready_from(rns_root: &Path, config_dir: &Path, deadline: Duration) {
    let start = tokio::time::Instant::now();
    let mut attempts = 0u32;
    loop {
        let output =
            run_python_tool_from(rns_root, "RNS/Utilities/rnstatus.py", &[], config_dir).await;
        attempts += 1;
        if output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty() {
            return;
        }
        if start.elapsed() >= deadline {
            panic!(
                "daemon RPC not ready after {deadline:?} ({attempts} rnstatus attempts); \
                 last status={:?}, stdout={}, stderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// [`python_client_ready`] against an arbitrary Reticulum tree.
async fn python_client_ready_from(
    rns_root: &Path,
    instance_name: &str,
    identity_bytes: &[u8; 64],
) -> PathBuf {
    let config_dir = create_python_config_dir(instance_name, identity_bytes);
    wait_for_rpc_ready_from(rns_root, &config_dir, Duration::from_secs(30)).await;
    config_dir
}

/// Resolve the Reticulum checkout named by [`RNS_ROOT_ENV`] and the version it
/// reports.
///
/// Panics rather than skips when the variable is unset, and refuses the
/// vendored 1.3.5: every test that calls this exists to measure a release
/// NEWER than the vendored one, and a green run on 1.3.5 would measure
/// nothing (1.3.5 has neither `txdrp` nor any of the three timing verbs).
fn rns_root_and_version() -> (PathBuf, String) {
    let rns_root = PathBuf::from(std::env::var(RNS_ROOT_ENV).unwrap_or_else(|_| {
        panic!(
            "{RNS_ROOT_ENV} must name a post-1.3.5 Reticulum source checkout; \
             without it this test would measure nothing"
        )
    }));

    let version = std::process::Command::new("python3")
        .arg("-c")
        .arg("import RNS; print(RNS.__version__)")
        .env("PYTHONPATH", &rns_root)
        .output()
        .expect("failed to spawn python3");
    let version = String::from_utf8_lossy(&version.stdout).trim().to_string();
    assert!(
        !version.is_empty() && version != "1.3.5",
        "{RNS_ROOT_ENV}={} reports RNS {version:?}; the point of this test is a \
         release NEWER than the vendored 1.3.5",
        rns_root.display()
    );
    (rns_root, version)
}

/// Run a Python utility out of an arbitrary Reticulum source tree.
async fn run_python_tool_from(
    rns_root: &Path,
    script_rel: &str,
    args: &[&str],
    config_dir: &Path,
) -> Output {
    let config_str = config_dir.to_str().expect("config dir must be valid UTF-8");

    tokio::process::Command::new("python3")
        .arg(rns_root.join(script_rel))
        .arg("--config")
        .arg(config_str)
        .args(args)
        .env("PYTHONPATH", rns_root)
        .output()
        .await
        .expect("failed to spawn python3")
}

/// The drop-in rule measured against the rnstatus operators actually install.
///
/// `reference/Reticulum` is pinned at 1.3.5, and 1.3.5's rnstatus reads a
/// strictly smaller `interface_stats` key set than current releases — it has
/// no `txdrp` at all. Codeberg #329 was exactly that blind spot: the
/// periculum-test image resolves `rns` from PyPI (its vendored LXMF requires
/// `rns>=1.4.0`, so pip installs the current release over the vendored
/// 1.3.5), and the rnstatus that lands indexes `ifstat["txdrp"]` with no
/// presence guard (1.5.2 rnstatus.py:495), so every invocation against lnsd
/// died with `KeyError`. Two regression cells went red on nothing but an
/// image rebuild.
///
/// One run of this test walks every rnstatus path that indexes an ifstat or
/// top-level key without a guard, which is the whole surface a new upstream
/// key can crash us on:
///
/// * default text path — `txdrp`/`txdrb`, `mtu` (reached because our
///   `bitrate` is never None), `txbuffered`/`txstalled`
/// * `--sort txdrp` / `--sort txbuf` — the sort keys
/// * `-A` / `-P` — `arxc`/`atxc`, `prxc`/`ptxc` and the `arxs`/`atxs`,
///   `prxs`/`ptxs` flow ratios
/// * `--queues` / `--pps` — the top-level `rxq*` and `rxpps`/`txpps` set
/// * `--json` — the one path that enumerates keys (to hex-encode bytes)
///
/// `#[ignore]`d because the tree it needs is not vendored: pointing the suite
/// at a second Reticulum release is a deliberate act, not a default. Run it
/// against the release you want lnsd to be drop-in with:
///
/// ```text
/// LEVICULUM_RNS_ROOT=/path/to/Reticulum-1.5.x \
///   cargo test -p leviculum-std --test rnsd_interop \
///   test_current_rns_rnstatus_against_rust_daemon -- --ignored --nocapture
/// ```
///
/// It panics rather than skips when the variable is unset, and refuses a tree
/// that is the vendored 1.3.5 — a green run must mean it measured something.
#[tokio::test]
#[ignore = "needs LEVICULUM_RNS_ROOT pointing at a post-1.3.5 Reticulum checkout"]
async fn test_current_rns_rnstatus_against_rust_daemon() {
    init_tracing();

    let (rns_root, version) = rns_root_and_version();
    eprintln!("measuring drop-in compatibility against RNS {version}");

    let (_node, instance_name, _tcp_addr, identity_bytes, _storage) =
        start_rust_daemon_with_rpc().await;
    let config_dir = python_client_ready(&instance_name, &identity_bytes).await;

    // Every unguarded-access path in one sweep. `--json` last so a failure in
    // a text path names the flag that broke rather than a parse error.
    let flag_sets: [&[&str]; 8] = [
        &[],
        &["--sort", "txdrp"],
        &["--sort", "txbuf"],
        &["-A"],
        &["-P"],
        &["-A", "-P", "-l"],
        &["--queues", "--pps"],
        &["--json"],
    ];

    for args in flag_sets {
        let output =
            run_python_tool_from(&rns_root, "RNS/Utilities/rnstatus.py", args, &config_dir).await;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "rnstatus {args:?} (RNS {version}) exited {:?}\n\
             === STDOUT ===\n{stdout}\n=== STDERR ===\n{stderr}",
            output.status.code(),
        );
        // rnstatus catches nothing around its render loop, so a KeyError is a
        // traceback on stderr and a non-zero exit — but a future release could
        // wrap it. Assert on the symptom name too.
        assert!(
            !stderr.contains("KeyError") && !stderr.contains("Traceback"),
            "rnstatus {args:?} (RNS {version}) faulted on our reply:\n{stderr}"
        );
        assert!(
            !stdout.trim().is_empty(),
            "rnstatus {args:?} (RNS {version}) produced no output"
        );
    }

    // The acceptance the batch is written against: the interface block
    // renders, not just the transport banner.
    let output =
        run_python_tool_from(&rns_root, "RNS/Utilities/rnstatus.py", &[], &config_dir).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Transport Instance"),
        "rnstatus (RNS {version}) must show the transport banner, got:\n{stdout}"
    );
    assert!(
        stdout.contains("TCPServerInterface[") && stdout.contains("Status"),
        "rnstatus (RNS {version}) must render the interface block, got:\n{stdout}"
    );

    cleanup_config_dir(&config_dir);
}

// --- The path-timing verbs (Codeberg #329 remainder) ----------------------
//
// `lowest_interface_bitrate`, `medium_path_timeout` and `active_link_count`
// postdate the vendored 1.3.5 reference, so nothing in `reference/Reticulum`
// asks for them and nothing there serves them. Both tests below therefore run
// out of `LEVICULUM_RNS_ROOT` and are `#[ignore]`d for the same reason the
// rnstatus drop-in test above is: pointing the suite at a second Reticulum
// release is a deliberate act.
//
// Why this matters beyond a missing key: `Reticulum.get_medium_path_timeout`
// (Reticulum 1.5.2) swallows the RPC failure and returns **0**, and
// `rnprobe`/`rnpath`/`rncp` feed that straight into
// `max(timeout, reticulum.get_medium_path_timeout())`. Against an lnsd
// without the verb the wait window silently collapses to the client default;
// against rnsd it is seconds. That is a config difference smuggled into every
// path-timing A/B, which is why the parity assertion below is the point of
// the batch and not a nicety.

/// A driver that asks a daemon the three verbs through the literal client
/// API — `RNS.Reticulum.get_*`, the exact methods 1.5.2's `rnprobe`,
/// `rnpath` and `rncp` call — and prints the
/// answers on marked lines.
///
/// The clients themselves never print the window they computed, so driving
/// only them would leave "resolved to the formula value, not 0" unassertable.
/// They are still run alongside this, for the crash/log surface.
/// Each answer is reported, never swallowed: `get_link_count` and
/// `get_active_link_count` carry no `try` of their own (Reticulum 1.5.2),
/// so against a daemon that does not know the verb they raise `EOFError`
/// rather than returning a fallback. Printing `ERR:<exc>` instead of dying
/// keeps the remaining lines readable — the Rust side then fails on the
/// unparseable value, so the failure is still loud, and the positive control
/// can assert on the raise arm directly.
const TIMING_DRIVER_PY: &str = r#"
import os, sys, RNS

def ask(label, fn):
    try:    value = repr(fn())
    except Exception as e: value = "ERR:%s" % (type(e).__name__,)
    print("DRIVER %s=%s" % (label, value))

r = RNS.Reticulum(configdir=sys.argv[1])
print("DRIVER shared=%r" % (r.is_connected_to_shared_instance,))
ask("bitrate", r.get_lowest_interface_bitrate)
ask("timeout", r.get_medium_path_timeout)
ask("link_count", r.get_link_count)
ask("active_link_count", r.get_active_link_count)
sys.stdout.flush()
# RNS leaves non-daemon threads running; a clean interpreter exit can hang.
os._exit(0)
"#;

/// What one driver run read off one daemon.
#[derive(Debug, PartialEq)]
struct TimingAnswers {
    bitrate: Option<i64>,
    timeout: f64,
    link_count: i64,
    active_link_count: i64,
}

/// Write the driver script once per test into `dir`.
fn write_timing_driver(dir: &Path) -> PathBuf {
    let script = dir.join("timing_driver.py");
    std::fs::write(&script, TIMING_DRIVER_PY).expect("write timing driver");
    script
}

/// Run the driver from `rns_root` against the daemon `config_dir` names, and
/// parse its marked lines.
///
/// Asserts on the way through that the driver actually spoke to the shared
/// instance (a driver that fell back to its own stack would answer from its
/// own Transport and prove nothing about the daemon) and that no verb took
/// the swallow-and-return-the-fallback arm, whose log line is
/// "An error occurred while getting ..." (Reticulum 1.5.2).
async fn run_timing_driver(rns_root: &Path, script: &Path, config_dir: &Path) -> TimingAnswers {
    let output = tokio::process::Command::new("python3")
        .arg(script)
        .arg(config_dir)
        .env("PYTHONPATH", rns_root)
        .output()
        .await
        .expect("failed to spawn python3");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "timing driver exited {:?}\n=== STDOUT ===\n{stdout}\n=== STDERR ===\n{stderr}",
        output.status.code()
    );
    assert!(
        !stdout.contains("An error occurred while getting")
            && !stderr.contains("An error occurred while getting"),
        "a verb fell back instead of resolving; the daemon does not know it:\n{stdout}\n{stderr}"
    );

    let field = |key: &str| -> String {
        let needle = format!("DRIVER {key}=");
        stdout
            .lines()
            .find_map(|l| l.trim().strip_prefix(&needle).map(|v| v.trim().to_string()))
            .unwrap_or_else(|| panic!("driver printed no {key} line:\n{stdout}\n{stderr}"))
    };

    assert_eq!(
        field("shared"),
        "True",
        "the driver must answer from the DAEMON, not from a stack of its own:\n{stdout}"
    );

    let bitrate = match field("bitrate").as_str() {
        "None" => None,
        n => Some(
            n.parse::<i64>()
                .unwrap_or_else(|e| panic!("bitrate {n:?} is not an int: {e}")),
        ),
    };
    let parse_int = |key: &str| -> i64 {
        let raw = field(key);
        raw.parse()
            .unwrap_or_else(|e| panic!("{key} {raw:?} is not an int: {e}"))
    };
    let raw_timeout = field("timeout");
    TimingAnswers {
        bitrate,
        timeout: raw_timeout
            .parse()
            .unwrap_or_else(|e| panic!("timeout {raw_timeout:?} is not a float: {e}")),
        link_count: parse_int("link_count"),
        active_link_count: parse_int("active_link_count"),
    }
}

/// Python `Transport.medium_path_timeout` (Reticulum 1.5.2) with its
/// constants, recomputed here so the assertion is against the formula
/// rather than against a number pasted from one run.
fn expected_medium_path_timeout(lowest_bitrate: Option<i64>) -> f64 {
    const PY_MTU: f64 = 500.0;
    const PY_MINIMUM_BITRATE: f64 = 5.0;
    const PY_DEFAULT_PER_HOP_TIMEOUT: f64 = 6.0;
    match lowest_bitrate {
        Some(b) if b > 0 => {
            2.0 * (PY_MTU * 8.0 / (b as f64).max(PY_MINIMUM_BITRATE)) + PY_DEFAULT_PER_HOP_TIMEOUT
        }
        _ => 0.0,
    }
}

/// The three verbs resolve against lnsd for a real 1.5.2 client, and the
/// clients that consume them run clean.
///
/// ```text
/// LEVICULUM_RNS_ROOT=/path/to/Reticulum-1.5.x \
///   cargo test -p leviculum-std --test rnsd_interop \
///   test_current_rns_timing_verbs_against_rust_daemon -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "needs LEVICULUM_RNS_ROOT pointing at a post-1.3.5 Reticulum checkout"]
async fn test_current_rns_timing_verbs_against_rust_daemon() {
    init_tracing();

    let (rns_root, version) = rns_root_and_version();
    eprintln!("measuring the timing verbs against RNS {version}");

    let (_node, instance_name, _tcp_addr, identity_bytes, _storage) =
        start_rust_daemon_with_rpc().await;
    let config_dir = python_client_ready(&instance_name, &identity_bytes).await;
    let script = write_timing_driver(&config_dir);

    let answers = run_timing_driver(&rns_root, &script, &config_dir).await;
    eprintln!("lnsd (RNS {version} client) answered {answers:?}");

    // The daemon runs one TCPServerInterface (10 Mbps guess) beside the
    // 1 Gbps shared-instance server, so the slowest online medium is the TCP
    // listener — the same number rnsd's `min` lands on for the same config.
    assert_eq!(
        answers.bitrate,
        Some(10_000_000),
        "the slowest online interface of a single-TCP daemon is the 10 Mbps \
         TCPServerInterface BITRATE_GUESS"
    );

    let expected = expected_medium_path_timeout(answers.bitrate);
    assert!(
        (answers.timeout - expected).abs() < 1e-9,
        "medium_path_timeout resolved to {} s, formula says {expected} s",
        answers.timeout
    );
    // The regression this batch closes: the fallback arm returns exactly 0,
    // and the formula value is never 0 for a daemon with an online interface.
    assert!(
        answers.timeout > 0.0,
        "medium_path_timeout is 0 — the client took the RPC-failure arm"
    );
    assert_eq!(
        answers.active_link_count, 0,
        "an idle daemon terminates no active links"
    );

    // The clients themselves, on the paths that call the verbs. They print no
    // window, so this is the crash-and-log surface: `rnpath -t` and
    // `rnstatus -l` must exit clean and log no RPC-failure line.
    for (script_rel, args) in [
        ("RNS/Utilities/rnpath.py", &["-t"][..]),
        ("RNS/Utilities/rnstatus.py", &["-l"][..]),
    ] {
        let output = run_python_tool_from(&rns_root, script_rel, args, &config_dir).await;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{script_rel} {args:?} (RNS {version}) exited {:?}\n{stdout}\n{stderr}",
            output.status.code()
        );
        assert!(
            !stdout.contains("An error occurred while getting")
                && !stderr.contains("An error occurred while getting"),
            "{script_rel} {args:?} logged an RPC fallback:\n{stdout}\n{stderr}"
        );
        assert!(
            !stderr.contains("Traceback"),
            "{script_rel} {args:?} faulted:\n{stderr}"
        );
    }

    cleanup_config_dir(&config_dir);
}

/// A Python `rnsd` from an arbitrary Reticulum checkout, killed on drop.
struct ForeignRnsd {
    child: std::process::Child,
    instance_name: String,
    identity_bytes: [u8; 64],
    config_dir: PathBuf,
}

impl Drop for ForeignRnsd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        cleanup_config_dir(&self.config_dir);
    }
}

/// Start an `rnsd` out of `daemon_root` whose interface inventory matches what
/// [`start_rust_daemon_with_rpc`] builds: transport on, shared instance on,
/// exactly one TCPServerInterface.
///
/// `daemon_root` is separate from the tree the CLIENT runs from on purpose:
/// the A/B runs a 1.5.2 rnsd, the positive control below runs the vendored
/// 1.3.5 rnsd, and both are driven by the same 1.5.2 client.
async fn start_foreign_rnsd(daemon_root: &Path) -> ForeignRnsd {
    // port-handoff: the foreign rnsd reads it from its config file.
    let (ports, _port_alloc) = find_available_ports::<2>()
        .await
        .expect("failed to allocate ports");
    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let instance_name = format!("timingrnsd_{}_{}", std::process::id(), test_id);
    let config_dir = std::env::temp_dir().join(format!("timing_parity_{instance_name}"));
    let _ = std::fs::remove_dir_all(&config_dir);
    std::fs::create_dir_all(config_dir.join("storage")).expect("create rnsd config dir");

    // Pre-seed the transport identity so the client config can be built with
    // the same authkey the daemon will derive.
    let identity = Identity::generate(&mut rand_core::OsRng);
    let identity_bytes = identity
        .private_key_bytes()
        .expect("generated identity must have private keys");
    std::fs::write(
        config_dir.join("storage").join("transport_identity"),
        identity_bytes,
    )
    .expect("write transport_identity");

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
         \x20 [[Timing TCP Server]]\n\
         \x20   type = TCPServerInterface\n\
         \x20   enabled = yes\n\
         \x20   listen_ip = 127.0.0.1\n\
         \x20   listen_port = {}\n",
        ports[0]
    );
    std::fs::write(config_dir.join("config"), config).expect("write rnsd config");

    let log = std::fs::File::create(config_dir.join("daemon.log")).expect("create rnsd log");
    let log_err = log.try_clone().expect("clone log handle");
    let mut cmd = std::process::Command::new("python3");
    cmd.arg(daemon_root.join("RNS/Utilities/rnsd.py"))
        .arg("--config")
        .arg(&config_dir)
        .env("PYTHONPATH", daemon_root)
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err));
    let child = leviculum_std::process::spawn_supervised(cmd).expect("spawn rnsd");

    ForeignRnsd {
        child,
        instance_name,
        identity_bytes,
        config_dir,
    }
}

/// The property this whole batch exists for: the SAME client, on an
/// equivalent single-TCP-interface config, computes the SAME wait window
/// against lnsd and against rnsd.
///
/// Drop-in discipline (CLAUDE.md §Debugging discipline, reference-first): one
/// driver script, run from one Reticulum checkout, pointed at either daemon.
/// The two client config dirs are built from the same template and differ
/// only in instance name and identity — the daemon is the only variable.
///
/// ```text
/// LEVICULUM_RNS_ROOT=/path/to/Reticulum-1.5.x \
///   cargo test -p leviculum-std --test rnsd_interop \
///   test_medium_path_timeout_parity_across_daemons -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "spawns a Python rnsd; needs LEVICULUM_RNS_ROOT"]
async fn test_medium_path_timeout_parity_across_daemons() {
    init_tracing();

    let (rns_root, version) = rns_root_and_version();
    eprintln!("A/B against RNS {version}");

    // lnsd side.
    let (_node, lnsd_instance, _tcp_addr, lnsd_identity, _storage) =
        start_rust_daemon_with_rpc().await;
    let lnsd_client = python_client_ready_from(&rns_root, &lnsd_instance, &lnsd_identity).await;

    // rnsd side, same client config template, same readiness client.
    let rnsd = start_foreign_rnsd(&rns_root).await;
    let rnsd_client =
        python_client_ready_from(&rns_root, &rnsd.instance_name, &rnsd.identity_bytes).await;

    let script = write_timing_driver(&lnsd_client);
    let lnsd_answers = run_timing_driver(&rns_root, &script, &lnsd_client).await;
    let rnsd_answers = run_timing_driver(&rns_root, &script, &rnsd_client).await;
    eprintln!("lnsd: {lnsd_answers:?}");
    eprintln!("rnsd: {rnsd_answers:?}");

    assert_eq!(
        lnsd_answers.bitrate, rnsd_answers.bitrate,
        "the two daemons disagree about their slowest online interface, so \
         every downstream timing comparison between them is invalid"
    );
    assert!(
        (lnsd_answers.timeout - rnsd_answers.timeout).abs() < 1e-9,
        "wait windows differ: lnsd {} s vs rnsd {} s",
        lnsd_answers.timeout,
        rnsd_answers.timeout
    );
    // Positive control: an equal-but-zero pair would satisfy the equality
    // above while proving neither daemon answered.
    assert!(
        lnsd_answers.timeout > 0.0,
        "both daemons answered 0 s — the equality above is vacuous"
    );
    assert!(
        (lnsd_answers.timeout - expected_medium_path_timeout(lnsd_answers.bitrate)).abs() < 1e-9,
        "the shared window is not the upstream formula's value"
    );

    cleanup_config_dir(&lnsd_client);
    cleanup_config_dir(&rnsd_client);
}

/// Positive control for both assertions the two tests above rest on.
///
/// The vendored 1.3.5 `rnsd` predates all three verbs: its `rpc_loop` matches
/// no arm, sends nothing, and closes the connection on the next `accept()`.
/// The 1.5.2 client then takes the swallow-and-fall-back arm of
/// `get_medium_path_timeout` (Reticulum 1.5.2) — it LOGS
/// "An error occurred while getting medium path timeout from shared instance"
/// and returns 0.
///
/// That is exactly what lnsd did before this batch, reproduced against a real
/// daemon with the same driver. Without it, "no fallback line" and
/// "timeout > 0" would be assertions nobody has seen fail.
///
/// The readiness probe here is the VENDORED rnstatus, not the 1.5.2 one: a
/// 1.5.2 rnstatus indexes `ifstat["txdrp"]` unguarded and dies with KeyError
/// against a 1.3.5 daemon (that is Codeberg #329 itself), so it cannot serve
/// as a liveness check for this daemon.
///
/// ```text
/// LEVICULUM_RNS_ROOT=/path/to/Reticulum-1.5.x \
///   cargo test -p leviculum-std --test rnsd_interop \
///   test_timing_verbs_fall_back_against_a_daemon_without_them -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "spawns the vendored Python rnsd; needs LEVICULUM_RNS_ROOT for the client"]
async fn test_timing_verbs_fall_back_against_a_daemon_without_them() {
    init_tracing();

    let (rns_root, version) = rns_root_and_version();
    let vendored = PathBuf::from(VENDOR_RNS_ROOT);
    eprintln!("driving an RNS {version} client against the vendored 1.3.5 rnsd");

    let rnsd = start_foreign_rnsd(&vendored).await;
    let client = python_client_ready(&rnsd.instance_name, &rnsd.identity_bytes).await;
    let script = write_timing_driver(&client);

    let output = tokio::process::Command::new("python3")
        .arg(&script)
        .arg(&client)
        .env("PYTHONPATH", &rns_root)
        .output()
        .await
        .expect("failed to spawn python3");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "driver exited {:?}\n{stdout}\n{stderr}",
        output.status.code()
    );

    assert!(
        stdout.contains("An error occurred while getting"),
        "a daemon without the verbs must make the client log the fallback; \
         if this line is absent the no-fallback assertion elsewhere is \
         asserting nothing:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("DRIVER timeout=0"),
        "the fallback value is 0 s — the collapsed wait window this batch \
         removes:\n{stdout}"
    );
    assert!(
        stdout.contains("DRIVER bitrate=None"),
        "the bitrate fallback is None (Reticulum 1.5.2):\n{stdout}"
    );
    // `get_active_link_count` has no `try` of its own, so an unknown verb
    // reaches the caller as an exception rather than a fallback value. The
    // main tests parse that value as an int and fail loudly on `ERR:`.
    assert!(
        stdout.contains("DRIVER active_link_count=ERR:"),
        "an unserved active_link_count must raise, not answer:\n{stdout}"
    );

    cleanup_config_dir(&client);
}

// --- The link-table pair under a real relayed link ------------------------
//
// `link_count`/`active_link_count` are only interesting when the table is not
// empty, and the table only fills when the daemon RELAYS a link — a link it
// terminates itself never enters `Transport.link_table` (Transport.py:1625,
// the transport-onward branch). Two shared-instance clients of the SAME
// daemon are the smallest topology that produces one: the announcing client's
// destination gets a path entry pointing at its local-client interface, and
// the second client's LINKREQUEST is then relayed across, leaving exactly one
// entry behind.
//
// Drop-in discipline: one pair of scripts, one driver, pointed at either
// daemon. The daemon is the only variable.

/// Hosts an IN/SINGLE destination on the shared instance and announces it.
/// Stays alive so the link it accepts stays up while the pair is read.
const LINK_SERVER_PY: &str = r#"
import os, sys, time, RNS
r = RNS.Reticulum(configdir=sys.argv[1])
print("SERVER shared=%r" % (r.is_connected_to_shared_instance,), flush=True)
ident = RNS.Identity()
d = RNS.Destination(ident, RNS.Destination.IN, RNS.Destination.SINGLE, "lnlctest", "peer")
d.set_proof_strategy(RNS.Destination.PROVE_ALL)
d.set_link_established_callback(lambda link: print("SERVER established", flush=True))
print("SERVER dest=%s" % (d.hash.hex(),), flush=True)
d.announce()
print("SERVER announced", flush=True)
while True: time.sleep(0.5)
"#;

/// Establishes a link to the server's destination through the daemon and holds
/// it open.
const LINK_CLIENT_PY: &str = r#"
import os, sys, time, RNS
r = RNS.Reticulum(configdir=sys.argv[1])
print("CLIENT shared=%r" % (r.is_connected_to_shared_instance,), flush=True)
dh = bytes.fromhex(sys.argv[2])
if not RNS.Transport.has_path(dh):
    RNS.Transport.request_path(dh)
    t0 = time.time()
    while not RNS.Transport.has_path(dh) and time.time() - t0 < 30: time.sleep(0.1)
print("CLIENT has_path=%r" % (RNS.Transport.has_path(dh),), flush=True)
d = RNS.Destination(RNS.Identity.recall(dh), RNS.Destination.OUT,
                    RNS.Destination.SINGLE, "lnlctest", "peer")
link = RNS.Link(d)
t0 = time.time()
while link.status != RNS.Link.ACTIVE and time.time() - t0 < 40: time.sleep(0.1)
print("CLIENT active=%r" % (link.status == RNS.Link.ACTIVE,), flush=True)
while True: time.sleep(0.5)
"#;

/// The two client processes whose link the daemon relays, killed on drop.
struct RelayedLink {
    procs: Vec<std::process::Child>,
}

impl Drop for RelayedLink {
    fn drop(&mut self) {
        for p in self.procs.iter_mut() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }
}

/// Spawn a Python helper from `rns_root` against `config_dir`, logging to
/// `config_dir/<name>.log`.
fn spawn_link_helper(
    rns_root: &Path,
    config_dir: &Path,
    name: &str,
    source: &str,
    extra_arg: Option<&str>,
) -> std::process::Child {
    let script = config_dir.join(format!("{name}.py"));
    std::fs::write(&script, source).expect("write link helper");
    let log = std::fs::File::create(config_dir.join(format!("{name}.log"))).expect("create log");
    let log_err = log.try_clone().expect("clone log handle");
    let mut cmd = std::process::Command::new("python3");
    cmd.arg(&script).arg(config_dir);
    if let Some(a) = extra_arg {
        cmd.arg(a);
    }
    cmd.env("PYTHONPATH", rns_root)
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err));
    leviculum_std::process::spawn_supervised(cmd).unwrap_or_else(|e| panic!("spawn {name}: {e}"))
}

/// Read `config_dir/<name>.log`.
fn helper_log(config_dir: &Path, name: &str) -> String {
    std::fs::read(config_dir.join(format!("{name}.log")))
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// Wait for `needle` to appear in a helper's log, or panic with the log.
async fn await_helper_line(
    config_dir: &Path,
    name: &str,
    needle: &str,
    timeout: Duration,
) -> String {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let log = helper_log(config_dir, name);
        if log.contains(needle) {
            return log;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{name} never printed {needle:?} within {timeout:?}; log:\n{log}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Bring up server + client on `config_dir`'s daemon and return once the link
/// is ACTIVE — at which point the daemon holds exactly one relayed link.
async fn establish_relayed_link(rns_root: &Path, config_dir: &Path) -> RelayedLink {
    let server = spawn_link_helper(rns_root, config_dir, "linkserver", LINK_SERVER_PY, None);
    let mut held = RelayedLink {
        procs: vec![server],
    };
    let log = await_helper_line(
        config_dir,
        "linkserver",
        "SERVER announced",
        Duration::from_secs(45),
    )
    .await;
    let dest = log
        .lines()
        .find_map(|l| l.trim().strip_prefix("SERVER dest="))
        .unwrap_or_else(|| panic!("server printed no destination:\n{log}"))
        .trim()
        .to_string();

    let client = spawn_link_helper(
        rns_root,
        config_dir,
        "linkclient",
        LINK_CLIENT_PY,
        Some(&dest),
    );
    held.procs.push(client);
    let log = await_helper_line(
        config_dir,
        "linkclient",
        "CLIENT active=",
        Duration::from_secs(90),
    )
    .await;
    assert!(
        log.contains("CLIENT active=True"),
        "the client never reached an ACTIVE link, so nothing was relayed:\n{log}"
    );
    held
}

/// The trailer line `rnstatus -l` renders for the link table, e.g.
/// "1 entry in link table (1 active)" — extracted from the Uptime line the
/// count is appended to (rnstatus.py:709-715, 1.5.2).
async fn link_table_line(rns_root: &Path, config_dir: &Path) -> String {
    let output =
        run_python_tool_from(rns_root, "RNS/Utilities/rnstatus.py", &["-l"], config_dir).await;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "rnstatus -l exited {:?}\n{stdout}\n{stderr}",
        output.status.code()
    );
    stdout
        .lines()
        .find(|l| l.contains("in link table"))
        .unwrap_or_else(|| panic!("rnstatus -l rendered no link-table trailer:\n{stdout}"))
        // The trailer is appended to the Uptime line as ", <count> entr…"
        // when the daemon has a transport_id (both do here), and `-l` alone
        // adds no further comma-separated tail after it.
        .rsplit_once(", ")
        .map(|(_, rest)| rest.trim().to_string())
        .unwrap_or_else(|| panic!("could not isolate the link-table trailer:\n{stdout}"))
}

/// The operator-visible pair, measured on both daemons with one relayed link
/// standing: `rnstatus -l` must render the SAME line, and the verbs must
/// answer the same `link_count`.
///
/// This is the assertion the semantics change exists for. Until it, lnsd
/// answered `link_count` from the links it TERMINATES — which is 0 in this
/// topology, because both link ends are Python clients and the daemon only
/// relays — so `rnstatus -l` read "0 entries in link table" against lnsd and
/// "1 entry in link table (1 active)" against rnsd for the same mesh state.
///
/// `active_link_count` is asserted against lnsd only, and deliberately not
/// compared across daemons: upstream's `Transport.active_link_count` indexes
/// `entry[IDX_LT_VALIDATED]` on the link table's KEYS, so rnsd's answer is a
/// function of the eighth byte of a random link id (non-zero with probability
/// 255/256) rather than of validation. Asserting equality would be asserting a
/// coin flip; we assert our own documented meaning instead.
///
/// ```text
/// LEVICULUM_RNS_ROOT=/path/to/Reticulum-1.5.x \
///   cargo test -p leviculum-std --test rnsd_interop \
///   test_link_table_pair_parity_across_daemons -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "spawns a Python rnsd and four client processes; needs LEVICULUM_RNS_ROOT"]
async fn test_link_table_pair_parity_across_daemons() {
    init_tracing();

    let (rns_root, version) = rns_root_and_version();
    eprintln!("link-table A/B against RNS {version}");

    let (_node, lnsd_instance, _tcp_addr, lnsd_identity, _storage) =
        start_rust_daemon_with_rpc().await;
    let lnsd_client = python_client_ready_from(&rns_root, &lnsd_instance, &lnsd_identity).await;

    let rnsd = start_foreign_rnsd(&rns_root).await;
    let rnsd_client =
        python_client_ready_from(&rns_root, &rnsd.instance_name, &rnsd.identity_bytes).await;

    // Idle baseline first: both daemons must agree on the empty table too,
    // otherwise "they agree on 1" could be luck rather than a shared meaning.
    let script = write_timing_driver(&lnsd_client);
    let rnsd_script = write_timing_driver(&rnsd_client);
    let lnsd_idle = run_timing_driver(&rns_root, &script, &lnsd_client).await;
    let rnsd_idle = run_timing_driver(&rns_root, &rnsd_script, &rnsd_client).await;
    assert_eq!(
        (lnsd_idle.link_count, rnsd_idle.link_count),
        (0, 0),
        "an idle daemon relays nothing: lnsd {lnsd_idle:?} rnsd {rnsd_idle:?}"
    );

    let _lnsd_link = establish_relayed_link(&rns_root, &lnsd_client).await;
    let _rnsd_link = establish_relayed_link(&rns_root, &rnsd_client).await;

    let lnsd_answers = run_timing_driver(&rns_root, &script, &lnsd_client).await;
    let rnsd_answers = run_timing_driver(&rns_root, &rnsd_script, &rnsd_client).await;
    eprintln!("lnsd: {lnsd_answers:?}");
    eprintln!("rnsd: {rnsd_answers:?}");

    assert_eq!(
        lnsd_answers.link_count, rnsd_answers.link_count,
        "the two daemons disagree about how many links they relay, so the \
         operator line means different things depending on the daemon"
    );
    // Positive control: an equal-but-zero pair would satisfy the equality
    // above while proving neither daemon counted the link that was stood up.
    assert_eq!(
        lnsd_answers.link_count, 1,
        "one link was relayed, so the relay table holds exactly one entry"
    );
    assert_eq!(
        lnsd_answers.active_link_count, 1,
        "the relayed link validated (the client reached ACTIVE), so it is in \
         the validated subset"
    );

    // The rendered line, from the literal 1.5.2 rnstatus, is the surface an
    // operator actually reads.
    let lnsd_line = link_table_line(&rns_root, &lnsd_client).await;
    let rnsd_line = link_table_line(&rns_root, &rnsd_client).await;
    eprintln!("lnsd rnstatus -l: {lnsd_line:?}\nrnsd rnstatus -l: {rnsd_line:?}");
    assert_eq!(
        lnsd_line, rnsd_line,
        "rnstatus -l renders a different link-table line depending on the daemon"
    );
    assert_eq!(
        lnsd_line, "1 entry in link table (1 active)",
        "the rendered line must be the upstream one, singular form and all"
    );

    drop(_lnsd_link);
    drop(_rnsd_link);
    cleanup_config_dir(&lnsd_client);
    cleanup_config_dir(&rnsd_client);
}
