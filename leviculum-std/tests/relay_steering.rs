//! leviculum#91 (+ciris): links steered through a chosen relay, over real
//! TCP.
//!
//! `A` and `B` are leaves, each a TCP client of two transport nodes `C1` and
//! `C2`. `B` announces; `A` hears it through both relays and lists both.
//! `connect_via` opens one link through each, both carry data at once, and
//! each link reports its relay. A relay `B` was never heard through is
//! refused up front.

use std::net::SocketAddr;
use std::time::Duration;

use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::{EventReceiver, NodeEvent};

#[path = "support/port_alloc.rs"]
#[allow(dead_code)]
mod port_alloc;

#[path = "support/ports.rs"]
#[allow(dead_code)]
mod ports;

struct TestNode {
    node: ReticulumNode,
    rx: EventReceiver,
}

async fn start(builder: ReticulumNodeBuilder) -> TestNode {
    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = builder
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    std::mem::forget(storage);
    node.start().await.expect("start node");
    let rx = node.take_event_receiver().expect("event rx");
    TestNode { node, rx }
}

async fn relay(port: u16) -> TestNode {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    start(
        ReticulumNodeBuilder::new()
            .enable_transport(true)
            .add_tcp_server(addr),
    )
    .await
}

async fn leaf(ports: [u16; 2]) -> TestNode {
    let mut builder = ReticulumNodeBuilder::new().enable_transport(false);
    for port in ports {
        builder = builder.add_tcp_client(format!("127.0.0.1:{port}").parse().unwrap());
    }
    start(builder).await
}

async fn next_event(
    rx: &mut EventReceiver,
    window: Duration,
    mut pred: impl FnMut(&NodeEvent) -> bool,
) -> Option<NodeEvent> {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(ev)) if pred(&ev) => return Some(ev),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_links_to_one_destination_each_through_its_own_relay() {
    let ports = [ports::free_port(), ports::free_port()];
    let _c1 = relay(ports[0]).await;
    let _c2 = relay(ports[1]).await;
    let mut a = leaf(ports).await;
    let mut b = leaf(ports).await;
    tokio::time::sleep(Duration::from_millis(800)).await;

    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "relaysteer",
        &["e2e"],
    )
    .expect("destination");
    dest.set_accepts_links(true);
    let hash = *dest.hash();
    b.node.register_destination(dest);
    b.node
        .announce_destination(&hash, None)
        .await
        .expect("announce");

    // A hears B through both relays.
    let mut relays = Vec::new();
    while relays.len() < 2 {
        let ev = next_event(&mut a.rx, Duration::from_secs(15), |ev| {
            matches!(ev, NodeEvent::RelayRouteLearned { destination_hash, .. }
                if *destination_hash == hash)
        })
        .await
        .expect("a relay row for each relay");
        if let NodeEvent::RelayRouteLearned { relay, .. } = ev {
            if !relays.contains(&relay) {
                relays.push(relay);
            }
        }
    }
    let routes = a.node.relay_routes(&hash);
    assert_eq!(routes.len(), 2, "{routes:?}");
    assert_ne!(routes[0].interface_index, routes[1].interface_index);

    // Refused up front through a relay B was never heard through.
    assert!(matches!(
        a.node
            .connect_via(&hash, &signing_key, [0xEE; 16], None)
            .await,
        Err(leviculum_std::Error::NoRouteViaRelay)
    ));

    let mut handles = Vec::new();
    for relay in &relays {
        let handle = a
            .node
            .connect_via(&hash, &signing_key, *relay, Some(Duration::from_secs(8)))
            .await
            .expect("connect_via");
        a.node
            .await_link_established(handle.link_id())
            .await
            .expect("link through the chosen relay");
        handles.push(handle);
    }

    let list = a.node.link_list();
    for (handle, relay) in handles.iter().zip(&relays) {
        let info = list
            .iter()
            .find(|l| l.link_id == *handle.link_id())
            .expect("listed");
        assert_eq!(info.via_relay, Some(*relay), "each link reports its relay");
    }

    // Both carry data at once.
    for (i, handle) in handles.iter().enumerate() {
        handle
            .try_send(format!("via {i}").as_bytes())
            .await
            .expect("send");
    }
    let mut got = Vec::new();
    while got.len() < 2 {
        let ev = next_event(&mut b.rx, Duration::from_secs(10), |ev| {
            matches!(ev, NodeEvent::MessageReceived { .. })
        })
        .await
        .unwrap_or_else(|| {
            panic!(
                "both links deliver: got {got:?}; B links {:?}; A links {:?}",
                b.node.link_list(),
                a.node.link_list()
            )
        });
        if let NodeEvent::MessageReceived { data, .. } = ev {
            got.push(data);
        }
    }
    got.sort();
    assert_eq!(got, [b"via 0".to_vec(), b"via 1".to_vec()]);
}

/// leviculum#94 (+ciris): a timed frame crosses a relay over real TCP and
/// arrives as an ordinary message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_timed_frame_crosses_a_relay_as_a_plain_message() {
    let port = ports::free_port();
    let _c = relay(port).await;
    let mut a = leaf([port, port]).await;
    let mut b = leaf([port, port]).await;
    tokio::time::sleep(Duration::from_millis(800)).await;

    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "relaysteer",
        &["timed"],
    )
    .expect("destination");
    dest.set_accepts_links(true);
    let hash = *dest.hash();
    b.node.register_destination(dest);
    b.node
        .announce_destination(&hash, None)
        .await
        .expect("announce");
    next_event(&mut a.rx, Duration::from_secs(15), |ev| {
        matches!(ev, NodeEvent::AnnounceReceived { .. })
    })
    .await
    .expect("A hears B");
    let handle = a.node.connect(&hash, &signing_key).await.expect("connect");
    a.node
        .await_link_established(handle.link_id())
        .await
        .expect("link");
    handle
        .try_send_timed(b"live", Duration::from_millis(500))
        .await
        .expect("timed send");
    let ev = next_event(&mut b.rx, Duration::from_secs(10), |ev| {
        matches!(ev, NodeEvent::MessageReceived { .. })
    })
    .await
    .expect("delivered");
    assert!(matches!(ev, NodeEvent::MessageReceived { data, msgtype: 0, .. } if data == b"live"));
}
