//! Local (Unix socket) interface for shared instance IPC
//!
//! Implements the data channel for Python Reticulum's "shared instance" feature.
//! A daemon listens on an abstract Unix domain socket (`\0rns/{instance_name}`)
//! and accepts connections from local client programs. Each connection becomes
//! an `InterfaceHandle` with `is_local_client = true`, which tells core to
//! forward announces and path requests to/from this client.
//!
//! Uses the same HDLC framing as TCP interfaces.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use reticulum_core::constants::MTU;
use reticulum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use reticulum_core::transport::InterfaceId;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use super::{IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket};

/// Bind a Unix listener to the given abstract name.
///
/// On Linux, uses abstract sockets (`\0name`). On other Unix systems,
/// falls back to filesystem sockets in the temp directory.
fn bind_local_listener(abstract_name: &str) -> Result<std::os::unix::net::UnixListener, io::Error> {
    #[cfg(target_os = "linux")]
    {
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        std::os::unix::net::UnixListener::bind_addr(&addr)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let path =
            std::env::temp_dir().join(format!("leviculum-{}", abstract_name.replace('/', "-")));
        // Remove stale socket file if it exists
        let _ = std::fs::remove_file(&path);
        std::os::unix::net::UnixListener::bind(&path)
    }
}

/// Connect to a Unix socket by abstract name.
///
/// On Linux, uses abstract sockets. On other Unix systems,
/// falls back to filesystem sockets in the temp directory.
fn connect_local(abstract_name: &str) -> Result<std::os::unix::net::UnixStream, io::Error> {
    #[cfg(target_os = "linux")]
    {
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        std::os::unix::net::UnixStream::connect_addr(&addr)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let path =
            std::env::temp_dir().join(format!("leviculum-{}", abstract_name.replace('/', "-")));
        std::os::unix::net::UnixStream::connect(&path)
    }
}

/// Default channel buffer size for local interfaces.
pub(crate) const LOCAL_DEFAULT_BUFFER_SIZE: usize = 256;

/// Hardware MTU for local interfaces (same as TCP, local IPC).
const LOCAL_HW_MTU: u32 = 262_144;

/// Frame buffer multiplier (accounts for HDLC escaping overhead)
const FRAME_BUFFER_MULTIPLIER: usize = 2;

/// Read buffer multiplier (handles multiple packets per read)
const READ_BUFFER_MULTIPLIER: usize = 4;

/// Start a local (Unix socket) server for shared instance IPC.
///
/// Binds to an abstract Unix socket at `\0rns/{instance_name}` and spawns an
/// async accept loop. Each accepted connection becomes an `InterfaceHandle`
/// sent to the event loop via `new_interface_tx`.
///
/// The accept loop exits when the event loop drops `new_interface_rx`
/// (detected via `Sender::closed()`).
pub(crate) fn spawn_local_server(
    instance_name: &str,
    next_id: Arc<AtomicUsize>,
    new_interface_tx: mpsc::Sender<InterfaceHandle>,
    buffer_size: usize,
) -> Result<(), io::Error> {
    // Build abstract socket name: "rns/{instance_name}"
    let abstract_name = format!("rns/{}", instance_name);

    let std_listener = bind_local_listener(&abstract_name)?;
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(std_listener)?;

    tracing::info!("Local server listening on socket {}", abstract_name);

    let client_counter = Arc::new(AtomicUsize::new(0));
    let instance_name_owned = abstract_name.clone();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _peer_addr)) => {
                            let id = InterfaceId(next_id.fetch_add(1, Ordering::Relaxed));
                            let client_num = client_counter.fetch_add(1, Ordering::Relaxed);
                            let name = format!("Local[{}]/{}", instance_name_owned, client_num);
                            let handle = spawn_local_interface_from_stream(
                                id, name.clone(), stream, buffer_size,
                            );
                            tracing::info!("Local client connected: {} ({})", name, id);
                            if new_interface_tx.send(handle).await.is_err() {
                                break; // event loop shut down
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Local accept error: {}", e);
                        }
                    }
                }
                _ = new_interface_tx.closed() => {
                    tracing::debug!("Local server shutting down (event loop exited)");
                    break;
                }
            }
        }
    });

    Ok(())
}

/// Create channels, spawn the I/O task for an accepted Unix stream,
/// and return the resulting `InterfaceHandle`.
fn spawn_local_interface_from_stream(
    id: InterfaceId,
    name: String,
    stream: UnixStream,
    buffer_size: usize,
) -> InterfaceHandle {
    let (incoming_tx, incoming_rx) = mpsc::channel(buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(buffer_size);
    let counters = Arc::new(InterfaceCounters::new());

    let task_name = name.clone();
    let task_counters = Arc::clone(&counters);

    tokio::spawn(async move {
        local_interface_task(task_name, stream, incoming_tx, outgoing_rx, task_counters).await;
    });

    InterfaceHandle {
        info: InterfaceInfo {
            id,
            name,
            hw_mtu: Some(LOCAL_HW_MTU),
            is_local_client: true,
            bitrate: None,
            ifac: None,
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
    }
}

/// Connect to an existing shared instance daemon as a client.
///
/// Connects to the abstract Unix socket `\0rns/{instance_name}` and returns
/// an `InterfaceHandle`. The handle has `is_local_client = false` because
/// from the client's perspective this is a regular interface; the daemon
/// marks its side as `is_local_client = true`.
///
/// Calls `tokio::spawn` for the I/O task, must be called from a context
/// where a tokio runtime is active (same as `spawn_local_server`).
///
/// No reconnection, returns an error if the daemon is not running.
pub(crate) fn spawn_local_client(
    id: InterfaceId,
    instance_name: &str,
    buffer_size: usize,
) -> Result<InterfaceHandle, io::Error> {
    let abstract_name = format!("rns/{}", instance_name);

    let std_stream = connect_local(&abstract_name)?;
    std_stream.set_nonblocking(true)?;
    let stream = tokio::net::UnixStream::from_std(std_stream)?;

    let (incoming_tx, incoming_rx) = mpsc::channel(buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(buffer_size);
    let counters = Arc::new(InterfaceCounters::new());

    let name = format!("LocalClient[{}]", instance_name);
    let task_name = name.clone();
    let task_counters = Arc::clone(&counters);

    tokio::spawn(async move {
        local_interface_task(task_name, stream, incoming_tx, outgoing_rx, task_counters).await;
    });

    Ok(InterfaceHandle {
        info: InterfaceInfo {
            id,
            name,
            hw_mtu: Some(LOCAL_HW_MTU),
            is_local_client: false,
            bitrate: None,
            ifac: None,
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
    })
}

/// I/O task owning the Unix stream.
///
/// Handles bidirectional I/O using HDLC framing, identical to the TCP
/// interface task. Uses poll_read_ready + try_read for edge-triggered reads.
async fn local_interface_task(
    name: String,
    stream: UnixStream,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
) {
    let (reader, mut writer) = stream.into_split();

    let mut deframer = Deframer::new();
    let mut read_buf = vec![0u8; MTU * READ_BUFFER_MULTIPLIER];
    let mut frame_buf = Vec::with_capacity(MTU * FRAME_BUFFER_MULTIPLIER);

    loop {
        tokio::select! {
            // Read path: wait for socket readability, then try_read + deframe
            result = reader.readable() => {
                match result {
                    Ok(()) => {
                        loop {
                            match reader.try_read(&mut read_buf) {
                                Ok(0) => {
                                    tracing::debug!("Local interface {} disconnected (EOF)", name);
                                    return;
                                }
                                Ok(n) => {
                                    counters.rx_bytes.fetch_add(n as u64, Ordering::Relaxed);
                                    let results = deframer.process(&read_buf[..n]);
                                    for r in results {
                                        if let DeframeResult::Frame(data) = r {
                                            if incoming_tx.send(IncomingPacket { data }).await.is_err() {
                                                return;
                                            }
                                        }
                                    }
                                }
                                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                                    break; // no more data, back to select!
                                }
                                Err(e) => {
                                    tracing::debug!("Local interface {} read error: {}", name, e);
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Local interface {} readability error: {}", name, e);
                        return;
                    }
                }
            }

            // Write path: receive outgoing packets and write HDLC-framed to stream
            msg = outgoing_rx.recv() => {
                match msg {
                    Some(pkt) => {
                        frame(&pkt.data, &mut frame_buf);
                        if let Err(e) = writer.write_all(&frame_buf).await {
                            tracing::debug!("Local interface {} write error: {}", name, e);
                            return;
                        }
                        counters.tx_bytes.fetch_add(frame_buf.len() as u64, Ordering::Relaxed);
                    }
                    None => {
                        tracing::debug!("Local interface {} outgoing channel closed", name);
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Connect to a local server in tests, using the platform-appropriate socket.
    fn test_connect(instance_name: &str) -> std::os::unix::net::UnixStream {
        let abstract_name = format!("rns/{}", instance_name);
        connect_local(&abstract_name).unwrap()
    }

    #[tokio::test]
    async fn test_local_server_accepts_connection() {
        let next_id = Arc::new(AtomicUsize::new(100));
        let (tx, mut rx) = mpsc::channel::<InterfaceHandle>(4);

        let instance_name = format!("test_{}", std::process::id());
        spawn_local_server(&instance_name, next_id.clone(), tx, 16).unwrap();

        let std_stream = test_connect(&instance_name);
        std_stream.set_nonblocking(true).unwrap();
        let _client = tokio::net::UnixStream::from_std(std_stream).unwrap();

        // Verify an InterfaceHandle arrives on the channel
        let handle = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout waiting for handle")
            .expect("channel closed");

        assert!(handle.info.name.starts_with("Local["));
        assert_eq!(handle.info.id, InterfaceId(100));
        assert!(handle.info.is_local_client);
        assert!(!handle.outgoing.is_closed());
    }

    #[tokio::test]
    async fn test_local_interface_hdlc_round_trip() {
        let next_id = Arc::new(AtomicUsize::new(200));
        let (tx, mut rx) = mpsc::channel::<InterfaceHandle>(4);

        let instance_name = format!("test_rt_{}", std::process::id());
        spawn_local_server(&instance_name, next_id.clone(), tx, 16).unwrap();

        let std_stream = test_connect(&instance_name);
        std_stream.set_nonblocking(true).unwrap();
        let mut client = tokio::net::UnixStream::from_std(std_stream).unwrap();

        let mut handle = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("closed");

        // Client sends HDLC-framed packet to server
        let payload = b"hello-local";
        let mut frame_buf = Vec::new();
        reticulum_core::framing::hdlc::frame(payload, &mut frame_buf);
        client.write_all(&frame_buf).await.unwrap();

        // Verify packet arrives on incoming channel
        let pkt = tokio::time::timeout(Duration::from_secs(2), handle.incoming.recv())
            .await
            .expect("timeout waiting for packet")
            .expect("channel closed");
        assert_eq!(pkt.data, payload);

        // Server sends HDLC-framed packet to client
        let response = b"reply-local";
        handle
            .outgoing
            .send(OutgoingPacket {
                data: response.to_vec(),
                high_priority: false,
            })
            .await
            .unwrap();

        // Read HDLC-framed response on client side
        let mut recv_buf = vec![0u8; 1024];
        let n = tokio::time::timeout(Duration::from_secs(2), client.read(&mut recv_buf))
            .await
            .expect("timeout reading response")
            .unwrap();
        assert!(n > 0);

        // Deframe and verify
        let mut deframer = Deframer::new();
        let results = deframer.process(&recv_buf[..n]);
        let mut frames = Vec::new();
        for r in results {
            if let DeframeResult::Frame(data) = r {
                frames.push(data);
            }
        }
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], response);
    }

    #[tokio::test]
    async fn test_local_client_disconnect_detected() {
        let next_id = Arc::new(AtomicUsize::new(300));
        let (tx, mut rx) = mpsc::channel::<InterfaceHandle>(4);

        let instance_name = format!("test_disc_{}", std::process::id());
        spawn_local_server(&instance_name, next_id.clone(), tx, 16).unwrap();

        let std_stream = test_connect(&instance_name);
        std_stream.set_nonblocking(true).unwrap();
        let client = tokio::net::UnixStream::from_std(std_stream).unwrap();

        let mut handle = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("closed");

        // Drop the client connection
        drop(client);

        // incoming channel should close (recv returns None)
        let result = tokio::time::timeout(Duration::from_secs(2), handle.incoming.recv()).await;
        match result {
            Ok(None) => {} // expected: channel closed on disconnect
            Ok(Some(_)) => panic!("should not receive a packet after disconnect"),
            Err(_) => panic!("timeout — disconnect was not detected"),
        }
    }

    #[tokio::test]
    async fn test_local_server_multiple_clients() {
        let next_id = Arc::new(AtomicUsize::new(400));
        let (tx, mut rx) = mpsc::channel::<InterfaceHandle>(4);

        let instance_name = format!("test_multi_{}", std::process::id());
        spawn_local_server(&instance_name, next_id.clone(), tx, 16).unwrap();

        // Connect two clients
        let std1 = test_connect(&instance_name);
        std1.set_nonblocking(true).unwrap();
        let _client1 = tokio::net::UnixStream::from_std(std1).unwrap();

        let std2 = test_connect(&instance_name);
        std2.set_nonblocking(true).unwrap();
        let _client2 = tokio::net::UnixStream::from_std(std2).unwrap();

        // Both should produce handles
        let h1 = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("closed");
        let h2 = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("closed");

        assert_ne!(h1.info.id, h2.info.id);
        assert!(h1.info.is_local_client);
        assert!(h2.info.is_local_client);
    }

    #[tokio::test]
    async fn test_local_client_connects_and_communicates() {
        let next_id = Arc::new(AtomicUsize::new(500));
        let (tx, mut rx) = mpsc::channel::<InterfaceHandle>(4);

        let instance_name = format!("test_client_{}", std::process::id());
        spawn_local_server(&instance_name, next_id.clone(), tx, 16).unwrap();

        // Give server time to bind
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect via spawn_local_client
        let id = InterfaceId(42);
        let mut client_handle =
            spawn_local_client(id, &instance_name, 16).expect("client connect failed");

        // Verify client handle properties
        assert_eq!(client_handle.info.id, InterfaceId(42));
        assert!(!client_handle.info.is_local_client);
        assert!(client_handle.info.name.contains("LocalClient"));

        // Server should have received a new handle with is_local_client = true
        let mut server_handle = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout waiting for server handle")
            .expect("channel closed");
        assert!(server_handle.info.is_local_client);

        // Client → Server: send HDLC-framed data through client handle's outgoing
        client_handle
            .outgoing
            .send(OutgoingPacket {
                data: b"client-to-server".to_vec(),
                high_priority: false,
            })
            .await
            .unwrap();

        let pkt = tokio::time::timeout(Duration::from_secs(2), server_handle.incoming.recv())
            .await
            .expect("timeout waiting for server packet")
            .expect("channel closed");
        assert_eq!(pkt.data, b"client-to-server");

        // Server → Client: send data through server handle's outgoing
        server_handle
            .outgoing
            .send(OutgoingPacket {
                data: b"server-to-client".to_vec(),
                high_priority: false,
            })
            .await
            .unwrap();

        let pkt = tokio::time::timeout(Duration::from_secs(2), client_handle.incoming.recv())
            .await
            .expect("timeout waiting for client packet")
            .expect("channel closed");
        assert_eq!(pkt.data, b"server-to-client");
    }

    #[tokio::test]
    async fn test_local_client_connect_failure() {
        let result = spawn_local_client(
            InterfaceId(99),
            "nonexistent_instance_that_does_not_exist",
            16,
        );
        assert!(
            result.is_err(),
            "connecting to nonexistent socket should fail"
        );
    }
}
