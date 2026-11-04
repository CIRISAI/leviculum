//! mvr for order 1317: the third announce a node puts on a fresh connection
//! to a Python node in one burst is held there, and stays held.
//!
//! Field shape: `loquax-android/tests/facade.rs`
//! `a_file_travels_both_ways_with_python_lxmf` went red in two land gates
//! (29aa5b2f, 229d285e) and 1 to 3 times in 15 runs beside a concurrent
//! `cargo test -p loquax`, never alone: "Python never learned the phone's
//! mailbox". A diagnostic hook on the Python peer showed three announces from
//! the phone's mailbox on its one connection within a millisecond (the home
//! destination twice, then the delivery destination), and the third held:
//! `ia_freq=5523 n=3`, then held again on every release attempt until the
//! test's 40 s deadline.
//!
//! The reference half, quoted from `reference/Reticulum`: an announce for a
//! destination not in the path table is held when the receiving interface
//! `should_ingress_limit()` (`RNS/Transport.py:1695-1706`). A spawned TCP
//! interface is younger than `IC_NEW_TIME` (two hours), so its threshold is
//! `IC_BURST_FREQ_NEW` = 3 Hz, and the frequency counts once the deque holds
//! more than `IC_DEQUE_MIN_SAMPLE` = 2 samples
//! (`RNS/Interfaces/Interface.py:75-84, 145-165, 279-288`): the third
//! announce of a burst measures thousands of hertz and sets the burst. The
//! burst ends only with at least `IC_BURST_MIN_SAMPLES` = 6 samples
//! (`Interface.py:151-152`), and a quiet connection never collects them, so
//! every release (`Interface.py:234-251`) is held again.
//!
//! The Rust half, how two destinations became three announces, is the
//! driver mvr `an_announce_committed_before_a_new_interface_reaches_it_once`
//! (`leviculum-std/src/driver/mod.rs`).
//!
//! Here: one Rust node with one TCP client to a Python daemon, the
//! connection up, then announces A, A again and B, back to back. Before
//! order 1320 Python learned A and not B within 5 s. Since 1320 the core
//! spaces its own announces on a fresh interface
//! (`leviculum-core/src/transport/fresh_announce_pacing.rs`), and Python
//! learns both; the third test holds five destinations to the same.
//! `local_clients_announces_on_a_fresh_interface_reach_python.rs` holds the
//! announces a shared instance forwards for its clients to the same (1358).

use std::time::Duration;

use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::ReticulumNodeBuilder;

use crate::harness::TestDaemon;

fn destination(aspect: &str) -> Destination {
    Destination::new(
        Some(Identity::generate(&mut rand_core::OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvr",
        &["1317", aspect],
    )
    .expect("create destination")
}

#[tokio::test]
async fn a_third_announce_on_a_fresh_interface_waits_at_python() {
    let daemon = TestDaemon::start().await.expect("start daemon");

    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(daemon.rns_addr())
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    node.start().await.expect("start node");
    node.wait_for_interface_ready(0, Duration::from_secs(5))
        .await
        .expect("the TCP client connects");

    let first = destination("first");
    let second = destination("second");
    let (first_hash, second_hash) = (*first.hash(), *second.hash());
    node.register_destination(first);
    node.register_destination(second);

    // The burst the phone's mailbox sent: one destination twice, then the
    // other, inside a millisecond on a connection Python spawned just now.
    node.announce_destination(&first_hash, None)
        .await
        .expect("announce the first");
    node.announce_destination(&first_hash, None)
        .await
        .expect("announce the first again");
    node.announce_destination(&second_hash, None)
        .await
        .expect("announce the second");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let (mut knows_first, mut knows_second) = (false, false);
    while tokio::time::Instant::now() < deadline && !(knows_first && knows_second) {
        knows_first = daemon.has_path(first_hash.as_bytes()).await;
        knows_second = daemon.has_path(second_hash.as_bytes()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        knows_first,
        "Python never learned the first destination {}: the control failed, \
         the connection carried nothing",
        hex::encode(first_hash.as_bytes())
    );
    assert!(
        knows_second,
        "Python never learned {}, the third announce on its fresh interface: \
         held by its ingress control (Transport.py:1705)",
        hex::encode(second_hash.as_bytes())
    );
}

/// The control: the same connection with two announces, one per destination,
/// and Python learns both. What holds the second destination above is the
/// third announce, not the connection or the destination.
#[tokio::test]
async fn control_two_announces_on_a_fresh_interface_both_arrive() {
    let daemon = TestDaemon::start().await.expect("start daemon");

    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(daemon.rns_addr())
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    node.start().await.expect("start node");
    node.wait_for_interface_ready(0, Duration::from_secs(5))
        .await
        .expect("the TCP client connects");

    let first = destination("first");
    let second = destination("second");
    let (first_hash, second_hash) = (*first.hash(), *second.hash());
    node.register_destination(first);
    node.register_destination(second);
    node.announce_destination(&first_hash, None)
        .await
        .expect("announce the first");
    node.announce_destination(&second_hash, None)
        .await
        .expect("announce the second");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut knows_both = false;
    while tokio::time::Instant::now() < deadline && !knows_both {
        knows_both = daemon.has_path(first_hash.as_bytes()).await
            && daemon.has_path(second_hash.as_bytes()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        knows_both,
        "Python did not learn both of two announces on a fresh interface"
    );
}

/// Five destinations announced back to back on a fresh connection: Python
/// learns all five within 5 s (order 1320). Unpaced, the third and every
/// later one arrive inside a millisecond of the first and are held.
#[tokio::test]
async fn five_destinations_on_a_fresh_interface_all_arrive_at_python() {
    let daemon = TestDaemon::start().await.expect("start daemon");

    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(daemon.rns_addr())
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    node.start().await.expect("start node");
    node.wait_for_interface_ready(0, Duration::from_secs(5))
        .await
        .expect("the TCP client connects");

    let mut hashes = Vec::new();
    for aspect in ["one", "two", "three", "four", "five"] {
        let dest = destination(aspect);
        hashes.push(*dest.hash());
        node.register_destination(dest);
    }
    for hash in &hashes {
        node.announce_destination(hash, None)
            .await
            .expect("announce");
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut unknown = hashes.clone();
    while tokio::time::Instant::now() < deadline && !unknown.is_empty() {
        let mut still = Vec::new();
        for hash in unknown {
            if !daemon.has_path(hash.as_bytes()).await {
                still.push(hash);
            }
        }
        unknown = still;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        unknown.is_empty(),
        "Python did not learn {} of five destinations within 5 s: {:?}",
        unknown.len(),
        unknown
            .iter()
            .map(|h| hex::encode(h.as_bytes()))
            .collect::<Vec<_>>()
    );
}
