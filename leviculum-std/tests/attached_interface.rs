//! +ciris: interfaces a host drives (`attach_interface`) and HostInterface
//! (`attach_host_interface`) between two nodes over in-memory carriers.
//!
//! A pair with message framing and a pair with stream framing (short writes,
//! would-block) each carry an announce, a link and data both ways; an IFAC'd
//! pair carries an announce and a mismatched one does not; attached
//! interfaces show in the stats, follow their up/down signals, and leave the
//! node when their guard or handle is dropped.

use std::time::Duration;

use leviculum_core::ifac::IfacConfig;
use leviculum_core::traits::InterfaceKind;
use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::{
    ExternalInterfaceConfig, HostInterfaceState, ReticulumNode, ReticulumNodeBuilder,
};
use leviculum_std::interfaces::host::{
    CarrierKind, CloseReason, Framing, HostInterfaceConfig, HostInterfaceEnd,
};
use leviculum_std::test_support::mem_carrier::{mem_carrier_pair, MemCarrierConfig};
use leviculum_std::{EventReceiver, NodeEvent};

struct TestNode {
    node: ReticulumNode,
    rx: EventReceiver,
    _storage: tempfile::TempDir,
}

async fn start() -> TestNode {
    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .share_instance(false)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    node.start().await.expect("start node");
    let rx = node.take_event_receiver().expect("event rx");
    TestNode {
        node,
        rx,
        _storage: storage,
    }
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

fn register_dest(
    node: &ReticulumNode,
    aspect: &str,
) -> ([u8; 32], leviculum_core::DestinationHash) {
    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "attachtest",
        &[aspect],
    )
    .expect("destination");
    dest.set_accepts_links(true);
    let hash = *dest.hash();
    node.register_destination(dest);
    (signing_key, hash)
}

async fn send(handle: &leviculum_std::LinkHandle, data: &[u8]) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match handle.try_send_seq(data).await {
            Ok(_) => return,
            Err(leviculum_std::Error::Send(_)) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("send: {e:?}"),
        }
    }
}

/// Announce until the other node hears it, or give up after `window`.
async fn announce_until_heard(
    from: &TestNode,
    hash: &leviculum_core::DestinationHash,
    rx: &mut EventReceiver,
    window: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + window;
    while tokio::time::Instant::now() < deadline {
        from.node
            .announce_destination(hash, None)
            .await
            .expect("announce");
        if next_event(rx, Duration::from_millis(700), |ev| {
            matches!(ev, NodeEvent::AnnounceReceived { announce, .. }
                if *announce.destination_hash() == *hash.as_bytes())
        })
        .await
        .is_some()
        {
            return true;
        }
    }
    false
}

/// Wait until `pred` holds for the node's interface stats.
async fn stats_until(
    node: &ReticulumNode,
    what: &str,
    mut pred: impl FnMut(&[leviculum_std::driver::InterfaceStatusSnapshot]) -> bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if pred(&node.interface_stats()) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: {:?}",
            node.interface_stats()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Two nodes joined by HostInterfaces over one in-memory pair: an announce
/// crosses, a link establishes, data flows both ways.
async fn round_trip(
    cfg: MemCarrierConfig,
) -> leviculum_std::test_support::mem_carrier::MemCarrierStats {
    let mut server = start().await;
    let mut client = start().await;
    let (a, b) = mem_carrier_pair(cfg);
    let observer = a.observer();
    let mut ha = server
        .node
        .attach_host_interface(a, HostInterfaceConfig::new("host_a"))
        .expect("attach a");
    let mut hb = client
        .node
        .attach_host_interface(b, HostInterfaceConfig::new("host_b"))
        .expect("attach b");
    assert!(matches!(ha.connected().await, HostInterfaceState::Up(_)));
    assert!(matches!(hb.connected().await, HostInterfaceState::Up(_)));

    let (signing_key, hash) = register_dest(&server.node, "pair");
    assert!(
        announce_until_heard(&server, &hash, &mut client.rx, Duration::from_secs(15)).await,
        "the client hears the server's announce"
    );

    let handle = client
        .node
        .connect(&hash, &signing_key)
        .await
        .expect("connect");
    client
        .node
        .await_link_established(handle.link_id())
        .await
        .expect("link establishes");
    let Some(NodeEvent::LinkEstablished { link_id, .. }) =
        next_event(&mut server.rx, Duration::from_secs(10), |ev| {
            matches!(
                ev,
                NodeEvent::LinkEstablished {
                    is_initiator: false,
                    ..
                }
            )
        })
        .await
    else {
        panic!("the server sees the link");
    };

    send(&handle, b"client to server").await;
    let ev = next_event(&mut server.rx, Duration::from_secs(10), |ev| {
        matches!(ev, NodeEvent::MessageReceived { .. })
    })
    .await
    .expect("the server receives");
    assert!(matches!(ev, NodeEvent::MessageReceived { data, .. } if data == b"client to server"));

    let back = server.node.link_handle(&link_id);
    send(&back, b"server to client").await;
    let ev = next_event(&mut client.rx, Duration::from_secs(10), |ev| {
        matches!(ev, NodeEvent::MessageReceived { .. })
    })
    .await
    .expect("the client receives");
    assert!(matches!(ev, NodeEvent::MessageReceived { data, .. } if data == b"server to client"));

    let stats = observer.stats();
    // Dropping one side's handle closes the carrier; the other side hears it
    // and its interface ends too.
    drop(ha);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), hb.ended())
            .await
            .expect("the far side ends"),
        HostInterfaceEnd::Closed(CloseReason::PeerClose)
    );
    stats_until(&client.node, "the far side detaches", |s| {
        !s.iter().any(|i| i.name == "host_b")
    })
    .await;
    stats_until(&server.node, "the dropped side detaches", |s| {
        !s.iter().any(|i| i.name == "host_a")
    })
    .await;

    client.node.stop().await.expect("client stop");
    server.node.stop().await.expect("server stop");
    stats
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_framed_pair_carries_an_announce_a_link_and_data_both_ways() {
    let mut cfg = MemCarrierConfig::new(CarrierKind::Ws, Framing::Message);
    cfg.max_message = Some(1200);
    round_trip(cfg).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_framed_pair_with_short_writes_carries_an_announce_a_link_and_data() {
    let mut cfg = MemCarrierConfig::new(CarrierKind::Tcp, Framing::Stream);
    cfg.capacity = 96;
    cfg.max_chunk = 13;
    let stats = round_trip(cfg).await;
    assert!(
        stats.short_writes > 0 && stats.send_would_blocks > 0,
        "the carrier made the interface handle partial sends: {stats:?}"
    );
}

/// The kind and the hardware MTU come from the carrier.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_interface_takes_its_kind_from_the_carrier() {
    let mut node = start().await;
    let (a, _b) = mem_carrier_pair(MemCarrierConfig::new(CarrierKind::Ws, Framing::Message));
    let mut h = node
        .node
        .attach_host_interface(a, HostInterfaceConfig::new("ws_host"))
        .expect("attach");
    let HostInterfaceState::Up(id) = h.connected().await else {
        panic!("connects");
    };
    assert_eq!(h.interface_id(), Some(id));
    stats_until(&node.node, "the interface shows", |s| {
        s.iter().any(|i| {
            i.interface_id == id && i.name == "ws_host" && i.kind == InterfaceKind::WebSocket
        })
    })
    .await;
    drop(h);
    stats_until(&node.node, "the handle's drop detaches it", |s| {
        !s.iter().any(|i| i.interface_id == id)
    })
    .await;
    node.node.stop().await.expect("stop");
}

/// Attached interfaces with the same IFAC carry an announce; with
/// different ones they do not. The host moves the frames itself, bridging
/// the two `ExternalInterface`s.
async fn ifac_pair(server_key: &str, client_key: &str) -> bool {
    let mut server = start().await;
    let mut client = start().await;
    let attach = |node: &ReticulumNode, name: &str, key: &str| {
        let mut cfg = ExternalInterfaceConfig::new(name, InterfaceKind::Tcp);
        cfg.hw_mtu = Some(16_384);
        cfg.ifac = Some(IfacConfig::new(Some("attachtest"), Some(key), 16).expect("ifac"));
        node.attach_interface(cfg).expect("attach")
    };
    let (in_s, mut out_s, guard_s) = attach(&server.node, "ext_s", server_key).into_parts();
    let (in_c, mut out_c, guard_c) = attach(&client.node, "ext_c", client_key).into_parts();
    let bridge_sc = tokio::spawn(async move {
        while let Some(f) = out_s.recv().await {
            let _ = in_c.deliver(f.data.to_vec()).await;
        }
    });
    let bridge_cs = tokio::spawn(async move {
        while let Some(f) = out_c.recv().await {
            let _ = in_s.deliver(f.data.to_vec()).await;
        }
    });
    let (_key, hash) = register_dest(&server.node, "ifac");
    let heard = announce_until_heard(&server, &hash, &mut client.rx, Duration::from_secs(4)).await;
    drop((guard_s, guard_c));
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge_sc).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge_cs).await;
    client.node.stop().await.expect("client stop");
    server.node.stop().await.expect("server stop");
    heard
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn matching_ifac_carries_an_announce_and_a_mismatch_does_not() {
    assert!(ifac_pair("same", "same").await, "matching IFAC carries it");
    assert!(
        !ifac_pair("one", "another").await,
        "a mismatched IFAC drops it"
    );
}

/// An attached interface shows in the stats with its kind, follows its
/// up/down signals, counts its bytes, and leaves when the guard is dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attached_interface_shows_in_the_stats_and_detaches_on_drop() {
    let mut node = start().await;
    let mut cfg = ExternalInterfaceConfig::new("ext_stats", InterfaceKind::Udp);
    cfg.up = false;
    let iface = node.node.attach_interface(cfg).expect("attach");
    let id = iface.id();
    let (inbound, mut outbound, guard) = iface.into_parts();
    stats_until(&node.node, "attached down", |s| {
        s.iter()
            .any(|i| i.interface_id == id && i.kind == InterfaceKind::Udp && !i.online)
    })
    .await;
    guard.set_up();
    stats_until(&node.node, "up", |s| {
        s.iter().any(|i| i.interface_id == id && i.online)
    })
    .await;
    guard.set_down();
    assert!(!guard.is_up());
    stats_until(&node.node, "down", |s| {
        s.iter().any(|i| i.interface_id == id && !i.online)
    })
    .await;
    guard.set_up();

    // A frame in counts as received bytes (the node drops the junk).
    inbound.try_deliver(vec![0u8; 40]).expect("deliver");
    stats_until(&node.node, "rx counted", |s| {
        s.iter().any(|i| i.interface_id == id && i.rx_bytes == 40)
    })
    .await;
    // An announce goes out on it.
    let (_key, hash) = register_dest(&node.node, "stats");
    node.node
        .announce_destination(&hash, None)
        .await
        .expect("announce");
    let frame = tokio::time::timeout(Duration::from_secs(5), outbound.recv())
        .await
        .expect("the announce goes out")
        .expect("attached");
    stats_until(&node.node, "tx counted", |s| {
        s.iter()
            .any(|i| i.interface_id == id && i.tx_bytes >= frame.data.len() as u64)
    })
    .await;

    drop(guard);
    stats_until(&node.node, "the guard's drop detaches it", |s| {
        !s.iter().any(|i| i.interface_id == id)
    })
    .await;
    let gone = tokio::time::timeout(Duration::from_secs(5), async {
        while outbound.recv().await.is_some() {}
    })
    .await;
    assert!(gone.is_ok(), "the outbound side ends with the interface");
    assert!(inbound.is_detached());
    node.node.stop().await.expect("stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_config_is_refused_and_a_stopped_node_refuses() {
    let mut node = start().await;
    let mut cfg = ExternalInterfaceConfig::new("small", InterfaceKind::Tcp);
    cfg.hw_mtu = Some(400);
    assert!(matches!(
        node.node.attach_interface(cfg),
        Err(leviculum_std::Error::Config(_))
    ));
    node.node.stop().await.expect("stop");
    assert!(matches!(
        node.node
            .attach_interface(ExternalInterfaceConfig::new("late", InterfaceKind::Tcp)),
        Err(leviculum_std::Error::NotRunning)
    ));
}
