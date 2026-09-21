//! `lnpnd --status` at the budget it actually ships with.
//!
//! The field symptom from 2026-09-20 14:51: a healthy, announcing node,
//! and the client still printed
//!
//!     Getting lnpnd statistics timed out, exiting now
//!
//! The existing `control_permission.rs` tests could not see it, because
//! they hand themselves a 30 s path budget with a 2 s retry. The shipped
//! client has 5 s with a 5 s retry, and at that pairing
//! `wait_for_path`'s deadline check fires on the same iteration the first
//! PATH_REQUEST would have — so the request never left the host. The
//! client cannot fall back on a warm table either: `main.rs` mints a
//! fresh storage directory per invocation, so every run starts with an
//! empty path table.
//!
//! This test therefore reproduces the field shape rather than the
//! laboratory one: the daemon announces *before* the client exists, so
//! the client has to ask. Every budget here comes from the shipped
//! constants — a test that re-types `5` proves nothing about the binary
//! an operator runs.

use std::time::Duration;

use leviculum_core::{Destination, DestinationHash, Identity};
use leviculum_lxmf::control::CONTROL_ASPECTS;
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::{MemoryPeerStore, MemoryPropagationStore, PropagationNodeConfig};
use leviculum_std::driver::ReticulumNodeBuilder;
use lnpnd::client::{resolve_control_path, DEFAULT_STATUS_TIMEOUT};
use lnpnd::engine::{Engine, EngineConfig, EngineEvent};

type EngineEvents = std::sync::mpsc::Receiver<EngineEvent>;

/// lnpnd's production engine on a stock configuration.
fn production_engine(identity: Identity) -> (Engine<MemoryPropagationStore>, EngineEvents) {
    Engine::new(EngineConfig {
        identity,
        node_config: PropagationNodeConfig::default(),
        store: MemoryPropagationStore::new(64_000),
        announce_interval_secs: 3600,
        announce_delay_secs: 0,
        peering: leviculum_lxmf::PeeringConfig::default(),
        peer_store: Box::new(MemoryPeerStore::default()),
        control_allowed: Vec::new(),
        auth_allowed: None,
        mailbox: None,
        store_limit_bytes: 64_000,
        delivery_limit_kb: 1000,
    })
}

/// A client with the shipped default budget must resolve the control
/// destination of a node that announced normally — before the client was
/// started, which is the ordinary case: the daemon has been up for days
/// and the operator types the command now.
#[tokio::test]
async fn status_resolves_the_control_path_at_the_shipped_default_budget() {
    let instance = format!("lnpnd_budget_{}", std::process::id());

    let instance_storage = tempfile::tempdir().expect("tempdir instance");
    let mut shared = ReticulumNodeBuilder::new()
        .identity(leviculum_std::generate_identity())
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .storage_path(instance_storage.path().to_path_buf())
        .build()
        .await
        .expect("build the shared instance");
    shared.start().await.expect("start the shared instance");

    let node_identity = leviculum_std::generate_identity();
    let (engine, _events) = production_engine(node_identity.clone());
    let daemon_storage = tempfile::tempdir().expect("tempdir daemon");
    let mut daemon = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&instance)
        .storage_path(daemon_storage.path().to_path_buf())
        .core_processor(engine)
        .build()
        .await
        .expect("build lnpnd");
    daemon.start().await.expect("start lnpnd");

    let name_hash = Destination::compute_name_hash(APP_NAME, &CONTROL_ASPECTS);
    let control_hash: DestinationHash =
        Destination::compute_destination_hash(&name_hash, node_identity.hash());

    // The daemon's announce reaches the instance and stops there. Wait for
    // it explicitly rather than sleeping a guessed interval: the point of
    // the test is that the *client* missed it, not that nobody sent it.
    let announced = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if shared.has_path(&control_hash) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(
        announced.is_ok(),
        "the daemon never announced its control destination — this test \
         would then prove nothing about the client"
    );

    // Now the operator types the command. A fresh storage directory, as
    // `main.rs` mints one per invocation, so the path table is empty.
    let client_storage = tempfile::tempdir().expect("tempdir client");
    let mut client = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&instance)
        .storage_path(client_storage.path().to_path_buf())
        .build()
        .await
        .expect("build the client");
    client.start().await.expect("start the client");
    assert!(
        !client.has_path(&control_hash),
        "the client started with a path already installed — it never had \
         to ask, so this test is not exercising the path request"
    );

    let started = std::time::Instant::now();
    let resolved = resolve_control_path(&client, &control_hash, DEFAULT_STATUS_TIMEOUT).await;
    let elapsed = started.elapsed();
    eprintln!(
        "PATH_RESOLVE resolved={resolved} elapsed_ms={}",
        elapsed.as_millis()
    );
    assert!(
        resolved,
        "the shipped --status budget of {:?} did not resolve the control \
         path in {elapsed:?}",
        DEFAULT_STATUS_TIMEOUT
    );

    let _ = client.stop().await;
    let _ = daemon.stop().await;
    let _ = shared.stop().await;
}
