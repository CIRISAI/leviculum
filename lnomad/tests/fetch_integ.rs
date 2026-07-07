//! Self-contained end-to-end test of [`lnomad::Session::fetch`] over a real
//! shared-instance IPC path, with no Python dependency.
//!
//! Topology:
//! ```text
//! lnomad Session ── Unix socket ── Rust daemon ── TCP ── Rust page responder
//! (IPC initiator)                  (share_instance,       (registers dest + page
//!                                   TCP server)             handlers, announces)
//! ```
//!
//! This exercises the full backend: connect to a shared instance, learn a path
//! forwarded through the daemon, establish a link through the daemon, issue a
//! request, and receive the response. It covers the single-packet page path, the
//! query-field round trip, and the clean timeout for an unregistered path. The
//! Python drop-in interop tests (against both `lnsd` and `rnsd`) additionally
//! cover the large `is_response` Resource path and byte-identity with Python.

use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use leviculum_core::RequestPolicy;
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{
    Destination, DestinationType, Direction, NodeEvent, ProofStrategy, ReticulumNode,
};

use lnomad::browser::{print_once, BrowserOptions};
use lnomad::fetch::{FetchError, Session};
use lnomad::url::parse_url;

const SMALL_PAGE: &[u8] = b"`F222`Bce2>Welcome\n\nThis is a small NomadNet page.\n";

/// A multi-kilobyte micron page: a heading, a link, and enough body to exceed a
/// single link packet so the response comes back over the `is_response` Resource
/// path. Rendered output and the collected link drive the print-mode assertions.
fn large_page() -> Vec<u8> {
    let mut page = String::new();
    page.push_str(">Node Directory\n\n");
    page.push_str("Browse the pages served by this node.\n\n");
    page.push_str("`[Documentation`:/page/docs.mu]\n\n");
    // Pad well past one packet's worth of payload with stable, greppable lines.
    for i in 0..200 {
        page.push_str(&format!("Entry {i:03} in the node directory listing.\n"));
    }
    page.into_bytes()
}

/// Grab a currently-free localhost TCP port by binding and immediately dropping.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// Encode a byte slice as a single msgpack bin value (how RNS packs a `bytes`
/// page response).
fn msgpack_bin(data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &rmpv::Value::Binary(data.to_vec()))
        .expect("msgpack encode");
    buf
}

/// A Rust node that serves NomadNet-style pages: `/page/small.mu` (a fixed page)
/// and `/page/echo.mu` (echoes the request data). Runs its own reply loop.
struct PageResponder {
    dest_hex: String,
    task: tokio::task::JoinHandle<()>,
    _storage: tempfile::TempDir,
}

impl PageResponder {
    async fn start(daemon_tcp: SocketAddr) -> Self {
        let storage = tempfile::tempdir().expect("responder storage");
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_client(daemon_tcp)
            .storage_path(storage.path().to_path_buf())
            .build_sync()
            .expect("build responder");
        let events = node.take_event_receiver().expect("responder events");
        node.start().await.expect("start responder");

        let identity = leviculum_std::generate_identity();
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "nomadnetwork",
            &["node"],
        )
        .expect("responder destination");
        dest.set_accepts_links(true);
        dest.set_proof_strategy(ProofStrategy::All);
        let dest_hash = *dest.hash();
        let dest_hex = hex::encode(dest_hash.as_bytes());
        node.register_destination(dest);
        node.register_request_handler(dest_hash, "/page/small.mu", RequestPolicy::AllowAll);
        node.register_request_handler(dest_hash, "/page/large.mu", RequestPolicy::AllowAll);
        node.register_request_handler(dest_hash, "/page/echo.mu", RequestPolicy::AllowAll);
        node.announce_destination(&dest_hash, Some(b"lnomad-page-node"))
            .await
            .expect("responder announce");

        let task = tokio::spawn(reply_loop(node, events));
        PageResponder {
            dest_hex,
            task,
            _storage: storage,
        }
    }
}

/// Drain the responder's events, answering each page request.
async fn reply_loop(node: ReticulumNode, mut events: leviculum_std::EventReceiver) {
    while let Some(event) = events.recv().await {
        if let NodeEvent::RequestReceived {
            link_id,
            request_id,
            path,
            data,
            ..
        } = event
        {
            let response = match path.as_str() {
                "/page/small.mu" => msgpack_bin(SMALL_PAGE),
                "/page/large.mu" => msgpack_bin(&large_page()),
                // Echo the raw request value (a msgpack map of query fields).
                "/page/echo.mu" => data,
                _ => continue,
            };
            let _ = node.send_response(&link_id, &request_id, &response).await;
        }
    }
}

/// Stand up the daemon + responder and connect an lnomad session to it.
async fn setup() -> (
    ReticulumNode,
    PageResponder,
    Session,
    tempfile::TempDir,
    String,
) {
    let daemon_tcp_port = free_port();
    let daemon_tcp: SocketAddr = format!("127.0.0.1:{daemon_tcp_port}").parse().unwrap();
    static SETUP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let instance_name = format!(
        "lnomad-selftest-{}-{}",
        std::process::id(),
        SETUP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );

    let daemon_storage = tempfile::tempdir().expect("daemon storage");
    let mut daemon = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.clone())
        .add_tcp_server(daemon_tcp)
        .storage_path(daemon_storage.path().to_path_buf())
        .build_sync()
        .expect("build daemon");
    daemon.start().await.expect("start daemon");
    // Let the abstract Unix socket listener come up before clients connect.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let responder = PageResponder::start(daemon_tcp).await;
    // Let the responder's TCP link to the daemon establish and its announce
    // propagate.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let session_storage = tempfile::tempdir().expect("session storage");
    let session = Session::connect_to(&instance_name, session_storage.path().to_path_buf())
        .await
        .expect("lnomad session connect");

    let dest_hex = responder.dest_hex.clone();
    (daemon, responder, session, daemon_storage, dest_hex)
}

#[tokio::test]
async fn fetch_small_page_over_shared_instance() {
    let (mut daemon, responder, mut session, _daemon_storage, dest_hex) = setup().await;

    let target = parse_url(&format!("{dest_hex}:/page/small.mu"), None).expect("parse url");
    let page = session
        .fetch(&target, Duration::from_secs(20))
        .await
        .expect("fetch small page");
    assert_eq!(page, SMALL_PAGE, "fetched page must match what was served");

    session.close().await.expect("close session");
    responder.task.abort();
    daemon.stop().await.expect("stop daemon");
}

#[tokio::test]
async fn echo_page_round_trips_query_fields() {
    let (mut daemon, responder, mut session, _daemon_storage, dest_hex) = setup().await;

    // Fields become a `var_*` msgpack map; the echo handler returns it verbatim.
    let target = parse_url(
        &format!("{dest_hex}:/page/echo.mu`name=alice|count=3"),
        None,
    )
    .expect("parse url");
    let raw = session
        .request(&target, Duration::from_secs(20))
        .await
        .expect("request echo page");

    let mut cursor = std::io::Cursor::new(raw.as_slice());
    let value = rmpv::decode::read_value(&mut cursor).expect("decode echo map");
    let map = match value {
        rmpv::Value::Map(entries) => entries,
        other => panic!("expected a msgpack map, got {other:?}"),
    };
    let pairs: Vec<(String, String)> = map
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().unwrap_or_default().to_string(),
                v.as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("var_name".to_string(), "alice".to_string()),
            ("var_count".to_string(), "3".to_string()),
        ],
        "echo must return the query fields as a var_ map"
    );

    session.close().await.expect("close session");
    responder.task.abort();
    daemon.stop().await.expect("stop daemon");
}

#[tokio::test]
async fn print_mode_renders_page_and_link_list() {
    let (mut daemon, responder, mut session, _daemon_storage, dest_hex) = setup().await;

    // Drive the browser's print-once path exactly as `lnomad --print` does:
    // fetch the large page over the Resource path, parse, render (no colour),
    // and print, into an in-memory buffer we can assert on.
    let target = parse_url(&format!("{dest_hex}:/page/large.mu"), None).expect("parse url");
    let opts = BrowserOptions {
        width: 80,
        no_color: true,
        timeout: Duration::from_secs(20),
    };
    let mut out: Vec<u8> = Vec::new();
    print_once(&mut out, &mut session, &target, &opts)
        .await
        .expect("print large page");
    let printed = String::from_utf8(out).expect("printed output is utf8");

    // Rendered page content: the heading and body text survive rendering.
    assert!(
        printed.contains("Node Directory"),
        "heading missing from output: {printed:?}"
    );
    assert!(
        printed.contains("Browse the pages served by this node."),
        "body text missing from output"
    );
    assert!(
        printed.contains("Entry 199 in the node directory listing."),
        "tail of the large page missing (Resource path truncated?)"
    );
    // Links render inline (set apart by underline + colour), with no `[N]`
    // marker and no trailing `Links:` legend: the label appears in the page body
    // but the numbered-legend forms do not.
    assert!(
        printed.contains("Documentation"),
        "inline link label missing from output: {printed:?}"
    );
    assert!(
        !printed.contains("Links:"),
        "legend header leaked into output: {printed:?}"
    );
    assert!(
        !printed.contains("[1] Documentation"),
        "numbered link marker leaked into output: {printed:?}"
    );
    assert!(
        !printed.contains("-> :/page/docs.mu"),
        "legend entry leaked into output: {printed:?}"
    );

    session.close().await.expect("close session");
    responder.task.abort();
    daemon.stop().await.expect("stop daemon");
}

#[tokio::test]
async fn unregistered_path_times_out_cleanly() {
    let (mut daemon, responder, mut session, _daemon_storage, dest_hex) = setup().await;

    let target =
        parse_url(&format!("{dest_hex}:/page/does-not-exist.mu"), None).expect("parse url");
    let result = session.fetch(&target, Duration::from_secs(2)).await;
    assert!(
        matches!(result, Err(FetchError::Timeout)),
        "unregistered path must surface a clean Timeout, got {result:?}"
    );

    session.close().await.expect("close session");
    responder.task.abort();
    daemon.stop().await.expect("stop daemon");
}
