//! Bidirectional discovery tests using the daemon harness.
//!
//! These tests verify that Rust can receive and process announce packets
//! broadcast by the Python daemon. This validates the announce format,
//! signature verification, and Transport path table creation.
//!
//! ## What These Tests Verify
//!
//! 1. **Daemon announce → Rust reception** - Python's announce broadcast reaches Rust
//! 2. **Announce signature validation** - Ed25519 signature interop
//! 3. **Hash derivation match** - Rust computes same destination hash as Python
//! 4. **Transport path creation** - Full announce processing pipeline works
//!
//! ## Running These Tests
//!
//! ```sh
//! # Run all discovery tests
//! cargo test --package leviculum-std --test rnsd_interop discovery_tests
//!
//! # Run with verbose output
//! cargo test --package leviculum-std --test rnsd_interop discovery_tests -- --nocapture
//! ```

use std::time::Duration;

use tokio::time::timeout;

use leviculum_core::constants::{ED25519_KEY_SIZE, TRUNCATED_HASHBYTES, X25519_KEY_SIZE};
use leviculum_core::identity::Identity;
use leviculum_core::transport::TransportEvent;
use leviculum_core::DestinationHash;
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::interfaces::hdlc::Deframer;
use leviculum_std::NodeEvent;

use crate::common::*;
use crate::harness::TestDaemon;

// =========================================================================
// Test 1: Daemon Announce Received
// =========================================================================

/// Verify that Python's announce broadcast reaches Rust correctly.
///
/// This test verifies:
/// - Daemon can register and announce a destination
/// - Rust receives the announce packet over TCP
/// - The packet can be parsed with ParsedAnnounce
/// - The destination hash matches the daemon's reported hash
#[tokio::test]
async fn test_daemon_announce_received() {
    let daemon = TestDaemon::start().await.expect("Failed to start daemon");

    // Register a destination in the daemon
    let dest_info = daemon
        .register_destination("discovery", &["test"])
        .await
        .expect("Failed to register destination");

    println!("Registered destination: {}", dest_info.hash);

    // Connect to daemon to receive announces
    let mut stream = connect_to_daemon(&daemon).await;
    let mut deframer = Deframer::new();

    // Trigger announce from daemon
    let app_data = b"discovery-test-data";
    daemon
        .announce_destination(&dest_info.hash, app_data)
        .await
        .expect("Failed to announce destination");

    println!("Daemon announced, waiting for packet...");

    // Wait for the announce packet
    let result =
        receive_announce_from_daemon(&mut stream, &mut deframer, Duration::from_secs(5)).await;

    assert!(
        result.is_some(),
        "Should receive announce packet from daemon"
    );
    let (packet, _raw) = result.unwrap();

    // Parse the announce
    let announce = ParsedAnnounce::from_packet(&packet);
    assert!(announce.is_some(), "Should be able to parse announce");
    let announce = announce.unwrap();

    // Verify destination hash matches daemon's reported hash
    let expected_hash = hex::decode(&dest_info.hash).expect("Invalid daemon hash");
    assert_eq!(
        announce.destination_hash.as_slice(),
        expected_hash.as_slice(),
        "Destination hash should match daemon's reported hash"
    );

    // Verify app_data
    assert_eq!(
        announce.app_data.as_slice(),
        app_data,
        "App data should match what was announced"
    );

    println!("SUCCESS: Received and parsed daemon announce correctly!");
}

// =========================================================================
// Test 2: Announce Signature Validation
// =========================================================================

/// Verify that Ed25519 signature validation works across Rust and Python.
///
/// This test verifies:
/// - The announce signature was created by Python's Ed25519 implementation
/// - Rust can verify the signature using the public key from the announce
/// - The computed destination hash matches the expected value
#[tokio::test]
async fn test_daemon_announce_signature_valid() {
    let daemon = TestDaemon::start().await.expect("Failed to start daemon");

    // Register destination
    let dest_info = daemon
        .register_destination("sigtest", &["verify"])
        .await
        .expect("Failed to register destination");

    println!("Registered destination: {}", dest_info.hash);
    println!("Public key: {} bytes", dest_info.public_key.len() / 2);

    // Connect and receive announce
    let mut stream = connect_to_daemon(&daemon).await;
    let mut deframer = Deframer::new();

    daemon
        .announce_destination(&dest_info.hash, b"sig-test")
        .await
        .expect("Failed to announce");

    let result =
        receive_announce_from_daemon(&mut stream, &mut deframer, Duration::from_secs(5)).await;

    assert!(result.is_some(), "Should receive announce");
    let (packet, _raw) = result.unwrap();

    let announce = ParsedAnnounce::from_packet(&packet).expect("Should parse announce");

    // Create an Identity from the public key to verify signature
    // Public key is 64 bytes: X25519 (32) + Ed25519 (32)
    assert_eq!(
        announce.public_key.len(),
        64,
        "Public key should be 64 bytes"
    );

    let x25519_pub: [u8; X25519_KEY_SIZE] = announce.public_key[..X25519_KEY_SIZE]
        .try_into()
        .expect("Invalid X25519 key length");
    let ed25519_pub: [u8; ED25519_KEY_SIZE] = announce.public_key[X25519_KEY_SIZE..]
        .try_into()
        .expect("Invalid Ed25519 key length");

    let verifier_identity = Identity::from_public_keys(&x25519_pub, &ed25519_pub)
        .expect("Should create Identity from public keys");

    // Compute signed data
    let signed_data = announce.signed_data();

    // Verify the signature using Identity
    let is_valid = verifier_identity
        .verify(&signed_data, &announce.signature)
        .expect("Signature verification should not error");

    assert!(is_valid, "Signature should verify");

    // Verify computed destination hash matches
    let computed_hash = announce.computed_destination_hash();
    assert_eq!(
        computed_hash, announce.destination_hash,
        "Computed destination hash should match packet hash"
    );

    println!("SUCCESS: Ed25519 signature verified correctly!");
}

// =========================================================================
// Test 3: Transport Path from Daemon Announce
// =========================================================================

/// Verify that the full announce processing pipeline creates a path entry.
///
/// This test verifies:
/// - Rust Transport can process raw announce packets
/// - A path entry is created in the path table
/// - The PathFound event is emitted
/// - The hop count is recorded correctly
#[tokio::test]
async fn test_transport_path_from_daemon_announce() {
    let daemon = TestDaemon::start().await.expect("Failed to start daemon");

    // Register destination
    let dest_info = daemon
        .register_destination("pathtest", &["entry"])
        .await
        .expect("Failed to register destination");

    let dest_hash: [u8; TRUNCATED_HASHBYTES] = hex::decode(&dest_info.hash)
        .expect("Invalid hash")
        .try_into()
        .expect("Invalid hash length");

    println!("Registered destination: {}", dest_info.hash);

    // Create Transport with mock interface
    let (mut transport, iface_idx) = create_test_transport();

    // Initial state: no path
    assert!(
        !transport.has_path(&dest_hash),
        "Should not have path initially"
    );

    // Connect to daemon and receive announce
    let mut stream = connect_to_daemon(&daemon).await;
    let mut deframer = Deframer::new();

    daemon
        .announce_destination(&dest_info.hash, b"path-test")
        .await
        .expect("Failed to announce");

    let result =
        receive_announce_from_daemon(&mut stream, &mut deframer, Duration::from_secs(5)).await;

    assert!(result.is_some(), "Should receive announce");
    let (_packet, raw) = result.unwrap();

    println!("Received announce, feeding to Transport...");

    // Feed raw packet to Transport
    let process_result = transport.process_incoming(iface_idx, &raw);
    assert!(
        process_result.is_ok(),
        "Transport should process announce: {:?}",
        process_result
    );

    // Verify path was created
    assert!(
        transport.has_path(&dest_hash),
        "Should have path after announce"
    );

    // Check hop count (daemon's announce comes with hops=0, but we received it so it should be 0)
    let hops = transport.hops_to(&dest_hash);
    assert!(hops.is_some(), "Should have hop count");
    println!("Path hops: {}", hops.unwrap());

    // Verify PathFound event was emitted
    let events: Vec<_> = transport.drain_events().collect();
    let path_found = events.iter().any(|e| {
        matches!(e, TransportEvent::PathFound { destination_hash, .. } if *destination_hash == dest_hash)
    });
    assert!(path_found, "Should emit PathFound event");

    // Also verify AnnounceReceived event
    let announce_received = events.iter().any(|e| {
        matches!(e, TransportEvent::AnnounceReceived { announce, .. } if *announce.destination_hash() == dest_hash)
    });
    assert!(announce_received, "Should emit AnnounceReceived event");

    println!("SUCCESS: Transport created path from daemon announce!");
}

// =========================================================================
// Test 4: Multiple Daemon Announces
// =========================================================================

/// Verify that multiple announces create multiple path entries.
///
/// This test verifies:
/// - Multiple destinations can be registered and announced
/// - Each announce creates a separate path entry
/// - Announces don't interfere with each other
///
/// Uses the high-level ReticulumNode API which handles HDLC deframing
/// correctly via the spawned TCP interface task's HDLC deframing.
///
/// The announces are spaced 1 s apart to stay below the ingress burst
/// threshold (Codeberg #87): on a new interface the limit is
/// IC_BURST_FREQ_NEW = 3 Hz, and the frequency at the third announce is
/// n / span = 3 / 2s = 1.5 Hz, a 2x margin. Python RNS 1.3.5 applies the
/// same limit (measured: at the previous 100 ms spacing a Python receiver
/// holds the third announce too), so rapid spacing would make this test
/// exercise burst limiting instead of discovery. Burst hold-and-release
/// has its own coverage in held_announce_interop_tests.
#[tokio::test]
async fn test_multiple_daemon_announces() {
    let daemon = TestDaemon::start().await.expect("Failed to start daemon");

    // Build and start node connecting to the daemon
    let _storage = crate::common::temp_storage("test_multiple_daemon_announces", "node");
    let mut node = ReticulumNodeBuilder::new()
        .add_tcp_client(daemon.rns_addr())
        .storage_path(_storage.path().to_path_buf())
        .build()
        .await
        .expect("Failed to build node");

    node.start().await.expect("Failed to start node");
    node.wait_for_interfaces_ready(Duration::from_secs(2))
        .await
        .expect("Interfaces should become ready");
    daemon
        .wait_for_peer_count(1, Duration::from_secs(2))
        .await
        .expect("Daemon should register peer");

    let mut events = node
        .take_event_receiver()
        .expect("Failed to get event receiver");

    // Register multiple destinations
    let dest_a = daemon
        .register_destination("multi", &["a"])
        .await
        .expect("Failed to register destination A");
    let dest_b = daemon
        .register_destination("multi", &["b"])
        .await
        .expect("Failed to register destination B");
    let dest_c = daemon
        .register_destination("multi", &["c"])
        .await
        .expect("Failed to register destination C");

    println!("Registered destinations:");
    println!("  A: {}", dest_a.hash);
    println!("  B: {}", dest_b.hash);
    println!("  C: {}", dest_c.hash);

    // Announce all three destinations, spaced below the 3 Hz ingress burst
    // threshold (see doc comment).
    daemon
        .announce_destination(&dest_a.hash, b"data-a")
        .await
        .expect("Failed to announce A");
    tokio::time::sleep(Duration::from_millis(1000)).await;

    daemon
        .announce_destination(&dest_b.hash, b"data-b")
        .await
        .expect("Failed to announce B");
    tokio::time::sleep(Duration::from_millis(1000)).await;

    daemon
        .announce_destination(&dest_c.hash, b"data-c")
        .await
        .expect("Failed to announce C");

    println!("All destinations announced, collecting events...");

    // Collect AnnounceReceived events
    let mut received_hashes = Vec::new();
    let collection_timeout = Duration::from_secs(10);
    let start = std::time::Instant::now();

    while received_hashes.len() < 3 && start.elapsed() < collection_timeout {
        match timeout(Duration::from_millis(500), events.recv()).await {
            Ok(Some(NodeEvent::AnnounceReceived { announce, .. })) => {
                let hash = hex::encode(announce.destination_hash());
                println!(
                    "Received announce {}/3: {}",
                    received_hashes.len() + 1,
                    hash
                );
                received_hashes.push(hash);
            }
            Ok(Some(_)) => {}   // Other event types, continue
            Ok(None) => break,  // Channel closed
            Err(_) => continue, // Timeout, try again
        }
    }

    println!("Received {} announces", received_hashes.len());

    // Verify all three announces were received
    assert!(
        received_hashes.contains(&dest_a.hash),
        "Should receive announce A"
    );
    assert!(
        received_hashes.contains(&dest_b.hash),
        "Should receive announce B"
    );
    assert!(
        received_hashes.contains(&dest_c.hash),
        "Should receive announce C"
    );

    // Verify all paths exist in the node
    let hash_a: [u8; TRUNCATED_HASHBYTES] = hex::decode(&dest_a.hash).unwrap().try_into().unwrap();
    let hash_b: [u8; TRUNCATED_HASHBYTES] = hex::decode(&dest_b.hash).unwrap().try_into().unwrap();
    let hash_c: [u8; TRUNCATED_HASHBYTES] = hex::decode(&dest_c.hash).unwrap().try_into().unwrap();

    assert!(
        node.has_path(&DestinationHash::new(hash_a)),
        "Path A must exist"
    );
    assert!(
        node.has_path(&DestinationHash::new(hash_b)),
        "Path B must exist"
    );
    assert!(
        node.has_path(&DestinationHash::new(hash_c)),
        "Path C must exist"
    );

    println!("All 3 announces processed and paths verified");

    node.stop().await.expect("Failed to stop node");
}

// =========================================================================
// Test 5: Announce Hash Derivation Match
// =========================================================================

/// Verify that Rust's hash derivation matches Python's exactly.
///
/// This test verifies:
/// - The computed destination hash from announce data matches packet's dest_hash
/// - The computed identity hash is correct
/// - Hash derivation is identical between Rust and Python implementations
#[tokio::test]
async fn test_announce_hash_derivation_match() {
    let daemon = TestDaemon::start().await.expect("Failed to start daemon");

    // Register destination
    let dest_info = daemon
        .register_destination("hashtest", &["derive"])
        .await
        .expect("Failed to register destination");

    let expected_hash: [u8; TRUNCATED_HASHBYTES] = hex::decode(&dest_info.hash)
        .expect("Invalid hash")
        .try_into()
        .expect("Invalid hash length");

    println!("Daemon's destination hash: {}", dest_info.hash);

    // Connect and receive announce
    let mut stream = connect_to_daemon(&daemon).await;
    let mut deframer = Deframer::new();

    daemon
        .announce_destination(&dest_info.hash, b"hash-test")
        .await
        .expect("Failed to announce");

    let result =
        receive_announce_from_daemon(&mut stream, &mut deframer, Duration::from_secs(5)).await;

    assert!(result.is_some(), "Should receive announce");
    let (packet, _raw) = result.unwrap();

    // Parse announce
    let announce = ParsedAnnounce::from_packet(&packet).expect("Should parse announce");

    // 1. Verify packet's destination_hash matches daemon's hash
    assert_eq!(
        packet.destination_hash, expected_hash,
        "Packet destination_hash should match daemon's hash"
    );

    // 2. Compute identity hash from public key
    let computed_identity_hash = announce.computed_identity_hash();
    println!(
        "Computed identity hash: {}",
        hex::encode(computed_identity_hash)
    );

    // 3. Compute destination hash
    let computed_dest_hash = announce.computed_destination_hash();
    println!(
        "Computed destination hash: {}",
        hex::encode(computed_dest_hash)
    );

    // 4. Verify all three hashes match
    assert_eq!(
        computed_dest_hash, expected_hash,
        "Rust's computed hash should match daemon's hash"
    );
    assert_eq!(
        computed_dest_hash, packet.destination_hash,
        "Computed hash should match packet's destination_hash"
    );
    assert_eq!(
        announce.destination_hash, expected_hash,
        "Announce's stored destination_hash should match daemon's hash"
    );

    println!("SUCCESS: Hash derivation matches exactly!");
    println!("  Daemon hash:   {}", dest_info.hash);
    println!("  Packet hash:   {}", hex::encode(packet.destination_hash));
    println!("  Computed hash: {}", hex::encode(computed_dest_hash));
}
