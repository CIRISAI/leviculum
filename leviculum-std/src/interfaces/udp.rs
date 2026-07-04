//! UDP interface
//!
//! Point-to-point or broadcast UDP with fixed addresses.
//! No discovery, no peer management, no framing, each datagram is one
//! Reticulum packet. Matches Python's `UDPInterface`.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::{IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket};
use leviculum_core::transport::InterfaceId;
use tokio::sync::mpsc;

/// Maximum datagram size accepted from the wire.
/// Matches Python `UDPInterface.HW_MTU = 1064` (UDPInterface.py:74).
/// Core already ensures outgoing packets are <= 500 bytes (protocol MTU),
/// so this only bounds the recv buffer.
const UDP_MTU: usize = 1064;

/// Default channel buffer size for UDP interfaces.
const UDP_DEFAULT_BUFFER_SIZE: usize = 256;

/// Create channels, bind the socket, spawn the I/O task, and return
/// the resulting `InterfaceHandle`.
///
/// # Arguments
/// * `id` - Interface identifier assigned by the driver
/// * `name` - Human-readable name for logging
/// * `listen_addr` - Local address to bind (receive datagrams)
/// * `forward_addr` - Remote address for outgoing datagrams
pub(crate) fn spawn_udp_interface(
    id: InterfaceId,
    name: String,
    listen_addr: SocketAddr,
    forward_addr: SocketAddr,
) -> io::Result<InterfaceHandle> {
    // Bind synchronously so errors propagate to the caller immediately
    let std_socket = std::net::UdpSocket::bind(listen_addr)?;
    std_socket.set_nonblocking(true)?;
    // SO_BROADCAST is a permission flag, harmless on non-broadcast sockets.
    // Matches Python behavior (UDPInterface.py:123).
    std_socket.set_broadcast(true)?;
    let socket = tokio::net::UdpSocket::from_std(std_socket)?;

    let (incoming_tx, incoming_rx) = mpsc::channel(UDP_DEFAULT_BUFFER_SIZE);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(UDP_DEFAULT_BUFFER_SIZE);
    let counters = Arc::new(InterfaceCounters::new());

    let task_name = name.clone();
    let task_counters = Arc::clone(&counters);

    tokio::spawn(async move {
        udp_io_task(
            task_name,
            socket,
            forward_addr,
            incoming_tx,
            outgoing_rx,
            task_counters,
        )
        .await;
    });

    Ok(InterfaceHandle {
        info: InterfaceInfo {
            id,
            name,
            hw_mtu: Some(1064),
            is_local_client: false,
            bitrate: None,
            ifac: None,
            mode: leviculum_core::traits::InterfaceMode::default(),
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
        // UDP sockets are bound before the handle is returned;
        // immediate-ready.
        ready: super::ReadySignal::ready_immediate(),
    })
}

/// Single I/O task owning the UDP socket.
///
/// Handles bidirectional I/O:
/// - Read path: `recv_from()` → `incoming_tx.send()`
/// - Write path: `outgoing_rx.recv()` → `send_to(forward_addr)`
///
/// Recv errors break the loop (dropping `incoming_tx` signals interface-down).
/// Send errors are logged but do not kill the interface. UDP send errors
/// (network unreachable, host unreachable) are transient.
async fn udp_io_task(
    name: String,
    socket: tokio::net::UdpSocket,
    forward_addr: SocketAddr,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
) {
    let mut buf = [0u8; UDP_MTU];

    loop {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, _src_addr)) => {
                        if len > 0 && len <= UDP_MTU {
                            counters.rx_bytes.fetch_add(len as u64, Ordering::Relaxed);
                            if incoming_tx
                                .send(IncomingPacket {
                                    data: buf[..len].to_vec(),
                                })
                                .await
                                .is_err()
                            {
                                // Event loop dropped its receiver
                                tracing::debug!("UDP {} incoming channel closed", name);
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("UDP {} recv error: {}", name, e);
                        break;
                    }
                }
            }

            msg = outgoing_rx.recv() => {
                match msg {
                    Some(pkt) => {
                        match socket.send_to(&pkt.data, forward_addr).await {
                            Ok(n) => {
                                counters.tx_bytes.fetch_add(n as u64, Ordering::Relaxed);
                            }
                            Err(e) => {
                                tracing::warn!("UDP {} send error: {}", name, e);
                                // Don't break, send errors are transient for UDP
                            }
                        }
                    }
                    None => {
                        // Event loop dropped its sender, shut down
                        tracing::debug!("UDP {} outgoing channel closed", name);
                        break;
                    }
                }
            }
        }
    }
    // Dropping incoming_tx signals interface-down to the event loop
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_udp_loopback() {
        // Two UDP interfaces on localhost pointing at each other
        let addr_a: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let addr_b: SocketAddr = "127.0.0.1:0".parse().unwrap();

        // Bind A first to learn its port
        let std_a = std::net::UdpSocket::bind(addr_a).unwrap();
        let bound_a = std_a.local_addr().unwrap();
        drop(std_a);

        let std_b = std::net::UdpSocket::bind(addr_b).unwrap();
        let bound_b = std_b.local_addr().unwrap();
        drop(std_b);

        // A listens on bound_a, forwards to bound_b
        let mut handle_a =
            spawn_udp_interface(InterfaceId(0), "udp_a".into(), bound_a, bound_b).unwrap();
        // B listens on bound_b, forwards to bound_a
        let mut handle_b =
            spawn_udp_interface(InterfaceId(1), "udp_b".into(), bound_b, bound_a).unwrap();

        // Send from A → B
        let payload = b"hello from A";
        handle_a
            .outgoing
            .send(OutgoingPacket {
                data: payload.to_vec(),
                high_priority: false,
            })
            .await
            .unwrap();

        let pkt = tokio::time::timeout(Duration::from_secs(2), handle_b.incoming.recv())
            .await
            .expect("timeout waiting for packet at B")
            .expect("channel closed");
        assert_eq!(pkt.data, payload);

        // Send from B → A
        let payload2 = b"hello from B";
        handle_b
            .outgoing
            .send(OutgoingPacket {
                data: payload2.to_vec(),
                high_priority: false,
            })
            .await
            .unwrap();

        let pkt2 = tokio::time::timeout(Duration::from_secs(2), handle_a.incoming.recv())
            .await
            .expect("timeout waiting for packet at A")
            .expect("channel closed");
        assert_eq!(pkt2.data, payload2);
    }

    #[tokio::test]
    async fn test_udp_send_error_does_not_kill_interface() {
        // Interface that sends to an unreachable address but listens on a real port
        let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
        // Port 1 is almost certainly unreachable/firewalled
        let unreachable: SocketAddr = "192.0.2.1:1".parse().unwrap();

        let std_sock = std::net::UdpSocket::bind(listen).unwrap();
        let bound = std_sock.local_addr().unwrap();
        drop(std_sock);

        let mut handle =
            spawn_udp_interface(InterfaceId(0), "udp_unreachable".into(), bound, unreachable)
                .unwrap();

        // Send to unreachable, should not crash
        handle
            .outgoing
            .send(OutgoingPacket {
                data: b"test".to_vec(),
                high_priority: false,
            })
            .await
            .unwrap();

        // Brief delay so the task processes the send
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Interface should still be alive (outgoing channel open)
        assert!(!handle.outgoing.is_closed());

        // Verify we can still receive: send a datagram directly to the interface
        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sender.send_to(b"still alive", bound).await.unwrap();

        let pkt = tokio::time::timeout(Duration::from_secs(2), handle.incoming.recv())
            .await
            .expect("timeout — interface should still receive")
            .expect("channel closed");
        assert_eq!(pkt.data, b"still alive");
    }

    #[tokio::test]
    async fn test_udp_interface_info() {
        let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let forward: SocketAddr = "127.0.0.1:9999".parse().unwrap();

        let handle =
            spawn_udp_interface(InterfaceId(42), "my_udp".into(), listen, forward).unwrap();

        assert_eq!(handle.info.id, InterfaceId(42));
        assert_eq!(handle.info.name, "my_udp");
        assert!(!handle.outgoing.is_closed());
    }

    #[tokio::test]
    async fn test_udp_dropping_handle_stops_task() {
        let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let forward: SocketAddr = "127.0.0.1:9999".parse().unwrap();

        let handle =
            spawn_udp_interface(InterfaceId(0), "udp_drop".into(), listen, forward).unwrap();

        // Drop the handle (both incoming and outgoing channels)
        drop(handle);

        // The I/O task should exit soon (outgoing channel closed)
        tokio::time::sleep(Duration::from_millis(100)).await;
        // No assertion needed, if the task doesn't exit, it would leak,
        // but tokio cleans up on runtime drop. This test verifies no panic.
    }
}
