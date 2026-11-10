//! The Browse tab fetches through the phone's own node (1337 pass two,
//! #475), end to end on the host: a Rust transport node with a TCP
//! server, a NomadNet page node behind it serving the Reticulum README
//! (23,490 bytes of micron) and a file, and the phone's session with its
//! mailbox on the phone, which dials the transport node. The page node's
//! announce reaches the tab's list through the mailbox's node, a tap on
//! the row brings the page through it, in Java's bytes, and a `/file/`
//! link brings the file, handed to Java once. The time from the tap to the
//! first screen that shows the page is printed.

use std::path::Path;
use std::time::{Duration, Instant};

use leviculum_core::RequestPolicy;
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{Destination, DestinationType, Direction, NodeEvent, ProofStrategy};
use loquax_android::schema::{self, BrowseScreen, Query, Request, Start};
use loquax_android::session::Session;

/// The page the node serves: the largest we have.
const README: &str = include_str!("../../lnomad/tests_data/reticulum_readme.mu");
/// The file it serves, and the name its metadata gives it.
const FILE: &[u8] = b"\xc1 a file, not msgpack \x00\x01\x02";
const FILE_NAME: &str = "hello.bin";

/// A msgpack bin, as RNS packs a page's bytes.
fn bin(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0xc6];
    out.extend_from_slice(&u32::try_from(data.len()).expect("small").to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// The msgpack map `{"name": name}` NomadNet's `serve_file` sends along.
fn name_map(name: &str) -> Vec<u8> {
    let mut out = vec![0x81, 0xa4];
    out.extend_from_slice(b"name");
    out.push(0xa0 | u8::try_from(name.len()).expect("a short name"));
    out.extend_from_slice(name.as_bytes());
    out
}

/// The page node: a leaf of the transport node, announcing as a NomadNet
/// node until told to stop, and answering its page and its file.
async fn page_node(
    transport: std::net::SocketAddr,
    storage: &Path,
) -> ([u8; 16], tokio::task::JoinHandle<()>) {
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(transport)
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
    node.register_request_handler(hash, "/file/hello.bin", RequestPolicy::AllowAll);
    let task = tokio::spawn(async move {
        let mut announce = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                _ = announce.tick() => {
                    let _ = node.announce_destination(&hash, Some(b"Testknoten")).await;
                }
                event = events.recv() => {
                    let Some(NodeEvent::RequestReceived { link_id, request_id, path, .. }) = event else {
                        continue;
                    };
                    match path.as_str() {
                        // Past the link MDU, as the reference answers it:
                        // a response Resource.
                        "/page/index.mu" => {
                            let _ = node
                                .send_response_resource(&link_id, &request_id, &bin(README.as_bytes()))
                                .await;
                        }
                        "/file/hello.bin" => {
                            let _ = node
                                .send_file_response(&link_id, &request_id, FILE, &name_map(FILE_NAME))
                                .await;
                        }
                        _ => {}
                    }
                }
            }
        }
    });
    (*hash.as_bytes(), task)
}

fn browse(session: &Session, verb: &str, node: &str) {
    session
        .request(Request::Browse {
            verb: verb.to_string(),
            node: node.to_string(),
            text: String::new(),
            cols: 0,
            rows: 0,
        })
        .unwrap_or_else(|error| panic!("{verb}: {error}"));
}

fn screen(session: &Session) -> BrowseScreen {
    schema::decode(&session.query(Query::Browse).expect("the screen")).expect("decoded")
}

/// The page's text as Java draws it, run by run.
fn page_text(screen: &BrowseScreen) -> String {
    screen
        .lines
        .iter()
        .map(|line| {
            line.runs
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Ask the screen until `want` holds, up to `secs`; the screens asked.
fn until(
    session: &Session,
    secs: u64,
    what: &str,
    want: impl Fn(&BrowseScreen) -> bool,
) -> Vec<BrowseScreen> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut seen = Vec::new();
    loop {
        let shown = screen(session);
        let done = want(&shown);
        seen.push(shown);
        if done {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "no {what} in {secs} s; last: {:?}",
            seen.last()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What the phone does in either case, once its session runs: the tab
/// opens on the phone's start page without the node (the control), the
/// node's announce makes it a row, a tap brings the README, a `/file/`
/// address brings the file, handed out once. The tap-to-paint time back.
async fn walk(
    session: std::sync::Arc<Session>,
    entry: std::net::SocketAddr,
    app: &Path,
    how: &str,
) -> Duration {
    let closed = screen(&session);
    assert!(closed.enabled && !closed.open, "the tab is there, closed");
    let s = std::sync::Arc::clone(&session);
    tokio::task::spawn_blocking(move || {
        browse(&s, "tab", "");
        let first = screen(&s);
        assert!(
            first
                .rows
                .iter()
                .all(|row| !row.text.contains("Testknoten")),
            "no node before its announce: {:?}",
            first.rows
        );
        assert!(
            page_text(&first).contains("Tap a node"),
            "the phone's start page"
        );
    })
    .await
    .expect("the tab");
    let page_storage = tempfile::tempdir().expect("page node storage");
    let (node_hash, node_task) = page_node(entry, page_storage.path()).await;

    let s = std::sync::Arc::clone(&session);
    let (tap_to_paint, page) = tokio::task::spawn_blocking(move || {
        let listed = until(&s, 60, "announced node in the list", |shown| {
            shown
                .rows
                .iter()
                .any(|row| row.kind == "item" && row.text.contains("Testknoten"))
        });
        let row = listed
            .last()
            .and_then(|shown| {
                shown
                    .rows
                    .iter()
                    .find(|row| row.text.contains("Testknoten"))
            })
            .expect("the row")
            .node
            .clone();
        let tapped = Instant::now();
        browse(&s, "activate", &row);
        let painted = until(&s, 60, "README page", |shown| {
            page_text(shown).contains("Reticulum")
        });
        (tapped.elapsed(), painted.last().cloned().expect("a screen"))
    })
    .await
    .expect("the page");
    println!(
        "TAP_TO_FIRST_PAINT {} ms for a {} B micron page through {how}",
        tap_to_paint.as_millis(),
        README.len()
    );
    // The field says the page's name (1621); its node is the dest.
    assert_eq!(page.address, "Testknoten");
    assert_eq!(page.dest, hex(&node_hash));
    assert!(
        page.total > 100,
        "the whole README laid out: {} lines",
        page.total
    );

    let s = std::sync::Arc::clone(&session);
    let file = tokio::task::spawn_blocking(move || {
        s.request(Request::Browse {
            verb: "go".to_string(),
            node: String::new(),
            text: format!("{}:/file/hello.bin", hex(&node_hash)),
            cols: 0,
            rows: 0,
        })
        .expect("go");
        let got = until(&s, 60, "downloaded file", |shown| !shown.file.is_empty());
        let handed = got.last().cloned().expect("a screen");
        assert!(screen(&s).file.is_empty(), "handed out once");
        handed
    })
    .await
    .expect("the file");
    assert_eq!(file.file_name, FILE_NAME);
    assert_eq!(std::fs::read(&file.file).expect("the kept file"), FILE);
    assert!(
        Path::new(&file.file).starts_with(app.join("files").join("browse")),
        "{}",
        file.file
    );
    node_task.abort();
    tap_to_paint
}

/// The mailbox on the phone: its node is the phone's way out, and lent.
#[tokio::test(flavor = "multi_thread")]
async fn a_tapped_node_brings_its_page_and_a_file_through_the_phones_mailbox_node() {
    let rns = tempfile::tempdir().expect("transport storage");
    let mut transport = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .add_tcp_server("127.0.0.1:0".parse().expect("an address"))
        .storage_path(rns.path().to_path_buf())
        .build_sync()
        .expect("the transport node");
    transport.start().await.expect("start it");
    let entry = *transport
        .tcp_listen_addrs()
        .first()
        .expect("its TCP server");

    // The phone: its mailbox here, dialling the transport node, and the
    // Browse tab switched on in its config.
    let phone = tempfile::tempdir().expect("app dir");
    let mailbox = phone.path().join(loquax_android::mailbox::DIR);
    std::fs::create_dir_all(&mailbox).expect("mailbox dir");
    std::fs::write(
        mailbox.join(loquax_android::mailbox::SERVERS_FILE),
        format!("{entry}\n"),
    )
    .expect("servers file");
    std::fs::write(phone.path().join("config"), "browse_tab = on\n").expect("config");
    let start = Start {
        dir: phone.path().display().to_string(),
        here: true,
        lang: Some("en".to_string()),
        ..Start::default()
    };
    let session = tokio::task::spawn_blocking(move || {
        Session::start(&start).unwrap_or_else(|error| panic!("{error:?}"))
    })
    .await
    .expect("the session");
    assert!(session.is_here());
    let session = std::sync::Arc::new(session);
    walk(
        std::sync::Arc::clone(&session),
        entry,
        phone.path(),
        "the phone's mailbox node",
    )
    .await;

    let s = std::sync::Arc::try_unwrap(session).unwrap_or_else(|_| panic!("one session"));
    tokio::task::spawn_blocking(move || s.stop())
        .await
        .expect("stopped");
    transport.stop().await.expect("stop the transport node");
}

/// The mailbox on a computer, beside a transport node: the phone's leaf
/// reaches the network through that node, and is lent.
#[tokio::test(flavor = "multi_thread")]
async fn a_tapped_node_brings_its_page_and_a_file_through_the_phones_leaf() {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let instance = format!(
        "loquax-browse-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let rns = tempfile::tempdir().expect("transport config dir");
    let mut transport = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .add_tcp_server("127.0.0.1:0".parse().expect("an address"))
        .storage_path(rns.path().join("storage"))
        .build_sync()
        .expect("the transport node");
    transport.start().await.expect("start it");
    let entry = *transport
        .tcp_listen_addrs()
        .first()
        .expect("its TCP server");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The mailbox, with a token for the phone.
    let home = tempfile::tempdir().expect("home node dir");
    let identity =
        loquax::identity::load_or_create(&home.path().join("identity")).expect("identity");
    let address = loquax::engine::delivery_address(&identity).expect("address");
    let token = [0x42; 16];
    loquax::store::Store::open_in(home.path(), address)
        .expect("the home store")
        .issue_token(&token, "phone", leviculum_std::local_time::unix_now_secs())
        .expect("a token");
    let code = loquax::home::code::HomeCode {
        public_key: identity.public_key_bytes(),
        token,
    }
    .encode();
    let storage = home.path().join("storage");
    std::fs::create_dir_all(&storage).expect("daemon storage");
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(loquax::home::daemon::run(
        loquax::home::daemon::DaemonOptions {
            home: home.path().to_path_buf(),
            upstream: loquax::home::daemon::DaemonUpstream::Shared {
                instance: instance.clone(),
                config_dir: rns.path().to_path_buf(),
                mailbox: Box::new(|| {
                    Box::pin(async { loquax::mailbox_state::MailboxRun { input: None } })
                }),
                rendezvous: Vec::new(),
                tcp: Default::default(),
            },
            storage_dir: storage,
            identity,
            display_name: b"home".to_vec(),
            archive_cap_bytes: 1 << 20,
            token_ttl_secs: 3600,
            policy: loquax::home::policy(&loquax::config::Config::default()),
            preferred_pn: None,
            pn_source: "none",
            sync_bound_ms: None,
            fetch_budget: Duration::from_secs(10),
            wake: true,
            stop: Some(stopped),
            network: Default::default(),
            lends: Vec::new(),
        },
    ));

    let phone = tempfile::tempdir().expect("app dir");
    std::fs::write(phone.path().join("config"), "browse_tab = on\n").expect("config");
    let start = Start {
        dir: phone.path().display().to_string(),
        server: Some(entry.to_string()),
        code: Some(code),
        name: Some("Pixel".to_string()),
        lang: Some("en".to_string()),
        ..Start::default()
    };
    let session = tokio::task::spawn_blocking(move || {
        Session::start(&start).unwrap_or_else(|error| panic!("{error:?}"))
    })
    .await
    .expect("the session");
    assert!(!session.is_here());
    let session = std::sync::Arc::new(session);
    walk(
        std::sync::Arc::clone(&session),
        entry,
        phone.path(),
        "the phone's leaf",
    )
    .await;

    let s = std::sync::Arc::try_unwrap(session).unwrap_or_else(|_| panic!("one session"));
    tokio::task::spawn_blocking(move || s.stop())
        .await
        .expect("stopped");
    let _ = stop.send(());
    daemon
        .await
        .expect("the daemon task")
        .expect("the daemon stopped cleanly");
    transport.stop().await.expect("stop the transport node");
}

fn hex(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
