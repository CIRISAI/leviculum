//! mvr for Codeberg #280 — a reliable channel message dropped at the data-plane
//! cap after the wire layer already proofed it to the sender.
//!
//! A channel message is proofed the moment the core delivers it as a
//! `MessageReceived`. From then on the sender's channel treats it as delivered
//! and stops retransmitting. If the receiving node's bounded data plane is full
//! at that moment, the event was discarded silently: the application never saw
//! the message, nothing recorded that it had existed, and the peer had already
//! been told it arrived. That is not backpressure on a lossy stream — it is a
//! confirmed message destroyed after the confirmation.
//!
//! Topology: two in-process `ReticulumNode`s on `127.0.0.1` over TCP loopback.
//! A = TCP server + responder, built with a data plane of **one** slot so it
//! fills after a single event. B = TCP client + initiator; it connects a link
//! and sends `MESSAGES` channel messages. A's application does not read its
//! event stream for the first `STALL`, then drains it to the end.
//!
//! The assertion is delivery, not a counter: every message B sent must reach
//! A's application, in the sender's order. Nothing about the transport is
//! lossy here — loopback drops nothing — so a missing message can only have
//! been destroyed inside A.
//!
//! Medium under test: TCP loopback only. No radio is involved and none is
//! configured, so there is nothing to switch off.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use leviculum_core::link::LinkId;
use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::NodeEvent;

/// Listener port from the host-wide allocator shared by every suite.
fn next_port() -> u16 {
    crate::harness::port_alloc::free_tcp_port()
}

/// How many channel messages B sends. More than A's data plane holds, so on
/// the unfixed path all but the first are destroyed after being proofed.
const MESSAGES: usize = 6;

/// How long A's application ignores its event stream. Long enough for the data
/// plane to be full when the messages arrive, short enough to stay well inside
/// the channel's retry budget (`CHANNEL_MAX_TRIES`), which is what a node under
/// real backpressure relies on.
const STALL: Duration = Duration::from_millis(400);

#[tokio::test]
async fn reliable_channel_messages_survive_a_full_data_plane() {
    let server_port = next_port();
    let server_addr: SocketAddr = format!("127.0.0.1:{server_port}").parse().unwrap();

    // A: server + responder, one single data-plane slot.
    let a_storage = tempfile::tempdir().expect("tempdir A");
    let mut a = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server(server_addr)
        .storage_path(a_storage.path().to_path_buf())
        .data_channel_capacity(1)
        .build()
        .await
        .expect("build A");
    a.start().await.expect("start A");

    // B: client + initiator.
    let b_storage = tempfile::tempdir().expect("tempdir B");
    let mut b = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(server_addr)
        .storage_path(b_storage.path().to_path_buf())
        .build()
        .await
        .expect("build B");
    b.start().await.expect("start B");

    // A's application takes its stream but reads nothing yet — the condition
    // the bug needs.
    let mut a_rx = a.take_event_receiver().expect("A event rx");

    // B drains normally; we need its LinkEstablished to know the link is up.
    let mut b_rx = b.take_event_receiver().expect("B event rx");
    let (b_est_tx, mut b_est_rx) = tokio::sync::mpsc::unbounded_channel::<LinkId>();
    let b_drain = tokio::spawn(async move {
        while let Some(ev) = b_rx.recv().await {
            if let NodeEvent::LinkEstablished {
                link_id,
                is_initiator: true,
                ..
            } = ev
            {
                let _ = b_est_tx.send(link_id);
            }
        }
    });

    // A registers + announces its destination.
    let a_identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = a_identity.public_key_bytes()[32..64].try_into().unwrap();
    let a_dest = Destination::new(
        Some(a_identity),
        Direction::In,
        DestinationType::Single,
        "mvr",
        &["chan280", "resp"],
    )
    .expect("A destination");
    let a_hash = *a_dest.hash();
    a.register_destination(a_dest);

    tokio::time::sleep(Duration::from_millis(500)).await;
    a.announce_destination(&a_hash, Some(b"chan280"))
        .await
        .expect("A announce");

    // B installs the path from the announce.
    let install_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < install_deadline && !b.has_path(&a_hash) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        b.has_path(&a_hash),
        "B never installed path from A announce"
    );

    let handle = b.connect(&a_hash, &signing_key).await.expect("B connect");
    tokio::time::timeout(Duration::from_secs(5), b_est_rx.recv())
        .await
        .expect("B link never established")
        .expect("B link never established");

    // B sends every message. `send()` absorbs pacing and window-full, so this
    // task simply keeps going until all of them are handed to the channel.
    let sender = tokio::spawn(async move {
        for i in 0..MESSAGES {
            handle
                .send(format!("msg-{i}").as_bytes())
                .await
                .unwrap_or_else(|e| panic!("B send {i} failed: {e:?}"));
        }
        handle
    });

    // A's application is busy elsewhere: its data plane fills and stays full.
    tokio::time::sleep(STALL).await;

    // Now it drains. Every message must be here.
    let mut received: Vec<(u16, Vec<u8>)> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while received.len() < MESSAGES && Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(250), a_rx.recv()).await {
            Ok(Some(NodeEvent::MessageReceived { sequence, data, .. })) => {
                received.push((sequence, data));
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    let _ = sender.await;
    b_drain.abort();
    let _ = a.stop().await;
    let _ = b.stop().await;

    let payloads: Vec<String> = received
        .iter()
        .map(|(_, d)| String::from_utf8_lossy(d).into_owned())
        .collect();
    let expected: Vec<String> = (0..MESSAGES).map(|i| format!("msg-{i}")).collect();
    assert_eq!(
        payloads,
        expected,
        "every proofed channel message must reach the application, in order \
         (got {} of {MESSAGES}; sequences {:?})",
        received.len(),
        received.iter().map(|(s, _)| *s).collect::<Vec<_>>()
    );
}
