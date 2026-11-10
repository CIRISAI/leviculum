//! Deterministic integration tests for the I2P interface against a mock SAM
//! bridge.
//!
//! These run in `just standard` with no i2pd: an in-process TCP listener speaks
//! just enough SAM v3 to drive the interface state machine, and splices a
//! client `STREAM CONNECT` to a server `STREAM ACCEPT` so a Reticulum packet
//! actually crosses a simulated I2P link end to end. This proves session setup,
//! the `SILENT=false` peer-destination handshake, HDLC framing in both
//! directions, and the `new_iface_tx` registration path — everything except the
//! live-network tunnel build, which is covered by the `#[ignore]` tests in
//! `i2pd_live.rs`.

use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use super::sam::{self, GOLDEN_B32, GOLDEN_PRIV, GOLDEN_PUB};
use super::*;

/// A minimal SAM v3 bridge for tests. Per connection it does the `HELLO`
/// handshake, then dispatches one command. `STREAM CONNECT` sockets are handed
/// to a rendezvous; `STREAM ACCEPT` sockets pull from it, write the connecting
/// peer's destination line (`SILENT=false` behaviour), and splice the two
/// sockets so bytes flow client <-> server.
async fn start_mock_sam() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Rendezvous between STREAM CONNECT (producer) and STREAM ACCEPT (consumer).
    let (connect_tx, connect_rx) = mpsc::unbounded_channel::<TcpStream>();
    let connect_rx = Arc::new(Mutex::new(connect_rx));

    tokio::spawn(async move {
        loop {
            let (mut sock, _peer) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let connect_tx = connect_tx.clone();
            let connect_rx = Arc::clone(&connect_rx);
            tokio::spawn(async move {
                // HELLO handshake.
                let hello = match sam::read_line(&mut sock).await {
                    Ok(l) => l,
                    Err(_) => return,
                };
                if !hello.starts_with("HELLO") {
                    return;
                }
                let _ = sock.write_all(b"HELLO REPLY RESULT=OK VERSION=3.1\n").await;

                // One command per connection.
                let cmd = match sam::read_line(&mut sock).await {
                    Ok(l) => l,
                    Err(_) => return,
                };

                if cmd.starts_with("SESSION CREATE") {
                    let reply = format!("SESSION STATUS RESULT=OK DESTINATION={GOLDEN_PRIV}\n");
                    let _ = sock.write_all(reply.as_bytes()).await;
                    // Hold the session socket open (drain to EOF).
                    let mut scratch = [0u8; 256];
                    loop {
                        match sock.read(&mut scratch).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                } else if cmd.starts_with("NAMING LOOKUP") {
                    let reply = format!("NAMING REPLY RESULT=OK NAME=x VALUE={GOLDEN_PUB}\n");
                    let _ = sock.write_all(reply.as_bytes()).await;
                } else if cmd.starts_with("STREAM CONNECT") {
                    let _ = sock.write_all(b"STREAM STATUS RESULT=OK\n").await;
                    // Hand the raw stream socket to a waiting STREAM ACCEPT.
                    let _ = connect_tx.send(sock);
                } else if cmd.starts_with("STREAM ACCEPT") {
                    let _ = sock.write_all(b"STREAM STATUS RESULT=OK\n").await;
                    // Wait for an inbound connection to splice with.
                    let mut connect_sock = {
                        let mut rx = connect_rx.lock().await;
                        match rx.recv().await {
                            Some(s) => s,
                            None => return,
                        }
                    };
                    // SILENT=false: deliver the connecting peer's destination
                    // line before the stream bytes.
                    let dest_line = format!("{GOLDEN_PUB}\n");
                    let _ = sock.write_all(dest_line.as_bytes()).await;
                    // Splice the two sockets together.
                    let _ = tokio::io::copy_bidirectional(&mut sock, &mut connect_sock).await;
                }
            });
        }
    });

    addr
}

fn out(data: &[u8]) -> OutgoingPacket {
    OutgoingPacket {
        ph: None,
        peer: None,
        data: data.to_vec().into(),
        high_priority: false,
    }
}

/// End-to-end: bring up one server (`connectable`) and one client I2P
/// sub-interface against the mock, then prove a packet crosses the link in both
/// directions. Also asserts the server persists its SAM private key for a
/// stable `.b32.i2p` address.
#[tokio::test]
async fn announce_crosses_i2p_loopback() {
    let mock = start_mock_sam().await;
    let tmp = tempfile::tempdir().unwrap();
    let keyfile = tmp.path().join("i2p").join("srv.i2p");

    let (new_tx, mut new_rx) = mpsc::channel::<InterfaceHandle>(8);
    let next_id = Arc::new(AtomicUsize::new(100));

    spawn_i2p_server(I2pServerConfig {
        sam_address: mock.to_string(),
        keyfile: keyfile.clone(),
        buffer_size: 32,
        name_prefix: "srv".to_string(),
        reconnect_wait: Duration::from_millis(200),
        timeouts: I2pTimeouts::default(),
        next_id,
        new_interface_tx: new_tx,
        ingress_control: false,
        ifac: None,
        outbound_socket_hook: None,
        endpoint: Arc::new(I2pEndpoint::new(true)),
        section: "srv".to_string(),
        parent_id: 0,
        inventory: crate::interfaces::inventory::InterfaceInventory::shared(),
        mode: leviculum_core::traits::InterfaceMode::default(),
    });

    let mut client = spawn_i2p_client(I2pClientConfig {
        id: InterfaceId(1),
        name: "cli".to_string(),
        sam_address: mock.to_string(),
        peer: "somepeer.b32.i2p".to_string(),
        buffer_size: 32,
        reconnect_wait: Duration::from_millis(200),
        timeouts: I2pTimeouts::default(),
        ifac: None,
        reconnect_notify: None,
        ingress_control: false,
        outbound_socket_hook: None,
        tunnel: Arc::new(I2pTunnelState::default()),
    });

    // The server accept loop spawns an interface once the client connects.
    let mut server_iface = tokio::time::timeout(Duration::from_secs(5), new_rx.recv())
        .await
        .expect("timeout waiting for accepted I2P interface")
        .expect("new-interface channel closed");
    assert!(server_iface.info.name.starts_with("srv/"));
    assert_eq!(server_iface.info.hw_mtu, Some(1064));

    // Client -> server.
    client.outgoing.send(out(b"announce-c2s")).await.unwrap();
    let pkt = tokio::time::timeout(Duration::from_secs(5), server_iface.incoming.recv())
        .await
        .expect("timeout: client->server packet")
        .expect("server incoming closed");
    assert_eq!(pkt.data, b"announce-c2s");

    // Server -> client.
    server_iface
        .outgoing
        .send(out(b"announce-s2c"))
        .await
        .unwrap();
    let pkt = tokio::time::timeout(Duration::from_secs(5), client.incoming.recv())
        .await
        .expect("timeout: server->client packet")
        .expect("client incoming closed");
    assert_eq!(pkt.data, b"announce-s2c");

    // The server persisted its private key, so its address is stable across
    // restarts, and the key yields the golden b32.
    let saved = std::fs::read_to_string(&keyfile).unwrap();
    assert_eq!(saved.trim(), GOLDEN_PRIV);
    let dest = sam::Destination::from_private_base64(saved.trim()).unwrap();
    assert_eq!(dest.base32(), GOLDEN_B32);
}

/// A packet with a payload byte equal to the HDLC flag/escape must survive the
/// round trip (framing escapes it correctly on the SAM stream).
#[tokio::test]
async fn hdlc_escaping_survives_i2p_stream() {
    let mock = start_mock_sam().await;
    let (new_tx, mut new_rx) = mpsc::channel::<InterfaceHandle>(8);
    let next_id = Arc::new(AtomicUsize::new(200));
    let tmp = tempfile::tempdir().unwrap();

    spawn_i2p_server(I2pServerConfig {
        sam_address: mock.to_string(),
        keyfile: tmp.path().join("k.i2p"),
        buffer_size: 32,
        name_prefix: "srv".to_string(),
        reconnect_wait: Duration::from_millis(200),
        timeouts: I2pTimeouts::default(),
        next_id,
        new_interface_tx: new_tx,
        ingress_control: false,
        ifac: None,
        outbound_socket_hook: None,
        endpoint: Arc::new(I2pEndpoint::new(true)),
        section: "srv".to_string(),
        parent_id: 0,
        inventory: crate::interfaces::inventory::InterfaceInventory::shared(),
        mode: leviculum_core::traits::InterfaceMode::default(),
    });
    let client = spawn_i2p_client(I2pClientConfig {
        id: InterfaceId(1),
        name: "cli".to_string(),
        sam_address: mock.to_string(),
        peer: "peer.b32.i2p".to_string(),
        buffer_size: 32,
        reconnect_wait: Duration::from_millis(200),
        timeouts: I2pTimeouts::default(),
        ifac: None,
        reconnect_notify: None,
        ingress_control: false,
        outbound_socket_hook: None,
        tunnel: Arc::new(I2pTunnelState::default()),
    });

    let mut server_iface = tokio::time::timeout(Duration::from_secs(5), new_rx.recv())
        .await
        .expect("timeout waiting for accepted interface")
        .unwrap();

    // Payload full of FLAG (0x7E) and ESC (0x7D) bytes plus their escaped forms.
    let payload = vec![0x7e, 0x7d, 0x5e, 0x5d, 0x7e, 0x7e, 0x00, 0x7d];
    client.outgoing.send(out(&payload)).await.unwrap();
    let pkt = tokio::time::timeout(Duration::from_secs(5), server_iface.incoming.recv())
        .await
        .expect("timeout: escaped packet")
        .unwrap();
    assert_eq!(pkt.data, payload);
}

/// The outbound-socket hook fires for every TCP dial the I2P client makes
/// toward the SAM bridge: session control, NAMING LOOKUP (`.i2p` peer), and
/// the stream socket — so an embedder's socket policy covers the I2P
/// transport, not just plain TCP interfaces.
#[tokio::test]
async fn socket_hook_covers_every_sam_dial() {
    let mock = start_mock_sam().await;

    let dials = Arc::new(AtomicUsize::new(0));
    let recorded = Arc::clone(&dials);
    let hook: crate::socket_hook::OutboundSocketHook = Arc::new(move |_fd| {
        recorded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });

    let handle = spawn_i2p_client(I2pClientConfig {
        id: InterfaceId(1),
        name: "cli".to_string(),
        sam_address: mock.to_string(),
        peer: "somepeer.b32.i2p".to_string(),
        buffer_size: 32,
        reconnect_wait: Duration::from_secs(30),
        timeouts: I2pTimeouts::default(),
        ifac: None,
        reconnect_notify: None,
        ingress_control: false,
        outbound_socket_hook: Some(hook),
        tunnel: Arc::new(I2pTunnelState::default()),
    });
    handle
        .ready
        .wait(Duration::from_secs(5))
        .await
        .expect("client should establish its stream against the mock SAM");

    assert_eq!(
        dials.load(std::sync::atomic::Ordering::Relaxed),
        3,
        "hook must fire for the control, lookup, and stream dials"
    );
}

#[test]
fn keyfile_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("i2p").join("iface.i2p");
    assert!(load_keyfile(&path).is_none());
    save_keyfile(&path, GOLDEN_PRIV);
    assert_eq!(load_keyfile(&path).as_deref(), Some(GOLDEN_PRIV));
}

/// Poll `cond` until it holds or `deadline` passes. Returns what it last saw.
async fn wait_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

/// A TCP listener that accepts and then says nothing at all, holding every
/// connection open so the client sees neither a reply nor an EOF. Counts the
/// connections it accepted.
async fn start_silent_listener(accepts: Arc<AtomicUsize>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // Hold every accepted socket: dropping one would close it, and the
        // client would fail on the EOF rather than on the deadline under test.
        let mut held = Vec::new();
        loop {
            match listener.accept().await {
                Ok((sock, _)) => {
                    accepts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    held.push(sock);
                }
                Err(_) => return,
            }
        }
    });
    addr
}

/// Codeberg #284, half one, end to end: a SAM bridge that accepts the TCP
/// connection and never answers must not wedge the client task. Before the
/// deadline on SAM commands the first `HELLO` parked forever, the task never
/// returned, and the reconnect loop that rebuilds the tunnel never ran once --
/// so the bridge saw exactly one connection for the lifetime of the daemon.
#[tokio::test]
async fn a_silent_sam_bridge_does_not_wedge_the_client() {
    let accepts = Arc::new(AtomicUsize::new(0));
    let addr = start_silent_listener(Arc::clone(&accepts)).await;

    let _client = spawn_i2p_client(I2pClientConfig {
        id: InterfaceId(1),
        name: "cli".to_string(),
        sam_address: addr.to_string(),
        peer: "peer.b32.i2p".to_string(),
        buffer_size: 32,
        reconnect_wait: Duration::from_millis(100),
        timeouts: I2pTimeouts {
            sam_command: Duration::from_millis(200),
            read_idle: Duration::from_secs(60),
            stale_after: Duration::from_secs(60),
            ..I2pTimeouts::default()
        },
        ifac: None,
        reconnect_notify: None,
        ingress_control: false,
        outbound_socket_hook: None,
        tunnel: Arc::new(I2pTunnelState::default()),
    });

    let retried = wait_until(Duration::from_secs(10), || {
        accepts.load(std::sync::atomic::Ordering::Relaxed) >= 3
    })
    .await;
    assert!(
        retried,
        "client must keep retrying a silent bridge; it dialled {} time(s)",
        accepts.load(std::sync::atomic::Ordering::Relaxed)
    );
}

/// Codeberg #284, half two, minimal: a stream whose socket is perfectly healthy
/// but carries no bytes is a dead tunnel, and nothing else can tell us so --
/// writes into the local SAM bridge keep succeeding. The stream task has to
/// return on its own, which is what puts the caller back on the rebuild path.
#[tokio::test]
async fn read_watchdog_tears_down_a_silent_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => return,
        };
        // Drain the keepalives so the socket never backs up, and never answer.
        let mut scratch = [0u8; 256];
        loop {
            match sock.read(&mut scratch).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    });

    let stream = TcpStream::connect(addr).await.unwrap();
    let (incoming_tx, _incoming_rx) = mpsc::channel(8);
    // Both ends of each channel stay alive for the whole test: a closed channel
    // would end the task for a reason that is not the watchdog.
    let (_outgoing_tx, outgoing_rx) = mpsc::channel(8);

    tokio::time::timeout(
        Duration::from_secs(10),
        i2p_stream_task(
            "silent".to_string(),
            stream,
            incoming_tx,
            outgoing_rx,
            Arc::new(InterfaceCounters::new()),
            I2pTimeouts {
                sam_command: Duration::from_secs(2),
                read_idle: Duration::from_millis(300),
                stale_after: Duration::from_millis(300),
                ..I2pTimeouts::default()
            },
            Arc::new(I2pTunnelState::default()),
        ),
    )
    .await
    .expect("the stream task must return on the read watchdog, not run forever");
}

/// A SAM bridge that completes the whole handshake and then delivers nothing on
/// the stream: the tunnel behind the loopback socket is dead, the socket is not.
/// Counts `STREAM CONNECT` commands, i.e. tunnel builds.
async fn start_dead_tunnel_sam(connects: Arc<AtomicUsize>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let connects = Arc::clone(&connects);
            tokio::spawn(async move {
                match sam::read_line(&mut sock).await {
                    Ok(l) if l.starts_with("HELLO") => {}
                    _ => return,
                }
                let _ = sock.write_all(b"HELLO REPLY RESULT=OK VERSION=3.1\n").await;
                let cmd = match sam::read_line(&mut sock).await {
                    Ok(l) => l,
                    Err(_) => return,
                };
                if cmd.starts_with("SESSION CREATE") {
                    let reply = format!("SESSION STATUS RESULT=OK DESTINATION={GOLDEN_PRIV}\n");
                    let _ = sock.write_all(reply.as_bytes()).await;
                } else if cmd.starts_with("NAMING LOOKUP") {
                    let reply = format!("NAMING REPLY RESULT=OK NAME=x VALUE={GOLDEN_PUB}\n");
                    let _ = sock.write_all(reply.as_bytes()).await;
                } else if cmd.starts_with("STREAM CONNECT") {
                    connects.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let _ = sock.write_all(b"STREAM STATUS RESULT=OK\n").await;
                }
                // Whatever the command was: hold the socket open and stay
                // silent from here on, draining whatever arrives.
                let mut scratch = [0u8; 256];
                loop {
                    match sock.read(&mut scratch).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                }
            });
        }
    });

    addr
}

/// Codeberg #284, half two, end to end: the rebuild path already existed but was
/// unreachable, because a dead tunnel behind a live loopback socket raises no
/// error to reach it with. With the read watchdog the client tears the stream
/// down itself and builds a new tunnel, so `STREAM CONNECT` happens again.
#[tokio::test]
async fn a_dead_tunnel_behind_a_live_socket_is_rebuilt() {
    let connects = Arc::new(AtomicUsize::new(0));
    let addr = start_dead_tunnel_sam(Arc::clone(&connects)).await;

    let handle = spawn_i2p_client(I2pClientConfig {
        id: InterfaceId(1),
        name: "cli".to_string(),
        sam_address: addr.to_string(),
        peer: "peer.b32.i2p".to_string(),
        buffer_size: 32,
        reconnect_wait: Duration::from_millis(100),
        timeouts: I2pTimeouts {
            sam_command: Duration::from_secs(2),
            read_idle: Duration::from_millis(300),
            stale_after: Duration::from_millis(300),
            ..I2pTimeouts::default()
        },
        ifac: None,
        reconnect_notify: None,
        ingress_control: false,
        outbound_socket_hook: None,
        tunnel: Arc::new(I2pTunnelState::default()),
    });

    handle
        .ready
        .wait(Duration::from_secs(5))
        .await
        .expect("client should establish its first stream");
    assert!(
        handle.counters.is_online(),
        "interface should report itself up once the stream is established"
    );

    let rebuilt = wait_until(Duration::from_secs(10), || {
        connects.load(std::sync::atomic::Ordering::Relaxed) >= 2
    })
    .await;
    assert!(
        rebuilt,
        "a dead tunnel must be rebuilt; STREAM CONNECT was issued {} time(s)",
        connects.load(std::sync::atomic::Ordering::Relaxed)
    );
}

/// 1119: the persisted I2P destination key is a private key, so it is
/// written 0600 whatever the umask, and narrowed when it is loaded.
#[cfg(unix)]
#[test]
fn the_i2p_keyfile_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &PathBuf| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("nested/i2p_key");
    save_keyfile(&path, "a2V5");
    assert_eq!(mode(&path), 0o600);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).expect("chmod");
    assert_eq!(load_keyfile(&path).as_deref(), Some("a2V5"));
    assert_eq!(mode(&path), 0o600);
}

// --- 1576: the reference's signature type and key file name ------------------

/// The node identity hash the key file tests run with, and the two names the
/// reference derives from it for a section called `I2P`. Computed with the
/// reference's own `RNS.Identity.full_hash` and `RNS.hexrep` over
/// `server_tunnel`'s expressions
/// (`reference/Reticulum/RNS/Interfaces/I2PInterface.py:269-273`), so the
/// names are the ones an rnsd on the same storage directory reads and writes.
const KEY_ID_HASH: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const KEY_SECTION: &str = "I2P";
const REFERENCE_OLD_FORMAT: &str =
    "1773daac0b85211719376f2e5c589ff25b351b46012d5c39635f260b494853c6.i2p";
const REFERENCE_NEW_FORMAT: &str =
    "42e8ea23de56f4915ab1dd48967906591e9d8f7dd553aebdb9d66c9c5bc5f5e2.i2p";

/// A SAM bridge that records every command line and answers it without ever
/// connecting a stream: `SESSION CREATE` with the golden key, the session
/// socket held open, `STREAM ACCEPT` and `STREAM CONNECT` with `OK` and then
/// silence. Enough to watch what a session asks SAM for.
async fn start_recording_sam(seen: Arc<std::sync::Mutex<Vec<String>>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                match sam::read_line(&mut sock).await {
                    Ok(l) if l.starts_with("HELLO") => {}
                    _ => return,
                }
                let _ = sock.write_all(b"HELLO REPLY RESULT=OK VERSION=3.1\n").await;
                let Ok(cmd) = sam::read_line(&mut sock).await else {
                    return;
                };
                seen.lock().unwrap().push(cmd.clone());
                let reply = if cmd.starts_with("SESSION CREATE") {
                    format!("SESSION STATUS RESULT=OK DESTINATION={GOLDEN_PRIV}\n")
                } else if cmd.starts_with("NAMING LOOKUP") {
                    format!("NAMING REPLY RESULT=OK NAME=x VALUE={GOLDEN_PUB}\n")
                } else {
                    "STREAM STATUS RESULT=OK\n".to_string()
                };
                let _ = sock.write_all(reply.as_bytes()).await;
                let mut scratch = [0u8; 256];
                while let Ok(n) = sock.read(&mut scratch).await {
                    if n == 0 {
                        break;
                    }
                }
            });
        }
    });
    addr
}

fn server_config(
    sam: SocketAddr,
    keyfile: PathBuf,
) -> (I2pServerConfig, mpsc::Receiver<InterfaceHandle>) {
    let (new_tx, new_rx) = mpsc::channel::<InterfaceHandle>(8);
    (
        I2pServerConfig {
            sam_address: sam.to_string(),
            keyfile,
            buffer_size: 32,
            name_prefix: "srv".to_string(),
            reconnect_wait: Duration::from_millis(200),
            timeouts: I2pTimeouts::default(),
            next_id: Arc::new(AtomicUsize::new(100)),
            new_interface_tx: new_tx,
            ifac: None,
            ingress_control: false,
            outbound_socket_hook: None,
            endpoint: Arc::new(I2pEndpoint::new(true)),
            section: "srv".to_string(),
            parent_id: 0,
            inventory: crate::interfaces::inventory::InterfaceInventory::shared(),
            mode: leviculum_core::traits::InterfaceMode::default(),
        },
        new_rx,
    )
}

fn session_creates(seen: &std::sync::Mutex<Vec<String>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|c| c.starts_with("SESSION CREATE"))
        .cloned()
        .collect()
}

/// A peer's session asks SAM for an Ed25519 destination, as the reference's
/// `DEST GENERATE SIGNATURE_TYPE=7` does; without the option i2pd made lnsd a
/// DSA-SHA1 one (1568).
#[tokio::test]
async fn a_peer_session_asks_for_an_ed25519_destination() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sam_addr = start_recording_sam(Arc::clone(&seen)).await;
    let _client = spawn_i2p_client(I2pClientConfig {
        id: InterfaceId(1),
        name: "cli".to_string(),
        sam_address: sam_addr.to_string(),
        peer: "somepeer.b32.i2p".to_string(),
        buffer_size: 32,
        reconnect_wait: Duration::from_millis(200),
        timeouts: I2pTimeouts::default(),
        ifac: None,
        reconnect_notify: None,
        ingress_control: false,
        outbound_socket_hook: None,
        tunnel: Arc::new(I2pTunnelState::default()),
    });
    assert!(
        wait_until(Duration::from_secs(5), || !session_creates(&seen)
            .is_empty())
        .await,
        "the client never created a session"
    );
    let create = &session_creates(&seen)[0];
    assert!(
        create.contains("DESTINATION=TRANSIENT SIGNATURE_TYPE=7"),
        "{create}"
    );
}

/// rnsd first, then lnsd, on one storage directory: lnsd finds the key the
/// reference wrote under the reference's name and opens its session with it,
/// so the node keeps rnsd's address. A stored key is sent with no signature
/// type, so a DSA key an older lnsd made stays the key it is.
#[tokio::test]
async fn lnsd_takes_over_the_key_rnsd_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let key_dir = tmp.path().join("i2p");
    std::fs::create_dir_all(&key_dir).unwrap();
    // The reference writes the bare base64 private key, no newline
    // (I2PInterface.py:285-287).
    std::fs::write(key_dir.join(REFERENCE_NEW_FORMAT), GOLDEN_PRIV).unwrap();

    let keyfile = resolve_keyfile(&key_dir, KEY_SECTION, &KEY_ID_HASH, 0);
    assert_eq!(keyfile, key_dir.join(REFERENCE_NEW_FORMAT));

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sam_addr = start_recording_sam(Arc::clone(&seen)).await;
    let (mut config, _rx) = server_config(sam_addr, keyfile);
    // The hold for a stored key is its own test below.
    config.timeouts.stored_key_hold = Duration::ZERO;
    spawn_i2p_server(config);
    assert!(
        wait_until(Duration::from_secs(5), || !session_creates(&seen)
            .is_empty())
        .await,
        "the endpoint never created a session"
    );
    let create = &session_creates(&seen)[0];
    assert!(
        create.contains(&format!("DESTINATION={GOLDEN_PRIV} ")),
        "the endpoint must open its session with rnsd's key: {create}"
    );
    assert!(!create.contains("SIGNATURE_TYPE"), "{create}");
}

/// lnsd first, then rnsd: lnsd on a fresh directory asks for an Ed25519 key
/// and stores what SAM made under the reference's new-format name, which is
/// the file rnsd opens for the same section and transport identity.
#[tokio::test]
async fn rnsd_finds_the_key_lnsd_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let key_dir = tmp.path().join("i2p");
    let keyfile = resolve_keyfile(&key_dir, KEY_SECTION, &KEY_ID_HASH, 0);
    assert_eq!(keyfile, key_dir.join(REFERENCE_NEW_FORMAT));

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sam_addr = start_recording_sam(Arc::clone(&seen)).await;
    let (config, _rx) = server_config(sam_addr, keyfile.clone());
    spawn_i2p_server(config);
    assert!(
        wait_until(Duration::from_secs(5), || keyfile.is_file()).await,
        "the endpoint stored no key"
    );
    let create = &session_creates(&seen)[0];
    assert!(
        create.contains("DESTINATION=TRANSIENT SIGNATURE_TYPE=7"),
        "{create}"
    );
    assert_eq!(
        std::fs::read_to_string(&keyfile).unwrap().trim(),
        GOLDEN_PRIV
    );
}

/// The reference prefers its old `sha256(sha256(name))` file when present
/// (I2PInterface.py:276-279), over a new-format one.
#[test]
fn the_old_format_key_is_preferred_when_present() {
    let tmp = tempfile::tempdir().unwrap();
    let key_dir = tmp.path();
    std::fs::write(key_dir.join(REFERENCE_OLD_FORMAT), GOLDEN_PRIV).unwrap();
    std::fs::write(key_dir.join(REFERENCE_NEW_FORMAT), "other").unwrap();
    assert_eq!(
        resolve_keyfile(key_dir, KEY_SECTION, &KEY_ID_HASH, 0),
        key_dir.join(REFERENCE_OLD_FORMAT)
    );
}

/// An lnsd before 1576 kept its key as `i2p_iface_<index>.i2p`. That file is
/// renamed to the reference's name once, with its content, so the node keeps
/// its address; the next start finds the new name and renames nothing.
#[test]
fn an_index_named_key_is_adopted_once() {
    let tmp = tempfile::tempdir().unwrap();
    let key_dir = tmp.path();
    let legacy = key_dir.join("i2p_iface_3.i2p");
    std::fs::write(&legacy, GOLDEN_PRIV).unwrap();

    let adopted = resolve_keyfile(key_dir, KEY_SECTION, &KEY_ID_HASH, 3);
    assert_eq!(adopted, key_dir.join(REFERENCE_NEW_FORMAT));
    assert!(!legacy.exists(), "the old name must be gone");
    assert_eq!(std::fs::read_to_string(&adopted).unwrap(), GOLDEN_PRIV);

    // A second section's index-named file is not taken by this one.
    let other = key_dir.join("i2p_iface_4.i2p");
    std::fs::write(&other, "other").unwrap();
    assert_eq!(
        resolve_keyfile(key_dir, KEY_SECTION, &KEY_ID_HASH, 4),
        key_dir.join(REFERENCE_NEW_FORMAT)
    );
    assert!(other.exists(), "the name is taken, so nothing is adopted");
}

// ---------------------------------------------------------------------------
// 1588: the reference's status surface (tunnel state, endpoint b32 and state,
// the accepted peer's row) and the SAM refusals that must each end in a retry.
// ---------------------------------------------------------------------------

use crate::interfaces::inventory::TunnelState;

fn client_config(
    sam: SocketAddr,
    timeouts: I2pTimeouts,
    tunnel: Arc<I2pTunnelState>,
) -> I2pClientConfig {
    I2pClientConfig {
        id: InterfaceId(1),
        name: "cli".to_string(),
        sam_address: sam.to_string(),
        peer: "somepeer.b32.i2p".to_string(),
        buffer_size: 32,
        reconnect_wait: Duration::from_millis(50),
        timeouts,
        ifac: None,
        reconnect_notify: None,
        ingress_control: false,
        outbound_socket_hook: None,
        tunnel,
    }
}

/// The tunnel state follows the read clock as the reference's read watchdog
/// sets it (I2PInterface.py:681-687): `Init` before any stream, `Active` once
/// one runs, `Stale` once nothing arrived for longer than the stale threshold,
/// and `Active` again on the next byte. The stale mark tears nothing down: the
/// packet after it arrives over the same accepted stream, not a rebuilt one.
#[tokio::test]
async fn the_tunnel_state_follows_the_read_clock() {
    let mock = start_mock_sam().await;
    let tmp = tempfile::tempdir().unwrap();
    let (config, mut new_rx) = server_config(mock, tmp.path().join("srv.i2p"));
    spawn_i2p_server(config);

    let tunnel = Arc::new(I2pTunnelState::default());
    assert_eq!(tunnel.get(), TunnelState::Init);
    assert_eq!(tunnel.get().description(), "Creating Tunnel");
    let mut client = spawn_i2p_client(client_config(
        mock,
        I2pTimeouts {
            sam_command: Duration::from_secs(2),
            read_idle: Duration::from_secs(60),
            stale_after: Duration::from_secs(2),
            ..I2pTimeouts::default()
        },
        Arc::clone(&tunnel),
    ));

    let server_iface = tokio::time::timeout(Duration::from_secs(5), new_rx.recv())
        .await
        .expect("timeout waiting for accepted I2P interface")
        .expect("new-interface channel closed");
    assert!(
        wait_until(Duration::from_secs(5), || tunnel.get()
            == TunnelState::Active)
        .await,
        "a running stream reads active"
    );
    assert_eq!(tunnel.get().description(), "Tunnel Active");

    // Nothing crosses (the keepalive is 10 s away): stale after 2 s.
    assert!(
        wait_until(Duration::from_secs(6), || tunnel.get()
            == TunnelState::Stale)
        .await,
        "a silent stream reads stale"
    );
    assert_eq!(tunnel.get().description(), "Tunnel Unresponsive");

    server_iface
        .outgoing
        .send(out(b"after-stale"))
        .await
        .unwrap();
    let pkt = tokio::time::timeout(Duration::from_secs(5), client.incoming.recv())
        .await
        .expect("timeout: the stream was torn down at the stale mark")
        .expect("client incoming closed");
    assert_eq!(pkt.data, b"after-stale");
    assert!(
        wait_until(Duration::from_secs(5), || tunnel.get()
            == TunnelState::Active)
        .await,
        "a byte after the stale mark reads active again"
    );
    assert!(
        new_rx.try_recv().is_err(),
        "no second stream was accepted: the stale mark rebuilt nothing"
    );
}

/// A fresh endpoint knows its b32 and is up once its session is, and the
/// connection it accepts is registered as the reference names it,
/// `I2PInterfacePeer[Connected peer on <section>]`, counted toward its parent,
/// with a tunnel state of its own, and takes the section's mode
/// (I2PInterface.py:992; on 1588's bed rnsd's accepted peers read `Gateway`
/// under a discoverable section where lnsd's read `Full`).
#[tokio::test]
async fn the_endpoint_publishes_its_b32_and_registers_what_it_accepts() {
    let mock = start_mock_sam().await;
    let tmp = tempfile::tempdir().unwrap();
    let (mut config, mut new_rx) = server_config(mock, tmp.path().join("srv.i2p"));
    config.mode = leviculum_core::traits::InterfaceMode::Gateway;
    let endpoint = Arc::clone(&config.endpoint);
    let inventory = Arc::clone(&config.inventory);
    assert_eq!(endpoint.b32(), None);
    assert!(!endpoint.is_online());
    spawn_i2p_server(config);

    assert!(wait_until(Duration::from_secs(5), || endpoint.is_online()).await);
    assert_eq!(endpoint.b32().as_deref(), Some(GOLDEN_B32));

    let _client = spawn_i2p_client(client_config(
        mock,
        I2pTimeouts::default(),
        Arc::new(I2pTunnelState::default()),
    ));
    let accepted = tokio::time::timeout(Duration::from_secs(5), new_rx.recv())
        .await
        .expect("timeout waiting for accepted I2P interface")
        .expect("new-interface channel closed");
    assert_eq!(
        accepted.info.mode,
        leviculum_core::traits::InterfaceMode::Gateway
    );
    let id = accepted.info.id.0;
    let inv = inventory.lock_recover();
    let identity = inv.identity(id).expect("accepted peer registered");
    assert_eq!(identity.name, "I2PInterfacePeer[Connected peer on srv]");
    assert_eq!(identity.short_name, "Connected peer on srv");
    assert_eq!(identity.type_name, "I2PInterfacePeer");
    assert_eq!(identity.parent, Some(0));
    assert!(inv.counts_toward_parent(id));
    assert_eq!(inv.i2p_tunnel(id), Some(TunnelState::Active));
}

/// A stored key names the b32 before any session exists, so a node whose
/// router is down still reports the address it will be reachable at, as the
/// reference sets `owner.b32` from the key first (I2PInterface.py:295-296).
/// The endpoint is not up.
#[tokio::test]
async fn a_stored_key_names_the_b32_before_any_session() {
    // A port nothing listens on: bind, learn it, close.
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap()
    };
    let tmp = tempfile::tempdir().unwrap();
    let keyfile = tmp.path().join("srv.i2p");
    save_keyfile(&keyfile, GOLDEN_PRIV);
    let (config, _rx) = server_config(closed, keyfile);
    let endpoint = Arc::clone(&config.endpoint);
    spawn_i2p_server(config);

    assert!(
        wait_until(Duration::from_secs(5), || endpoint.b32().is_some()).await,
        "the stored key's b32 is known without a router"
    );
    assert_eq!(endpoint.b32().as_deref(), Some(GOLDEN_B32));
    assert!(!endpoint.is_online());
}

/// Where a refusing SAM bridge says no.
#[derive(Clone, Copy)]
enum Refusal {
    /// `HELLO REPLY RESULT=NOVERSION` to every HELLO.
    Hello,
    /// `SESSION STATUS RESULT=<code>` to every SESSION CREATE.
    Session(&'static str),
    /// `NAMING REPLY RESULT=INVALID_KEY` to every NAMING LOOKUP.
    Naming,
}

/// A SAM bridge that refuses at one step and answers every other step as the
/// mock does, counting how often the refused step was asked.
async fn start_refusing_sam(refusal: Refusal, asked: Arc<AtomicUsize>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let asked = Arc::clone(&asked);
            tokio::spawn(async move {
                match sam::read_line(&mut sock).await {
                    Ok(l) if l.starts_with("HELLO") => {}
                    _ => return,
                }
                if matches!(refusal, Refusal::Hello) {
                    asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let _ = sock.write_all(b"HELLO REPLY RESULT=NOVERSION\n").await;
                    return;
                }
                let _ = sock.write_all(b"HELLO REPLY RESULT=OK VERSION=3.1\n").await;
                let Ok(cmd) = sam::read_line(&mut sock).await else {
                    return;
                };
                let reply = match refusal {
                    Refusal::Session(code) if cmd.starts_with("SESSION CREATE") => {
                        asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        format!("SESSION STATUS RESULT={code} MESSAGE=\"refused\"\n")
                    }
                    Refusal::Naming if cmd.starts_with("NAMING LOOKUP") => {
                        asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        "NAMING REPLY RESULT=INVALID_KEY NAME=somepeer.b32.i2p\n".to_string()
                    }
                    _ if cmd.starts_with("SESSION CREATE") => {
                        format!("SESSION STATUS RESULT=OK DESTINATION={GOLDEN_PRIV}\n")
                    }
                    _ if cmd.starts_with("NAMING LOOKUP") => {
                        format!("NAMING REPLY RESULT=OK NAME=x VALUE={GOLDEN_PUB}\n")
                    }
                    _ => "STREAM STATUS RESULT=OK\n".to_string(),
                };
                let _ = sock.write_all(reply.as_bytes()).await;
                let mut scratch = [0u8; 256];
                while let Ok(n) = sock.read(&mut scratch).await {
                    if n == 0 {
                        break;
                    }
                }
            });
        }
    });
    addr
}

/// Ask a client and (where the step is the endpoint's too) an endpoint
/// through a bridge that refuses at `refusal`, and require that each asks
/// again: no SAM error is terminal (the reference retries on every one of
/// them, "Retrying tunnel setup later"), and the client's interface stays
/// alive and never reads a tunnel it does not have.
async fn sam_refusal_is_retried(refusal: Refusal, endpoint_too: bool) {
    let asked = Arc::new(AtomicUsize::new(0));
    let sam_addr = start_refusing_sam(refusal, Arc::clone(&asked)).await;
    let tunnel = Arc::new(I2pTunnelState::default());
    let client = spawn_i2p_client(client_config(
        sam_addr,
        I2pTimeouts::default(),
        Arc::clone(&tunnel),
    ));
    assert!(
        wait_until(Duration::from_secs(5), || {
            asked.load(std::sync::atomic::Ordering::Relaxed) >= 3
        })
        .await,
        "the client asked again after the refusal (asked {} times)",
        asked.load(std::sync::atomic::Ordering::Relaxed)
    );
    assert!(
        !client.outgoing.is_closed(),
        "the client interface is alive"
    );
    assert!(!client.counters.is_online());
    assert_eq!(tunnel.get(), TunnelState::Init);
    drop(client);

    if endpoint_too {
        let asked = Arc::new(AtomicUsize::new(0));
        let sam_addr = start_refusing_sam(refusal, Arc::clone(&asked)).await;
        let tmp = tempfile::tempdir().unwrap();
        let (mut config, _rx) = server_config(sam_addr, tmp.path().join("srv.i2p"));
        config.reconnect_wait = Duration::from_millis(50);
        let endpoint = Arc::clone(&config.endpoint);
        spawn_i2p_server(config);
        assert!(
            wait_until(Duration::from_secs(5), || {
                asked.load(std::sync::atomic::Ordering::Relaxed) >= 3
            })
            .await,
            "the endpoint asked again after the refusal (asked {} times)",
            asked.load(std::sync::atomic::Ordering::Relaxed)
        );
        assert!(!endpoint.is_online());
    }
}

/// `i2p_sam_refuses` (docs/src/concepts/i2p-interface.md, the cells): a
/// SESSION CREATE answered `I2P_ERROR` is retried by peer and endpoint.
#[tokio::test]
async fn i2p_sam_refuses_i2p_error() {
    sam_refusal_is_retried(Refusal::Session("I2P_ERROR"), true).await;
}

/// `i2p_sam_refuses`: a SESSION CREATE answered `DUPLICATED_ID` (a nick
/// still taken on the bridge) is retried by peer and endpoint.
#[tokio::test]
async fn i2p_sam_refuses_duplicated_id() {
    sam_refusal_is_retried(Refusal::Session("DUPLICATED_ID"), true).await;
}

/// `i2p_sam_refuses`: a NAMING LOOKUP answered `INVALID_KEY` (the peer's
/// LeaseSet not yet spread, as measured on the bed) is retried by the peer;
/// an endpoint looks nothing up.
#[tokio::test]
async fn i2p_sam_refuses_invalid_key() {
    sam_refusal_is_retried(Refusal::Naming, false).await;
}

/// `i2p_sam_refuses`: a HELLO answered `NOVERSION` is retried by peer and
/// endpoint.
#[tokio::test]
async fn i2p_sam_refuses_hello_noversion() {
    sam_refusal_is_retried(Refusal::Hello, true).await;
}

/// 1654: the first SESSION CREATE on a stored key waits out the destination
/// a previous process may have left in i2pd, whose postponed destructor
/// would otherwise stop it under the new session five seconds later
/// (`I2P_STORED_KEY_HOLD`). Only the first session is held: a rebuild inside
/// the process asks again after `reconnect_wait` alone. A fresh key is not
/// held at all; the `i2p_sam_refuses_*` endpoints run on a fresh key with the
/// default hold and ask three times within five seconds.
#[tokio::test]
async fn a_stored_key_holds_the_first_session_only() {
    assert!(
        I2pTimeouts::default().stored_key_hold > Duration::from_secs(5),
        "the default hold must outlast i2pd's five-second session linger"
    );
    let hold = Duration::from_secs(2);
    let asked = Arc::new(AtomicUsize::new(0));
    let sam_addr = start_refusing_sam(Refusal::Session("I2P_ERROR"), Arc::clone(&asked)).await;
    let tmp = tempfile::tempdir().unwrap();
    let keyfile = tmp.path().join("srv.i2p");
    save_keyfile(&keyfile, GOLDEN_PRIV);
    let (mut config, _rx) = server_config(sam_addr, keyfile);
    config.reconnect_wait = Duration::from_millis(50);
    config.timeouts.stored_key_hold = hold;
    let endpoint = Arc::clone(&config.endpoint);
    let started = std::time::Instant::now();
    spawn_i2p_server(config);

    // The address is known during the hold, from the key alone.
    assert!(
        wait_until(Duration::from_secs(1), || endpoint.b32().is_some()).await,
        "the stored key's b32 is known while the session is held"
    );
    let count = || asked.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        wait_until(Duration::from_secs(10), || count() >= 1).await,
        "the endpoint never created its session"
    );
    let first_ask = started.elapsed();
    assert!(
        first_ask >= hold,
        "the first SESSION CREATE on a stored key came {first_ask:?} after the start, inside the {hold:?} hold"
    );
    let after_first = std::time::Instant::now();
    assert!(
        wait_until(Duration::from_secs(10), || count() >= 3).await,
        "the endpoint asked again after the refusal (asked {} times)",
        count()
    );
    let rebuilds = after_first.elapsed();
    assert!(
        rebuilds < hold,
        "two rebuilds took {rebuilds:?}: a rebuild must wait reconnect_wait, not the {hold:?} hold"
    );
}
