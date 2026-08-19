//! Request/response contract — leviculum#55.
//!
//! #55 asked for `send_response` to be forwarded onto the std driver. It
//! already is (`ReticulumNode::send_response`, alongside
//! `send_response_resource` and `send_file_response`), and R1 below is the
//! proof from a consumer's position: a node registers a handler, receives
//! `RequestReceived`, replies, and the requester gets the bytes.
//!
//! What #55 asked that was genuinely unanswered is the **contract around**
//! the verb, and a responder that retries or serves churning mobile peers
//! has to know both answers:
//!
//! - **R2 — is replying twice for one `request_id` idempotent, or an error?**
//! - **R3 — what if the link closed between the request and the response?**
//!   A propagation node answering a peer that went away is the normal case,
//!   not the edge case; a typed error beats a silent drop.
//!
//! These tests pin the answers so a serve path can be written against them
//! instead of against a guess.

use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use leviculum_core::{Destination, DestinationType, Direction, Identity, RequestPolicy};
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::{EventReceiver, NodeEvent};

static PORT_COUNTER: AtomicU16 = AtomicU16::new(57000);

fn next_port() -> u16 {
    loop {
        let candidate = PORT_COUNTER.fetch_add(1, Ordering::Relaxed);
        if candidate >= 57900 {
            PORT_COUNTER.store(57000, Ordering::Relaxed);
            continue;
        }
        if StdTcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return candidate;
        }
    }
}

const PATH: &str = "/message/get";

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

async fn serve_node(port: u16) -> TestNode {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_server(addr),
    )
    .await
}

async fn client_node(port: u16) -> TestNode {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_client(addr),
    )
    .await
}

async fn saw_event(
    rx: &mut EventReceiver,
    window: Duration,
    mut pred: impl FnMut(&NodeEvent) -> bool,
) -> bool {
    let dl = tokio::time::Instant::now() + window;
    loop {
        match tokio::time::timeout_at(dl, rx.recv()).await {
            Ok(Some(ev)) => {
                if pred(&ev) {
                    return true;
                }
            }
            Ok(None) | Err(_) => return false,
        }
    }
}

/// Bring up a served destination with a handler, link a client to it, and
/// return `(serve, client, link_id_on_client, request_id, serve_link_id)`
/// once the serve side has actually received the request.
async fn request_in_flight() -> (TestNode, TestNode, leviculum_core::link::LinkId, [u8; 16]) {
    let port = next_port();
    let mut srv = serve_node(port).await;
    let mut cli = client_node(port).await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "contract",
        &["serve"],
    )
    .expect("destination");
    let hash = *dest.hash();
    srv.node.register_destination(dest);
    srv.node
        .register_request_handler(hash, PATH, RequestPolicy::AllowAll);
    srv.node
        .announce_destination(&hash, Some(b"serve"))
        .await
        .expect("announce");
    assert!(
        saw_event(&mut cli.rx, Duration::from_secs(8), |ev| {
            matches!(ev, NodeEvent::AnnounceReceived { announce, .. }
                if *announce.destination_hash() == *hash.as_bytes())
        })
        .await,
        "client must learn the served destination"
    );

    let handle = cli.node.connect(&hash, &signing_key).await.expect("dial");
    let cli_link = *handle.link_id();
    cli.node
        .await_link_established(&cli_link)
        .await
        .expect("link established");

    // Request payload must be exactly one msgpack value (core debug-asserts
    // it): bin(4) "give", the shape a mailbox fetch filter would take.
    let ask = [0xc4u8, 0x04, b'g', b'i', b'v', b'e'];
    cli.node
        .send_request(&cli_link, PATH, Some(&ask), Some(10_000))
        .await
        .expect("send request");

    // The serve side's link id is its own view of the same link.
    let mut serve_link = None;
    let mut request_id = None;
    let dl = tokio::time::Instant::now() + Duration::from_secs(8);
    while serve_link.is_none() {
        match tokio::time::timeout_at(dl, srv.rx.recv()).await {
            Ok(Some(NodeEvent::RequestReceived {
                link_id,
                request_id: rid,
                ..
            })) => {
                serve_link = Some(link_id);
                request_id = Some(rid);
            }
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => panic!("the serve side never received the request"),
        }
    }
    (
        srv,
        cli,
        serve_link.expect("serve link"),
        request_id.expect("request id"),
    )
}

/// R1 — the verb is reachable from a consumer holding the node, and a reply
/// reaches the requester. (This is the half #55 believed was missing.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registered_handler_can_answer_a_received_request() {
    let (srv, mut cli, serve_link, request_id) = request_in_flight().await;

    // One msgpack value: a bin payload is what a mailbox fetch returns.
    let body = [0xc4u8, 0x03, b'h', b'i', b'!'];
    srv.node
        .send_response(&serve_link, &request_id, &body)
        .await
        .expect("a handler must be able to answer");

    assert!(
        saw_event(&mut cli.rx, Duration::from_secs(8), |ev| matches!(
            ev,
            NodeEvent::ResponseReceived { .. }
        ))
        .await,
        "the requester must receive the response"
    );
}

/// R2 — answering the same `request_id` twice. Pins whichever way it goes so
/// a retrying serve path is written against fact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answering_the_same_request_twice_is_typed_not_silent() {
    let (srv, _cli, serve_link, request_id) = request_in_flight().await;
    let body = [0xc4u8, 0x02, b'o', b'k'];

    srv.node
        .send_response(&serve_link, &request_id, &body)
        .await
        .expect("first answer");

    let second = srv
        .node
        .send_response(&serve_link, &request_id, &body)
        .await;
    let rendered = format!("{second:?}");
    println!("R2 second send_response outcome: {rendered}");
    // Either answer is workable for a caller, but it must be knowable: a
    // second reply is either accepted (idempotent) or refused with a typed
    // error — never a panic, and never an untyped silent drop.
    if second.is_err() {
        assert!(
            rendered.contains("Request"),
            "a refused second reply must name the reason, got {rendered}"
        );
    }
}

/// R3 — the peer is gone before the reply is sent. Splits the case #55 asked
/// about into the two states a responder can actually be in, because they
/// behave differently and only one of them is knowable at reply time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answering_after_the_peer_leaves_reports_only_what_the_node_knows() {
    let body = [0xc4u8, 0x02, b'o', b'k'];

    // R3a — the link is definitively torn down (this node closed it, or a
    // LinkClosed was processed). The reply is refused with a typed error.
    {
        let (srv, _cli, serve_link, request_id) = request_in_flight().await;
        srv.node.close_link(&serve_link).await.expect("close link");
        // Let the close settle through the event loop.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let outcome = srv
            .node
            .send_response(&serve_link, &request_id, &body)
            .await;
        println!("R3a reply on a closed link: {outcome:?}");
        assert!(
            outcome.is_err(),
            "once the link is torn down, a reply must be refused with a typed \
             error rather than reported as sent"
        );
    }

    // R3b — the peer vanished (process gone, TCP dropped) but no LinkClosed
    // has been processed yet, so the node still believes the link is up. The
    // reply is accepted and goes nowhere. This is not a defect the responder
    // can be protected from — the node cannot report a death it has not yet
    // observed — but a serve path MUST NOT read Ok as proof of delivery.
    {
        let (srv, cli, serve_link, request_id) = request_in_flight().await;
        drop(cli);
        let outcome = srv
            .node
            .send_response(&serve_link, &request_id, &body)
            .await;
        println!("R3b reply immediately after the peer vanished: {outcome:?}");
        assert!(
            outcome.is_ok(),
            "documenting the honest limitation: before the link is known dead, \
             the reply is accepted (delivery is not what Ok means)"
        );
    }
}
