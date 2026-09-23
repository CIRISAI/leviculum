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

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use leviculum_core::RequestPolicy;
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{
    Destination, DestinationType, Direction, LinkId, NodeEvent, ProofStrategy, ReticulumNode,
};

use lnomad::browser::{print_once, BrowserOptions};
use lnomad::fetch::{FetchError, Session};
use lnomad::url::parse_url;

const SMALL_PAGE: &[u8] = b"`F222`Bce2>Welcome\n\nThis is a small NomadNet page.\n";

/// A raw file payload as NomadNet's `serve_file` delivers it: the file's bytes
/// with NO msgpack wrapping. The leading `0xc1` is the one byte msgpack never
/// uses, so any accidental decode attempt on this payload fails loudly: a
/// verbatim round trip proves the download path performs no page decode.
const FILE_PAYLOAD: &[u8] = b"\xc1raw file bytes \x00\x01\x02 not msgpack\xff";

/// A deterministic multi-kilobyte raw file body, well past one link packet so
/// the response arrives over the `is_response` Resource path.
fn big_file() -> Vec<u8> {
    (0..16384u32).map(|i| (i * 31 % 251) as u8).collect()
}

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

/// Encode a byte slice as a single msgpack bin value (how RNS packs a `bytes`
/// page response).
fn msgpack_bin(data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &rmpv::Value::Binary(data.to_vec()))
        .expect("msgpack encode");
    buf
}

/// The responder's view of link lifecycle: every link it saw established, and
/// every link it saw closed, in order. This is the only place the *peer's*
/// behaviour is observable — a client that never closes leaves `closed` empty
/// no matter what its own bookkeeping says.
#[derive(Default)]
struct LinkLog {
    established: Vec<LinkId>,
    closed: Vec<LinkId>,
}

/// A Rust node that serves NomadNet-style pages: `/page/small.mu` (a fixed page)
/// and `/page/echo.mu` (echoes the request data). Runs its own reply loop.
///
/// It hosts TWO destinations with the same page handlers, so a test can make a
/// session navigate from one node to another without a second daemon hop, and
/// watch both links from a single event stream.
struct PageResponder {
    dest_hex: String,
    dest_hex_b: String,
    links: Arc<Mutex<LinkLog>>,
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

        let dest_hex = register_node_destination(&mut node).await;
        let dest_hex_b = register_node_destination(&mut node).await;

        let links = Arc::new(Mutex::new(LinkLog::default()));
        let task = tokio::spawn(reply_loop(node, events, links.clone()));
        PageResponder {
            dest_hex,
            dest_hex_b,
            links,
            task,
            _storage: storage,
        }
    }
}

/// Register one NomadNet-style node destination with the full page/file handler
/// set on `node`, announce it, and return its hash as hex. Each call mints a
/// fresh identity, so the two destinations of one responder have distinct
/// hashes and a session treats them as two different nodes.
async fn register_node_destination(node: &mut ReticulumNode) -> String {
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
    node.register_request_handler(dest_hash, "/page/whoami.mu", RequestPolicy::AllowAll);
    node.register_request_handler(dest_hash, "/file/hello.bin", RequestPolicy::AllowAll);
    node.register_request_handler(dest_hash, "/file/big.bin", RequestPolicy::AllowAll);
    node.announce_destination(&dest_hash, Some(b"lnomad-page-node"))
        .await
        .expect("responder announce");
    dest_hex
}

/// Drain the responder's events, answering each page request and recording
/// every link establish/close it observes.
async fn reply_loop(
    node: ReticulumNode,
    mut events: leviculum_std::EventReceiver,
    links: Arc<Mutex<LinkLog>>,
) {
    while let Some(event) = events.recv().await {
        match &event {
            NodeEvent::LinkEstablished { link_id, .. } => {
                links.lock().expect("link log").established.push(*link_id);
            }
            NodeEvent::LinkClosed { link_id, .. } => {
                links.lock().expect("link log").closed.push(*link_id);
            }
            _ => {}
        }
        if let NodeEvent::RequestReceived {
            link_id,
            request_id,
            path,
            data,
            ..
        } = event
        {
            // File targets are served exactly as NomadNet's `serve_file` sends
            // them: a response Resource of the RAW bytes (no msgpack wrapping)
            // plus a msgpack `{"name": ...}` metadata map. hello.bin's metadata
            // name deliberately DIFFERS from the URL basename, so the tests
            // prove the server-sent name wins over the URL-derived one.
            let file = match path.as_str() {
                "/file/hello.bin" => Some((FILE_PAYLOAD.to_vec(), "greeting.txt")),
                "/file/big.bin" => Some((big_file(), "big.bin")),
                _ => None,
            };
            if let Some((bytes, name)) = file {
                let mut metadata = Vec::new();
                rmpv::encode::write_value(
                    &mut metadata,
                    &rmpv::Value::Map(vec![(
                        rmpv::Value::String("name".into()),
                        rmpv::Value::String(name.into()),
                    )]),
                )
                .expect("encode file metadata");
                let _ = node
                    .send_file_response(&link_id, &request_id, &bytes, &metadata)
                    .await;
                continue;
            }
            let response = match path.as_str() {
                "/page/small.mu" => msgpack_bin(SMALL_PAGE),
                "/page/large.mu" => msgpack_bin(&large_page()),
                // Echo the raw request value (a msgpack map of query fields).
                "/page/echo.mu" => data,
                // The remote identity the responder observed on this link at
                // request-handling time (i.e. AFTER any identify the client
                // sent before requesting): its fingerprint, or empty for an
                // anonymous link. This is how a forum/guestbook page would
                // read `remote_identity`.
                "/page/whoami.mu" => {
                    let fingerprint = node
                        .get_remote_identity(&link_id)
                        .map(|identity| identity.hash().to_vec())
                        .unwrap_or_default();
                    msgpack_bin(&fingerprint)
                }
                _ => continue,
            };
            let _ = node.send_response(&link_id, &request_id, &response).await;
        }
    }
}

/// Stand up the daemon + responder and connect an lnomad session to it. The
/// session gets a throwaway app config dir (identity + identify store), so
/// tests never touch the user's real `~/.config/lnomad`.
async fn setup() -> (
    ReticulumNode,
    PageResponder,
    Session,
    tempfile::TempDir,
    tempfile::TempDir,
    String,
) {
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
        // `:0`: the daemon binds the port itself and reports it below, so no
        // free-port probe races the bind (Codeberg #221).
        .add_tcp_server("127.0.0.1:0".parse().unwrap())
        .storage_path(daemon_storage.path().to_path_buf())
        .build_sync()
        .expect("build daemon");
    daemon.start().await.expect("start daemon");
    let daemon_tcp: SocketAddr = *daemon
        .tcp_listen_addrs()
        .first()
        .expect("daemon has a TCP listener");
    // Let the abstract Unix socket listener come up before clients connect.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let responder = PageResponder::start(daemon_tcp).await;
    // Let the responder's TCP link to the daemon establish and its announce
    // propagate.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let session_storage = tempfile::tempdir().expect("session storage");
    let app_dir = tempfile::tempdir().expect("session app dir");
    let session = Session::connect_to_with_app_dir(
        &instance_name,
        session_storage.path().to_path_buf(),
        Some(app_dir.path().to_path_buf()),
    )
    .await
    .expect("lnomad session connect");

    let dest_hex = responder.dest_hex.clone();
    (
        daemon,
        responder,
        session,
        daemon_storage,
        app_dir,
        dest_hex,
    )
}

/// The responder's destination hash as raw bytes, for `set_identify`.
fn dest_bytes(dest_hex: &str) -> [u8; 16] {
    let bytes = hex::decode(dest_hex).expect("dest hex decodes");
    bytes.try_into().expect("dest hash is 16 bytes")
}

#[tokio::test]
async fn fetch_small_page_over_shared_instance() {
    let (mut daemon, responder, mut session, _daemon_storage, _app_dir, dest_hex) = setup().await;

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
    let (mut daemon, responder, mut session, _daemon_storage, _app_dir, dest_hex) = setup().await;

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
    let (mut daemon, responder, mut session, _daemon_storage, _app_dir, dest_hex) = setup().await;

    // Drive the browser's print-once path exactly as `lnomad --print` does:
    // fetch the large page over the Resource path, parse, render (no colour),
    // and print, into an in-memory buffer we can assert on.
    let target = parse_url(&format!("{dest_hex}:/page/large.mu"), None).expect("parse url");
    let opts = BrowserOptions {
        width: 80,
        no_color: true,
        depth: lnomad::color::ColorDepth::Truecolor,
        timeout: Duration::from_secs(20),
        no_images: false,
        image_cache_bytes: lnomad::image_cache::DEFAULT_MAX_BYTES,
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
async fn identified_fetch_reveals_fingerprint_to_server() {
    let (mut daemon, responder, mut session, _daemon_storage, _app_dir, dest_hex) = setup().await;
    let dest = dest_bytes(&dest_hex);
    let target = parse_url(&format!("{dest_hex}:/page/whoami.mu"), None).expect("parse url");

    // Opt in: the fetch identifies on the fresh link before the request, so
    // the responder's handler sees lnomad's identity as `remote_identity`.
    session
        .set_identify(&dest, true)
        .await
        .expect("persist identify");
    assert!(session.is_identifying(&dest));
    let observed = session
        .fetch(&target, Duration::from_secs(20))
        .await
        .expect("fetch whoami identified");
    assert_eq!(
        observed,
        session.fingerprint().to_vec(),
        "responder must observe exactly lnomad's fingerprint"
    );

    // Turn identify off. The reused link is identified and cannot be
    // un-identified, so the session must drop it and the next fetch must build
    // a brand new anonymous link: the responder sees no remote identity. Were
    // the old link reused, it would still return the fingerprint.
    session
        .set_identify(&dest, false)
        .await
        .expect("persist identify");
    assert!(!session.is_identifying(&dest));
    let observed = session
        .fetch(&target, Duration::from_secs(20))
        .await
        .expect("fetch whoami after identify off");
    assert!(
        observed.is_empty(),
        "after identify off the fresh link must be anonymous, responder saw {}",
        hex::encode(&observed)
    );

    session.close().await.expect("close session");
    responder.task.abort();
    daemon.stop().await.expect("stop daemon");
}

#[tokio::test]
async fn anonymous_fetch_reveals_nothing() {
    let (mut daemon, responder, mut session, _daemon_storage, _app_dir, dest_hex) = setup().await;

    // Control arm: no identify decision for this dest, so the fetch stays
    // anonymous and the responder observes no remote identity.
    assert!(!session.is_identifying(&dest_bytes(&dest_hex)));
    let target = parse_url(&format!("{dest_hex}:/page/whoami.mu"), None).expect("parse url");
    let observed = session
        .fetch(&target, Duration::from_secs(20))
        .await
        .expect("fetch whoami anonymously");
    assert!(
        observed.is_empty(),
        "anonymous fetch must reveal no identity, responder saw {}",
        hex::encode(&observed)
    );

    session.close().await.expect("close session");
    responder.task.abort();
    daemon.stop().await.expect("stop daemon");
}

#[tokio::test]
async fn download_file_returns_raw_bytes_verbatim() {
    let (mut daemon, responder, mut session, _daemon_storage, _app_dir, dest_hex) = setup().await;

    // The payload is deliberately NOT valid msgpack (leading 0xc1): a verbatim
    // round trip proves download_file skips the page decode entirely. The
    // responder's metadata names the file greeting.txt, NOT the URL basename.
    let target = parse_url(&format!("{dest_hex}:/file/hello.bin"), None).expect("parse url");
    assert!(target.is_file, "a /file/ path must be flagged as a file");
    let (bytes, server_name) = session
        .download_file(&target, Duration::from_secs(20))
        .await
        .expect("download small file");
    assert_eq!(bytes, FILE_PAYLOAD, "file bytes must arrive verbatim");
    assert_eq!(
        server_name.as_deref(),
        Some("greeting.txt"),
        "download_file must surface the server-sent metadata name"
    );

    // End-to-end save path as `lnomad --print --output <dir>` runs it: the file
    // lands in the directory under the server-sent name (not the URL basename)
    // and the save line names it.
    let dir = tempfile::tempdir().expect("download dir");
    let mut out: Vec<u8> = Vec::new();
    lnomad::browser::download_once(
        &mut out,
        &mut session,
        &target,
        Some(dir.path()),
        Duration::from_secs(20),
    )
    .await
    .expect("download to dir");
    assert!(
        !dir.path().join("hello.bin").exists(),
        "the URL basename must not be used when the server sent a name"
    );
    let saved = dir.path().join("greeting.txt");
    assert_eq!(
        std::fs::read(&saved).expect("saved file readable"),
        FILE_PAYLOAD,
        "saved file must hold the exact payload"
    );
    let line = String::from_utf8(out).expect("save line is utf8");
    assert!(
        line.starts_with(&format!("saved {} bytes to ", FILE_PAYLOAD.len())),
        "save line must report the byte count: {line:?}"
    );
    assert!(
        line.trim_end().ends_with("greeting.txt"),
        "save line must name the saved path: {line:?}"
    );

    session.close().await.expect("close session");
    responder.task.abort();
    daemon.stop().await.expect("stop daemon");
}

#[tokio::test]
async fn download_large_file_over_resource_path() {
    let (mut daemon, responder, mut session, _daemon_storage, _app_dir, dest_hex) = setup().await;

    // Well past one packet: the response arrives as an `is_response` Resource,
    // and the raw bytes must still round-trip exactly.
    let target = parse_url(&format!("{dest_hex}:/file/big.bin"), None).expect("parse url");
    let (bytes, server_name) = session
        .download_file(&target, Duration::from_secs(30))
        .await
        .expect("download large file");
    assert_eq!(
        bytes,
        big_file(),
        "large file bytes must arrive verbatim over the Resource path"
    );
    assert_eq!(
        server_name.as_deref(),
        Some("big.bin"),
        "the metadata name must survive the Resource path"
    );

    session.close().await.expect("close session");
    responder.task.abort();
    daemon.stop().await.expect("stop daemon");
}

#[tokio::test]
async fn unregistered_path_times_out_cleanly() {
    let (mut daemon, responder, mut session, _daemon_storage, _app_dir, dest_hex) = setup().await;

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

/// Poll the responder's link log until `pred` holds or `budget` runs out, and
/// return a snapshot either way. A close travels client -> daemon -> responder,
/// so it is never visible the instant the client call returns; failing the
/// assertion on the snapshot (not in here) keeps the failure message useful.
async fn await_link_log(
    links: &Arc<Mutex<LinkLog>>,
    budget: Duration,
    pred: impl Fn(&LinkLog) -> bool,
) -> (Vec<LinkId>, Vec<LinkId>) {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        {
            let log = links.lock().expect("link log");
            if pred(&log) || tokio::time::Instant::now() >= deadline {
                return (log.established.clone(), log.closed.clone());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Minimal reproducer for #322: browsing from one node to another must close
/// the link to the node being left. Before the fix the session merely forgot
/// it, so the responder saw two links established and none closed, and the
/// abandoned link kept exchanging keepalives for the life of the process.
#[tokio::test]
async fn switching_destination_closes_the_link_to_the_previous_one() {
    let (mut daemon, responder, mut session, _daemon_storage, _app_dir, dest_hex) = setup().await;
    let dest_hex_b = responder.dest_hex_b.clone();

    let target_a = parse_url(&format!("{dest_hex}:/page/small.mu"), None).expect("parse url a");
    session
        .fetch(&target_a, Duration::from_secs(20))
        .await
        .expect("fetch page on node a");
    let (established, _) = await_link_log(&responder.links, Duration::from_secs(5), |log| {
        !log.established.is_empty()
    })
    .await;
    assert_eq!(
        established.len(),
        1,
        "the first fetch must establish exactly one link"
    );
    let first_link = established[0];

    // Navigate to the other node. This is the ordinary switch, nothing
    // exceptional: no error, no cancellation, no identify change.
    let target_b = parse_url(&format!("{dest_hex_b}:/page/small.mu"), None).expect("parse url b");
    session
        .fetch(&target_b, Duration::from_secs(20))
        .await
        .expect("fetch page on node b");

    let (established, closed) = await_link_log(&responder.links, Duration::from_secs(10), |log| {
        log.closed.contains(&first_link)
    })
    .await;
    assert_eq!(
        established.len(),
        2,
        "the switch must build a second link, saw {established:?}"
    );
    assert!(
        closed.contains(&first_link),
        "link {first_link} to the node we left must be closed, closed links: {closed:?}"
    );

    session.close().await.expect("close session");
    responder.task.abort();
    daemon.stop().await.expect("stop daemon");
}

/// Closing the session closes the link it still holds, so quitting the browser
/// does not leave the last-visited node keepaliving a peer that is gone.
#[tokio::test]
async fn closing_the_session_closes_its_link() {
    let (mut daemon, responder, mut session, _daemon_storage, _app_dir, dest_hex) = setup().await;

    let target = parse_url(&format!("{dest_hex}:/page/small.mu"), None).expect("parse url");
    session
        .fetch(&target, Duration::from_secs(20))
        .await
        .expect("fetch page");
    session.close().await.expect("close session");

    let (established, closed) = await_link_log(&responder.links, Duration::from_secs(10), |log| {
        !log.closed.is_empty()
    })
    .await;
    assert_eq!(established.len(), 1, "one fetch, one link");
    assert_eq!(
        closed, established,
        "the link held at close time must be closed"
    );

    responder.task.abort();
    daemon.stop().await.expect("stop daemon");
}

/// The TUI aborts the in-flight fetch task on every navigation, so a fetch can
/// be cancelled at any await inside link setup. Whichever await it lands on,
/// the session must still own the link it built and close it later: no link the
/// responder saw established may outlive the session.
///
/// The cancellation deadlines are swept rather than pinned to one value on
/// purpose — the point is that the invariant holds wherever the cut falls, and
/// the assertion is on the invariant, not on where it fell.
#[tokio::test]
async fn cancelled_link_setup_leaves_no_link_behind() {
    let (mut daemon, responder, mut session, _daemon_storage, _app_dir, dest_hex) = setup().await;
    let dest_hex_b = responder.dest_hex_b.clone();

    let target_a = parse_url(&format!("{dest_hex}:/page/small.mu"), None).expect("parse url a");
    let target_b = parse_url(&format!("{dest_hex_b}:/page/small.mu"), None).expect("parse url b");

    // Learn both paths first, so the cancellations below land in link setup
    // rather than in path discovery.
    session
        .fetch(&target_a, Duration::from_secs(20))
        .await
        .expect("fetch page on node a");
    session
        .fetch(&target_b, Duration::from_secs(20))
        .await
        .expect("fetch page on node b");

    for cut in [1u64, 2, 3, 5, 8, 13, 21, 34, 55] {
        // Alternate destinations so each attempt has to build a fresh link.
        let target = if cut % 2 == 0 { &target_a } else { &target_b };
        let _ = tokio::time::timeout(
            Duration::from_millis(cut),
            session.fetch(target, Duration::from_secs(20)),
        )
        .await;
    }

    // A clean fetch afterwards proves the session is still usable, and closes
    // whatever the last cancellation left owned.
    session
        .fetch(&target_a, Duration::from_secs(20))
        .await
        .expect("fetch after cancellations");
    session.close().await.expect("close session");

    let (established, closed) = await_link_log(&responder.links, Duration::from_secs(15), |log| {
        log.established.len() == log.closed.len() && !log.established.is_empty()
    })
    .await;
    assert!(
        established.len() >= 2,
        "the test must actually have built links, saw {established:?}"
    );
    for link in &established {
        assert!(
            closed.contains(link),
            "link {link} was established but never closed; established {established:?}, closed {closed:?}"
        );
    }

    responder.task.abort();
    daemon.stop().await.expect("stop daemon");
}
