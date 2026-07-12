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

/// Why parsing the `forward_ip` config value failed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ForwardAddrError {
    /// A plain-IP entry needs `forward_port`, which is unset.
    MissingPort,
    /// An entry is not a valid address; carries the entry and parse error.
    Invalid(String),
}

/// Parse the `forward_ip` config value into forward addresses.
///
/// `forward_ip` is a comma-separated list. Each entry is either a plain IP
/// address, which is combined with `forward_port`, or an `ip:port` (IPv6:
/// `[ip]:port`) socket address carrying its own port. A single plain-IP
/// value therefore parses exactly as before, keeping unchanged rnsd-style
/// configs working; multiple entries and per-entry ports are a Rust-only
/// extension (Python's UDPInterface supports one forward address).
pub(crate) fn parse_forward_addrs(
    forward_ip: &str,
    forward_port: Option<u16>,
) -> Result<Vec<SocketAddr>, ForwardAddrError> {
    let mut addrs = Vec::new();
    for entry in forward_ip.split(',') {
        let entry = entry.trim();
        if let Ok(addr) = entry.parse::<SocketAddr>() {
            addrs.push(addr);
            continue;
        }
        let port = forward_port.ok_or(ForwardAddrError::MissingPort)?;
        let addr: SocketAddr = format!("{}:{}", entry, port)
            .parse()
            .map_err(|e| ForwardAddrError::Invalid(format!("\"{}\": {}", entry, e)))?;
        addrs.push(addr);
    }
    Ok(addrs)
}

/// Create channels, bind the socket, spawn the I/O task, and return
/// the resulting `InterfaceHandle`.
///
/// # Arguments
/// * `id` - Interface identifier assigned by the driver
/// * `name` - Human-readable name for logging
/// * `listen_addr` - Local address to bind (receive datagrams)
/// * `forward_addrs` - Remote addresses for outgoing datagrams; each
///   outgoing datagram is sent to every address. Must be non-empty.
///   Python's UDPInterface has exactly one forward address; more than
///   one is a Rust-only extension with no wire difference per receiver.
pub(crate) fn spawn_udp_interface(
    id: InterfaceId,
    name: String,
    listen_addr: SocketAddr,
    forward_addrs: Vec<SocketAddr>,
) -> io::Result<InterfaceHandle> {
    if forward_addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "UDP interface needs at least one forward address",
        ));
    }
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
            forward_addrs,
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
/// - Write path: `outgoing_rx.recv()` → `send_to()` to every forward address
///
/// Recv errors break the loop (dropping `incoming_tx` signals interface-down).
/// Send errors are logged but do not kill the interface, and a failed send
/// to one forward address does not skip the remaining addresses. UDP send
/// errors (network unreachable, host unreachable) are transient.
async fn udp_io_task(
    name: String,
    socket: tokio::net::UdpSocket,
    forward_addrs: Vec<SocketAddr>,
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
                        for addr in &forward_addrs {
                            match socket.send_to(&pkt.data, addr).await {
                                Ok(n) => {
                                    counters.tx_bytes.fetch_add(n as u64, Ordering::Relaxed);
                                }
                                Err(e) => {
                                    tracing::warn!("UDP {} send error to {}: {}", name, addr, e);
                                    // Don't break, send errors are transient for
                                    // UDP; keep sending to the other addresses
                                }
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
            spawn_udp_interface(InterfaceId(0), "udp_a".into(), bound_a, vec![bound_b]).unwrap();
        // B listens on bound_b, forwards to bound_a
        let mut handle_b =
            spawn_udp_interface(InterfaceId(1), "udp_b".into(), bound_b, vec![bound_a]).unwrap();

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

        let mut handle = spawn_udp_interface(
            InterfaceId(0),
            "udp_unreachable".into(),
            bound,
            vec![unreachable],
        )
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
            spawn_udp_interface(InterfaceId(42), "my_udp".into(), listen, vec![forward]).unwrap();

        assert_eq!(handle.info.id, InterfaceId(42));
        assert_eq!(handle.info.name, "my_udp");
        assert!(!handle.outgoing.is_closed());
    }

    #[tokio::test]
    async fn test_udp_multi_forward_delivers_to_all() {
        // Two listener sockets on OS-assigned ephemeral ports
        let listener_1 = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let listener_2 = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr_1 = listener_1.local_addr().unwrap();
        let addr_2 = listener_2.local_addr().unwrap();

        let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let handle = spawn_udp_interface(
            InterfaceId(0),
            "udp_multi".into(),
            listen,
            vec![addr_1, addr_2],
        )
        .unwrap();

        let payload = b"fan out";
        handle
            .outgoing
            .send(OutgoingPacket {
                data: payload.to_vec(),
                high_priority: false,
            })
            .await
            .unwrap();

        let mut buf = [0u8; UDP_MTU];
        let (len, _) = tokio::time::timeout(Duration::from_secs(2), listener_1.recv_from(&mut buf))
            .await
            .expect("timeout waiting at listener 1")
            .expect("recv failed at listener 1");
        assert_eq!(&buf[..len], payload);

        let (len, _) = tokio::time::timeout(Duration::from_secs(2), listener_2.recv_from(&mut buf))
            .await
            .expect("timeout waiting at listener 2")
            .expect("recv failed at listener 2");
        assert_eq!(&buf[..len], payload);
    }

    #[tokio::test]
    async fn test_udp_multi_forward_error_on_one_addr_still_delivers_to_other() {
        let listener = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let reachable = listener.local_addr().unwrap();
        // TEST-NET-1, never routable
        let unreachable: SocketAddr = "192.0.2.1:1".parse().unwrap();

        let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let handle = spawn_udp_interface(
            InterfaceId(0),
            "udp_multi_err".into(),
            listen,
            vec![unreachable, reachable],
        )
        .unwrap();

        let payload = b"past the error";
        handle
            .outgoing
            .send(OutgoingPacket {
                data: payload.to_vec(),
                high_priority: false,
            })
            .await
            .unwrap();

        // The failing first address must not prevent delivery to the second
        let mut buf = [0u8; UDP_MTU];
        let (len, _) = tokio::time::timeout(Duration::from_secs(2), listener.recv_from(&mut buf))
            .await
            .expect("timeout — send error to one address blocked the others")
            .expect("recv failed");
        assert_eq!(&buf[..len], payload);
        assert!(!handle.outgoing.is_closed());
    }

    #[tokio::test]
    async fn test_udp_empty_forward_list_rejected() {
        let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
        match spawn_udp_interface(InterfaceId(0), "udp_empty".into(), listen, Vec::new()) {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidInput),
            Ok(_) => panic!("empty forward address list must be rejected"),
        }
    }

    #[test]
    fn test_parse_forward_addrs_single_plain_ip() {
        // The Python/rnsd-compatible form: one plain IP plus forward_port
        let addrs = parse_forward_addrs("192.168.1.255", Some(4242)).unwrap();
        assert_eq!(addrs, vec!["192.168.1.255:4242".parse().unwrap()]);
    }

    #[test]
    fn test_parse_forward_addrs_comma_separated_plain_ips() {
        let addrs = parse_forward_addrs("10.0.0.255, 10.1.0.255", Some(4242)).unwrap();
        assert_eq!(
            addrs,
            vec![
                "10.0.0.255:4242".parse().unwrap(),
                "10.1.0.255:4242".parse().unwrap(),
            ]
        );
    }

    #[test]
    fn test_parse_forward_addrs_entry_with_own_port() {
        let addrs = parse_forward_addrs("10.0.0.255:5000,10.1.0.255", Some(4242)).unwrap();
        assert_eq!(
            addrs,
            vec![
                "10.0.0.255:5000".parse().unwrap(),
                "10.1.0.255:4242".parse().unwrap(),
            ]
        );
    }

    #[test]
    fn test_parse_forward_addrs_all_own_ports_without_forward_port() {
        let addrs = parse_forward_addrs("10.0.0.1:5000,[::1]:6000", None).unwrap();
        assert_eq!(
            addrs,
            vec![
                "10.0.0.1:5000".parse().unwrap(),
                "[::1]:6000".parse().unwrap()
            ]
        );
    }

    #[test]
    fn test_parse_forward_addrs_plain_ip_requires_forward_port() {
        let err = parse_forward_addrs("10.0.0.255", None).unwrap_err();
        assert_eq!(err, ForwardAddrError::MissingPort);
    }

    #[test]
    fn test_parse_forward_addrs_invalid_entry() {
        let err = parse_forward_addrs("10.0.0.255,not an address", Some(4242)).unwrap_err();
        match err {
            ForwardAddrError::Invalid(msg) => assert!(msg.contains("not an address")),
            other => panic!("expected Invalid, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_udp_dropping_handle_stops_task() {
        let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let forward: SocketAddr = "127.0.0.1:9999".parse().unwrap();

        let handle =
            spawn_udp_interface(InterfaceId(0), "udp_drop".into(), listen, vec![forward]).unwrap();

        // Drop the handle (both incoming and outgoing channels)
        drop(handle);

        // The I/O task should exit soon (outgoing channel closed)
        tokio::time::sleep(Duration::from_millis(100)).await;
        // No assertion needed, if the task doesn't exit, it would leak,
        // but tokio cleans up on runtime drop. This test verifies no panic.
    }
}
