//! Live interface-discovery interop tests against a real Python `rnsd`
//! (Codeberg #106, closing the #32 live-integration gap).
//!
//! The existing `discovery_tests` cover the wire format with golden vectors and
//! a Rust<->Rust auto-connect; these drive the REAL Python
//! `RNS.Discovery.InterfaceAnnouncer` / `InterfaceDiscovery` end to end:
//!
//! * our lnsd discovers ONE live Python rnsd (record + `config_entry`);
//! * our lnsd discovers MULTIPLE live Python rnsd at once (distinct records);
//! * our lnsd AUTO-CONNECTS a discovered Backbone/TCP Python rnsd and traffic
//!   crosses the auto-established link;
//! * encrypted discovery: a matching network identity is discovered, a
//!   mismatched or absent one is not;
//! * reverse: a real Python rnsd discovers OUR announced interface.
//!
//! Each Python daemon runs the real announcer (`emit_discovery_announce` drives
//! its own `get_interface_announce_data` + `discovery_destination.announce`, so
//! stamps and encryption are genuine); the Rust node persists validated records
//! under `<storage>/discovery/interfaces`, exactly as `lnstatus -d` reads them.

use std::path::Path;
use std::time::{Duration, Instant};

use leviculum_core::discovery::{
    build_announce_app_data, DiscoveredInterfaceRecord, InterfaceDescriptor,
};
use leviculum_core::{Destination, DestinationHash, DestinationType, Direction, Identity};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::Config;

use crate::common::temp_storage;
use crate::harness::TestDaemon;

/// Read all persisted discovered-interface records from a node's storage dir
/// (`<storage>/discovery/interfaces`), the same files `lnstatus -d` lists.
fn read_discovered_records(storage: &Path) -> Vec<DiscoveredInterfaceRecord> {
    let dir = storage.join("discovery").join("interfaces");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) == Some("tmp") {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                if let Some(rec) = DiscoveredInterfaceRecord::decode_msgpack(&bytes) {
                    out.push(rec);
                }
            }
        }
    }
    out
}

/// Parse a 16-byte destination hash from the daemon's hex reply.
fn dest_hash_from_hex(hex_str: &str) -> DestinationHash {
    let bytes = hex::decode(hex_str).expect("valid hex hash");
    let arr: [u8; 16] = bytes.as_slice().try_into().expect("16-byte hash");
    DestinationHash::new(arr)
}

/// Expected `config_entry` for a discovered Python TCPServer/Backbone endpoint.
/// On non-Windows hosts Python and our stack both render it as a
/// `BackboneClientInterface` block (`Discovery.py` / `build_config_entry`).
fn expected_backbone_config_entry(name: &str, port: u16, transport_id_hex: &str) -> String {
    format!(
        "[[{name}]]\n  type = BackboneInterface\n  enabled = yes\n  \
         remote = 127.0.0.1\n  target_port = {port}\n  transport_identity = {transport_id_hex}"
    )
}

/// Poll `cond` every 100 ms until it is true or the deadline passes.
async fn wait_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    cond()
}

/// Re-drive the daemon's discovery announce on a fixed cadence until `cond`
/// holds or the deadline passes. A single announce can be lost or held by
/// ingress control under load, so the emit is repeated (Codeberg #105 hardening
/// style); each emit runs the real announcer.
async fn drive_discovery_until(
    daemon: &TestDaemon,
    deadline: Duration,
    mut cond: impl FnMut() -> bool,
) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        let _ = daemon.emit_discovery_announce().await;
        if wait_until(Duration::from_millis(700), &mut cond).await {
            return true;
        }
    }
    cond()
}

/// Build a Rust node connected to `daemon` as a TCP client, with an optional
/// shared discovery network identity and optional auto-connect cap.
async fn build_connected_node(
    daemon: &TestDaemon,
    storage: &tempfile::TempDir,
    network_identity: Option<&Path>,
    autoconnect_max: usize,
) -> leviculum_std::driver::ReticulumNode {
    let mut config = Config::default();
    if let Some(path) = network_identity {
        config.reticulum.network_identity = Some(path.to_path_buf());
    }
    let mut builder = ReticulumNodeBuilder::new()
        .config(config)
        .add_tcp_client(daemon.rns_addr())
        .storage_path(storage.path().to_path_buf());
    if autoconnect_max > 0 {
        builder = builder.autoconnect_discovered_interfaces(autoconnect_max);
    }
    let mut node = builder.build().await.expect("build node");
    node.start().await.expect("start node");
    node.wait_for_interfaces_ready(Duration::from_secs(5))
        .await
        .expect("interfaces ready");
    daemon
        .wait_for_peer_count(1, Duration::from_secs(5))
        .await
        .expect("daemon registers peer");
    node
}

// =========================================================================
// Test 1: our lnsd discovers ONE live Python rnsd
// =========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_lnsd_discovers_one_python_rnsd() {
    let name = "OneNode";
    let daemon = TestDaemon::start_discoverable(name)
        .await
        .expect("start discoverable daemon");
    let port = daemon.rns_port();

    let storage = temp_storage("disco_one", "node");
    let mut node = build_connected_node(&daemon, &storage, None, 0).await;

    let found = drive_discovery_until(&daemon, Duration::from_secs(20), || {
        read_discovered_records(storage.path())
            .iter()
            .any(|r| r.name == name)
    })
    .await;
    assert!(found, "lnsd did not discover the Python rnsd");

    let records = read_discovered_records(storage.path());
    let rec = records
        .iter()
        .find(|r| r.name == name)
        .expect("record present");

    assert_eq!(rec.interface_type, "TCPServerInterface");
    assert_eq!(rec.reachable_on.as_deref(), Some("127.0.0.1"));
    assert_eq!(rec.port, Some(port as u64));
    assert!(rec.value >= 14, "stamp value must meet the discovery cost");

    // config_entry is byte-identical to what Python's `rnstatus -d` renders for
    // the same discovered endpoint.
    let expected = expected_backbone_config_entry(name, port, &rec.transport_id);
    assert_eq!(
        rec.config_entry.as_deref(),
        Some(expected.as_str()),
        "config_entry must match the Python-rendered entry"
    );

    node.stop().await.expect("stop node");
}

// =========================================================================
// Test 2: our lnsd discovers MULTIPLE live Python rnsd simultaneously
// =========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_lnsd_discovers_multiple_python_rnsd() {
    let daemon_a = TestDaemon::start_discoverable("MultiA")
        .await
        .expect("start daemon A");
    let daemon_b = TestDaemon::start_discoverable("MultiB")
        .await
        .expect("start daemon B");
    let daemon_c = TestDaemon::start_discoverable("MultiC")
        .await
        .expect("start daemon C");

    // One Rust node, three separate TCP client interfaces (one per daemon), so
    // each announce arrives on its own interface.
    let storage = temp_storage("disco_multi", "node");
    let mut node = ReticulumNodeBuilder::new()
        .add_tcp_client(daemon_a.rns_addr())
        .add_tcp_client(daemon_b.rns_addr())
        .add_tcp_client(daemon_c.rns_addr())
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    node.start().await.expect("start node");
    node.wait_for_interfaces_ready(Duration::from_secs(5))
        .await
        .expect("interfaces ready");

    let names = ["MultiA", "MultiB", "MultiC"];
    let all_found = {
        let end = Instant::now() + Duration::from_secs(30);
        let mut ok = false;
        while Instant::now() < end {
            let _ = daemon_a.emit_discovery_announce().await;
            let _ = daemon_b.emit_discovery_announce().await;
            let _ = daemon_c.emit_discovery_announce().await;
            tokio::time::sleep(Duration::from_millis(700)).await;
            let recs = read_discovered_records(storage.path());
            ok = names.iter().all(|n| recs.iter().any(|r| &r.name == n));
            if ok {
                break;
            }
        }
        ok
    };
    assert!(all_found, "lnsd did not discover all three Python rnsd");

    // Distinct records: three names, three distinct discovery hashes, three
    // distinct transport ids.
    let recs = read_discovered_records(storage.path());
    let mut disco_hashes: Vec<_> = recs.iter().map(|r| r.discovery_hash).collect();
    disco_hashes.sort();
    disco_hashes.dedup();
    assert!(
        disco_hashes.len() >= 3,
        "expected >=3 distinct discovery records, got {}",
        disco_hashes.len()
    );
    for n in names {
        assert!(recs.iter().any(|r| r.name == n), "missing record for {n}");
    }

    node.stop().await.expect("stop node");
}

// =========================================================================
// Test 3: our lnsd auto-connects a discovered Backbone/TCP Python rnsd
// =========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_lnsd_autoconnects_discovered_python_rnsd() {
    // The daemon exposes a bootstrap TCP server (the node connects here to hear
    // the announce) plus a SECOND discoverable server on `backbone_port` (the
    // advertised endpoint the node auto-connects to -- a genuinely new link).
    let (daemon, backbone_port) = TestDaemon::start_discoverable_backbone("AutoBackbone")
        .await
        .expect("start backbone daemon");

    let storage = temp_storage("disco_auto", "node");
    let mut node = build_connected_node(&daemon, &storage, None, 4).await;

    // The node persists the record and then spawns an auto-connected interface
    // to 127.0.0.1:backbone_port.
    let auto_connected = drive_discovery_until(&daemon, Duration::from_secs(30), || {
        node.interface_stats()
            .iter()
            .any(|i| i.name.starts_with("autoconnect/"))
    })
    .await;
    assert!(
        auto_connected,
        "lnsd did not auto-connect the discovered Python endpoint; interfaces = {:?}",
        node.interface_stats()
            .iter()
            .map(|i| i.name.clone())
            .collect::<Vec<_>>()
    );

    // Sanity: the discovered record advertises the backbone port we auto-connect.
    let recs = read_discovered_records(storage.path());
    assert!(
        recs.iter()
            .any(|r| r.name == "AutoBackbone" && r.port == Some(backbone_port as u64)),
        "record must advertise the backbone port {backbone_port}"
    );

    // Traffic crosses the auto-established link: further announces from the
    // daemon reach the node over the auto-connected interface (rx_bytes > 0).
    let carried = drive_discovery_until(&daemon, Duration::from_secs(20), || {
        node.interface_stats()
            .iter()
            .any(|i| i.name.starts_with("autoconnect/") && i.online && i.rx_bytes > 0)
    })
    .await;
    assert!(
        carried,
        "auto-connected interface carried no traffic; interfaces = {:?}",
        node.interface_stats()
    );

    node.stop().await.expect("stop node");
}

// =========================================================================
// Test 4: encrypted discovery -- matching network identity is discovered
// =========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_encrypted_discovery_matching_identity_is_discovered() {
    // Shared 64-byte network identity file: Python generates it on startup, the
    // Rust node loads the same file to decrypt.
    let netid_dir = tempfile::tempdir().expect("netid dir");
    let netid_path = netid_dir.path().join("network_identity");

    let daemon = TestDaemon::start_discoverable_encrypted(
        "EncNode",
        netid_path.to_str().expect("utf8 path"),
    )
    .await
    .expect("start encrypted discoverable daemon");
    let port = daemon.rns_port();

    let storage = temp_storage("disco_enc_match", "node");
    let mut node = build_connected_node(&daemon, &storage, Some(&netid_path), 0).await;

    let found = drive_discovery_until(&daemon, Duration::from_secs(20), || {
        read_discovered_records(storage.path())
            .iter()
            .any(|r| r.name == "EncNode")
    })
    .await;
    assert!(
        found,
        "matching network identity must decrypt and discover the encrypted announce"
    );

    let recs = read_discovered_records(storage.path());
    let rec = recs
        .iter()
        .find(|r| r.name == "EncNode")
        .expect("record present");
    assert_eq!(rec.interface_type, "TCPServerInterface");
    assert_eq!(rec.port, Some(port as u64));
    // Encrypted announces are owned by the network identity, so network_id is
    // the network identity's hash (distinct from the transport id).
    assert_ne!(
        rec.network_id, rec.transport_id,
        "encrypted discovery: network_id is the network identity, not the transport id"
    );

    node.stop().await.expect("stop node");
}

// =========================================================================
// Test 5: encrypted discovery -- mismatched / absent identity is NOT discovered
// =========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_encrypted_discovery_mismatched_or_absent_not_discovered() {
    let netid_dir = tempfile::tempdir().expect("netid dir");
    let netid_path = netid_dir.path().join("network_identity");

    let daemon = TestDaemon::start_discoverable_encrypted(
        "SecretNode",
        netid_path.to_str().expect("utf8 path"),
    )
    .await
    .expect("start encrypted discoverable daemon");

    // Node A: a DIFFERENT network identity (mismatch). Node B: none (absent).
    let wrong_dir = tempfile::tempdir().expect("wrong netid dir");
    let wrong_path = wrong_dir.path().join("network_identity");
    // Materialise a distinct identity file so the Rust node loads a mismatched key.
    std::fs::write(
        &wrong_path,
        Identity::generate(&mut rand_core::OsRng)
            .private_key_bytes()
            .expect("private key bytes"),
    )
    .expect("write wrong identity");

    let storage_a = temp_storage("disco_enc_mismatch", "a");
    let mut node_a = build_connected_node(&daemon, &storage_a, Some(&wrong_path), 0).await;

    let storage_b = temp_storage("disco_enc_absent", "b");
    let mut node_b = build_connected_node(&daemon, &storage_b, None, 0).await;

    // The discovery destination hash: used to confirm the announce actually
    // REACHED each node (path learned) so a missing record is a decrypt
    // rejection, not a delivery failure.
    let emit = daemon
        .emit_discovery_announce()
        .await
        .expect("emit encrypted announce");
    let disco_hash = dest_hash_from_hex(
        emit.get("discovery_dest_hash")
            .and_then(|v| v.as_str())
            .expect("discovery_dest_hash"),
    );

    // Drive announces so both nodes hear it (path learned on both).
    let delivered = drive_discovery_until(&daemon, Duration::from_secs(15), || {
        node_a.has_path(&disco_hash) && node_b.has_path(&disco_hash)
    })
    .await;
    assert!(
        delivered,
        "encrypted announce did not reach both nodes (delivery precondition)"
    );

    // A couple more emits to give any (incorrect) persistence a chance to appear.
    for _ in 0..3 {
        let _ = daemon.emit_discovery_announce().await;
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    assert!(
        read_discovered_records(storage_a.path()).is_empty(),
        "mismatched network identity must NOT decrypt/discover the announce"
    );
    assert!(
        read_discovered_records(storage_b.path()).is_empty(),
        "absent network identity must NOT decrypt/discover the encrypted announce"
    );

    node_a.stop().await.expect("stop node a");
    node_b.stop().await.expect("stop node b");
}

// =========================================================================
// Test 6: reverse -- a real Python rnsd discovers OUR announced interface
// =========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_python_discovers_rust_announced_interface() {
    // Python daemon running the InterfaceDiscovery listener; the Rust node
    // connects to it as a TCP client and announces its own discoverable
    // interface, which the daemon must surface via `get_discovered_interfaces`.
    let daemon = TestDaemon::start_discovering()
        .await
        .expect("start discovering daemon");

    let storage = temp_storage("disco_reverse", "node");
    let mut node = build_connected_node(&daemon, &storage, None, 0).await;

    // The Rust node's discovery destination + a TCPServer descriptor advertising
    // a reachable endpoint. `transport = true` so it passes the only-transport
    // filters; the endpoint host/port need only be a valid IP/port for Python's
    // reachable_on validation.
    let identity = Identity::generate(&mut rand_core::OsRng);
    let disco_dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "rnstransport",
        &["discovery", "interface"],
    )
    .expect("discovery destination");
    let disco_hash = *disco_dest.hash();
    node.register_destination(disco_dest);

    let advertised_port: u16 = 45999;
    let descriptor = InterfaceDescriptor {
        interface_type: "TCPServerInterface".to_string(),
        name: Some("RustNode".to_string()),
        reachable_on: Some("127.0.0.1".to_string()),
        port: Some(advertised_port as u64),
        ..Default::default()
    };
    let transport_id = [0x5Au8; 16];
    let app_data = build_announce_app_data(&descriptor, &transport_id, true, &mut rand_core::OsRng)
        .expect("build discovery announce app_data");

    // Re-drive our announce until the Python daemon lists our interface.
    let discovered = {
        let end = Instant::now() + Duration::from_secs(20);
        let mut ok = false;
        while Instant::now() < end {
            node.announce_destination(&disco_hash, Some(&app_data))
                .await
                .expect("announce discovery record");
            tokio::time::sleep(Duration::from_millis(700)).await;
            let listed = daemon
                .get_discovered_interfaces()
                .await
                .expect("query discovered interfaces");
            ok = listed
                .iter()
                .any(|info| info.get("name").and_then(|v| v.as_str()) == Some("RustNode"));
            if ok {
                break;
            }
        }
        ok
    };
    assert!(
        discovered,
        "Python rnsd did not discover the Rust-announced interface"
    );

    // The Python-side record carries the advertised endpoint we announced.
    let listed = daemon
        .get_discovered_interfaces()
        .await
        .expect("query discovered interfaces");
    let info = listed
        .iter()
        .find(|i| i.get("name").and_then(|v| v.as_str()) == Some("RustNode"))
        .expect("record present");
    assert_eq!(
        info.get("type").and_then(|v| v.as_str()),
        Some("TCPServerInterface")
    );
    assert_eq!(
        info.get("reachable_on").and_then(|v| v.as_str()),
        Some("127.0.0.1")
    );
    assert_eq!(
        info.get("port").and_then(|v| v.as_u64()),
        Some(advertised_port as u64)
    );

    node.stop().await.expect("stop node");
}
