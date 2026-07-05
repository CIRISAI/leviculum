//! End-to-end integration test for runtime auto-connect of discovered
//! interfaces (Codeberg #32, sub-task b).
//!
//! Proves the whole discovery -> auto-connect -> traffic chain with two of our
//! own nodes over TCP loopback:
//!
//!   * Node B is discoverable: it runs a TCP server on its "backbone" port and
//!     emits a discovery announce (32a wire format) advertising that endpoint.
//!   * Node A has auto-connect enabled and reaches B over a separate bootstrap
//!     link. When A hears B's discovery announce it persists the record and,
//!     at runtime, spawns a TCP client to B's advertised host:port and
//!     registers it with the transport.
//!   * Traffic then crosses the auto-established link: B re-announces over the
//!     accepted connection, so A's auto-connected interface receives bytes.
//!
//! The bootstrap link (A -> B:bootstrap_port) exists only so A can *hear* B's
//! discovery announce; the endpoint A auto-connects to is B's *second*,
//! separately-advertised server port, so the auto-connected interface is a
//! genuinely new link, distinct from the bootstrap one.

use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use leviculum_core::discovery::{build_announce_app_data, InterfaceDescriptor};
use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::ReticulumNodeBuilder;

static PORT_COUNTER: AtomicU16 = AtomicU16::new(53100);

fn next_port() -> u16 {
    loop {
        let candidate = PORT_COUNTER.fetch_add(1, Ordering::Relaxed);
        if candidate >= 54000 {
            PORT_COUNTER.store(53100, Ordering::Relaxed);
            continue;
        }
        if StdTcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return candidate;
        }
    }
}

/// Poll `cond` every 100 ms until it returns true or the deadline passes.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discovered_backbone_endpoint_is_auto_connected_and_carries_traffic() {
    let bootstrap_port = next_port();
    let backbone_port = next_port();
    let bootstrap_addr: SocketAddr = format!("127.0.0.1:{bootstrap_port}").parse().unwrap();
    let backbone_addr: SocketAddr = format!("127.0.0.1:{backbone_port}").parse().unwrap();

    // Node B: discoverable server. Bootstrap port carries the discovery
    // announce to A; the backbone port is the endpoint A will auto-connect to.
    let b_storage = tempfile::tempdir().expect("tempdir b");
    let mut node_b = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server(bootstrap_addr)
        .add_tcp_server(backbone_addr)
        .storage_path(b_storage.path().to_path_buf())
        .build()
        .await
        .expect("build b");
    node_b.start().await.expect("start b");

    // Node A: auto-connect enabled, bootstrap client to B.
    let a_storage = tempfile::tempdir().expect("tempdir a");
    let mut node_a = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .autoconnect_discovered_interfaces(4)
        .add_tcp_client(bootstrap_addr)
        .storage_path(a_storage.path().to_path_buf())
        .build()
        .await
        .expect("build a");
    node_a.start().await.expect("start a");

    // Let the bootstrap link peer before B announces itself.
    let bootstrap_up = wait_until(Duration::from_secs(10), || {
        node_a
            .interface_stats()
            .iter()
            .any(|i| i.online && !i.is_local_client)
    })
    .await;
    assert!(bootstrap_up, "bootstrap A->B link did not come online");

    // B emits a discovery announce advertising its backbone endpoint. The
    // descriptor carries reachable_on/port; the announce app_data is the 32a
    // wire format (PoW-stamped msgpack). `transport = true` so it passes the
    // only-transport auto-connect filter.
    let disco_identity = Identity::generate(&mut rand_core::OsRng);
    let disco_dest = Destination::new(
        Some(disco_identity),
        Direction::In,
        DestinationType::Single,
        "rnstransport",
        &["discovery", "interface"],
    )
    .expect("discovery destination");
    let disco_hash = *disco_dest.hash();
    node_b.register_destination(disco_dest);

    let descriptor = InterfaceDescriptor {
        interface_type: "BackboneInterface".to_string(),
        name: Some("B Backbone".to_string()),
        reachable_on: Some("127.0.0.1".to_string()),
        port: Some(backbone_port as u64),
        ..Default::default()
    };
    let transport_id = [0xB0u8; 16];
    let app_data = build_announce_app_data(&descriptor, &transport_id, true, &mut rand_core::OsRng)
        .expect("build discovery announce app_data");

    node_b
        .announce_destination(&disco_hash, Some(&app_data))
        .await
        .expect("announce discovery record");

    // A should persist the record, spawn a TCP client to B's backbone port,
    // and register it. The auto-connected interface is named `autoconnect/*`.
    let auto_connected = wait_until(Duration::from_secs(20), || {
        node_a
            .interface_stats()
            .iter()
            .any(|i| i.name.starts_with("autoconnect/"))
    })
    .await;
    assert!(
        auto_connected,
        "A did not auto-connect a discovered interface; interfaces = {:?}",
        node_a
            .interface_stats()
            .iter()
            .map(|i| i.name.clone())
            .collect::<Vec<_>>()
    );

    // Traffic crosses the auto-established link: B re-announces so the accepted
    // connection carries protocol bytes back to A's auto-connected interface.
    node_b
        .announce_destination(&disco_hash, Some(&app_data))
        .await
        .expect("re-announce over auto link");

    let carried_traffic = wait_until(Duration::from_secs(20), || {
        node_a
            .interface_stats()
            .iter()
            .any(|i| i.name.starts_with("autoconnect/") && i.online && i.rx_bytes > 0)
    })
    .await;
    assert!(
        carried_traffic,
        "auto-connected interface carried no traffic; interfaces = {:?}",
        node_a.interface_stats()
    );

    let _ = node_a.stop().await;
    let _ = node_b.stop().await;
}
