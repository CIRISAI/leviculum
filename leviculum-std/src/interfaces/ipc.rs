//! Where the shared instance's two channels live, and the sockets on them.
//!
//! A shared instance serves a data channel (`rns/<instance>`, HDLC-framed
//! packets, see `local.rs`) and an RPC channel (`rns/<instance>/rpc`, the
//! `multiprocessing.connection` protocol, see `rpc/mod.rs`). Python puts both
//! on the abstract AF_UNIX socket by default and moves BOTH to TCP loopback,
//! on any platform, when the config says `shared_instance_type = tcp`, or on a
//! platform without AF_UNIX (`reference/Reticulum/RNS/Reticulum.py:683-694`,
//! the RPC address at `:348-353`). The ports are `shared_instance_port` and
//! `instance_control_port`, 37428 and 37429 by default, and on the TCP path the
//! instance name plays no part.
//!
//! This module is the one place that turns a channel name and a
//! [`SharedInstanceTransport`] into an endpoint, binds it and dials it, so the
//! listener, the clients and the socket a diagnostic names cannot disagree.
//!
//! Platform support: Linux (abstract Unix sockets) and TCP loopback on Linux
//! are the tested paths. The macOS/BSD filesystem-socket fallback and the
//! Windows TCP-loopback fallback are community-supported and are not
//! exercised by our CI.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::config::ReticulumConfig;
use crate::sync_ext::MutexRecover;

/// Python's `local_interface_port`, the data channel's default TCP port
/// (`reference/Reticulum/RNS/Reticulum.py:284`).
pub(crate) const DEFAULT_DATA_PORT: u16 = 37428;

/// Python's `local_control_port`, the RPC channel's default TCP port
/// (`reference/Reticulum/RNS/Reticulum.py:285`).
pub(crate) const DEFAULT_CONTROL_PORT: u16 = 37429;

/// How a process serves or reaches a shared instance.
///
/// The default is the abstract AF_UNIX socket keyed by the instance name
/// (TCP loopback on Windows, where there is no AF_UNIX). Built from a config
/// with [`SharedInstanceTransport::from_config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SharedInstanceTransport {
    /// Both channels on TCP loopback (`shared_instance_type = tcp`).
    pub tcp: bool,
    /// `shared_instance_port`; `None` is 37428.
    pub data_port: Option<u16>,
    /// `instance_control_port`; `None` is 37429.
    pub control_port: Option<u16>,
}

impl SharedInstanceTransport {
    /// The transport a `[reticulum]` section names, as rnsd reads it.
    pub fn from_config(reticulum: &ReticulumConfig) -> Self {
        Self {
            tcp: reticulum.shared_instance_type.as_deref() == Some("tcp"),
            data_port: reticulum.shared_instance_port,
            control_port: reticulum.instance_control_port,
        }
    }

    /// The endpoint of the channel `abstract_name` (`rns/<instance>` or
    /// `rns/<instance>/rpc`) under this transport.
    pub(crate) fn endpoint(&self, abstract_name: &str) -> IpcEndpoint {
        let is_rpc = abstract_name.ends_with("/rpc");
        if self.tcp {
            // Python's rule exactly: the configured port or its default, the
            // instance name unused.
            let port = if is_rpc {
                self.control_port.unwrap_or(DEFAULT_CONTROL_PORT)
            } else {
                self.data_port.unwrap_or(DEFAULT_DATA_PORT)
            };
            return IpcEndpoint::Tcp(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        }
        #[cfg(windows)]
        {
            let port = super::local::resolve_loopback_port(
                abstract_name,
                self.data_port.unwrap_or(0),
                self.control_port.unwrap_or(0),
            );
            IpcEndpoint::Tcp(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        }
        #[cfg(unix)]
        {
            IpcEndpoint::Unix(abstract_name.to_string())
        }
    }
}

/// The transport this process's client tools dial when nobody says
/// otherwise. A client tool sets it once from the config it read
/// ([`set_shared_instance_transport`]); a daemon never reads it, it takes its
/// transport from its own config through the node builder.
static PROCESS_TRANSPORT: Mutex<SharedInstanceTransport> = Mutex::new(SharedInstanceTransport {
    tcp: false,
    data_port: None,
    control_port: None,
});

/// Point this process's shared-instance clients (the RPC queries and a node
/// built without a config) at `transport`. A client tool calls it with the
/// transport of the config it read, so it dials where an rnsd or lnsd built
/// from that config listens.
pub fn set_shared_instance_transport(transport: SharedInstanceTransport) {
    *PROCESS_TRANSPORT.lock_recover() = transport;
}

/// The transport set by [`set_shared_instance_transport`], else the default.
pub fn shared_instance_transport() -> SharedInstanceTransport {
    *PROCESS_TRANSPORT.lock_recover()
}

/// One shared-instance channel's address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IpcEndpoint {
    /// The abstract socket name without the NUL (`rns/<instance>`), or on a
    /// non-Linux Unix the same name mapped to a file under the temp dir.
    #[cfg(unix)]
    Unix(String),
    /// TCP loopback.
    Tcp(SocketAddr),
}

#[cfg(all(unix, not(target_os = "linux")))]
fn filesystem_socket_path(abstract_name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("leviculum-{}", abstract_name.replace('/', "-")))
}

impl std::fmt::Display for IpcEndpoint {
    /// Spelled for an operator's eyes: `@rns/<name>` for the Linux abstract
    /// socket (the form `ss -xlp` prints), the file on other Unixes, the
    /// `127.0.0.1:<port>` of the TCP path.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(target_os = "linux")]
            IpcEndpoint::Unix(name) => write!(f, "@{name}"),
            #[cfg(all(unix, not(target_os = "linux")))]
            IpcEndpoint::Unix(name) => write!(f, "{}", filesystem_socket_path(name).display()),
            IpcEndpoint::Tcp(addr) => write!(f, "{addr}"),
        }
    }
}

impl IpcEndpoint {
    /// Bind a listener here. Must run inside a tokio runtime.
    pub(crate) fn bind(&self) -> io::Result<IpcListener> {
        match self {
            #[cfg(unix)]
            IpcEndpoint::Unix(name) => {
                let std_listener = bind_unix(name)?;
                std_listener.set_nonblocking(true)?;
                Ok(IpcListener::Unix(tokio::net::UnixListener::from_std(
                    std_listener,
                )?))
            }
            IpcEndpoint::Tcp(addr) => {
                let std_listener = std::net::TcpListener::bind(addr)?;
                std_listener.set_nonblocking(true)?;
                Ok(IpcListener::Tcp(tokio::net::TcpListener::from_std(
                    std_listener,
                )?))
            }
        }
    }

    /// Dial here. The connect itself is synchronous: on a loopback transport
    /// it succeeds or fails at once. Must run inside a tokio runtime.
    pub(crate) fn connect(&self) -> io::Result<IpcStream> {
        match self {
            #[cfg(unix)]
            IpcEndpoint::Unix(name) => {
                let std_stream = connect_unix(name)?;
                std_stream.set_nonblocking(true)?;
                Ok(IpcStream::Unix(tokio::net::UnixStream::from_std(
                    std_stream,
                )?))
            }
            IpcEndpoint::Tcp(addr) => {
                let std_stream = std::net::TcpStream::connect(addr)?;
                // Python sets TCP_NODELAY on the IPC socket
                // (`reference/Reticulum/RNS/Interfaces/LocalInterface.py:99`).
                std_stream.set_nodelay(true)?;
                std_stream.set_nonblocking(true)?;
                Ok(IpcStream::Tcp(tokio::net::TcpStream::from_std(std_stream)?))
            }
        }
    }
}

#[cfg(unix)]
fn bind_unix(abstract_name: &str) -> io::Result<std::os::unix::net::UnixListener> {
    #[cfg(target_os = "linux")]
    {
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        std::os::unix::net::UnixListener::bind_addr(&addr)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let path = filesystem_socket_path(abstract_name);
        // Remove a stale socket file if it exists.
        let _ = std::fs::remove_file(&path);
        std::os::unix::net::UnixListener::bind(&path)
    }
}

#[cfg(unix)]
fn connect_unix(abstract_name: &str) -> io::Result<std::os::unix::net::UnixStream> {
    #[cfg(target_os = "linux")]
    {
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        std::os::unix::net::UnixStream::connect_addr(&addr)
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::os::unix::net::UnixStream::connect(filesystem_socket_path(abstract_name))
    }
}

/// A bound shared-instance listener.
pub(crate) enum IpcListener {
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
    Tcp(tokio::net::TcpListener),
}

impl IpcListener {
    /// Accept one connection, with the peer's TCP port on the TCP path:
    /// Python names an accepted TCP client by it
    /// (`reference/Reticulum/RNS/Interfaces/LocalInterface.py:433`).
    pub(crate) async fn accept(&self) -> io::Result<(IpcStream, Option<u16>)> {
        match self {
            #[cfg(unix)]
            IpcListener::Unix(l) => {
                let (s, _) = l.accept().await?;
                Ok((IpcStream::Unix(s), None))
            }
            IpcListener::Tcp(l) => {
                let (s, peer) = l.accept().await?;
                s.set_nodelay(true)?;
                Ok((IpcStream::Tcp(s), Some(peer.port())))
            }
        }
    }

    /// The bound TCP address, `None` on the Unix path.
    pub(crate) fn tcp_addr(&self) -> Option<SocketAddr> {
        match self {
            #[cfg(unix)]
            IpcListener::Unix(_) => None,
            IpcListener::Tcp(l) => l.local_addr().ok(),
        }
    }
}

/// A connected shared-instance stream, either transport.
pub(crate) enum IpcStream {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    Tcp(tokio::net::TcpStream),
}

impl AsyncRead for IpcStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            IpcStream::Unix(s) => Pin::new(s).poll_read(cx, buf),
            IpcStream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for IpcStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            #[cfg(unix)]
            IpcStream::Unix(s) => Pin::new(s).poll_write(cx, buf),
            IpcStream::Tcp(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            IpcStream::Unix(s) => Pin::new(s).poll_flush(cx),
            IpcStream::Tcp(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            IpcStream::Unix(s) => Pin::new(s).poll_shutdown(cx),
            IpcStream::Tcp(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp(data: Option<u16>, control: Option<u16>) -> SharedInstanceTransport {
        SharedInstanceTransport {
            tcp: true,
            data_port: data,
            control_port: control,
        }
    }

    /// `shared_instance_type = tcp` puts the data channel on 37428 and the
    /// RPC channel on 37429 whatever the instance is called, as Python does
    /// (`reference/Reticulum/RNS/Reticulum.py:348-353`, `:690-691`).
    #[test]
    fn tcp_mode_uses_python_ports_and_ignores_the_instance_name() {
        let t = tcp(None, None);
        for name in ["default", "other"] {
            assert_eq!(
                t.endpoint(&format!("rns/{name}")),
                IpcEndpoint::Tcp("127.0.0.1:37428".parse().unwrap())
            );
            assert_eq!(
                t.endpoint(&format!("rns/{name}/rpc")),
                IpcEndpoint::Tcp("127.0.0.1:37429".parse().unwrap())
            );
        }
    }

    #[test]
    fn tcp_mode_honours_the_configured_ports() {
        let t = tcp(Some(40001), Some(40002));
        assert_eq!(
            t.endpoint("rns/default").to_string(),
            "127.0.0.1:40001".to_string()
        );
        assert_eq!(
            t.endpoint("rns/default/rpc").to_string(),
            "127.0.0.1:40002".to_string()
        );
    }

    /// The default stays the abstract socket, ports or not.
    #[cfg(target_os = "linux")]
    #[test]
    fn default_mode_is_the_abstract_socket() {
        let t = SharedInstanceTransport {
            tcp: false,
            data_port: Some(40001),
            control_port: Some(40002),
        };
        assert_eq!(
            t.endpoint("rns/x/rpc"),
            IpcEndpoint::Unix("rns/x/rpc".to_string())
        );
        assert_eq!(t.endpoint("rns/x").to_string(), "@rns/x");
    }

    #[test]
    fn from_config_reads_the_three_keys() {
        let config = crate::ini_config::parse_ini(
            "[reticulum]\n  shared_instance_type = tcp\n  shared_instance_port = 40011\n  instance_control_port = 40012\n",
        )
        .unwrap();
        assert_eq!(
            SharedInstanceTransport::from_config(&config.reticulum),
            tcp(Some(40011), Some(40012))
        );
        let config =
            crate::ini_config::parse_ini("[reticulum]\n  shared_instance_port = 40011\n").unwrap();
        assert!(!SharedInstanceTransport::from_config(&config.reticulum).tcp);
    }

    /// A TCP endpoint binds and dials, and bytes cross it both ways.
    #[tokio::test]
    async fn a_tcp_endpoint_binds_dials_and_carries_bytes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = IpcEndpoint::Tcp("127.0.0.1:0".parse().unwrap())
            .bind()
            .unwrap();
        let addr = listener.tcp_addr().unwrap();
        let mut client = IpcEndpoint::Tcp(addr).connect().unwrap();
        let (mut server, peer_port) = listener.accept().await.unwrap();
        assert!(peer_port.is_some());
        client.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }
}
