//! leviculum#90 (+ciris): a node held behind a shared handle stops and
//! starts again, for a phone app suspending the node while backgrounded.
//!
//! The witness from the issue: start, take the event receiver, stop; then no
//! listener or connection of the node remains and its traffic-counter thread
//! has ended. Start again: a link from a second node establishes and its
//! events arrive on the SAME receiver. Several cycles, with no thread or
//! file-descriptor growth.

use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{EventReceiver, NodeEvent};

/// Host-wide listener-port allocator, shared with the other suites; `ports`
/// needs it in scope.
#[path = "support/port_alloc.rs"]
#[allow(dead_code)]
mod port_alloc;

#[path = "support/ports.rs"]
#[allow(dead_code)]
mod ports;

/// One test at a time: the first counts threads and descriptors across the
/// whole process.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn saw(
    rx: &mut EventReceiver,
    window: Duration,
    mut pred: impl FnMut(&NodeEvent) -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(ev)) if pred(&ev) => return true,
            Ok(Some(_)) => {}
            _ => return false,
        }
    }
}

/// Threads named `reticulum-traffic-counter` in this process (Linux).
fn counter_threads() -> usize {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return 0;
    };
    tasks
        .flatten()
        .filter(|t| {
            std::fs::read_to_string(t.path().join("comm"))
                .map(|c| c.trim().starts_with("reticulum-traf"))
                .unwrap_or(false)
        })
        .count()
}

fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|d| d.count())
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_node_stops_closes_everything_and_starts_again() {
    let _serial = SERIAL.lock().await;
    // port-handoff: the server must come back on the SAME port after each
    // restart so the client's reconnect finds it, so the number is fixed.
    let port = ports::free_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let storage = tempfile::tempdir().expect("tempdir");
    let mut server = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server(addr)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("server");
    let mut rx = server.take_event_receiver().expect("event receiver");
    // From here on only a shared handle, as the host holds it.
    let server = Arc::new(server);

    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "restart",
        &["witness"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    let hash = *dest.hash();
    server.register_destination(dest);

    let client_storage = tempfile::tempdir().expect("tempdir");
    let mut client = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(addr)
        .storage_path(client_storage.path().to_path_buf())
        .build()
        .await
        .expect("client");
    let mut client_rx = client.take_event_receiver().expect("client events");
    client.start().await.expect("client start");
    // The client's own counter thread, once it has its name: see below.
    let named = tokio::time::Instant::now() + Duration::from_secs(2);
    while counter_threads() < 1 && tokio::time::Instant::now() < named {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mut baseline: Option<(usize, usize)> = None;
    for cycle in 0..3 {
        let before = counter_threads();
        server.start_shared().await.expect("start");
        assert!(server.is_running());
        // A new thread is listed before it has set its name, so give it a
        // moment to appear under it.
        let named = tokio::time::Instant::now() + Duration::from_secs(2);
        while counter_threads() < before + 1 && tokio::time::Instant::now() < named {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            counter_threads(),
            before + 1,
            "cycle {cycle}: one counter thread per run"
        );

        // A link from the client establishes, and the server's events arrive
        // on the receiver taken before the first start.
        server
            .announce_destination(&hash, None)
            .await
            .expect("announce");
        assert!(
            saw(&mut client_rx, Duration::from_secs(15), |ev| matches!(
                ev,
                NodeEvent::AnnounceReceived { .. }
            ))
            .await,
            "cycle {cycle}: the client hears the restarted server"
        );
        let handle = client.connect(&hash, &signing_key).await.expect("connect");
        client
            .await_link_established(handle.link_id())
            .await
            .expect("link established");
        assert!(
            saw(&mut rx, Duration::from_secs(10), |ev| matches!(
                ev,
                NodeEvent::LinkEstablished { .. }
            ))
            .await,
            "cycle {cycle}: the server's events reach the same receiver"
        );

        server.stop_shared().await.expect("stop");
        assert!(!server.is_running());
        assert!(
            TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err(),
            "cycle {cycle}: the listener is closed once stop returns"
        );
        assert_eq!(
            counter_threads(),
            before,
            "cycle {cycle}: the counter thread ended"
        );
        // Calls that need a run fail as not running.
        assert!(server.announce_destination(&hash, None).await.is_err());

        let now = (counter_threads(), open_fds());
        match baseline {
            None => baseline = Some(now),
            Some((threads, fds)) => {
                assert!(now.0 <= threads, "cycle {cycle}: no thread growth");
                // A little slack for the client's own reconnect socket.
                assert!(
                    now.1 <= fds + 4,
                    "cycle {cycle}: no fd growth, {fds} -> {}",
                    now.1
                );
            }
        }
        // Stopping a stopped node is a no-op.
        server.stop_shared().await.expect("second stop");
    }
    client.stop().await.expect("client stop");
}

/// leviculum#97: a link the host never closed does not outlive `stop_shared`.
/// The host hears `LinkClosed`, and the peer gets the close frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_shared_closes_a_link_the_host_left_open() {
    let _serial = SERIAL.lock().await;
    let port = ports::free_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let storage = tempfile::tempdir().expect("tempdir");
    let mut server = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server(addr)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("server");
    let mut rx = server.take_event_receiver().expect("event receiver");
    let server = Arc::new(server);
    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "restart",
        &["landing"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    let hash = *dest.hash();
    server.register_destination(dest);
    server.start_shared().await.expect("start");

    let client_storage = tempfile::tempdir().expect("tempdir");
    let mut client = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(addr)
        .storage_path(client_storage.path().to_path_buf())
        .build()
        .await
        .expect("client");
    let mut client_rx = client.take_event_receiver().expect("client events");
    client.start().await.expect("client start");
    tokio::time::sleep(Duration::from_millis(500)).await;
    server
        .announce_destination(&hash, None)
        .await
        .expect("announce");
    assert!(
        saw(&mut client_rx, Duration::from_secs(15), |ev| matches!(
            ev,
            NodeEvent::AnnounceReceived { .. }
        ))
        .await
    );
    let handle = client.connect(&hash, &signing_key).await.expect("connect");
    client
        .await_link_established(handle.link_id())
        .await
        .expect("established");
    assert!(
        saw(&mut rx, Duration::from_secs(10), |ev| matches!(
            ev,
            NodeEvent::LinkEstablished { .. }
        ))
        .await
    );

    server.stop_shared().await.expect("stop");
    assert!(server.link_list().is_empty(), "nothing left frozen");
    assert!(
        saw(&mut rx, Duration::from_secs(5), |ev| matches!(
            ev,
            NodeEvent::LinkClosed { .. }
        ))
        .await,
        "the host hears it"
    );
    assert!(
        saw(&mut client_rx, Duration::from_secs(5), |ev| matches!(
            ev,
            NodeEvent::LinkClosed { .. }
        ))
        .await,
        "the peer got the close frame"
    );
    client.stop().await.expect("client stop");
}
