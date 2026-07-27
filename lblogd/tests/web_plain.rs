//! The plaintext development mode of the web server: with ACME disabled the
//! HTTP listener serves the blog itself instead of redirecting to HTTPS.
//!
//! The requests go over a real TCP socket rather than through `tower`'s
//! oneshot service call, so this covers what the router unit tests cannot:
//! that `run_web` actually binds, accepts, and answers on `http_bind` without
//! touching Let's Encrypt. The ACME path stays compile-verified only (it
//! needs a publicly reachable domain), but the branch guarding it is asserted
//! here too.

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use lblogd::content::{Reloader, SnapshotRx};
use lblogd::web::{run_web, AcmeSettings, WebConfig, WebError};

/// Grab a currently-free localhost TCP port by binding and immediately dropping.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

fn write_fixture_posts(dir: &Path) {
    std::fs::write(
        dir.join("hello.md"),
        "+++\ntitle = \"Hello Mesh\"\ndate = \"2026-07-01\"\n+++\n\nFirst post, **small** enough for one packet.\n",
    )
    .expect("write hello.md");
}

/// Wait until something accepts connections on `addr`, so the assertions do
/// not race the listener coming up.
async fn wait_for_listener(addr: SocketAddr) {
    for _ in 0..100 {
        if TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no listener on {addr} after 2 s");
}

/// One plain HTTP/1.1 GET, returning the whole raw response. `Connection:
/// close` makes the server end the stream, so reading to EOF terminates.
async fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read response");
    String::from_utf8_lossy(&raw).into_owned()
}

/// Start a plaintext server over `content` on a free port and wait until it
/// accepts connections.
async fn serve_plain(content: SnapshotRx) -> SocketAddr {
    let bind: SocketAddr = format!("127.0.0.1:{}", free_port())
        .parse()
        .expect("parse bind addr");
    tokio::spawn(run_web(
        WebConfig {
            acme: None,
            http_bind: bind,
            // Unused in plaintext mode; a bogus value must not be bound.
            https_bind: "127.0.0.1:1".parse().expect("parse https bind"),
        },
        content,
    ));
    wait_for_listener(bind).await;
    bind
}

#[tokio::test]
async fn plain_http_serves_the_blog_without_acme() {
    let posts_dir = tempfile::tempdir().expect("posts dir");
    write_fixture_posts(posts_dir.path());
    let (_reloader, content) = Reloader::new(posts_dir.path()).expect("initial load");
    let bind = serve_plain(content).await;

    let index = http_get(bind, "/").await;
    assert!(
        index.starts_with("HTTP/1.1 200"),
        "index must be served, not redirected: {index}"
    );
    assert!(index.contains("Hello Mesh"), "{index}");
    assert!(index.contains("/posts/hello-mesh"), "{index}");

    let post = http_get(bind, "/posts/hello-mesh").await;
    assert!(post.starts_with("HTTP/1.1 200"), "{post}");
    assert!(post.contains("<strong>small</strong>"), "{post}");

    let missing = http_get(bind, "/nicht-da").await;
    assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");
}

#[tokio::test]
async fn a_reload_reaches_the_running_listener() {
    // The listener is bound once and never rebound, so this proves publishing
    // needs no restart: same socket, new content.
    let posts_dir = tempfile::tempdir().expect("posts dir");
    write_fixture_posts(posts_dir.path());
    let (reloader, content) = Reloader::new(posts_dir.path()).expect("initial load");
    let bind = serve_plain(content).await;

    assert!(
        http_get(bind, "/posts/zweiter-post")
            .await
            .starts_with("HTTP/1.1 404"),
        "the post does not exist yet"
    );

    std::fs::write(
        posts_dir.path().join("zweiter.md"),
        "+++\ntitle = \"Zweiter Post\"\ndate = \"2026-07-27\"\n+++\n\nFrisch dazugekommen.\n",
    )
    .expect("write second post");
    reloader.reload().expect("reload");

    let page = http_get(bind, "/posts/zweiter-post").await;
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    assert!(page.contains("Zweiter Post"), "{page}");

    let index = http_get(bind, "/").await;
    assert!(index.contains("/posts/zweiter-post"), "{index}");

    // And removing it again takes it back out, on the same listener.
    std::fs::remove_file(posts_dir.path().join("zweiter.md")).expect("remove second post");
    reloader.reload().expect("reload after removal");
    assert!(
        http_get(bind, "/posts/zweiter-post")
            .await
            .starts_with("HTTP/1.1 404"),
        "a removed post must stop being served"
    );
}

#[tokio::test]
async fn a_failed_reload_keeps_the_server_serving() {
    // A typo in a post must not take a running blog offline.
    let posts_dir = tempfile::tempdir().expect("posts dir");
    write_fixture_posts(posts_dir.path());
    let (reloader, content) = Reloader::new(posts_dir.path()).expect("initial load");
    let bind = serve_plain(content).await;

    std::fs::write(
        posts_dir.path().join("kaputt.md"),
        "+++\ntitle = \"Kaputt\"\ndate = \"2026-13-45\"\n+++\n\nText.\n",
    )
    .expect("write broken post");
    let err = reloader
        .reload()
        .expect_err("a bad date must fail the load");
    assert!(err.to_string().contains("kaputt.md"), "{err}");

    let page = http_get(bind, "/posts/hello-mesh").await;
    assert!(
        page.starts_with("HTTP/1.1 200"),
        "the previous content must still be served: {page}"
    );
}

#[tokio::test]
async fn acme_mode_still_requires_domains() {
    let posts_dir = tempfile::tempdir().expect("posts dir");
    write_fixture_posts(posts_dir.path());
    let cache_dir = tempfile::tempdir().expect("acme cache dir");
    let (_reloader, content) = Reloader::new(posts_dir.path()).expect("initial load");

    let err = run_web(
        WebConfig {
            acme: Some(AcmeSettings {
                domains: Vec::new(),
                cache_dir: cache_dir.path().to_path_buf(),
                contact_email: "ops@example.org".to_string(),
                staging: true,
            }),
            http_bind: "127.0.0.1:1".parse().expect("parse http bind"),
            https_bind: "127.0.0.1:1".parse().expect("parse https bind"),
        },
        content,
    )
    .await
    .expect_err("empty domains must be rejected");
    assert!(matches!(err, WebError::NoDomains), "{err}");
}
