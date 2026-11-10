//! +ciris: `LinkHandle::try_send_seq` over a real link (TCP through one
//! transport node). The sequences it returns are consecutive, and
//! `LinkStats::delivered_through` turns true for each once the peer has it,
//! read back through `ReticulumNode::link_stats`.

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
async fn a_sequenced_send_is_proved_delivered_by_its_own_sequence() {
    let port = ports::free_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let _c = start(
        ReticulumNodeBuilder::new()
            .enable_transport(true)
            .add_tcp_server(addr),
    )
    .await;
    let leaf = || {
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_client(addr)
    };
    let mut a = start(leaf()).await;
    let mut b = start(leaf()).await;
    tokio::time::sleep(Duration::from_millis(800)).await;

    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "sendseq",
        &["proof"],
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

    let mut seqs = Vec::new();
    for i in 0..5u8 {
        // The window opens as proofs come back: retry a busy or paced send.
        let seq = loop {
            match handle.try_send_seq(&[i; 32]).await {
                Ok(seq) => break seq,
                Err(leviculum_std::Error::Send(_)) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("send {i}: {e:?}"),
            }
        };
        seqs.push(seq);
        let stats = a.node.link_stats(handle.link_id()).expect("stats");
        assert_eq!(
            stats.last_sent_sequence(),
            Some(seq),
            "nothing else sent on this link"
        );
    }
    for w in seqs.windows(2) {
        assert_eq!(w[1], w[0].wrapping_add(1), "consecutive: {seqs:?}");
    }
    for i in 0..5u8 {
        let ev = next_event(&mut b.rx, Duration::from_secs(10), |ev| {
            matches!(ev, NodeEvent::MessageReceived { .. })
        })
        .await
        .unwrap_or_else(|| panic!("message {i} delivered"));
        assert!(matches!(ev, NodeEvent::MessageReceived { data, .. } if data == [i; 32]));
    }
    let last = *seqs.last().unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let stats = a.node.link_stats(handle.link_id()).expect("stats");
        if stats.delivered_through(last) {
            assert_eq!(stats.oldest_unacked_sequence(), None);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "proved through {last}: oldest unacked {:?}",
            stats.oldest_unacked_sequence()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
