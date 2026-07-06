//! Drop-in interop: `lnomad`'s shared-instance fetch backend against BOTH a Rust
//! `lnsd` daemon and a Python `rnsd` daemon, using the same Python NomadNet page
//! node in each case.
//!
//! The proof Lew asked for is that lnomad is a genuine drop-in client: the exact
//! same `lnomad::Session::fetch` driver, pointed at either daemon over the shared
//! instance IPC socket, retrieves the exact same Python-served page bytes.
//!
//! Topology (identical for both daemons; only the middle box changes):
//! ```text
//! lnomad Session ── Unix socket ── daemon ── TCP ── Python page node
//! (IPC initiator)                  (lnsd or rnsd)    (nomad_page handlers)
//! ```
//!
//! Coverage per daemon:
//! - `/page/small.mu`  — single-packet response, byte-identical to Python.
//! - `/page/large.mu`  — `is_response` Resource (> max link MDU), byte-identical.
//! - `/page/echo.mu`   — query fields round-trip as a `var_*` msgpack map.
//! - an unregistered path — a clean [`FetchError::Timeout`], never a hang.
//!
//! ## Running
//!
//! ```sh
//! cargo test -p leviculum-std --test rnsd_interop lnomad_fetch -- --test-threads=1
//! ```

use std::net::SocketAddr;
use std::time::Duration;

use leviculum_std::driver::ReticulumNodeBuilder;

use lnomad::fetch::{FetchError, Session};
use lnomad::url::parse_url;

use crate::harness::{find_available_ports, DestinationInfo, TestDaemon};

/// Stand up the Python NomadNet page node, dial it into the daemon's TCP server,
/// register its page handlers, and announce it. Returns the node and its
/// destination info (whose `hash` is the URL destination).
async fn start_page_node(
    page_rns_port: u16,
    page_cmd_port: u16,
    daemon_tcp_port: u16,
) -> (TestDaemon, DestinationInfo) {
    let page = TestDaemon::start_with_ports(page_rns_port, page_cmd_port)
        .await
        .expect("start Python page node");

    let dest_info = page
        .register_destination("nomadnetwork", &["node"])
        .await
        .expect("register page destination");
    page.set_proof_strategy(&dest_info.hash, "PROVE_ALL")
        .await
        .expect("set proof strategy");
    page.register_page_request_handler(&dest_info.hash)
        .await
        .expect("register page handlers");

    // The page node dials the daemon's TCP server, joining the same mesh as the
    // lnomad IPC client.
    page.add_client_interface("127.0.0.1", daemon_tcp_port, Some("to-daemon"))
        .await
        .expect("dial daemon TCP server");

    // Let the TCP link establish before announcing so the announce reaches the
    // daemon (and via it the IPC client).
    tokio::time::sleep(Duration::from_secs(1)).await;
    page.announce_destination(&dest_info.hash, b"lnomad-page-node")
        .await
        .expect("announce page destination");

    (page, dest_info)
}

/// Run the full page-fetch suite against a connected session and page node.
async fn run_page_suite(session: &mut Session, page: &TestDaemon, dest_hex: &str) {
    // --- small: single-packet response, byte-identical ---
    let served_small = page
        .get_page_content("/page/small.mu")
        .await
        .expect("get small page content");
    let target = parse_url(&format!("{dest_hex}:/page/small.mu"), None).expect("parse small url");
    let got_small = session
        .fetch(&target, Duration::from_secs(30))
        .await
        .expect("fetch small page");
    assert_eq!(
        got_small, served_small,
        "small page must be byte-identical to what Python served"
    );

    // --- large: is_response Resource path, byte-identical ---
    let served_large = page
        .get_page_content("/page/large.mu")
        .await
        .expect("get large page content");
    assert!(
        served_large.len() > 262_144,
        "large page must exceed the max link MDU to force the resource path (got {})",
        served_large.len()
    );
    let target = parse_url(&format!("{dest_hex}:/page/large.mu"), None).expect("parse large url");
    let got_large = session
        .fetch(&target, Duration::from_secs(60))
        .await
        .expect("fetch large page");
    assert_eq!(
        got_large, served_large,
        "large page must be byte-identical to what Python served"
    );

    // --- echo: query fields round-trip as a var_ msgpack map ---
    let target = parse_url(&format!("{dest_hex}:/page/echo.mu`field=value|n=42"), None)
        .expect("parse echo url");
    let raw = session
        .request(&target, Duration::from_secs(30))
        .await
        .expect("request echo page");
    let mut cursor = std::io::Cursor::new(raw.as_slice());
    let value = rmpv::decode::read_value(&mut cursor).expect("decode echo response");
    let map = match value {
        rmpv::Value::Map(entries) => entries,
        other => panic!("echo response must be a msgpack map, got {other:?}"),
    };
    let mut pairs: Vec<(String, String)> = map
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().unwrap_or_default().to_string(),
                v.as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("var_field".to_string(), "value".to_string()),
            ("var_n".to_string(), "42".to_string()),
        ],
        "echo must return the query fields as a var_ map"
    );

    // --- unregistered path: clean timeout, no hang ---
    let target =
        parse_url(&format!("{dest_hex}:/page/does-not-exist.mu"), None).expect("parse unknown url");
    let result = session.fetch(&target, Duration::from_secs(3)).await;
    assert!(
        matches!(result, Err(FetchError::Timeout)),
        "an unregistered path must surface a clean Timeout, got {result:?}"
    );
}

/// lnomad fetches Python-served pages through a Rust `lnsd` shared instance.
#[tokio::test]
async fn lnomad_fetches_pages_via_lnsd_shared_instance() {
    let (ports, _alloc) = find_available_ports::<3>().await.expect("allocate ports");
    let [daemon_tcp_port, page_rns_port, page_cmd_port] = ports;
    let daemon_tcp: SocketAddr = format!("127.0.0.1:{daemon_tcp_port}").parse().unwrap();
    let instance_name = format!("lnomad-lnsd-{}", std::process::id());

    // The lnsd daemon owns the abstract Unix socket and a TCP server for the
    // page node to dial.
    let daemon_storage = tempfile::tempdir().expect("daemon storage");
    let mut daemon = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.clone())
        .add_tcp_server(daemon_tcp)
        .storage_path(daemon_storage.path().to_path_buf())
        .build_sync()
        .expect("build lnsd daemon");
    daemon.start().await.expect("start lnsd daemon");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let (page, dest_info) = start_page_node(page_rns_port, page_cmd_port, daemon_tcp_port).await;

    let session_storage = tempfile::tempdir().expect("session storage");
    let mut session = Session::connect_to(&instance_name, session_storage.path().to_path_buf())
        .await
        .expect("connect lnomad session to lnsd");

    run_page_suite(&mut session, &page, &dest_info.hash).await;

    session.close().await.expect("close session");
    daemon.stop().await.expect("stop lnsd daemon");
}

/// lnomad fetches Python-served pages through a Python `rnsd` shared instance:
/// the same driver and page node, drop-in against the reference daemon.
#[tokio::test]
async fn lnomad_fetches_pages_via_rnsd_shared_instance() {
    let (ports, _alloc) = find_available_ports::<4>().await.expect("allocate ports");
    let [daemon_rns_port, page_rns_port, daemon_cmd_port, page_cmd_port] = ports;
    let instance_name = format!("lnomad-rnsd-{}", std::process::id());

    // The rnsd daemon owns the abstract Unix socket and a TCP server on its
    // rns_port for the page node to dial.
    let _daemon = TestDaemon::start_with_shared_instance_ports(
        daemon_rns_port,
        daemon_cmd_port,
        &instance_name,
    )
    .await
    .expect("start rnsd shared instance");

    let (page, dest_info) = start_page_node(page_rns_port, page_cmd_port, daemon_rns_port).await;

    let session_storage = tempfile::tempdir().expect("session storage");
    let mut session = Session::connect_to(&instance_name, session_storage.path().to_path_buf())
        .await
        .expect("connect lnomad session to rnsd");

    run_page_suite(&mut session, &page, &dest_info.hash).await;

    session.close().await.expect("close session");
}
