//! Who may ask a propagation node how it is, and what a refusal sounds
//! like.
//!
//! The field report that opened this: on a freshly installed, running,
//! healthy node, `lnpnd --status --config /etc/lnpnd` printed
//!
//!     Getting lnpnd statistics timed out, exiting now
//!
//! Two questions fall out of that, and they are separable.
//!
//! **May the installer ask at all?** The reference answers yes without any
//! configuration: `reference/LXMF/LXMF/LXMRouter.py:672` seeds the list as
//! `self.control_allowed_list = [self.identity.hash]`, so a stock `lxmd
//! --status` on its own host works with an empty `control_allowed`, and
//! `control_allowed` (`lxmd.py:219-222`) only ever *adds* remotes. We do
//! the same (`engine.rs`, `register_if_needed`), and the first test here
//! pins it end-to-end rather than by reading the vector.
//!
//! **What does a refusal sound like?** That was the real defect. The
//! reference registers the three control paths behind
//! `RNS.Destination.ALLOW_LIST`, which drops a disallowed request without
//! a word (`reference/Reticulum/RNS/Link.py:867-874`), so its own
//! `ERROR_NO_ACCESS` branch (`LXMRouter.py:840`) is unreachable and every
//! refusal reaches the operator as a timeout. A timeout says *nobody
//! answered* and sends the reader to the mesh, the instance and the
//! interfaces; the truth is *you are not allowed to ask*. We answer the
//! refusal instead — the reference's own error value, which the
//! reference's own client already decodes (`lxmd.py` exit 204).
//!
//! Both run against lnpnd's production engine over a real link, because
//! the defect lives in the seam between the core's request policy and the
//! engine's handler, and no unit test spans that seam.

use std::net::SocketAddr;
use std::time::Duration;

use leviculum_core::{Destination, DestinationHash, Identity};
use leviculum_lxmf::control::{ControlResponse, CONTROL_ASPECTS, STATS_GET_PATH};
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::{
    MemoryPeerStore, MemoryPropagationStore, PeerError, PropagationNodeConfig, PROPAGATION_ASPECT,
};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::ReticulumNode;
use lnpnd::engine::{Engine, EngineConfig, EngineEvent};

/// The engine's event channel. Nothing here reads it, but it has to be
/// held: a dropped receiver turns every engine event into a send error.
type EngineEvents = std::sync::mpsc::Receiver<EngineEvent>;

/// A port nothing is listening on right now.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind an ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

/// lnpnd's production engine, configured the way an ordinary
/// installation is: `control_allowed` empty, because the packaged config
/// is written from `--exampleconfig`, which leaves the key commented out
/// exactly as `lxmd --exampleconfig` does.
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

/// The daemon over TCP: one node, one engine, a listener a client dials.
async fn daemon_node(
    identity: Identity,
    addr: SocketAddr,
    dir: &std::path::Path,
) -> (ReticulumNode, EngineEvents) {
    let (engine, events) = production_engine(identity);
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .storage_path(dir.to_path_buf())
        .add_tcp_server(addr)
        .core_processor(engine)
        .build()
        .await
        .expect("build the daemon node");
    node.start().await.expect("start the daemon node");
    // The caller holds the receiver: a dropped one makes every engine
    // event a send error, which is not what this test wants to measure.
    (node, events)
}

async fn client_node(addr: SocketAddr, dir: &std::path::Path) -> ReticulumNode {
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .storage_path(dir.to_path_buf())
        .add_tcp_client(addr)
        .build()
        .await
        .expect("build the client node");
    node.start().await.expect("start the client node");
    node
}

/// The exact flow `lnpnd --status` runs (`client.rs`, and `lxmd.py:649-680`
/// before it): derive `lxmf.propagation.control` from the node's identity,
/// link, identify, request. `None` is the client's timeout — the silence
/// that was being reported as "timed out".
async fn control_request(
    node: &ReticulumNode,
    identify_as: &Identity,
    node_identity: &Identity,
    timeout: Duration,
) -> Option<Vec<u8>> {
    let name_hash = Destination::compute_name_hash(APP_NAME, &CONTROL_ASPECTS);
    let control_hash: DestinationHash =
        Destination::compute_destination_hash(&name_hash, node_identity.hash());
    assert!(
        node.wait_for_path(
            &control_hash,
            Duration::from_secs(30),
            Duration::from_secs(2)
        )
        .await
        .unwrap_or(false),
        "no path to the control destination — its announce never arrived"
    );
    let signing_key = node_identity.ed25519_verifying().to_bytes();
    let (link, established) = node
        .connect_awaited(&control_hash, &signing_key)
        .await
        .expect("link setup");
    tokio::time::timeout(Duration::from_secs(30), established)
        .await
        .expect("link establishes")
        .expect("link establishes cleanly");
    node.identify_link(link.link_id(), identify_as)
        .await
        .expect("identify");
    let (_, response) = node
        .send_request_awaited(
            link.link_id(),
            STATS_GET_PATH,
            None,
            Some(timeout.as_millis() as u64),
        )
        .await
        .expect("request sent");
    let outcome = tokio::time::timeout(timeout, response).await;
    let _ = node.close_link(link.link_id()).await;
    match outcome {
        Ok(Ok(info)) => Some(info.response_data),
        _ => None,
    }
}

/// The installer's own query, on a config that says nothing about
/// `control_allowed`, and a stranger's query on the same node.
///
/// One test for both because they share a node: the second assertion is
/// only worth anything if the first one's permission was not widened into
/// it.
#[tokio::test]
async fn the_nodes_own_identity_is_served_and_a_stranger_is_told_no() {
    let port = free_port();
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let node_identity = leviculum_std::generate_identity();

    let daemon_dir = tempfile::tempdir().expect("tempdir daemon");
    let (mut daemon, _events) = daemon_node(node_identity.clone(), addr, daemon_dir.path()).await;

    let client_dir = tempfile::tempdir().expect("tempdir client");
    let mut client = client_node(addr, client_dir.path()).await;

    // `lnpnd --status` on the host: the identity at /etc/lnpnd/identity is
    // the daemon's own, and the reference serves it with no configuration
    // (`LXMRouter.py:672`).
    let response = control_request(
        &client,
        &node_identity,
        &node_identity,
        Duration::from_secs(30),
    )
    .await
    .expect("the node's own identity must be served without configuration");
    match ControlResponse::decode(&response).expect("decodable") {
        ControlResponse::Stats(stats) => {
            assert_eq!(
                stats.destination_hash,
                Destination::compute_destination_hash(
                    &Destination::compute_name_hash(APP_NAME, &[PROPAGATION_ASPECT]),
                    node_identity.hash(),
                )
                .as_bytes()
                .to_owned(),
                "the stats must describe this node"
            );
        }
        other => panic!("expected the stats map, got {other:?}"),
    }

    // An identity nobody allowed. It must still be refused — and it must
    // be TOLD it is refused, because "no answer" is the one diagnosis that
    // is not true here.
    let stranger = leviculum_std::generate_identity();
    let response = control_request(&client, &stranger, &node_identity, Duration::from_secs(10))
        .await
        .expect("a refusal must be answered, not sat out as a timeout");
    assert_eq!(
        ControlResponse::decode(&response).expect("decodable"),
        ControlResponse::Error(PeerError::NoAccess),
        "an identity outside control_allowed must be refused in words"
    );

    let _ = client.stop().await;
    let _ = daemon.stop().await;
}

/// The command the package itself prints after installation, in the
/// topology the package itself creates.
///
/// `packaging/lnpnd/postinst` tells the operator to run
///
///     sudo -u lnpnd lnpnd --status --config /etc/lnpnd
///
/// against a daemon attached to a shared instance, and that is the
/// command that came back "timed out" in the field. The TCP test above
/// isolates the permission decision; this one spans what the field
/// spanned — an `lnsd` stand-in with two attached programs, lnpnd and the
/// query client, talking over the real abstract-unix IPC — because an
/// isolated green says nothing about whether the announce, the path and
/// the link survive the extra hop through the instance.
#[tokio::test]
async fn the_installed_command_reaches_the_daemon_through_a_shared_instance() {
    let instance = format!("lnpnd_ctl_{}", std::process::id());

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

    // `lnpnd --status`, attached exactly as `client.rs` attaches it, and
    // identifying with /etc/lnpnd/identity — the node's own.
    let client_storage = tempfile::tempdir().expect("tempdir client");
    let mut client = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&instance)
        .storage_path(client_storage.path().to_path_buf())
        .build()
        .await
        .expect("build the client");
    client.start().await.expect("start the client");

    let response = control_request(
        &client,
        &node_identity,
        &node_identity,
        Duration::from_secs(30),
    )
    .await
    .expect("the installed --status command must be answered");
    assert!(
        matches!(
            ControlResponse::decode(&response).expect("decodable"),
            ControlResponse::Stats(_)
        ),
        "the installed --status command must get the stats map"
    );

    let _ = client.stop().await;
    let _ = daemon.stop().await;
    let _ = shared.stop().await;
}
