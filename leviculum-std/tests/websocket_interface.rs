//! +ciris (#109): the WebSocket interfaces over loopback.
//!
//! A server node and a client node: an announce crosses, a link establishes,
//! and data flows both ways; the server's per-source cap refuses a
//! connection past it, and a text message is closed with 1003; a client
//! redials a server that restarted on the same port; and both sections come
//! from a config file.
#![cfg(feature = "websocket")]

use std::net::SocketAddr;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::config::InterfaceConfig;
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::{EventReceiver, NodeEvent};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message;

#[path = "support/port_alloc.rs"]
#[allow(dead_code)]
mod port_alloc;

#[path = "support/ports.rs"]
#[allow(dead_code)]
mod ports;

struct TestNode {
    node: ReticulumNode,
    rx: EventReceiver,
    _storage: tempfile::TempDir,
}

async fn start(builder: ReticulumNodeBuilder) -> TestNode {
    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = builder
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

/// A destination that accepts links, registered on `node`.
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
        "wstest",
        &[aspect],
    )
    .expect("destination");
    dest.set_accepts_links(true);
    let hash = *dest.hash();
    node.register_destination(dest);
    (signing_key, hash)
}

fn ws_addr(node: &ReticulumNode) -> SocketAddr {
    let addrs = node.websocket_listen_addrs();
    assert_eq!(addrs.len(), 1, "one WebSocket listener: {addrs:?}");
    addrs[0]
}

/// Send on a link, retrying while the window is closed.
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

/// The server announces until the client hears it (a client that is still
/// redialing misses an announce sent once).
async fn announce_until_heard(
    server: &TestNode,
    hash: &leviculum_core::DestinationHash,
    client_rx: &mut EventReceiver,
    window: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + window;
    while tokio::time::Instant::now() < deadline {
        server
            .node
            .announce_destination(hash, None)
            .await
            .expect("announce");
        if next_event(client_rx, Duration::from_secs(1), |ev| {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_websocket_pair_carries_an_announce_a_link_and_data_both_ways() {
    pair_round_trip(None).await;
}

/// The same over `rns.hdlc`: the client asks for it, the server serves it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_hdlc_framed_pair_carries_an_announce_a_link_and_data_both_ways() {
    pair_round_trip(Some("hdlc")).await;
}

/// `framing`: `None` is the client's default (raw), otherwise the config
/// value.
async fn pair_round_trip(framing: Option<&str>) {
    let mut server = start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_websocket_server(ports::LOOPBACK_ANY_PORT),
    )
    .await;
    let addr = ws_addr(&server.node);
    assert!(
        server.node.tcp_listen_addrs().is_empty(),
        "a WebSocket listener is not a TCP listener"
    );
    let mut client = start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_interface_config(InterfaceConfig {
                name: "Pair".to_string(),
                interface_type: "WebSocketClientInterface".to_string(),
                target_host: Some(addr.ip().to_string()),
                target_port: Some(addr.port()),
                websocket_path: Some("/rns".to_string()),
                websocket_framing: framing.map(str::to_string),
                ..Default::default()
            }),
    )
    .await;

    let (signing_key, hash) = register_dest(&server.node, "pair");
    assert!(
        announce_until_heard(&server, &hash, &mut client.rx, Duration::from_secs(15)).await,
        "the client hears the server's announce over WebSocket"
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

    client.node.stop().await.expect("client stop");
    server.node.stop().await.expect("server stop");
}

/// Dial the server with a bare WebSocket client.
async fn raw_dial(
    addr: SocketAddr,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Error,
> {
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(format!("ws://{addr}/")),
    )
    .await
    .expect("dial does not hang")
    .map(|(ws, _)| ws)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_per_source_cap_refuses_a_connection_past_it() {
    let mut server = start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_interface_config(InterfaceConfig {
                name: "Capped".to_string(),
                interface_type: "WebSocketServerInterface".to_string(),
                listen_ip: Some("127.0.0.1".to_string()),
                listen_port: Some(0),
                max_connections_per_source: Some(2),
                ..Default::default()
            }),
    )
    .await;
    let addr = ws_addr(&server.node);

    let mut first = raw_dial(addr).await.expect("first is admitted");
    let _second = raw_dial(addr).await.expect("second is admitted");
    assert!(
        raw_dial(addr).await.is_err(),
        "a third connection from the same source is refused"
    );

    // A text message is not this protocol: the server closes with 1003.
    first
        .send(Message::Text("hello".into()))
        .await
        .expect("send text");
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(msg) = first.next().await {
            match msg {
                Ok(Message::Close(frame)) => return frame.map(|f| f.code),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
        None
    })
    .await
    .expect("the server closes");
    assert_eq!(closed, Some(CloseCode::Unsupported));
    drop(first);

    // The closed connection gave its place back.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match raw_dial(addr).await {
            Ok(_) => break,
            Err(e) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "a place freed by a close is reusable: {e}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    let _ = server.rx.try_recv();
    server.node.stop().await.expect("server stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_reconnects_after_the_server_restarts() {
    // port-handoff: the server must come back on the SAME port after the
    // restart so the client's redial finds it, so the number is fixed.
    let port = ports::free_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let mut server = start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_websocket_server(addr),
    )
    .await;
    let mut client = start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_websocket_interface(format!("ws://127.0.0.1:{port}/")),
    )
    .await;
    let (_, hash) = register_dest(&server.node, "before");
    assert!(
        announce_until_heard(&server, &hash, &mut client.rx, Duration::from_secs(15)).await,
        "the client hears the first server"
    );
    server.node.stop().await.expect("server stop");
    drop(server);

    let restarted = start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_websocket_server(addr),
    )
    .await;
    let (_, hash) = register_dest(&restarted.node, "after");
    assert!(
        announce_until_heard(&restarted, &hash, &mut client.rx, Duration::from_secs(40)).await,
        "the client redials and hears the restarted server"
    );
    client.node.stop().await.expect("client stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_websocket_sections_come_from_a_config_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server_cfg = dir.path().join("server_config");
    std::fs::write(
        &server_cfg,
        "[reticulum]\n  enable_transport = No\n  share_instance = No\n\n\
         [interfaces]\n  [[Browsers]]\n    type = WebSocketServerInterface\n    \
         enabled = yes\n    listen_ip = 127.0.0.1\n    listen_port = 0\n    \
         max_connections_per_source = 8\n",
    )
    .expect("write server config");
    let mut server = start(ReticulumNodeBuilder::new().config_file(server_cfg)).await;
    let addr = ws_addr(&server.node);

    let client_cfg = dir.path().join("client_config");
    std::fs::write(
        &client_cfg,
        format!(
            "[reticulum]\n  enable_transport = No\n  share_instance = No\n\n\
             [interfaces]\n  [[Canonical]]\n    type = WebSocketClientInterface\n    \
             enabled = yes\n    target_host = {}\n    target_port = {}\n    \
             path = /reticulum\n    tls = no\n",
            addr.ip(),
            addr.port()
        ),
    )
    .expect("write client config");
    let mut client = start(ReticulumNodeBuilder::new().config_file(client_cfg)).await;

    let (_, hash) = register_dest(&server.node, "config");
    assert!(
        announce_until_heard(&server, &hash, &mut client.rx, Duration::from_secs(15)).await,
        "an announce crosses between two nodes built from config files"
    );
    let _ = server.rx.try_recv();
    client.node.stop().await.expect("client stop");
    server.node.stop().await.expect("server stop");
}

/// Dial with a subprotocol offer; the answer's protocol header comes back.
async fn raw_dial_offering(
    addr: SocketAddr,
    offer: Option<&str>,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Option<String>,
) {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = format!("ws://{addr}/").into_client_request().unwrap();
    if let Some(offer) = offer {
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", offer.parse().unwrap());
    }
    let (ws, response) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .expect("dial does not hang")
    .expect("handshake");
    let picked = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .map(|v| v.to_str().unwrap().to_string());
    (ws, picked)
}

/// Read HDLC frames off an `rns.hdlc` connection until `n` arrived.
async fn read_frames(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    n: usize,
) -> Vec<Vec<u8>> {
    let mut deframer = Deframer::new();
    let mut frames = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        while frames.len() < n {
            match ws.next().await {
                Some(Ok(Message::Binary(data))) => {
                    for r in deframer.process(&data) {
                        if let DeframeResult::Frame(f) = r {
                            frames.push(f);
                        }
                    }
                }
                Some(Ok(_)) => {}
                other => panic!("connection ended: {other:?}"),
            }
        }
    })
    .await
    .expect("frames arrive");
    frames
}

/// The negotiation, and `rns.hdlc` with boundaries that mean nothing: two
/// announces the first server framed are fed to a second server as one
/// frame split across two messages, the second frame riding in the same
/// message as the first one's tail. Both arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hdlc_frames_may_span_messages_and_share_them() {
    let first = start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_websocket_server(ports::LOOPBACK_ANY_PORT),
    )
    .await;
    let first_addr = ws_addr(&first.node);

    // No offer: raw, and no protocol named in the answer.
    let (_plain, picked) = raw_dial_offering(first_addr, None).await;
    assert_eq!(picked, None);
    // An offer is answered with the first name the server knows.
    let (_raw, picked) = raw_dial_offering(first_addr, Some("chat, rns.raw")).await;
    assert_eq!(picked.as_deref(), Some("rns.raw"));
    let (mut tap, picked) = raw_dial_offering(first_addr, Some("rns.hdlc, rns.raw")).await;
    assert_eq!(picked.as_deref(), Some("rns.hdlc"));

    // Two announces, as the first server frames them.
    let (_, one) = register_dest(&first.node, "one");
    let (_, two) = register_dest(&first.node, "two");
    // Whatever else rides the connection, keep reading until both are in.
    first.node.announce_destination(&one, None).await.unwrap();
    first.node.announce_destination(&two, None).await.unwrap();
    let mut frames = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let carries = |f: &Vec<u8>, h: &leviculum_core::DestinationHash| {
        f.windows(h.as_bytes().len()).any(|w| w == h.as_bytes())
    };
    while !(frames.iter().any(|f| carries(f, &one)) && frames.iter().any(|f| carries(f, &two))) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "both announces framed"
        );
        frames.extend(read_frames(&mut tap, 1).await);
    }
    let p1 = frames.iter().find(|f| carries(f, &one)).unwrap().clone();
    let p2 = frames.iter().find(|f| carries(f, &two)).unwrap().clone();

    let mut second = start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_websocket_server(ports::LOOPBACK_ANY_PORT),
    )
    .await;
    let (mut feed, picked) = raw_dial_offering(ws_addr(&second.node), Some("rns.hdlc")).await;
    assert_eq!(picked.as_deref(), Some("rns.hdlc"));
    let (mut f1, mut f2) = (Vec::new(), Vec::new());
    frame(&p1, &mut f1);
    frame(&p2, &mut f2);
    let cut = f1.len() / 2;
    feed.send(Message::Binary(f1[..cut].to_vec().into()))
        .await
        .unwrap();
    let mut tail = f1[cut..].to_vec();
    tail.extend_from_slice(&f2);
    feed.send(Message::Binary(tail.into())).await.unwrap();

    let mut heard = Vec::new();
    while heard.len() < 2 {
        let Some(NodeEvent::AnnounceReceived { announce, .. }) =
            next_event(&mut second.rx, Duration::from_secs(10), |ev| {
                matches!(ev, NodeEvent::AnnounceReceived { .. })
            })
            .await
        else {
            panic!("the second server hears both announces, heard {heard:?}");
        };
        heard.push(*announce.destination_hash());
    }
    assert!(heard.contains(&one) && heard.contains(&two), "{heard:?}");
}
