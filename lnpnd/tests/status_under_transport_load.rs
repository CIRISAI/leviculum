//! `lnpnd --status` against a *loaded* shared instance (Codeberg #427).
//!
//! `status_default_budget.rs` reproduces the 2026-09-20 field shape — a
//! fresh client, a daemon that announced before the client existed — and
//! answers at the shipped budget on an **idle** instance. The 2026-09-25
//! field (miauhaus) fails 7 of 8 times at 5, 20 and 60 s alike on an
//! instance that differs in exactly one variable: load. Its transport
//! node holds ~24 700 paths, announces keep arriving, and lnpnd's engine
//! logs `CORE_PROCESSOR_OVER_BUDGET hook=on_tick elapsed_us=56000..152000`.
//!
//! This test adds that variable and it reproduced the field red on first
//! measurement (2026-09-25, `STATUS_UNDER_LOAD code=200 elapsed_ms=5066`),
//! which root-caused two defects on one causal chain, both divergences
//! from the reference:
//!
//! 1. A local client's path request for a destination absent from the
//!    instance's path table was forwarded to network interfaces only —
//!    never to the other local clients, one of whom (the lnpnd daemon)
//!    hosts the destination. The reference forwards it on every other
//!    interface (`elif is_from_local_client`, Transport.py:3006-3013).
//!    Fixed in `transport.rs::handle_path_request` case 4, pinned by
//!    `test_path_request_from_local_client_forwarded_to_other_local_clients`.
//! 2. The client's own uplink interface ran the announce ingress burst
//!    limiter (the reference's `LocalClientInterface.should_ingress_limit`
//!    returns False, LocalInterface.py:137-138), so a client born into a
//!    busy instance HELD the very path-response announce it had asked
//!    for. Fixed in `interfaces/local.rs::spawn_local_client`.
//!
//! The load here: the instance's path table driven past its ceiling
//! (6c9735fe2 gave every transport table one; the ceiling is set low
//! enough to be reachable in a test's budget), which evicts the control
//! destination's entry, then a sustained announce storm — the field's
//! "a few thousand PATH_ADDs per minute" — while the *shipped* client
//! flow runs at the *shipped* budget: `lnpnd::client::run`, fresh storage
//! per invocation as `main.rs` mints it, `DEFAULT_STATUS_TIMEOUT`. The
//! fill backlog is drained before the query so the storm, not a
//! test-artifact burst queue, is what the query runs against.
//!
//! Without either fix this is red at every budget: the request
//! black-holes (1), or its answer is held past any budget for as long as
//! the storm keeps the burst limiter armed (2).

use std::time::Duration;

use leviculum_core::{Destination, DestinationHash, DestinationType, Direction, Identity};
use leviculum_lxmf::control::CONTROL_ASPECTS;
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::{MemoryPeerStore, MemoryPropagationStore, PropagationNodeConfig};
use leviculum_std::config::Config;
use leviculum_std::driver::ReticulumNodeBuilder;
use lnpnd::client::{ClientAction, ClientOptions, DEFAULT_STATUS_TIMEOUT};
use lnpnd::engine::{Engine, EngineConfig, EngineEvent};

type EngineEvents = std::sync::mpsc::Receiver<EngineEvent>;

/// The instance's path-table ceiling for this test. Low enough that the
/// feeder can fill it inside the test budget, high enough that the tick's
/// per-entry work resembles a real table rather than a toy.
const PATH_TABLE_CAP: usize = 4096;

/// Distinct destinations the feeder announces before the query: past the
/// ceiling, so the table is full, evicting, and the daemon's control
/// entry is gone when the client arrives.
const FILL_ANNOUNCES: usize = PATH_TABLE_CAP + 1000;

/// Fresh destinations announced per second while the query runs — the
/// field's "a few thousand PATH_ADD per minute", sustained.
const STORM_PER_SEC: u64 = 50;

/// lnpnd's production engine on a stock configuration, as in
/// `status_default_budget.rs`.
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

/// One announceable destination with its own identity, registered on the
/// feeder. The aspect varies so the destination hashes differ too.
fn feeder_destination(node: &leviculum_std::ReticulumNode, index: usize) -> DestinationHash {
    let identity = Identity::generate(&mut rand_core::OsRng);
    let aspect = format!("load{index}");
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "pathload",
        &[&aspect],
    )
    .expect("destination");
    let hash = *dest.hash();
    node.register_destination(dest);
    hash
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_at_the_shipped_budget_on_a_loaded_instance() {
    let instance = format!("lnpnd_load_{}", std::process::id());

    // The instance: transport on, path table capped where the test can
    // reach it (`path_table_cap` is the same knob a config file sets).
    let mut config = Config::default();
    config.reticulum.path_table_cap = Some(PATH_TABLE_CAP);
    let instance_storage = tempfile::tempdir().expect("tempdir instance");
    let mut shared = ReticulumNodeBuilder::new()
        .identity(leviculum_std::generate_identity())
        .config(config)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .storage_path(instance_storage.path().to_path_buf())
        .build()
        .await
        .expect("build the shared instance");
    shared.start().await.expect("start the shared instance");

    // lnpnd, attached exactly as `main.rs` attaches it.
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

    // The daemon's announce reaches the instance before any load exists,
    // as in the field: the node had been announcing normally all along.
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
        "the daemon never announced its control destination — the test \
         would then measure a different absence than #427's"
    );

    // The feeder: a third local client that fills the instance's path
    // table past its ceiling.
    let feeder_storage = tempfile::tempdir().expect("tempdir feeder");
    let mut feeder = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&instance)
        .storage_path(feeder_storage.path().to_path_buf())
        .build()
        .await
        .expect("build the feeder");
    feeder.start().await.expect("start the feeder");

    let fill_started = std::time::Instant::now();
    for index in 0..FILL_ANNOUNCES {
        let hash = feeder_destination(&feeder, index);
        feeder
            .announce_destination(&hash, None)
            .await
            .expect("announce");
    }
    // The announces are queued through channels on both sides; the table
    // is loaded when the instance's count says so, not when the loop ends.
    let filled = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if shared.path_count() >= PATH_TABLE_CAP {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    eprintln!(
        "PATH_FILL announced={FILL_ANNOUNCES} table={} cap={PATH_TABLE_CAP} elapsed_ms={}",
        shared.path_count(),
        fill_started.elapsed().as_millis()
    );
    assert!(
        filled.is_ok(),
        "the instance never reached its path-table ceiling ({} of \
         {PATH_TABLE_CAP} after {FILL_ANNOUNCES} announces) — the load \
         this test exists to apply was not applied",
        shared.path_count()
    );
    assert!(
        !shared.has_path(&control_hash),
        "the fill was meant to evict the control destination's path entry \
         from the instance's capped table; with it still present the query \
         is answered from the cache and the recovery path is not exercised"
    );

    // Drain the fill burst before the query: the instance fans every
    // announce out to its local clients, and the daemon verifies each, so
    // a query launched into that backlog measures the test's own burst
    // instead of the sustained load. Field announce arrival is a rate,
    // not a one-shot 5000-announce burst; the plateau of the daemon's own
    // path count marks the backlog processed.
    let drained = tokio::time::timeout(Duration::from_secs(60), async {
        let mut last = daemon.path_count();
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let now = daemon.path_count();
            if now == last {
                return;
            }
            last = now;
        }
    })
    .await;
    assert!(
        drained.is_ok(),
        "the daemon never finished the fill backlog"
    );

    // Sustained churn while the query runs: fresh destinations at
    // `STORM_PER_SEC`, each one a PATH_ADD plus an eviction at the cap —
    // and, fanned out to the clients, exactly the announce pressure that
    // used to keep the client's burst limiter armed (defect 2 above).
    let storm = tokio::spawn(async move {
        let mut index = FILL_ANNOUNCES;
        loop {
            let hash = feeder_destination(&feeder, index);
            index += 1;
            if feeder.announce_destination(&hash, None).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1000 / STORM_PER_SEC)).await;
        }
    });

    // The shipped client at the shipped budget: `client::run` is the code
    // `main.rs` runs, fresh per-invocation storage and all.
    let client_storage = tempfile::tempdir().expect("tempdir client");
    let query_started = std::time::Instant::now();
    let code = lnpnd::client::run(
        ClientOptions {
            instance: instance.clone(),
            storage_dir: client_storage.path().join("state"),
            identity: node_identity,
            remote: None,
            timeout: DEFAULT_STATUS_TIMEOUT,
        },
        ClientAction::Status {
            show_status: true,
            show_peers: false,
        },
    )
    .await;
    let query_elapsed = query_started.elapsed();

    storm.abort();
    eprintln!(
        "STATUS_UNDER_LOAD code={code} elapsed_ms={} table={} cap={PATH_TABLE_CAP}",
        query_elapsed.as_millis(),
        shared.path_count()
    );

    assert_eq!(
        code, 0,
        "the shipped --status budget of {DEFAULT_STATUS_TIMEOUT:?} did not \
         survive a path table at its ceiling with announces still arriving \
         — the Codeberg #427 field shape (elapsed {query_elapsed:?})"
    );

    let _ = daemon.stop().await;
    let _ = shared.stop().await;
}
