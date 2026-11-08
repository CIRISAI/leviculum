//! Live tests against a real i2pd SAM bridge (`#[ignore]` by default).
//!
//! Prerequisite: i2pd running with `sam.enabled = true` (SAM on
//! 127.0.0.1:7656, or wherever `LEVICULUM_I2P_SAM` points). See
//! `docs/src/development-testing.md`. `just i2p-live` starts a private I2P
//! network on loopback for the run (scripts/run-i2p-live.sh on
//! scripts/i2p-privnet.sh, no internet) and points the tests at two of its
//! routers: `LEVICULUM_I2P_SAM` and `LEVICULUM_I2P_SAM_PEER`.
//!
//! What is covered here vs the deterministic mock-SAM tests in `tests.rs`:
//!
//! * `live_session_create_matches_dialect` — fast. Proves a real i2pd accepts
//!   our exact `HELLO` / `SESSION CREATE STYLE=STREAM` dialect and that the
//!   private key it returns parses into a destination whose b32 we can derive.
//!   This is the "reference-first" check: it is the same wire our SAM client
//!   drives in production.
//!
//! * `live_loopback_stream_crosses` — slow (minutes). Builds two real I2P
//!   sessions on the same router and pushes an HDLC frame from a `STREAM
//!   CONNECT` client to a `STREAM ACCEPT` server over live tunnels. This is the
//!   real-network analogue of `announce_crosses_i2p_loopback`, deferred out of
//!   the default suite because tunnel build is slow and network-dependent.
//!
//! Still deferred beyond even these: a real Python rnsd on an `I2PInterface`
//! sharing the same i2pd with our lnsd. That is a multi-daemon, live-network
//! interop scenario; the wire dialect it exercises is exactly what these two
//! tests plus the golden-vector unit tests already pin down.

use std::time::Duration;

use tokio::net::TcpStream;

use super::sam::{self, Destination};

const SAM: &str = "127.0.0.1:7656";

/// The SAM bridge under test: `LEVICULUM_I2P_SAM` if set, i2pd's default
/// otherwise. `just i2p-live` sets it to a router of the private net it
/// starts, so a system i2pd on the default port is neither used nor disturbed.
fn sam_addr() -> String {
    std::env::var("LEVICULUM_I2P_SAM").unwrap_or_else(|_| SAM.to_string())
}

/// The bridge the loopback test's connecting side uses:
/// `LEVICULUM_I2P_SAM_PEER` if set, so on the private net the stream crosses
/// from one router to another; the same bridge as [`sam_addr`] otherwise.
fn peer_sam_addr() -> String {
    std::env::var("LEVICULUM_I2P_SAM_PEER").unwrap_or_else(|_| sam_addr())
}

async fn sam_socket() -> TcpStream {
    sam_socket_at(&sam_addr()).await
}

async fn sam_socket_at(addr: &str) -> TcpStream {
    let mut s = TcpStream::connect(addr)
        .await
        .expect("connect to i2pd SAM (is i2pd running with sam.enabled?)");
    sam::handshake(&mut s, sam::SAM_COMMAND_TIMEOUT)
        .await
        .expect("SAM HELLO handshake");
    s
}

#[tokio::test]
#[ignore = "requires a running i2pd SAM bridge"]
async fn live_session_create_matches_dialect() {
    let mut ctrl = sam_socket().await;
    let reply = sam::command(
        &mut ctrl,
        &sam::session_create(
            "STREAM",
            "reticulum-livetest",
            sam::TRANSIENT_DESTINATION,
            "",
        ),
        sam::SAM_COMMAND_TIMEOUT,
    )
    .await
    .expect("SESSION CREATE reply");
    assert!(reply.ok(), "SESSION CREATE failed: {}", reply.result());

    let priv_key = reply
        .get("DESTINATION")
        .expect("SESSION STATUS should carry the generated DESTINATION");
    let dest = Destination::from_private_base64(priv_key)
        .expect("generated private key parses into a destination");
    let b32 = dest.base32();
    assert_eq!(b32.len(), 52, "b32 label must be 52 chars, got {b32}");
    // 1576: a TRANSIENT session asks for Ed25519, the reference's type; i2pd
    // answered the request without it with a DSA-SHA1 (type 0) destination.
    assert_eq!(
        dest.sig_type(),
        sam::SIGNATURE_TYPE_ED25519,
        "the router made a destination of another signature type"
    );
    println!(
        "live i2pd session destination: {b32}.b32.i2p signature type {}",
        dest.sig_type()
    );
}

#[tokio::test]
#[ignore = "requires a running i2pd SAM bridge; builds real tunnels (minutes)"]
async fn live_loopback_stream_crosses() {
    let t0 = std::time::Instant::now();
    // Server session with a persistent (transient-then-known) destination.
    let mut srv_ctrl = sam_socket().await;
    let reply = sam::command(
        &mut srv_ctrl,
        &sam::session_create("STREAM", "reticulum-srv", sam::TRANSIENT_DESTINATION, ""),
        sam::SAM_COMMAND_TIMEOUT,
    )
    .await
    .expect("server SESSION CREATE");
    assert!(reply.ok(), "server SESSION CREATE: {}", reply.result());
    let srv_dest = Destination::from_private_base64(reply.get("DESTINATION").unwrap()).unwrap();
    let srv_full = srv_dest.public_base64();
    println!(
        "server reachable at {}.b32.i2p, session after {:.1} s",
        srv_dest.base32(),
        t0.elapsed().as_secs_f64()
    );

    // Server accept task: waits for the inbound connection, reads the peer
    // destination line, then the HDLC frame.
    let accept = tokio::spawn(async move {
        let mut a = sam_socket().await;
        let r = sam::command(
            &mut a,
            &sam::stream_accept("reticulum-srv", false),
            sam::SAM_COMMAND_TIMEOUT,
        )
        .await
        .expect("STREAM ACCEPT");
        assert!(r.ok(), "STREAM ACCEPT: {}", r.result());
        // First line once a peer connects: its destination.
        let _peer = sam::read_line(&mut a).await.expect("peer dest line");
        // Then the raw stream. Read the HDLC-framed bytes.
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 64];
        let n = a.read(&mut buf).await.expect("read stream");
        buf.truncate(n);
        buf
    });

    // Give the accept a moment to register before connecting.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Client session + STREAM CONNECT to the server's full destination, on
    // the peer router when there is one.
    let peer_sam = peer_sam_addr();
    let mut cli_ctrl = sam_socket_at(&peer_sam).await;
    let reply = sam::command(
        &mut cli_ctrl,
        &sam::session_create("STREAM", "reticulum-cli", sam::TRANSIENT_DESTINATION, ""),
        sam::SAM_COMMAND_TIMEOUT,
    )
    .await
    .expect("client SESSION CREATE");
    assert!(reply.ok(), "client SESSION CREATE: {}", reply.result());
    println!(
        "client session on {peer_sam} after {:.1} s",
        t0.elapsed().as_secs_f64()
    );

    let mut stream = sam_socket_at(&peer_sam).await;
    // Building the path to a brand-new destination can take a while; retry the
    // connect until the tunnels are up.
    let connect = tokio::time::timeout(Duration::from_secs(240), async {
        loop {
            let r = sam::command(
                &mut stream,
                &sam::stream_connect("reticulum-cli", &srv_full, false),
                sam::SAM_COMMAND_TIMEOUT,
            )
            .await
            .expect("STREAM CONNECT");
            if r.ok() {
                break;
            }
            // Reconnect a fresh socket and retry.
            stream = sam_socket_at(&peer_sam).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
    .await;
    assert!(
        connect.is_ok(),
        "STREAM CONNECT did not succeed within timeout"
    );
    println!("stream connected after {:.1} s", t0.elapsed().as_secs_f64());

    // Send an HDLC-framed packet over the live tunnel.
    use leviculum_core::framing::hdlc;
    use tokio::io::AsyncWriteExt;
    let mut framed = Vec::new();
    hdlc::frame(b"live-i2p-crossing", &mut framed);
    stream.write_all(&framed).await.expect("write framed");

    let received = tokio::time::timeout(Duration::from_secs(30), accept)
        .await
        .expect("timeout waiting for server to receive")
        .expect("accept task panicked");
    // The server read the raw HDLC frame; deframe it.
    let mut deframer = hdlc::Deframer::new();
    let frames: Vec<_> = deframer
        .process(&received)
        .into_iter()
        .filter_map(|r| match r {
            hdlc::DeframeResult::Frame(d) => Some(d),
            _ => None,
        })
        .collect();
    assert_eq!(frames, vec![b"live-i2p-crossing".to_vec()]);
    println!("frame across after {:.1} s", t0.elapsed().as_secs_f64());
}
