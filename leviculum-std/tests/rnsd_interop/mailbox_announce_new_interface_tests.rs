//! A Python peer that a node dials while it announces learns every
//! destination the node announced (order 1656, Refs #232, #24).
//!
//! The phone mailbox's start-up, reduced: the node holds a home and a
//! delivery destination, spawns a TCP client to a Python daemon (the
//! rendezvous dial, `loquax/src/home/rendezvous.rs`) and announces home and
//! then delivery from the caller's thread while the event loop registers the
//! connection. Before 1656 an announce that committed between the loop's
//! drain of the action channel and the new interface's interface-up reached
//! the connection twice, Python counted three announces in one burst on an
//! interface younger than `IC_NEW_TIME` and held the third
//! (`RNS/Transport.py:1695-1706`), and the delivery destination stayed
//! unknown until the next announce six minutes later. The wire half of the
//! mechanism is
//! `leviculum-std/tests/mvr/a_new_interface_hears_an_announce_made_before_it.rs`.
//!
//! Each round is a fresh node with fresh destinations on a new connection to
//! one daemon, so each round meets a fresh spawned interface on the Python
//! side; the spin after the spawn sweeps the window.

use std::time::Duration;

use leviculum_core::{Destination, DestinationHash, DestinationType, Direction, Identity};
use leviculum_std::config::InterfaceConfig;
use leviculum_std::driver::ReticulumNodeBuilder;

use crate::harness::TestDaemon;

/// Rounds, and the spin of round `r` is `r` microseconds. With Branch 5's
/// gate disabled, 15 of these 24 rounds left the delivery destination
/// unknown to Python (1656's negative control).
const ROUNDS: u32 = 24;

/// How long Python has to learn both destinations from the spawn. Learning
/// takes about 0.8 s (the delivery announce is paced 600 ms behind home on
/// the fresh interface, order 1320); a held announce is never learned
/// before the node's next one, so the bound separates the two with room for
/// a loaded host.
const LEARN_BOUND: Duration = Duration::from_secs(3);

fn destination(aspect: &str) -> Destination {
    Destination::new(
        Some(Identity::generate(&mut rand_core::OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvr",
        &["1656", aspect],
    )
    .expect("create destination")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_python_peer_dialled_during_the_announces_learns_the_mailbox() {
    let daemon = TestDaemon::start().await.expect("start daemon");
    let addr = daemon.rns_addr();
    let mut unlearned = Vec::new();
    for round in 0..ROUNDS {
        let storage = tempfile::tempdir().expect("tempdir");
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(false)
            .storage_path(storage.path().to_path_buf())
            .build()
            .await
            .expect("build node");
        node.start().await.expect("start node");
        let (home, delivery) = (destination("home"), destination("delivery"));
        let hashes: [DestinationHash; 2] = [*home.hash(), *delivery.hash()];
        node.register_destination(home);
        node.register_destination(delivery);

        let start = tokio::time::Instant::now();
        node.spawn_interface(InterfaceConfig {
            name: "Rendezvous".to_string(),
            interface_type: "TCPClientInterface".to_string(),
            enabled: true,
            target_host: Some(addr.ip().to_string()),
            target_port: Some(addr.port()),
            ..Default::default()
        })
        .expect("spawn the client");
        let spin = std::time::Instant::now() + Duration::from_micros(u64::from(round));
        while std::time::Instant::now() < spin {
            std::hint::spin_loop();
        }
        for hash in &hashes {
            node.announce_destination(hash, None)
                .await
                .expect("announce");
        }

        let mut missing = hashes.to_vec();
        while !missing.is_empty() && start.elapsed() < LEARN_BOUND {
            let mut still = Vec::new();
            for hash in missing {
                if !daemon.has_path(hash.as_bytes()).await {
                    still.push(hash);
                }
            }
            missing = still;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        node.stop().await.ok();
        if !missing.is_empty() {
            unlearned.push((
                round,
                missing
                    .iter()
                    .map(|h| hex::encode(&h.as_bytes()[..4]))
                    .collect::<Vec<_>>(),
            ));
        }
    }
    assert!(
        unlearned.is_empty(),
        "rounds (spin µs, destinations Python never learned within {LEARN_BOUND:?}; \
         the second is the delivery one): {unlearned:?}"
    );
}
