//! End-to-end test of the lblogd NomadNet page node over a real
//! shared-instance IPC path, with no Python and no hardware.
//!
//! Topology (the production shape, all in one process):
//! ```text
//! lnomad Session ── Unix socket ── Rust daemon ── Unix socket ── BlogNode
//! (the browser's                   (share_instance,              (shared-instance
//!  fetch path)                      TCP server)                   client, page node)
//! ```
//!
//! The assertions prove the served content is byte-exactly the renderer
//! output through the real fetch path: the index page and a small post over
//! the single-packet RESPONSE path, a post above the 262144 byte link MDU
//! over the `send_response_resource` fallback, and a clean client timeout
//! for an unknown path.

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::time::Duration;

use leviculum_std::driver::ReticulumNodeBuilder;

use lblogd::node::{BlogNode, BlogNodeConfig};
use lblogd::post::load_posts_dir;
use lblogd::render::{render_index_micron, render_post_micron};
use lnomad::fetch::{FetchError, Session};
use lnomad::url::parse_url;

/// Grab a currently-free localhost TCP port by binding and immediately dropping.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// Write the fixture posts: two small ones and one whose body pushes the
/// rendered Micron well past the 262144 byte negotiated TCP/IPC link MDU, so
/// serving it forces the response Resource path.
fn write_fixture_posts(dir: &Path) {
    std::fs::write(
        dir.join("hello.md"),
        "+++\ntitle = \"Hello Mesh\"\ndate = \"2026-07-01\"\n+++\n\nFirst post, *small* enough for one packet.\n",
    )
    .expect("write hello.md");
    std::fs::write(
        dir.join("second.md"),
        "+++\ntitle = \"Second Post\"\ndate = \"2026-07-05\"\nslug = \"second\"\n+++\n\nAnother small post.\n",
    )
    .expect("write second.md");

    let mut large = String::from(
        "+++\ntitle = \"Large Post\"\ndate = \"2026-07-10\"\nslug = \"large\"\n+++\n\n",
    );
    for i in 0..9000 {
        large.push_str(&format!("Line {i:04} of the large fixture post body.\n"));
    }
    std::fs::write(dir.join("large.md"), large).expect("write large.md");
}

#[tokio::test]
async fn blog_node_serves_pages_end_to_end() {
    // The daemon: a transport node sharing its instance over IPC, standing in
    // for a production lnsd.
    let daemon_tcp: SocketAddr = format!("127.0.0.1:{}", free_port()).parse().unwrap();
    let instance_name = format!("lblogd-b2-test-{}", std::process::id());
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

    // The blog node: a shared-instance client of the daemon (the production
    // topology), serving the fixture posts.
    let posts_dir = tempfile::tempdir().expect("posts dir");
    write_fixture_posts(posts_dir.path());
    let data_dir = tempfile::tempdir().expect("data dir");
    let blog = BlogNode::start(BlogNodeConfig {
        instance_name: instance_name.clone(),
        data_dir: data_dir.path().to_path_buf(),
        posts_dir: posts_dir.path().to_path_buf(),
        display_name: "lblogd test blog".to_string(),
        announce_interval: Duration::from_secs(3600),
    })
    .await
    .expect("start blog node");
    let dest_hex = hex::encode(blog.destination_hash().as_bytes());
    let blog_task = tokio::spawn(blog.run());
    // Let the node's IPC link establish and its announce propagate.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // What the node must serve: exactly the renderer output over the same
    // fixture directory.
    let posts = load_posts_dir(posts_dir.path()).expect("load fixture posts");
    let expected_index = render_index_micron(&posts).into_bytes();
    let small = posts
        .iter()
        .find(|p| p.slug == "second")
        .expect("second fixture post");
    let expected_small = render_post_micron(small).into_bytes();
    let large = posts
        .iter()
        .find(|p| p.slug == "large")
        .expect("large fixture post");
    let expected_large = render_post_micron(large).into_bytes();
    assert!(
        expected_large.len() > 262_144,
        "large post must exceed the max link MDU to force the resource path (got {})",
        expected_large.len()
    );

    // The client: lnomad's real fetch path over its own IPC connection.
    let session_storage = tempfile::tempdir().expect("session storage");
    let app_dir = tempfile::tempdir().expect("session app dir");
    let mut session = Session::connect_to_with_app_dir(
        &instance_name,
        session_storage.path().to_path_buf(),
        Some(app_dir.path().to_path_buf()),
    )
    .await
    .expect("lnomad session connect");

    // Index and a small post: single-packet RESPONSE path, byte-exact.
    let target = parse_url(&format!("{dest_hex}:/page/index.mu"), None).expect("parse index url");
    let page = session
        .fetch(&target, Duration::from_secs(20))
        .await
        .expect("fetch index page");
    assert_eq!(
        page, expected_index,
        "fetched index must be byte-exactly the rendered index"
    );

    let target = parse_url(&format!("{dest_hex}:/page/second.mu"), None).expect("parse post url");
    let page = session
        .fetch(&target, Duration::from_secs(20))
        .await
        .expect("fetch small post");
    assert_eq!(
        page, expected_small,
        "fetched post must be byte-exactly the rendered post"
    );

    // The large post: send_response returns PayloadTooLarge, the node falls
    // back to send_response_resource, and the client reassembles the full
    // page from the is_response Resource.
    let target = parse_url(&format!("{dest_hex}:/page/large.mu"), None).expect("parse large url");
    let page = session
        .fetch(&target, Duration::from_secs(60))
        .await
        .expect("fetch large post over the resource path");
    assert_eq!(
        page, expected_large,
        "large post must round-trip byte-exactly over the resource path"
    );

    // Unknown path: the stack drops it silently (no 404 in the protocol),
    // the client sees a clean timeout, nothing crashes.
    let target = parse_url(&format!("{dest_hex}:/page/nope.mu"), None).expect("parse bad url");
    let result = session.fetch(&target, Duration::from_secs(2)).await;
    assert!(
        matches!(result, Err(FetchError::Timeout)),
        "unknown path must surface a clean Timeout, got {result:?}"
    );

    session.close().await.expect("close session");
    blog_task.abort();
    daemon.stop().await.expect("stop daemon");
}
