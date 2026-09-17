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
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use leviculum_core::constants::MTU;
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::transport::InterfaceId;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use super::inventory::{self as inventory_names, InterfaceIdentity, ListenerRow, SharedInventory};
use super::{IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket};
use crate::sync_ext::MutexRecover;

// Platform IPC transport. Unix domain sockets on Unix; TCP loopback on Windows,
// matching Python-RNS, which falls back to 127.0.0.1 (AF_INET) when AF_UNIX is
// unavailable (default local_interface_port 37428 / local_control_port 37429).
// `UnixStream`/`TcpStream` are symmetric (both halves impl AsyncRead/AsyncWrite
// and `into_split()`), so the I/O code below is unchanged across platforms.
//
// Platform support: Linux (abstract Unix sockets) is the tested path, exercised
// by our CI. The macOS/BSD filesystem-socket and Windows TCP-loopback fallbacks
// below are community-supported and are not exercised by our CI.
#[cfg(windows)]
use tokio::net::TcpListener as LocalListener;
#[cfg(windows)]
use tokio::net::TcpStream as LocalStream;
#[cfg(unix)]
use tokio::net::UnixListener as LocalListener;
#[cfg(unix)]
use tokio::net::UnixStream as LocalStream;

/// Bind a local listener for the given abstract instance name.
///
/// On Linux, uses abstract Unix sockets (`\0name`); on other Unix systems,
/// filesystem sockets in the temp directory.
#[cfg(unix)]
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

/// Windows: bind a TCP loopback listener, matching Python-RNS's AF_INET fallback.
#[cfg(windows)]
fn bind_local_listener(abstract_name: &str) -> Result<std::net::TcpListener, io::Error> {
    std::net::TcpListener::bind(loopback_addr(abstract_name))
}

/// Connect to a local shared instance by abstract name.
#[cfg(unix)]
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

/// Windows: connect to the TCP loopback shared instance.
#[cfg(windows)]
fn connect_local(abstract_name: &str) -> Result<std::net::TcpStream, io::Error> {
    std::net::TcpStream::connect(loopback_addr(abstract_name))
}

/// Configured TCP-loopback ports for the shared-instance data (`.0`) and RPC
/// (`.1`) channels (`shared_instance_port` / `instance_control_port`). `0`
/// means unset, so `loopback_addr` falls back to the Python defaults. Set once
/// at process startup from the parsed config (see `set_loopback_ports`).
///
/// Only the AF_INET (Windows / `shared_instance_type = tcp`) path reads these;
/// the AF_UNIX path keys the socket by `instance_name` and ignores the ports,
/// matching Python. The store is unconditional so the config value is captured
/// on every platform; only the Windows reader consults it.
static CONFIGURED_LOOPBACK_PORTS: (AtomicU32, AtomicU32) = (AtomicU32::new(0), AtomicU32::new(0));

/// Record the configured shared-instance TCP-loopback ports for later binds.
///
/// Called from the node builder with the parsed `shared_instance_port` /
/// `instance_control_port`. `None` leaves the Python default in force. A no-op
/// on the AF_UNIX path (the ports are never read there).
pub(crate) fn set_loopback_ports(
    shared_instance_port: Option<u16>,
    instance_control_port: Option<u16>,
) {
    CONFIGURED_LOOPBACK_PORTS.0.store(
        u32::from(shared_instance_port.unwrap_or(0)),
        Ordering::Relaxed,
    );
    CONFIGURED_LOOPBACK_PORTS.1.store(
        u32::from(instance_control_port.unwrap_or(0)),
        Ordering::Relaxed,
    );
}

/// Resolve the TCP-loopback port for an abstract shared-instance name.
///
/// A configured override (`shared_instance_port` for the data channel,
/// `instance_control_port` for the `/rpc` channel) wins. Otherwise the Python
/// defaults hold — 37428 (`local_interface_port`) for the data channel, 37429
/// (`local_control_port`) for RPC — and a non-default instance name derives a
/// stable FNV-1a port so independent *leviculum* peers agree without config
/// (a leviculum-local convention that does not match Python's port for the same
/// name). Kept platform-agnostic so the resolution is unit-testable off Windows.
#[cfg_attr(not(any(test, windows)), allow(dead_code))]
pub(crate) fn resolve_loopback_port(
    abstract_name: &str,
    configured_data: u16,
    configured_control: u16,
) -> u16 {
    let is_rpc = abstract_name.ends_with("/rpc");
    let configured = if is_rpc {
        configured_control
    } else {
        configured_data
    };
    if configured != 0 {
        return configured;
    }
    match abstract_name {
        "rns/default" => 37428,
        "rns/default/rpc" => 37429,
        other => name_to_port(other),
    }
}

/// Map an abstract instance name to a TCP loopback address (Windows).
///
/// Python-RNS, when AF_UNIX is unavailable, binds fixed ports — 37428
/// (`local_interface_port`) for the shared instance and 37429
/// (`local_control_port`) for RPC — and, critically, does **not** derive a
/// port from `instance_name` on the AF_INET path (instance_name only varies
/// the AF_UNIX socket name; see Reticulum.py). A Windows `rnsd` runs multiple
/// instances by setting `shared_instance_port`/`instance_control_port`
/// explicitly, not by hashing the name.
///
/// A configured `shared_instance_port` / `instance_control_port` (captured by
/// `set_loopback_ports`) overrides the default; otherwise the default-instance
/// path matches 37428/37429 and interops cleanly with a Windows `rnsd`, and a
/// non-default instance name derives a stable FNV-1a port (see
/// `resolve_loopback_port`).
#[cfg(windows)]
pub(crate) fn loopback_addr(abstract_name: &str) -> std::net::SocketAddr {
    use std::net::{Ipv4Addr, SocketAddr};
    let port = resolve_loopback_port(
        abstract_name,
        CONFIGURED_LOOPBACK_PORTS.0.load(Ordering::Relaxed) as u16,
        CONFIGURED_LOOPBACK_PORTS.1.load(Ordering::Relaxed) as u16,
    );
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// Stable FNV-1a hash of a name into the unprivileged 37430..=65534 range.
///
/// Only reached via the Windows loopback path (or its unit tests); allowed to
/// be dead on the non-test AF_UNIX build.
#[cfg_attr(not(any(test, windows)), allow(dead_code))]
pub(crate) fn name_to_port(name: &str) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    37430 + (h % (65535 - 37430)) as u16
}

/// Default channel buffer size for local interfaces.
///
/// Sized to absorb announce-burst fan-out from transit peers: a single
/// transit-active node has been observed emitting ~500 directed
/// SendPackets per Local-Client in a single event-loop tick. 4096 gives
/// 16× headroom on the original 256-cap and ~8× on the worst-case burst.
pub(crate) const LOCAL_DEFAULT_BUFFER_SIZE: usize = 4096;

/// Hardware MTU for local interfaces (same as TCP, local IPC).
const LOCAL_HW_MTU: u32 = 262_144;

/// Bitrate a shared-instance interface reports: 1 Gbps, as the reference sets
/// on both the server and every accepted client (LocalInterface.py:431).
pub(crate) const LOCAL_BITRATE: i64 = 1_000_000_000;

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
/// The shared-instance server itself carries no packets, so it never becomes
/// a routable interface; like a TCP listener it is announced to the reporting
/// inventory (Codeberg #177), and every accepted IPC client registers its
/// reference display identity there.
///
/// The accept loop exits when the event loop drops `new_interface_rx`
/// (detected via `Sender::closed()`).
pub(crate) fn spawn_local_server(
    instance_name: &str,
    next_id: Arc<AtomicUsize>,
    new_interface_tx: mpsc::Sender<InterfaceHandle>,
    buffer_size: usize,
    server_id: usize,
    inventory: SharedInventory,
    live_clients: Arc<AtomicUsize>,
) -> Result<(), io::Error> {
    // Build abstract socket name: "rns/{instance_name}"
    let abstract_name = format!("rns/{}", instance_name);

    let std_listener = bind_local_listener(&abstract_name)?;
    std_listener.set_nonblocking(true)?;
    let listener = LocalListener::from_std(std_listener)?;

    tracing::info!("Local server listening on socket {}", abstract_name);

    inventory.lock_recover().add_listener(
        server_id,
        ListenerRow {
            identity: InterfaceIdentity {
                name: inventory_names::shared_instance_name(&abstract_name),
                // Python names the shared-instance server "Reticulum"
                // (LocalInterface.py:391).
                short_name: "Reticulum".to_string(),
                type_name: "LocalServerInterface",
                parent: None,
            },
            bitrate: LOCAL_BITRATE,
            hw_mtu: LOCAL_HW_MTU as i64,
            mode: leviculum_core::traits::InterfaceMode::default(),
            // The reference pins all three to None on this interface
            // (LocalInterface.py:427-429), unlike a config interface.
            announce_rate: (None, None, None),
            ifac_size_bits: None,
            departed_rxb: 0,
            departed_txb: 0,
            bound_addr: None,
        },
    );

    let instance_name_owned = abstract_name.clone();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _peer_addr)) => {
                            let id = InterfaceId(next_id.fetch_add(1, Ordering::Relaxed));
                            // Python labels an accepted client with the LIVE
                            // client count at accept time (LocalInterface.py:441
                            // with the matching `clients -= 1` on teardown,
                            // LocalInterface.py:355), so the counter has to be
                            // the live one, not a monotonic connection index.
                            let client_num = live_clients.fetch_add(1, Ordering::Relaxed);
                            let name = format!("Local[{}]/{}", instance_name_owned, client_num);
                            inventory.lock_recover().add_spawned(
                                id.0,
                                InterfaceIdentity {
                                    name: inventory_names::local_client_name(&instance_name_owned),
                                    short_name: inventory_names::local_client_short_name(
                                        client_num,
                                        &instance_name_owned,
                                    ),
                                    type_name: "LocalClientInterface",
                                    parent: Some(server_id),
                                },
                            );
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
    stream: LocalStream,
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
            announce_cap_bitrate: None,
            tx_jitter_max_ms: None,
            ifac: None,
            mode: leviculum_core::traits::InterfaceMode::default(),
            kind: leviculum_core::traits::InterfaceKind::Local,
            // A shared-instance IPC client is never ingress-limited, in the
            // reference by construction: `LocalClientInterface` overrides
            // `should_ingress_limit()` to return False unconditionally
            // (LocalInterface.py:137-138), regardless of the flat
            // `ingress_control = True` it inherits from `Interface.__init__`.
            // Stated here rather than left to a fallback so the local server
            // says what it means (Codeberg #189).
            ingress_control: Some(false),
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
        // The IPC stream already exists when this function
        // is called (server-accepted), so the interface is ready
        // immediately.
        ready: super::ReadySignal::ready_immediate(),
    }
}

/// Name the socket a failed shared-instance connect was aiming at, and, when
/// nothing was listening there, what to do about it.
///
/// The raw error is `Connection refused (os error 111)` with no hint of what
/// was being connected to, which every client of the shared instance
/// (`lblogd`, `lnomad`, `lncp`, `lnstatus`) then surfaces verbatim. Naming
/// the socket also makes an `instance_name` mismatch visible: the socket in
/// the message is the one the client wanted, so it can be compared against
/// what the daemon actually listens on. The error kind is preserved so
/// callers matching on it keep working.
fn absent_daemon_error(source: io::Error, abstract_name: &str) -> io::Error {
    // NotFound is the same condition on the non-Linux filesystem-socket path.
    let nobody_listening = matches!(
        source.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
    );
    let message = if nobody_listening {
        format!(
            "no local Leviculum daemon reachable over IPC socket \"{abstract_name}\": \
             is lnsd or rnsd running?"
        )
    } else {
        format!("IPC socket \"{abstract_name}\": {source}")
    };
    io::Error::new(source.kind(), message)
}

/// How long a shared-instance connection must hold before it counts as
/// healthy, for the purpose of resetting the reconnect backoff.
///
/// A daemon that accepts and immediately drops (a crash loop under a
/// supervisor, a half-configured instance) would otherwise reset the
/// backoff on every accept and be dialled at the base interval forever.
/// A connection that survives this long has done real work; one that does
/// not keeps the outage's attempt count and keeps backing off.
const LOCAL_STABLE_CONNECTION: Duration = Duration::from_secs(8);

/// Delay before the first few reconnect attempts.
///
/// The common case this exists for is `systemctl restart lnsd`, which is
/// over in well under a second. Python waits `RECONNECT_WAIT = 8` s before
/// its FIRST retry (LocalInterface.py:63,167), so a Python client is off
/// the mesh for at least 8 s after a restart that took 200 ms. Dialling a
/// local socket is close to free, so the first attempts come quickly and
/// the restart costs a fraction of a second.
const LOCAL_RECONNECT_BASE: Duration = Duration::from_millis(250);

/// Ceiling on the reconnect backoff — Python's `RECONNECT_WAIT`.
///
/// A daemon that is down for a package upgrade, or gone for good, is
/// retried at most this often. Landing on Python's own steady-state
/// cadence is deliberate: an operator running both stacks sees the same
/// retry pressure and the same journal volume from either, while our first
/// attempts still heal a restart far faster than Python's do. We never give
/// up (no attempt limit): giving up is what the finding was about.
const LOCAL_RECONNECT_MAX: Duration = Duration::from_secs(8);

/// Configuration for a reconnecting shared-instance client interface.
pub(crate) struct LocalClientConfig {
    pub id: InterfaceId,
    /// Instance name, without the `rns/` prefix.
    pub instance_name: String,
    pub buffer_size: usize,
    /// Fired after every successful RE-connect (never after the first
    /// connect), so the driver can re-announce this node's destinations on
    /// the recovered interface — Python's
    /// `Transport.shared_connection_reappeared` (Transport.py:3158-3162).
    pub reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    /// Fired the moment the daemon connection is lost, so the driver can
    /// drop the routing state cached against this interface — Python's
    /// `Transport.shared_connection_disappeared` (Transport.py:3143-3155).
    pub disconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    /// Backoff base and ceiling. Production passes `None` and gets
    /// [`LOCAL_RECONNECT_BASE`] / [`LOCAL_RECONNECT_MAX`]; tests shorten
    /// them so a retry schedule can be observed in milliseconds.
    pub backoff: Option<(Duration, Duration)>,
}

impl LocalClientConfig {
    /// The production configuration: default backoff, no notifications.
    pub(crate) fn new(id: InterfaceId, instance_name: &str, buffer_size: usize) -> Self {
        Self {
            id,
            instance_name: instance_name.to_string(),
            buffer_size,
            reconnect_notify: None,
            disconnect_notify: None,
            backoff: None,
        }
    }
}

/// Bounded exponential backoff between reconnect attempts.
///
/// Attempts `1..=3` wait `base`, then the delay doubles each attempt and is
/// clamped at `max`. Monotonically non-decreasing in `attempt` and never
/// above `max`, so a daemon that never comes back is dialled at most once
/// per `max` — it cannot spin. Same shape as the TCP client's
/// [`backoff_delay`](super::tcp), kept separate because the two have
/// different constants and different reasons for them.
fn local_backoff_delay(attempt: u64, base: Duration, max: Duration) -> Duration {
    if attempt <= 3 {
        return base.min(max);
    }
    let doublings = attempt - 3;
    let scaled = if doublings >= 128 {
        u128::MAX
    } else {
        base.as_nanos().saturating_mul(1u128 << doublings)
    };
    let capped = scaled.min(max.as_nanos());
    Duration::from_nanos(capped.min(u64::MAX as u128) as u64)
}

/// Whether a failed attempt gets a `warn!` rather than a `debug!`.
///
/// Every attempt is logged either way — the operator complaint behind this
/// work was silence, not verbosity. What the level decides is how loud a
/// daemon that stays down is on a default (INFO) journal: attempts 1..=3
/// and each doubling after them, so the outage keeps announcing itself at
/// a widening interval instead of once per capped retry forever.
fn local_failure_is_loud(attempt: u64) -> bool {
    attempt <= 3 || attempt.is_power_of_two()
}

/// Connect to an existing shared instance daemon as a client, and keep the
/// connection up across restarts of that daemon.
///
/// Connects to the abstract Unix socket `\0rns/{instance_name}` and returns
/// an `InterfaceHandle`. The handle has `is_local_client = false` because
/// from the client's perspective this is a regular interface; the daemon
/// marks its side as `is_local_client = true`.
///
/// Calls `tokio::spawn` for the I/O task, must be called from a context
/// where a tokio runtime is active (same as `spawn_local_server`).
///
/// **The first connect is synchronous and its failure is returned**: a
/// client started against a daemon that is not there says so and exits,
/// exactly as before. That is a different situation from losing a daemon
/// that was there, and Python separates them the same way: a `connect`
/// that raises out of the constructor (LocalInterface.py:112) takes
/// Reticulum down, while a socket that closes later goes to `reconnect`
/// (LocalInterface.py:312).
///
/// **Interface identity is preserved across a reconnect.** One handle, one
/// interface index, one pair of channels, one mode, for the life of the
/// process; the driver is never told the interface went away, so nothing
/// re-registers it and nothing renumbers it. The consequences are
/// deliberate and are the reason the notification channels exist: the
/// transport's own cache against that index is dropped on the loss
/// (`disconnect_notify`) because a restarted daemon has an empty path
/// table, and this node's destinations are re-announced on the recovered
/// interface (`reconnect_notify`) because the restarted daemon has never
/// heard them. Packets the driver queues during the outage wait in the
/// outgoing channel and go out on the new stream.
pub(crate) fn spawn_local_client(config: LocalClientConfig) -> Result<InterfaceHandle, io::Error> {
    let abstract_name = format!("rns/{}", config.instance_name);

    // The first connect, synchronous: an absent daemon at startup is an
    // error for the caller, not something to wait for.
    let stream = connect_local_stream(&abstract_name)?;

    let (incoming_tx, incoming_rx) = mpsc::channel(config.buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(config.buffer_size);
    let counters = Arc::new(InterfaceCounters::new());

    let name = format!("LocalClient[{}]", config.instance_name);
    let (base, max) = config
        .backoff
        .unwrap_or((LOCAL_RECONNECT_BASE, LOCAL_RECONNECT_MAX));
    let task = LocalClientTask {
        id: config.id,
        name: name.clone(),
        abstract_name,
        base,
        max,
        counters: Arc::clone(&counters),
        reconnect_notify: config.reconnect_notify,
        disconnect_notify: config.disconnect_notify,
    };

    tokio::spawn(task.run(stream, incoming_tx, outgoing_rx));

    Ok(InterfaceHandle {
        info: InterfaceInfo {
            id: config.id,
            name,
            hw_mtu: Some(LOCAL_HW_MTU),
            is_local_client: false,
            bitrate: None,
            announce_cap_bitrate: None,
            tx_jitter_max_ms: None,
            ifac: None,
            mode: leviculum_core::traits::InterfaceMode::default(),
            kind: leviculum_core::traits::InterfaceKind::Local,
            ingress_control: None,
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
        // The first connect above already succeeded, so the interface is
        // ready immediately. The signal is idempotent and stays ready
        // across reconnects, like the TCP client's.
        ready: super::ReadySignal::ready_immediate(),
    })
}

/// One connect attempt, wrapped in the error message that names the socket.
fn connect_local_stream(abstract_name: &str) -> Result<LocalStream, io::Error> {
    let std_stream =
        connect_local(abstract_name).map_err(|e| absent_daemon_error(e, abstract_name))?;
    std_stream.set_nonblocking(true)?;
    LocalStream::from_std(std_stream)
}

/// The reconnect loop for a shared-instance client. Owns the channel
/// endpoints across every connection the interface has in its life.
struct LocalClientTask {
    id: InterfaceId,
    name: String,
    abstract_name: String,
    base: Duration,
    max: Duration,
    counters: Arc<InterfaceCounters>,
    reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    disconnect_notify: Option<mpsc::Sender<InterfaceId>>,
}

impl LocalClientTask {
    /// Serve `stream`, then reconnect for as long as anyone is listening on
    /// the incoming channel. Never returns while the driver is alive and
    /// the daemon is merely absent.
    async fn run(
        self,
        stream: LocalStream,
        incoming_tx: mpsc::Sender<IncomingPacket>,
        outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    ) {
        let mut outgoing_rx = outgoing_rx;
        let mut stream = stream;
        // Carried across connections so a daemon that accepts and drops
        // cannot reset the backoff by accepting (see
        // [`LOCAL_STABLE_CONNECTION`]).
        let mut carried_attempts = 0u64;
        loop {
            let connected_at = Instant::now();
            outgoing_rx = local_interface_task(
                self.name.clone(),
                stream,
                incoming_tx.clone(),
                outgoing_rx,
                Arc::clone(&self.counters),
            )
            .await;

            // Whose end went away decides what this is. Either channel
            // losing its far half means the driver dropped this interface —
            // the node is shutting down or detaching it — and there is
            // nobody left to reconnect for. Checked BEFORE the warning, so
            // an orderly shutdown does not report a daemon outage that never
            // happened.
            if incoming_tx.is_closed() || outgoing_rx.is_closed() {
                tracing::debug!("{}: event loop shut down, not reconnecting", self.name);
                return;
            }

            // The carrier is down from here until a connect succeeds; say so
            // where `rnstatus` reads it, and say it in the journal, because
            // the whole point of this is that a cut-off client must not look
            // like a healthy one.
            self.counters.set_online(false);
            tracing::warn!(
                "{}: lost the shared instance on \"{}\", reconnecting",
                self.name,
                self.abstract_name
            );
            if let Some(ref notify) = self.disconnect_notify {
                let _ = notify.try_send(self.id);
            }

            // A connection that never got going must not reset the backoff,
            // or a daemon in a crash loop is dialled at the base interval
            // forever.
            let mut attempt = if connected_at.elapsed() >= LOCAL_STABLE_CONNECTION {
                0
            } else {
                carried_attempts
            };
            let outage_start = Instant::now();
            loop {
                attempt += 1;
                let delay = local_backoff_delay(attempt, self.base, self.max);
                tokio::time::sleep(delay).await;
                if incoming_tx.is_closed() {
                    tracing::debug!("{}: event loop shut down, not reconnecting", self.name);
                    return;
                }
                match connect_local_stream(&self.abstract_name) {
                    Ok(s) => {
                        self.counters.set_online(true);
                        tracing::info!(
                            "{}: reconnected to the shared instance on \"{}\" after {} attempt(s), {:.1?} offline",
                            self.name,
                            self.abstract_name,
                            attempt,
                            outage_start.elapsed()
                        );
                        if let Some(ref notify) = self.reconnect_notify {
                            let _ = notify.try_send(self.id);
                        }
                        stream = s;
                        carried_attempts = attempt;
                        break;
                    }
                    Err(e) => {
                        // Every attempt is logged; the level widens the
                        // journal spacing for a daemon that stays away.
                        if local_failure_is_loud(attempt) {
                            tracing::warn!(
                                "{}: reconnect attempt {} failed: {} ({:.1?} offline)",
                                self.name,
                                attempt,
                                e,
                                outage_start.elapsed()
                            );
                        } else {
                            tracing::debug!(
                                "{}: reconnect attempt {} failed: {} ({:.1?} offline)",
                                self.name,
                                attempt,
                                e,
                                outage_start.elapsed()
                            );
                        }
                    }
                }
            }
        }
    }
}

/// I/O task owning the IPC stream.
///
/// Handles bidirectional I/O using HDLC framing, identical to the TCP
/// interface task. Uses poll_read_ready + try_read for edge-triggered reads.
async fn local_interface_task(
    name: String,
    stream: LocalStream,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
) -> mpsc::Receiver<OutgoingPacket> {
    let (reader, mut writer) = stream.into_split();

    let mut deframer = Deframer::with_max_frame(LOCAL_HW_MTU as usize);
    let mut read_buf = vec![0u8; MTU * READ_BUFFER_MULTIPLIER];
    let mut frame_buf = Vec::with_capacity(MTU * FRAME_BUFFER_MULTIPLIER);

    // Labelled so every exit leaves through one place: the caller (the
    // reconnect loop) takes the outgoing receiver back and hands it to the
    // next connection, so packets queued during an outage survive it.
    'io: loop {
        tokio::select! {
            // Read path: wait for socket readability, then try_read + deframe
            result = reader.readable() => {
                match result {
                    Ok(()) => {
                        loop {
                            match reader.try_read(&mut read_buf) {
                                Ok(0) => {
                                    tracing::debug!("Local interface {} disconnected (EOF)", name);
                                    break 'io;
                                }
                                Ok(n) => {
                                    counters.rx_bytes.fetch_add(n as u64, Ordering::Relaxed);
                                    let results = deframer.process(&read_buf[..n]);
                                    for r in results {
                                        // HW_MTU enforcement lives in the deframer now.
                                        if matches!(r, DeframeResult::Oversized) {
                                            tracing::trace!(
                                                "Local {}: frame exceeds HW_MTU, discarded", name);
                                            continue;
                                        }
                                        if let DeframeResult::Frame(data) = r {
                                            if incoming_tx.send(IncomingPacket { data }).await.is_err() {
                                                break 'io;
                                            }
                                        }
                                    }
                                }
                                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                                    break; // no more data, back to select!
                                }
                                Err(e) => {
                                    tracing::debug!("Local interface {} read error: {}", name, e);
                                    break 'io;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Local interface {} readability error: {}", name, e);
                        break 'io;
                    }
                }
            }

            // Write path: receive outgoing packets and write HDLC-framed to stream
            msg = outgoing_rx.recv() => {
                match msg {
                    Some(pkt) => {
                        frame(&pkt.data, &mut frame_buf);
                        // Counted before the write: the moment the peer can
                        // observe any byte of this frame, the counter must
                        // already cover it (Codeberg #389, same ordering as
                        // TCP). On a write error the dying stream charges one
                        // frame whose tail never left — bounded by that frame.
                        counters.tx_bytes.fetch_add(frame_buf.len() as u64, Ordering::Relaxed);
                        if let Err(e) = writer.write_all(&frame_buf).await {
                            tracing::debug!("Local interface {} write error: {}", name, e);
                            break 'io;
                        }
                    }
                    None => {
                        tracing::debug!("Local interface {} outgoing channel closed", name);
                        break 'io;
                    }
                }
            }
        }
    }

    outgoing_rx
}

#[cfg(all(test, unix))]
mod tests {

    /// `spawn_local_server` with a private reporting inventory and client
    /// counter, for tests that only exercise the socket itself.
    fn spawn_test_local_server(
        instance_name: &str,
        next_id: Arc<AtomicUsize>,
        tx: mpsc::Sender<InterfaceHandle>,
        buffer_size: usize,
    ) -> Result<(), io::Error> {
        spawn_local_server(
            instance_name,
            next_id,
            tx,
            buffer_size,
            0,
            crate::interfaces::inventory::InterfaceInventory::shared(),
            Arc::new(AtomicUsize::new(0)),
        )
    }
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Connect to a local server in tests, using the platform-appropriate socket.
    fn test_connect(instance_name: &str) -> std::os::unix::net::UnixStream {
        let abstract_name = format!("rns/{}", instance_name);
        connect_local(&abstract_name).unwrap()
    }

    /// The bare `Connection refused` a failed connect produces says nothing
    /// about what was being connected to, which turns a missing or
    /// misnamed daemon into a mystery for every client of the shared
    /// instance. The message has to name the socket and the fix.
    #[tokio::test]
    async fn absent_daemon_error_names_the_socket_and_the_daemons() {
        let instance_name = format!("no_such_instance_{}", std::process::id());
        // InterfaceHandle is not Debug, so unwrap the Result by hand.
        let Err(err) =
            spawn_local_client(LocalClientConfig::new(InterfaceId(1), &instance_name, 16))
        else {
            panic!("no daemon listens on this name, connecting must fail");
        };

        let message = err.to_string();
        assert!(
            message.contains(&format!("rns/{instance_name}")),
            "must name the socket, so a wrong instance_name is visible: {message}"
        );
        assert!(
            message.contains("lnsd") && message.contains("rnsd"),
            "must say which daemon to start: {message}"
        );
        // The underlying connect error differs by platform — Linux's abstract
        // namespace refuses an unbound name (ConnectionRefused), while the
        // filesystem-socket fallback on other Unixes fails to find the path
        // (NotFound). The invariant is that whichever kind the platform
        // produced survives the rewrap, not that every platform is Linux.
        #[cfg(target_os = "linux")]
        let expected_kind = io::ErrorKind::ConnectionRefused;
        #[cfg(not(target_os = "linux"))]
        let expected_kind = io::ErrorKind::NotFound;
        assert_eq!(
            err.kind(),
            expected_kind,
            "the error kind must survive the rewrap"
        );
    }

    #[tokio::test]
    async fn test_local_server_accepts_connection() {
        let next_id = Arc::new(AtomicUsize::new(100));
        let (tx, mut rx) = mpsc::channel::<InterfaceHandle>(4);

        // Use a unique instance name to avoid conflicts
        let instance_name = format!("test_{}", std::process::id());
        spawn_test_local_server(&instance_name, next_id.clone(), tx, 16).unwrap();

        // Connect as a local client
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
        spawn_test_local_server(&instance_name, next_id.clone(), tx, 16).unwrap();

        // Connect
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
        leviculum_core::framing::hdlc::frame(payload, &mut frame_buf);
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
                peer: None,
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
        spawn_test_local_server(&instance_name, next_id.clone(), tx, 16).unwrap();

        // Connect and immediately drop
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
        spawn_test_local_server(&instance_name, next_id.clone(), tx, 16).unwrap();

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
        spawn_test_local_server(&instance_name, next_id.clone(), tx, 16).unwrap();

        // Give server time to bind
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect via spawn_local_client
        let id = InterfaceId(42);
        let mut client_handle = spawn_local_client(LocalClientConfig::new(id, &instance_name, 16))
            .expect("client connect failed");

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
                peer: None,
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
                peer: None,
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

    #[test]
    fn test_resolve_loopback_port_defaults_and_overrides() {
        // Codeberg #112: the AF_INET bind port resolution. Unset (0) keeps the
        // Python defaults; a configured port wins for the matching channel.
        assert_eq!(resolve_loopback_port("rns/default", 0, 0), 37428);
        assert_eq!(resolve_loopback_port("rns/default/rpc", 0, 0), 37429);

        // Configured data port applies to the data channel, control to /rpc.
        assert_eq!(resolve_loopback_port("rns/default", 37500, 37501), 37500);
        assert_eq!(
            resolve_loopback_port("rns/default/rpc", 37500, 37501),
            37501
        );

        // The data override does not leak onto the RPC channel and vice versa.
        assert_eq!(resolve_loopback_port("rns/default/rpc", 37500, 0), 37429);
        assert_eq!(resolve_loopback_port("rns/default", 0, 37501), 37428);

        // A non-default instance name without an override derives a stable port.
        let a = resolve_loopback_port("rns/alpha", 0, 0);
        let b = resolve_loopback_port("rns/beta", 0, 0);
        assert_ne!(a, b);
        assert_eq!(a, resolve_loopback_port("rns/alpha", 0, 0), "stable");
        assert!((37430..=65534).contains(&a));
    }

    #[tokio::test]
    async fn test_two_instances_different_names_no_collision() {
        // Codeberg #112, functional: two shared-instance servers on one host
        // start without an AddrInUse collision when they use different instance
        // names. On Linux the shared instance is an abstract AF_UNIX socket
        // keyed by `instance_name` (`\0rns/{instance_name}`), so the instance
        // name -- not `shared_instance_port` -- is what separates two daemons,
        // matching Python's AF_UNIX behaviour (the port is only bound on the
        // AF_INET / `shared_instance_type = tcp` path).
        let next_id = Arc::new(AtomicUsize::new(600));
        let (tx1, _rx1) = mpsc::channel::<InterfaceHandle>(4);
        let (tx2, _rx2) = mpsc::channel::<InterfaceHandle>(4);

        let base = std::process::id();
        let name_a = format!("test_collide_a_{base}");
        let name_b = format!("test_collide_b_{base}");

        spawn_test_local_server(&name_a, next_id.clone(), tx1, 16).expect("first instance binds");
        // A second instance under a different name must not hit AddrInUse.
        spawn_test_local_server(&name_b, next_id.clone(), tx2, 16)
            .expect("second instance under a different name must bind");

        // Sanity: reusing the first name does collide (proves the bind is real).
        //
        // This holds where the shared-instance socket has kernel-enforced name
        // uniqueness: Linux's abstract AF_UNIX namespace (`\0rns/{name}`) and
        // the Windows TCP-loopback fallback both reject a duplicate bind. macOS
        // has no abstract namespace and falls back to a *filesystem* AF_UNIX
        // path, where a stale socket file is unlinked and re-bound instead of
        // colliding — so two same-named daemons can both bind. That is a real
        // shared-instance robustness gap on macOS (tracked for upstream), not a
        // property this test can assert there.
        #[cfg(not(target_os = "macos"))]
        {
            let (tx3, _rx3) = mpsc::channel::<InterfaceHandle>(4);
            let dup = spawn_test_local_server(&name_a, next_id, tx3, 16);
            assert!(
                dup.is_err(),
                "re-binding the same instance name must fail with AddrInUse"
            );
        }
        #[cfg(target_os = "macos")]
        let _ = next_id; // silence unused on the macOS path
    }

    #[tokio::test]
    async fn test_local_client_connect_failure() {
        let result = spawn_local_client(LocalClientConfig::new(
            InterfaceId(99),
            "nonexistent_instance_that_does_not_exist",
            16,
        ));
        assert!(
            result.is_err(),
            "connecting to nonexistent socket should fail"
        );
    }

    /// Capture tracing output for the duration of the returned guard.
    ///
    /// Thread-local default subscriber on a current-thread runtime, so the
    /// tasks spawned by the test log into this buffer and no other test's.
    /// Same pattern as the serial interface's arming-line test.
    #[derive(Clone)]
    struct LogSink(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock_recover().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogSink {
        type Writer = LogSink;
        fn make_writer(&'a self) -> LogSink {
            self.clone()
        }
    }

    fn capture_logs() -> (
        Arc<std::sync::Mutex<Vec<u8>>>,
        tracing::subscriber::DefaultGuard,
    ) {
        let buf = Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(LogSink(Arc::clone(&buf)))
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        (buf, guard)
    }

    fn captured(buf: &Arc<std::sync::Mutex<Vec<u8>>>) -> String {
        String::from_utf8_lossy(&buf.lock_recover()).into_owned()
    }

    /// Bring a shared-instance server up on `instance_name`, retrying the
    /// bind until the previous one's socket is released (dropping a listener
    /// is asynchronous — the accept task has to be polled before the kernel
    /// name is free).
    async fn bind_server_when_free(
        instance_name: &str,
        next_id: Arc<AtomicUsize>,
        tx: mpsc::Sender<InterfaceHandle>,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match spawn_test_local_server(instance_name, Arc::clone(&next_id), tx.clone(), 16) {
                Ok(()) => return,
                Err(e) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let _ = e;
                }
                Err(e) => panic!("server never bound: {e}"),
            }
        }
    }

    /// The backoff schedule with the constants production actually runs:
    /// never below the base, never above the ceiling, never decreasing, and
    /// pinned at the ceiling long before an attempt count could overflow.
    /// A retry loop with these properties cannot spin, whatever the daemon
    /// does.
    #[test]
    fn reconnect_backoff_is_bounded_and_never_decreases() {
        let base = LOCAL_RECONNECT_BASE;
        let max = LOCAL_RECONNECT_MAX;

        // The first three attempts are the base interval, so a daemon
        // restart is healed in well under a second.
        for attempt in 1..=3 {
            assert_eq!(local_backoff_delay(attempt, base, max), base);
        }

        let mut previous = Duration::ZERO;
        for attempt in 1..=10_000u64 {
            let delay = local_backoff_delay(attempt, base, max);
            assert!(delay >= previous, "attempt {attempt} went backwards");
            assert!(delay <= max, "attempt {attempt} exceeded the ceiling");
            assert!(delay >= base.min(max), "attempt {attempt} below the base");
            previous = delay;
        }
        assert_eq!(
            local_backoff_delay(u64::MAX, base, max),
            max,
            "an unbounded attempt count must pin at the ceiling, not overflow"
        );

        // Worst case over a full day of a daemon that never returns: the
        // ceiling is what bounds the work, and it is Python's own cadence.
        assert_eq!(max, Duration::from_secs(8));
    }

    /// The finding in one test: the daemon goes away under a connected
    /// client, comes back, and the client is carrying traffic again —
    /// having said each step out loud, because a cut-off client that looks
    /// healthy is the actual complaint.
    #[tokio::test]
    async fn a_client_reattaches_to_a_restarted_daemon_and_says_so() {
        let (logs, _log_guard) = capture_logs();

        let next_id = Arc::new(AtomicUsize::new(700));
        let (tx, mut rx) = mpsc::channel::<InterfaceHandle>(4);
        let instance_name = format!("test_reconn_{}", std::process::id());
        bind_server_when_free(&instance_name, Arc::clone(&next_id), tx).await;

        let (reconnect_tx, mut reconnect_rx) = mpsc::channel::<InterfaceId>(4);
        let (down_tx, mut down_rx) = mpsc::channel::<InterfaceId>(4);
        let id = InterfaceId(77);
        let mut client = spawn_local_client(LocalClientConfig {
            reconnect_notify: Some(reconnect_tx),
            disconnect_notify: Some(down_tx),
            // Milliseconds rather than the production quarter-second, so the
            // test observes the schedule instead of waiting for it.
            backoff: Some((Duration::from_millis(20), Duration::from_millis(100))),
            ..LocalClientConfig::new(id, &instance_name, 16)
        })
        .expect("first connect must succeed against a live daemon");
        assert!(client.counters.is_online());

        let server_handle = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("closed");
        assert!(server_handle.info.name.contains("Local["));

        // The daemon exits: dropping the receiver drops both the accepted
        // connection's handle and the accept loop, exactly as a daemon
        // shutdown does.
        drop(server_handle);
        drop(rx);

        let dropped = tokio::time::timeout(Duration::from_secs(2), down_rx.recv())
            .await
            .expect("the client must report the loss")
            .expect("channel closed");
        assert_eq!(dropped, id, "the loss is reported for this interface");

        // The daemon stays away for several retry intervals — a package
        // upgrade, not an instant restart — so the retry lines this test
        // asserts on are produced, and so the client is shown to keep trying
        // rather than to have caught the socket on its first dial.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !client.counters.is_online(),
            "a client whose daemon is gone must report itself offline"
        );

        // The daemon comes back.
        let (tx2, mut rx2) = mpsc::channel::<InterfaceHandle>(4);
        bind_server_when_free(&instance_name, next_id, tx2).await;

        let back = tokio::time::timeout(Duration::from_secs(5), reconnect_rx.recv())
            .await
            .expect("the client must reconnect on its own")
            .expect("channel closed");
        assert_eq!(
            back, id,
            "the same interface index comes back, not a new one"
        );
        assert!(
            client.counters.is_online(),
            "a reconnected client must report itself online again"
        );

        // Traffic flows on the recovered connection, in both directions.
        let mut server_handle = tokio::time::timeout(Duration::from_secs(2), rx2.recv())
            .await
            .expect("timeout")
            .expect("closed");
        client
            .outgoing
            .send(OutgoingPacket {
                peer: None,
                data: b"after-the-restart".to_vec(),
                high_priority: false,
            })
            .await
            .expect("send on the recovered interface");
        let pkt = tokio::time::timeout(Duration::from_secs(2), server_handle.incoming.recv())
            .await
            .expect("timeout waiting for the packet")
            .expect("channel closed");
        assert_eq!(pkt.data, b"after-the-restart");

        server_handle
            .outgoing
            .send(OutgoingPacket {
                peer: None,
                data: b"and-back".to_vec(),
                high_priority: false,
            })
            .await
            .expect("daemon send");
        let pkt = tokio::time::timeout(Duration::from_secs(2), client.incoming.recv())
            .await
            .expect("timeout waiting for the daemon's packet")
            .expect("channel closed");
        assert_eq!(pkt.data, b"and-back");

        // The three lines an operator needs: the loss, the retrying, and the
        // recovery. Asserted, because a silent recovery would be half a fix.
        let text = captured(&logs);
        assert!(
            text.contains(&format!(
                "LocalClient[{instance_name}]: lost the shared instance"
            )),
            "the disconnect must be logged; logs:\n{text}"
        );
        assert!(
            text.contains(&format!(
                "LocalClient[{instance_name}]: reconnect attempt 1 failed"
            )),
            "each retry must be logged; logs:\n{text}"
        );
        assert!(
            text.contains(&format!(
                "LocalClient[{instance_name}]: reconnected to the shared instance"
            )),
            "the recovery must be logged; logs:\n{text}"
        );
    }

    /// A node shutting down is not a daemon outage. When the driver drops
    /// the interface, both channels lose their far half; the client stops
    /// without reporting a loss that never happened and without leaving a
    /// task dialling a socket nobody is listening for.
    #[tokio::test]
    async fn an_orderly_shutdown_is_not_reported_as_an_outage() {
        let (logs, _log_guard) = capture_logs();

        let next_id = Arc::new(AtomicUsize::new(900));
        let (tx, mut rx) = mpsc::channel::<InterfaceHandle>(4);
        let instance_name = format!("test_shutdown_{}", std::process::id());
        bind_server_when_free(&instance_name, next_id, tx).await;

        let (down_tx, mut down_rx) = mpsc::channel::<InterfaceId>(4);
        let client = spawn_local_client(LocalClientConfig {
            disconnect_notify: Some(down_tx),
            backoff: Some((Duration::from_millis(10), Duration::from_millis(20))),
            ..LocalClientConfig::new(InterfaceId(99), &instance_name, 16)
        })
        .expect("first connect");
        let _server_handle = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("closed");

        // The driver drops the interface: the handle, and with it both
        // channel ends it owned, go away while the daemon is still there.
        drop(client);
        tokio::time::sleep(Duration::from_millis(200)).await;

        let text = captured(&logs);
        assert!(
            !text.contains("lost the shared instance"),
            "a dropped interface is not a daemon outage; logs:\n{text}"
        );
        assert!(
            !text.contains("reconnect attempt"),
            "a dropped interface must not go on dialling; logs:\n{text}"
        );
        assert!(
            down_rx.try_recv().is_err(),
            "no disconnect is reported for an interface the driver itself dropped"
        );
    }

    /// A daemon that never comes back is retried forever, and that has to
    /// cost nearly nothing. The observable is the per-attempt log line the
    /// test above requires: over a window, a backed-off client emits a
    /// handful, a spinning one would emit thousands.
    #[tokio::test]
    async fn a_daemon_that_stays_away_is_retried_without_spinning() {
        let (logs, _log_guard) = capture_logs();

        let next_id = Arc::new(AtomicUsize::new(800));
        let (tx, mut rx) = mpsc::channel::<InterfaceHandle>(4);
        let instance_name = format!("test_noreturn_{}", std::process::id());
        bind_server_when_free(&instance_name, next_id, tx).await;

        let base = Duration::from_millis(20);
        let max = Duration::from_millis(60);
        let client = spawn_local_client(LocalClientConfig {
            backoff: Some((base, max)),
            ..LocalClientConfig::new(InterfaceId(88), &instance_name, 16)
        })
        .expect("first connect");
        let server_handle = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("closed");

        // The daemon goes away for good.
        drop(server_handle);
        drop(rx);

        let window = Duration::from_secs(1);
        tokio::time::sleep(window).await;

        let text = captured(&logs);
        let attempts = text.matches("reconnect attempt").count();
        // The schedule over this window is 20, 20, 20, 40, then 60 forever:
        // about 19 attempts. The bound is generous because the point is the
        // order of magnitude — a tight loop would be five figures.
        let ceiling = (window.as_millis() / base.as_millis()) as usize;
        assert!(
            attempts >= 3 && attempts <= ceiling,
            "expected a backed-off handful of attempts in {window:?}, got {attempts} \
             (ceiling {ceiling}); logs:\n{text}"
        );
        assert!(
            !client.counters.is_online(),
            "a client with no daemon must not report itself online"
        );
        assert!(
            text.contains("reconnect attempt 1 failed")
                && text.contains("reconnect attempt 2 failed"),
            "every attempt is logged, not just the first; logs:\n{text}"
        );
    }

    /// Codeberg #389 mvr (tx counter siblings): once the peer can observe
    /// bytes of a frame, the interface's tx counter already covers that
    /// frame — same ordering the TCP interface pins. A minimal send buffer
    /// parks `write_all` mid-frame, the peer reads the frame's head, and
    /// the counter is inspected inside that window.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tx_counter_covers_bytes_the_peer_can_already_observe() {
        use std::io::Read as _;

        let (iface_side, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        // Minimal send buffer (the kernel clamps to its floor, still far
        // below the frame) so the write parks mid-frame.
        socket2::SockRef::from(&iface_side)
            .set_send_buffer_size(4096)
            .unwrap();
        iface_side.set_nonblocking(true).unwrap();
        let stream = tokio::net::UnixStream::from_std(iface_side).unwrap();

        let (incoming_tx, _incoming_rx) = mpsc::channel(4);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(4);
        let counters = Arc::new(InterfaceCounters::new());
        let task_counters = Arc::clone(&counters);
        tokio::spawn(local_interface_task(
            "mvr_389".to_string(),
            stream,
            incoming_tx,
            outgoing_rx,
            task_counters,
        ));

        // One frame far larger than the send buffer: the write cannot
        // complete until the peer drains, so the task stays parked in
        // write_all while the frame's head is already at the peer.
        outgoing_tx
            .send(OutgoingPacket {
                data: vec![0x42u8; 1 << 20],
                high_priority: false,
                peer: None,
            })
            .await
            .unwrap();

        // Blocking read on a dedicated worker thread: returns as soon as
        // the peer has observable bytes of the frame.
        let mut head = [0u8; 1024];
        let n = peer.read(&mut head).unwrap();
        assert!(n > 0, "peer must observe bytes of the frame");

        let tx = counters.tx_bytes.load(Ordering::Relaxed);
        assert!(
            tx > 0,
            "peer observed {n} bytes but the tx counter reads {tx}"
        );
    }
}
