//! The phone's first link to a page behind its mailbox's relay (order
//! 1669, walk 12, finding 69): STRUDEL and rns.moscow failed after 31 s
//! with `link_failed`, the next tap loaded in 2 s.
//!
//! The facade: the phone's leaf on relay A's access point port (miauhaus),
//! its mailbox on A's peer port, the page two relays further. A holds the
//! page's path through hub B; B comes back as another node (a stand-in for
//! a hub that no longer carries the path), so the link request A forwards
//! dies there. A gives up on it after its proof window, `(hops + 2) x 6 s`,
//! asks the path anew and takes the longer way round through C. The core
//! on the phone sends its request again only once its own establishment
//! timeout passes, 43 s and more four hops out; a fetch that waited the bare
//! 30-s request timeout gave up before that and failed while the network
//! was mending itself, and the reader's second tap was the retry.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use leviculum_core::RequestPolicy;
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{Destination, DestinationType, Direction, NodeEvent, ProofStrategy};
use loquax::browse_fetch::{self, Ask, Told};

#[path = "../../loquax/tests/support/reach.rs"]
mod reach;

use reach::{active, entry, logs, wait_until, Mailbox, Phone, Relay};

/// A msgpack bin, as RNS packs a page's bytes.
fn bin(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0xc6];
    out.extend_from_slice(&u32::try_from(data.len()).expect("small").to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// The page node: a leaf announcing once as a NomadNet node, answering
/// its index page with a line of micron.
async fn page_node(
    upstream: SocketAddr,
    storage: &Path,
) -> ([u8; 16], tokio::task::JoinHandle<()>) {
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(upstream)
        .storage_path(storage.to_path_buf())
        .build_sync()
        .expect("the page node");
    let mut events = node.take_event_receiver().expect("its events");
    node.start().await.expect("start it");
    let mut destination = Destination::new(
        Some(leviculum_std::generate_identity()),
        Direction::In,
        DestinationType::Single,
        "nomadnetwork",
        &["node"],
    )
    .expect("its destination");
    destination.set_accepts_links(true);
    destination.set_proof_strategy(ProofStrategy::All);
    let hash = *destination.hash();
    node.register_destination(destination);
    node.register_request_handler(hash, "/page/index.mu", RequestPolicy::AllowAll);
    let task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = node.announce_destination(&hash, Some(b"Testknoten")).await;
        while let Some(event) = events.recv().await {
            let NodeEvent::RequestReceived {
                link_id,
                request_id,
                path,
                ..
            } = event
            else {
                continue;
            };
            if path == "/page/index.mu" {
                let _ = node
                    .send_response(&link_id, &request_id, &bin(b">Hello"))
                    .await;
            }
        }
    });
    (*hash.as_bytes(), task)
}

/// Ask for the index page; how it went (`ok` or the failure in English)
/// and after how long.
async fn fetch_once(fetching: &mut browse_fetch::Fetching, dest: [u8; 16]) -> (Duration, String) {
    let asked = Instant::now();
    fetching
        .asks
        .send(Ask::Navigate(lnomad::Target {
            dest_hash: dest,
            path: "/page/index.mu".into(),
            fields: Vec::new(),
            is_file: false,
        }))
        .expect("asked");
    loop {
        let told = tokio::time::timeout(Duration::from_secs(120), fetching.told.recv())
            .await
            .expect("an answer within two minutes")
            .expect("the fetch runs");
        match told {
            Told::Browse(loquax::ui::browse::BrowseEvent::Loaded { .. }) => {
                return (asked.elapsed(), "ok".into())
            }
            Told::Browse(loquax::ui::browse::BrowseEvent::FailedText(text)) => {
                return (
                    asked.elapsed(),
                    text.resolve(lnomad::app::strings::Lang::En),
                )
            }
            _ => {}
        }
    }
}

/// The first fetch waits for the core's retry, which goes out after the
/// relay mended its path, and loads; red before 1669, when it failed at
/// 30 s.
#[tokio::test(flavor = "multi_thread")]
async fn the_first_fetch_outlasts_a_relay_mending_its_path() {
    logs();
    let b = Relay::start(&[]).await;
    let d = Relay::start(&[b.peer]).await;
    let e = Relay::start(&[d.peer]).await;
    let c = Relay::start(&[e.peer]).await;
    let a = Relay::start(&[b.peer, c.peer]).await;
    let page_dir = tempfile::tempdir().expect("dir");
    let (dest, page) = page_node(d.peer, page_dir.path()).await;
    let dest_hash = leviculum_core::DestinationHash::new(dest);
    // Through B, the shorter way: the copy round C may come first.
    wait_until("A learns the page's path through B", 15, || {
        a.node.hops_to(&dest_hash) == Some(3)
    })
    .await;
    wait_until("C learns the page's path", 15, || {
        c.node.has_path(&dest_hash)
    })
    .await;
    // B comes back as another node on the same ports: A's path still names
    // the old one as its next hop.
    let (peer, ap) = b.stop().await;
    let b = Relay::on(
        std::net::TcpListener::bind(peer).expect("the peer port again"),
        std::net::TcpListener::bind(ap).expect("the access point port again"),
        &[],
    )
    .await;
    wait_until("A dials B again", 15, || b.node.has_path(&dest_hash)).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        a.node.hops_to(&dest_hash),
        Some(3),
        "A still holds the old path"
    );

    let mailbox = Mailbox::start(vec![entry(a.peer)]).await;
    let phone = Phone::start(vec![entry(a.ap)], &mailbox, true).await;
    wait_until("the phone's rendezvous is up", 10, || {
        active(&phone.attached).is_some()
    })
    .await;
    let wake = Arc::new(tokio::sync::Notify::new());
    let (lend, lends) = tokio::sync::mpsc::unbounded_channel();
    lend.send(phone.attached.lend()).expect("lent");
    let mut fetching = browse_fetch::start(
        &tokio::runtime::Handle::current(),
        browse_fetch::Options {
            lends,
            identity: phone.device.clone(),
            identify: phone.dir.path().join("identify.toml"),
        },
        wake,
    );
    // The lend is taken before the first ask, as the session's start lends
    // long before any tap.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (took, how) = fetch_once(&mut fetching, dest).await;
    eprintln!("the first fetch: {how} after {took:?}");
    assert_eq!(how, "ok", "the first fetch, after {took:?}");
    // The first request died at B: the page came with the core's retry,
    // after A's proof window for it, (1 + 3 + 2) x 6 s, had closed and A
    // had asked the path anew.
    assert!(took > Duration::from_secs(36), "loaded after {took:?}");
    let (took, how) = fetch_once(&mut fetching, dest).await;
    assert_eq!(how, "ok", "the second fetch, after {took:?}");

    drop(fetching);
    page.abort();
    phone.attached.stop().await;
    mailbox.stop().await;
    for relay in [a, b, c, d, e] {
        relay.stop().await;
    }
}
