//! What a receiver actually gets for a transfer larger than one segment —
//! leviculum#61.
//!
//! `RESOURCE_MAX_EFFICIENT_SIZE` (1 MiB − 1) matches reference RNS
//! (`Resource.py:116`, `1 * 1024 * 1024 - 1`), so a payload past it is not a
//! misconfigured ceiling — it is a **segmented** transfer, exactly as Python
//! does it. The open question this pins is what the API hands the consumer:
//! one completion carrying the whole payload, or one per segment carrying a
//! slice. The answer decides whether a consumer that decodes the first
//! completion sees a truncated body.

use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::{EventReceiver, NodeEvent};

static PORT_COUNTER: AtomicU16 = AtomicU16::new(56000);

fn next_port() -> u16 {
    loop {
        let candidate = PORT_COUNTER.fetch_add(1, Ordering::Relaxed);
        if candidate >= 56900 {
            PORT_COUNTER.store(56000, Ordering::Relaxed);
            continue;
        }
        if StdTcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return candidate;
        }
    }
}

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

/// Deterministic, incompressible-ish payload whose every byte position is
/// checkable, so a truncation is visible as a length AND a content mismatch.
fn payload(len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    let mut x: u64 = 0x243F_6A88_85A3_08D3;
    for b in v.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
    v
}

/// Send `bytes` over one link and report every receiver-side completion as
/// `(segment_index, total_segments, data_len)`, plus the concatenation.
async fn transfer(bytes: Vec<u8>) -> (Vec<(u32, u32, usize)>, Vec<u8>) {
    let port = next_port();
    let mut srv = start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_server(format!("127.0.0.1:{port}").parse::<SocketAddr>().unwrap()),
    )
    .await;
    let mut cli = start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_client(format!("127.0.0.1:{port}").parse::<SocketAddr>().unwrap()),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "seg",
        &["sink"],
    )
    .expect("destination");
    let hash = *dest.hash();
    srv.node.register_destination(dest);
    srv.node
        .announce_destination(&hash, Some(b"seg"))
        .await
        .expect("announce");

    let dl = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(dl, cli.rx.recv()).await {
            Ok(Some(NodeEvent::AnnounceReceived { announce, .. }))
                if *announce.destination_hash() == *hash.as_bytes() =>
            {
                break
            }
            Ok(Some(_)) => continue,
            _ => panic!("client never learned the destination"),
        }
    }

    let handle = cli.node.connect(&hash, &signing_key).await.expect("dial");
    let link_id = *handle.link_id();
    cli.node
        .await_link_established(&link_id)
        .await
        .expect("establish");
    // The serve side must have the link too before a resource can land.
    let dl = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(dl, srv.rx.recv()).await {
            Ok(Some(NodeEvent::LinkEstablished { .. })) => break,
            Ok(Some(_)) => continue,
            _ => panic!("serve side never established"),
        }
    }
    srv.node
        .set_resource_strategy(
            &link_id,
            leviculum_core::resource::ResourceStrategy::AcceptAll,
        )
        .ok();

    let (_hash, sent) = cli
        .node
        .send_resource_awaited(&link_id, &bytes, None, false)
        .await
        .expect("send resource");

    // Collect every receiver-side completion until the sender's future says
    // the whole transfer is done, plus a grace window for the last segment.
    let mut segments = Vec::new();
    let mut assembled = Vec::new();
    let sender_done = tokio::time::timeout(Duration::from_secs(120), sent);
    tokio::pin!(sender_done);
    let overall = tokio::time::Instant::now() + Duration::from_secs(150);
    let mut done = false;
    loop {
        tokio::select! {
            r = &mut sender_done, if !done => {
                r.expect("sender completion timed out").expect("transfer failed");
                done = true;
            }
            ev = tokio::time::timeout_at(overall, srv.rx.recv()) => {
                match ev {
                    Ok(Some(NodeEvent::ResourceCompleted {
                        data, is_sender: false, segment_index, total_segments, ..
                    })) => {
                        segments.push((segment_index, total_segments, data.len()));
                        assembled.extend_from_slice(&data);
                        if segment_index == total_segments { break; }
                    }
                    Ok(Some(_)) => continue,
                    _ => break,
                }
            }
        }
    }
    (segments, assembled)
}

/// Baseline: a payload comfortably under the ceiling is one segment.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_small_payload_arrives_as_one_segment() {
    let bytes = payload(64 * 1024);
    let (segments, assembled) = transfer(bytes.clone()).await;
    println!("small: segments={segments:?} assembled={}", assembled.len());
    assert_eq!(segments.len(), 1);
    assert_eq!(assembled, bytes, "single-segment payload must round-trip");
}

/// The case from the field: a payload past `RESOURCE_MAX_EFFICIENT_SIZE`.
/// This test records what the API actually hands a receiver — how many
/// completions, how much data each carries, and whether the concatenation
/// reconstructs the original.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_payload_past_the_efficient_size_round_trips_intact() {
    let bytes = payload(2 * 1024 * 1024 + 12_345);
    let (segments, assembled) = transfer(bytes.clone()).await;
    println!(
        "large: sent={} segments={segments:?} assembled={}",
        bytes.len(),
        assembled.len()
    );
    assert_eq!(
        assembled.len(),
        bytes.len(),
        "every byte sent must reach the receiver"
    );
    assert_eq!(assembled, bytes, "content must survive segmentation");
    // leviculum#62: the consumer sees ONE completion for the transfer, the
    // shape the reference delivers — not one per segment. This is the
    // assertion that would have caught the downstream truncation: a consumer
    // decoding the first event now has the whole payload.
    assert_eq!(
        segments.len(),
        1,
        "a segmented transfer must arrive as a single assembled completion, got {segments:?}"
    );
    let (seg, total, len) = segments[0];
    assert_eq!(
        (seg, total, len),
        (3, 3, bytes.len()),
        "the delivered event is the final segment, carrying the whole payload"
    );
}
