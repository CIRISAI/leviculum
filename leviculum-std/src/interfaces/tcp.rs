//! TCP interfaces (client and server)
//!
//! Client: connects to a Reticulum TCP server (e.g. rnsd TCPServerInterface).
//! Server: listens for incoming connections and spawns an interface per client.
//!
//! Both use HDLC framing to delimit packets on the TCP stream,
//! matching Python Reticulum's `TCPClientInterface` / `TCPServerInterface`.

use std::io;
use std::net::SocketAddr;
#[cfg(test)]
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{IncomingPacket, InterfaceCounters, InterfaceInfo, OutgoingPacket, ReadySignal};
use leviculum_core::constants::MTU;
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::transport::InterfaceId;
use rand_core::RngCore;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use super::InterfaceHandle;

/// Default channel buffer size for TCP interfaces.
/// Used for both incoming and outgoing channels.
/// Must be large enough to absorb short bursts during reconnection.
pub(crate) const TCP_DEFAULT_BUFFER_SIZE: usize = 256;

/// TCP liveness parity with Python Reticulum (Codeberg #63).
///
/// Values mirror TCPInterface.py:84-87 / set_timeouts_linux():
/// TCP_USER_TIMEOUT 24 s, SO_KEEPALIVE with idle 5 s / interval 2 s /
/// 12 probes. A silently-dead link (e.g. an iptables-dropped path with
/// no FIN/RST) then surfaces as a read/write error within ~24 s, the
/// driver marks the interface offline, `handle_interface_down` culls
/// its path entries, and the reconnect loop takes over — without these
/// options the kernel defaults let such a connection linger for many
/// minutes. No config surface yet, by design (reference parity).
#[cfg(any(target_os = "linux", target_os = "android"))]
const TCP_USER_TIMEOUT: Duration = Duration::from_secs(24);
const TCP_PROBE_AFTER: Duration = Duration::from_secs(5);
#[cfg(any(target_os = "linux", target_os = "android"))]
const TCP_PROBE_INTERVAL: Duration = Duration::from_secs(2);
#[cfg(any(target_os = "linux", target_os = "android"))]
const TCP_PROBES: u32 = 12;

/// Apply the liveness options above to a TCP socket (std or tokio).
/// Best-effort by contract at the call sites: a socket that cannot take
/// the options still works, it just falls back to kernel default
/// dead-peer detection.
#[cfg(unix)]
fn apply_liveness_options<S: std::os::fd::AsFd>(stream: &S) -> io::Result<()> {
    apply_liveness_sockref(socket2::SockRef::from(stream))
}

#[cfg(windows)]
fn apply_liveness_options<S: std::os::windows::io::AsSocket>(stream: &S) -> io::Result<()> {
    apply_liveness_sockref(socket2::SockRef::from(stream))
}

/// Shared body. The keepalive interval/retry tuning and `TCP_USER_TIMEOUT` are
/// Linux/Android-only (socket2 gates the setters; `TCP_USER_TIMEOUT` is a Linux
/// socket option). Other platforms get the universal idle-keepalive and fall
/// back to kernel-default dead-peer detection (best-effort by contract).
fn apply_liveness_sockref(sock: socket2::SockRef<'_>) -> io::Result<()> {
    use socket2::TcpKeepalive;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let keepalive = TcpKeepalive::new()
            .with_time(TCP_PROBE_AFTER)
            .with_interval(TCP_PROBE_INTERVAL)
            .with_retries(TCP_PROBES);
        sock.set_tcp_keepalive(&keepalive)?;
        sock.set_tcp_user_timeout(Some(TCP_USER_TIMEOUT))?;
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        sock.set_tcp_keepalive(&TcpKeepalive::new().with_time(TCP_PROBE_AFTER))?;
    }
    Ok(())
}

/// Process-global gate for fault injection (`--corrupt-every`).
/// Default `true` so existing invocations are unaffected.
static CORRUPT_ACTIVE: AtomicBool = AtomicBool::new(true);

/// Re-enable byte-flip fault injection on TCP writes.
/// Counterpart of [`disable_fault_injection`].
pub fn enable_fault_injection() {
    CORRUPT_ACTIVE.store(true, Ordering::Relaxed);
}

/// Disable byte-flip fault injection on TCP writes without
/// changing per-iface `corrupt_every` configuration. Used by
/// `lnstest selftest` during Phase-2 mutual discovery so announces
/// cross a clean stream (otherwise a corrupted announce yields
/// a deterministic 60 s Phase-2 timeout — Bug #31).
pub fn disable_fault_injection() {
    CORRUPT_ACTIVE.store(false, Ordering::Relaxed);
}

/// Configuration for a reconnecting TCP client interface.
pub(crate) struct TcpClientConfig {
    pub id: InterfaceId,
    pub name: String,
    pub addr: SocketAddr,
    pub buffer_size: usize,
    pub corrupt_every: Option<u64>,
    pub reconnect_interval: Duration,
    pub max_reconnect_tries: Option<u64>,
    /// Upper bound on the backoff delay between reconnect attempts. The base
    /// `reconnect_interval` is doubled on each attempt past the third and
    /// clamped here, so a permanently-dead peer is retried at most once per
    /// this interval instead of every `reconnect_interval`. Default 60 s.
    pub reconnect_max_interval: Duration,
    /// Upper bound on a single connect attempt. A connect that does not
    /// complete within this window is abandoned and counted as a failed
    /// attempt, so reconnect accounting (and give-up) stays responsive even
    /// when the OS does not refuse promptly. Platforms differ here: a refused
    /// loopback connect returns instantly on Linux but stalls on SYN-retransmit
    /// for ~1s+ on Windows, and a black-holed peer never refuses at all. The
    /// interface owns this carrier-medium quirk so the driver need not.
    pub connect_timeout: Duration,
    pub reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    /// Tunnel-synthesize signal (Codeberg #64 initiator side). When present, the
    /// interface fires its `InterfaceId` here on every successful connect (the
    /// initial one AND every reconnect), so the driver initiates the tunnel
    /// synthesize handshake. `None` for KISS-framed or non-tunnel clients, which
    /// mirrors Python's `if not self.kiss_framing: wants_tunnel = True`. The
    /// presence of the channel is the `wants_tunnel` flag; interface isolation
    /// keeps the medium-specific "when to want a tunnel" decision here.
    pub tunnel_notify: Option<mpsc::Sender<InterfaceId>>,
}

/// Default per-attempt connect timeout for reconnecting TCP clients.
pub(crate) const DEFAULT_TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default cap on the exponential reconnect backoff. See [`backoff_delay`].
pub(crate) const DEFAULT_RECONNECT_MAX_INTERVAL: Duration = Duration::from_secs(60);

/// Fast non-cryptographic PRNG (xorshift64). Seeded from OsRng once per task.
struct Xorshift64(u64);

impl Xorshift64 {
    fn from_entropy() -> Self {
        Self(rand_core::OsRng.next_u64() | 1) // ensure non-zero seed
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Corrupt bytes in `buf` with probability 1/N per byte.
///
/// Uses a fast inline PRNG (not OsRng) to avoid syscalls per byte.
/// XORs with a random non-zero value to guarantee the byte actually changes.
/// Returns the number of bytes corrupted.
fn maybe_corrupt(buf: &mut [u8], every_n: u64, rng: &mut Xorshift64) -> usize {
    if every_n == 0 {
        return 0;
    }
    let mut corrupted = 0;
    for byte in buf.iter_mut() {
        if rng.next().is_multiple_of(every_n) {
            let flip = loop {
                let v = (rng.next() & 0xFF) as u8;
                if v != 0 {
                    break v;
                }
            };
            *byte ^= flip;
            corrupted += 1;
        }
    }
    corrupted
}

/// Frame buffer multiplier (accounts for HDLC escaping overhead)
const FRAME_BUFFER_MULTIPLIER: usize = 2;

/// Read buffer multiplier (handles multiple packets per read)
const READ_BUFFER_MULTIPLIER: usize = 4;

/// Create channels, spawn the I/O task for an already-connected TCP stream,
/// and return the resulting `InterfaceHandle`.
///
/// Used by the TCP server accept loop for each incoming connection.
pub(crate) fn spawn_tcp_interface_from_stream(
    id: InterfaceId,
    name: String,
    stream: tokio::net::TcpStream,
    buffer_size: usize,
    corrupt_every: Option<u64>,
) -> InterfaceHandle {
    let (incoming_tx, incoming_rx) = mpsc::channel(buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(buffer_size);
    let counters = Arc::new(InterfaceCounters::new());

    let task_name = name.clone();
    let task_counters = Arc::clone(&counters);

    tokio::spawn(async move {
        let _rx = tcp_interface_task(
            task_name,
            stream,
            incoming_tx,
            outgoing_rx,
            corrupt_every,
            task_counters,
        )
        .await;
    });

    InterfaceHandle {
        info: InterfaceInfo {
            id,
            name,
            hw_mtu: Some(262_144),
            is_local_client: false,
            bitrate: None,
            ifac: None,
            mode: leviculum_core::traits::InterfaceMode::default(),
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
        // Server-spawned children are pre-signaled — by the time we
        // hand the handle to the registry, the underlying TCP stream
        // already exists and is bidirectionally usable.
        ready: ReadySignal::ready_immediate(),
    }
}

/// Spawn a TCP client interface task (synchronous connect, no reconnect).
///
/// Connects to the given address synchronously (with timeout), then spawns
/// a tokio task that handles all I/O through channels. Returns an
/// `InterfaceHandle` for the event loop to use.
///
/// Production code uses `spawn_tcp_client_with_reconnect` instead. This
/// function is retained for tests that need a one-shot, synchronous connect.
#[cfg(test)]
pub(crate) fn spawn_tcp_interface<A: ToSocketAddrs>(
    id: InterfaceId,
    name: String,
    addr: A,
    connect_timeout: Duration,
    buffer_size: usize,
    corrupt_every: Option<u64>,
) -> Result<InterfaceHandle, io::Error> {
    let addr = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no addresses found"))?;

    let std_stream = std::net::TcpStream::connect_timeout(&addr, connect_timeout)?;
    std_stream.set_nonblocking(true)?;
    std_stream.set_nodelay(true)?;
    apply_liveness_options(&std_stream).ok();
    let stream = tokio::net::TcpStream::from_std(std_stream)?;

    Ok(spawn_tcp_interface_from_stream(
        id,
        name,
        stream,
        buffer_size,
        corrupt_every,
    ))
}

/// Start a TCP server that listens for incoming connections.
///
/// Binds synchronously (so errors propagate to the caller), then spawns
/// an async accept loop. Each accepted connection becomes an
/// `InterfaceHandle` sent to the event loop via `new_interface_tx`.
///
/// The accept loop exits when the event loop drops `new_interface_rx`
/// (detected via `Sender::closed()`).
pub(crate) fn spawn_tcp_server(
    bind_addr: SocketAddr,
    next_id: Arc<AtomicUsize>,
    new_interface_tx: mpsc::Sender<InterfaceHandle>,
    buffer_size: usize,
    corrupt_every: Option<u64>,
    ifac: Option<leviculum_core::ifac::IfacConfig>,
    mode: leviculum_core::traits::InterfaceMode,
) -> Result<(), io::Error> {
    // Bind synchronously so errors propagate to the caller immediately
    let std_listener = std::net::TcpListener::bind(bind_addr)?;
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(std_listener)?;

    tracing::info!("TCP server listening on {}", bind_addr);

    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, peer_addr)) => {
                            let id = InterfaceId(next_id.fetch_add(1, Ordering::Relaxed));
                            let name = format!("tcp_server/{}", peer_addr);
                            stream.set_nodelay(true).ok();
                            apply_liveness_options(&stream).ok();
                            let mut handle = spawn_tcp_interface_from_stream(
                                id, name.clone(), stream, buffer_size, corrupt_every,
                            );
                            // Inherit IFAC config from parent TCP server listener.
                            handle.info.ifac = ifac.clone();
                            // Codeberg #104: the accepted (spawned) child inherits
                            // the listener's propagation mode so inbound-side mode
                            // rules (AP/roaming/etc.) apply to peers connecting to
                            // this server, mirroring Python
                            // `spawned_interface.mode = self.mode` (TCPInterface.py:625).
                            handle.info.mode = mode;
                            tracing::info!("Accepted connection: {} ({})", name, id);
                            if new_interface_tx.send(handle).await.is_err() {
                                break; // event loop shut down
                            }
                        }
                        Err(e) => {
                            tracing::warn!("TCP accept error: {}", e);
                        }
                    }
                }
                _ = new_interface_tx.closed() => {
                    tracing::debug!("TCP server shutting down (event loop exited)");
                    break;
                }
            }
        }
    });

    Ok(())
}

/// Spawn a TCP client interface with automatic reconnection.
///
/// Creates the channel pair once and spawns a reconnect task that owns them.
/// The `InterfaceHandle` is returned immediately, the initial connect happens
/// asynchronously in the background, so `start()` returns without blocking.
///
/// During disconnect, the `incoming_tx` stays alive so the driver never sees
/// `Disconnected`. Outgoing packets buffer in the channel (up to `buffer_size`);
/// excess packets are dropped with `BufferFull`. On reconnect, buffered packets
/// are sent on the new stream.
pub(crate) fn spawn_tcp_client_with_reconnect(config: TcpClientConfig) -> InterfaceHandle {
    let (incoming_tx, incoming_rx) = mpsc::channel(config.buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(config.buffer_size);
    let counters = Arc::new(InterfaceCounters::new());
    let ready = ReadySignal::new();

    let id = config.id;
    let task_name = config.name.clone();
    let task_counters = Arc::clone(&counters);
    let task_ready = Arc::clone(&ready);

    tokio::spawn(async move {
        tcp_client_reconnect_task(
            id,
            config.addr,
            task_name,
            incoming_tx,
            outgoing_rx,
            config.corrupt_every,
            config.reconnect_interval,
            config.max_reconnect_tries,
            config.reconnect_max_interval,
            config.connect_timeout,
            task_counters,
            config.reconnect_notify,
            config.tunnel_notify,
            task_ready,
        )
        .await;
    });

    InterfaceHandle {
        info: InterfaceInfo {
            id,
            name: config.name,
            hw_mtu: Some(262_144),
            is_local_client: false,
            bitrate: None,
            ifac: None,
            mode: leviculum_core::traits::InterfaceMode::default(),
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
        ready,
    }
}

/// Bounded exponential backoff between reconnect attempts (no jitter).
///
/// Attempts `1..=3` wait exactly `base`, so a transient blip heals as fast as a
/// Python peer would (see the deviation note on [`tcp_client_reconnect_task`]).
/// From attempt 4 on the delay doubles each attempt, clamped at `max`. The
/// result is monotonically non-decreasing in `attempt` and never exceeds `max`.
fn backoff_delay(attempt: u64, base: Duration, max: Duration) -> Duration {
    if attempt <= 3 {
        return base.min(max);
    }
    // 1 doubling at attempt 4, 2 at attempt 5, ... All arithmetic saturates so
    // a large attempt count can never overflow; it simply pins to `max`.
    let doublings = attempt - 3;
    let base_nanos = base.as_nanos();
    let scaled = if doublings >= 128 {
        u128::MAX
    } else {
        base_nanos.saturating_mul(1u128 << doublings)
    };
    let capped = scaled.min(max.as_nanos());
    Duration::from_nanos(capped.min(u64::MAX as u128) as u64)
}

/// Deterministic +/-20 % jitter on a backoff delay, keyed on interface name and
/// attempt. No RNG (so it is unit-testable), and two differently-named
/// interfaces retrying the same dead peer draw different offsets, so a fleet of
/// nodes does not reconnect in lockstep.
fn backoff_jitter(name: &str, attempt: u64, delay: Duration) -> Duration {
    // FNV-1a over the name bytes followed by the attempt number.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    for b in attempt.to_le_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Offset in parts-per-100_000, uniform over [-20_000, +20_000] => +/-20 %.
    let offset = (hash % 40_001) as i64 - 20_000;
    let nanos = delay.as_nanos() as i128;
    let adjusted = nanos + nanos * offset as i128 / 100_000;
    Duration::from_nanos(adjusted.max(0) as u64)
}

/// Whether a reconnect failure at this attempt should emit a `warn!` line.
///
/// Attempts `1..=3` always log (they mirror the base-rate retries). After that,
/// log only on attempt-count doublings (4, 8, 16, ...): each doubling of the
/// backoff delay gets at most one line, and once the delay caps the spacing
/// keeps widening, so a permanently-dead peer logs at most once per capped
/// interval instead of twice every `reconnect_interval`.
fn should_log_failure(attempt: u64) -> bool {
    attempt <= 3 || attempt.is_power_of_two()
}

/// The single info line emitted on a successful connect. A connect that
/// followed at least one failed attempt reports the recovery (attempt count and
/// how long the peer was gone); a clean first connect just states the endpoint.
/// Exactly one line per successful connect, by construction.
fn connect_log_line(
    name: &str,
    addr: SocketAddr,
    failed_attempts: u64,
    outage: Option<Duration>,
) -> String {
    if failed_attempts > 0 {
        let elapsed = outage.unwrap_or_default();
        format!("{name}: reconnected to {addr} after {failed_attempts} attempt(s), {elapsed:.1?}")
    } else {
        format!("{name}: connected to {addr}")
    }
}

/// Reconnect wrapper for TCP client connections.
///
/// Owns the channel endpoints and keeps them alive across reconnection cycles.
/// The driver never sees `RecvEvent::Disconnected`, only a gap in incoming
/// packets during downtime. On reconnection (not the first connect), sends a
/// notification on `reconnect_notify` so the driver can call
/// `handle_interface_up` to re-announce destinations (Block D).
#[allow(clippy::too_many_arguments)]
async fn tcp_client_reconnect_task(
    id: InterfaceId,
    addr: SocketAddr,
    name: String,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    corrupt_every: Option<u64>,
    reconnect_interval: Duration,
    max_reconnect_tries: Option<u64>,
    reconnect_max_interval: Duration,
    connect_timeout: Duration,
    counters: Arc<InterfaceCounters>,
    reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    tunnel_notify: Option<mpsc::Sender<InterfaceId>>,
    ready: Arc<ReadySignal>,
) {
    // Backoff DELIBERATELY DEVIATES from Python `RNS/Interfaces/TCPInterface.py`,
    // which uses `RECONNECT_WAIT = 5` and `RECONNECT_MAX_TRIES = None`: a fixed
    // 5 s retry, forever, logging each attempt. A dead peer there costs ~17,280
    // attempts and ~34,560 journal lines per day. This deviation is permitted by
    // the project deviation rule: a client's reconnect cadence and its local
    // logging are wire-invisible and semantics-invisible (no peer observes them),
    // and the change measurably improves Priority-1 operation on constrained
    // nodes (far less wasted work, a readable journal). The trade is that a
    // returning peer's reconnect latency rises from <=`reconnect_interval` to
    // <=`reconnect_max_interval`. We still NEVER give up by default
    // (`max_reconnect_tries = None`): backoff replaces abandonment, which would
    // cost delivery. Attempts 1..=3 stay at the base interval so a transient
    // blip heals exactly as fast as a Python peer would.
    let mut attempt = 0u64;
    let mut has_connected_before = false;
    // Set on the first failed cycle of an outage, cleared on the next success,
    // so the success line can report how long the peer was gone.
    let mut outage_start: Option<Instant> = None;
    loop {
        // Bound each attempt: a connect that does not resolve within
        // `connect_timeout` (Windows SYN-retransmit to a closed loopback port,
        // a black-holed peer that never sends RST) is abandoned and counted,
        // keeping give-up deterministic across platforms.
        let connect_result =
            match tokio::time::timeout(connect_timeout, tokio::net::TcpStream::connect(addr)).await
            {
                Ok(res) => res,
                Err(_elapsed) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "connect attempt timed out",
                )),
            };
        match connect_result {
            Ok(stream) => {
                stream.set_nodelay(true).ok();
                apply_liveness_options(&stream).ok();
                let is_reconnect = has_connected_before;
                let failed_attempts = attempt;
                let outage = outage_start.take();
                has_connected_before = true;
                // RESET the backoff after any successful connect.
                attempt = 0;
                // Per the wait_for_interface_ready contract (Option α):
                // ready fires when the kernel-level TCP three-way
                // handshake has succeeded.  This is idempotent, so
                // reconnects after a drop are safe — the signal stays
                // ready for the lifetime of the interface.
                ready.signal_ready();

                // Exactly ONE info line per successful connect.
                tracing::info!(
                    "{}",
                    connect_log_line(&name, addr, failed_attempts, outage.map(|t| t.elapsed()))
                );

                // Notify the driver about reconnection so it can re-announce
                // destinations on the recovered interface (Block D).
                if is_reconnect {
                    if let Some(ref notify) = reconnect_notify {
                        let _ = notify.try_send(id);
                    }
                }

                // Initiate the tunnel synthesize handshake on every successful
                // connect (initial AND reconnect), matching Python's
                // synthesize_tunnel on connect (:179) and reconnect (:297-298).
                // Codeberg #64 initiator side.
                if let Some(ref notify) = tunnel_notify {
                    let _ = notify.try_send(id);
                }

                // Packets queued in outgoing_rx during disconnect will be sent on
                // the new stream. If the channel overflowed (capacity limited),
                // excess packets were dropped by the event loop (BufferFull).
                outgoing_rx = tcp_interface_task(
                    name.clone(),
                    stream,
                    incoming_tx.clone(),
                    outgoing_rx,
                    corrupt_every,
                    Arc::clone(&counters),
                )
                .await;
                tracing::warn!("{}: connection lost, will reconnect", name);
            }
            Err(e) => {
                // The failure warn! is throttled: attempts 1,2,3 and each
                // backoff doubling, then at most once per capped interval.
                // `attempt + 1` is the value the tail below increments to.
                if should_log_failure(attempt + 1) {
                    tracing::warn!(
                        "{}: connect to {} failed: {} (attempt {})",
                        name,
                        addr,
                        e,
                        attempt + 1
                    );
                }
            }
        }
        if outage_start.is_none() {
            outage_start = Some(Instant::now());
        }
        attempt += 1;
        if let Some(max) = max_reconnect_tries {
            if attempt >= max {
                tracing::error!("{}: max reconnect attempts ({}) reached", name, max);
                return; // drops incoming_tx → driver sees Disconnected
            }
        }
        // Check if event loop shut down (incoming receiver dropped)
        if incoming_tx.is_closed() {
            tracing::debug!("{}: event loop shut down, stopping reconnect", name);
            return;
        }
        let delay = backoff_jitter(
            &name,
            attempt,
            backoff_delay(attempt, reconnect_interval, reconnect_max_interval),
        );
        tracing::debug!(
            "{}: reconnecting in {:.1?} (attempt {})",
            name,
            delay,
            attempt
        );
        tokio::time::sleep(delay).await;
    }
}

/// Interface task owning the TCP stream
///
/// Handles bidirectional I/O:
/// - Read path: poll_read_ready → try_read → HDLC deframe → incoming_tx.send()
/// - Write path: outgoing_rx.recv() → HDLC frame → stream.write_all()
///
/// Returns the `outgoing_rx` when the connection is lost, enabling the
/// reconnect wrapper to reuse the channel with a new stream. Packets
/// queued during disconnect are sent on the new connection.
async fn tcp_interface_task(
    name: String,
    stream: tokio::net::TcpStream,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    corrupt_every: Option<u64>,
    counters: Arc<InterfaceCounters>,
) -> mpsc::Receiver<OutgoingPacket> {
    let (reader, mut writer) = stream.into_split();

    let mut deframer = Deframer::new();
    let mut read_buf = vec![0u8; MTU * READ_BUFFER_MULTIPLIER];
    let mut frame_buf = Vec::with_capacity(MTU * FRAME_BUFFER_MULTIPLIER);
    let mut corrupt_rng = Xorshift64::from_entropy();

    loop {
        tokio::select! {
            // Read path: wait for socket readability, then try_read + deframe
            result = reader.readable() => {
                match result {
                    Ok(()) => {
                        // Drain all available data from the socket
                        loop {
                            match reader.try_read(&mut read_buf) {
                                Ok(0) => {
                                    tracing::debug!("TCP interface {} disconnected (EOF)", name);
                                    return outgoing_rx;
                                }
                                Ok(n) => {
                                    counters.rx_bytes.fetch_add(n as u64, Ordering::Relaxed);
                                    let results = deframer.process(&read_buf[..n]);
                                    for r in results {
                                        if let DeframeResult::Frame(data) = r {
                                            if incoming_tx.send(IncomingPacket { data }).await.is_err() {
                                                // Event loop dropped its receiver
                                                return outgoing_rx;
                                            }
                                        }
                                    }
                                }
                                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                                    // No more data, go back to select!
                                    break;
                                }
                                Err(e) => {
                                    tracing::debug!("TCP interface {} read error: {}", name, e);
                                    return outgoing_rx;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!("TCP interface {} readability error: {}", name, e);
                        return outgoing_rx;
                    }
                }
            }

            // Write path: receive outgoing packets and write to stream
            msg = outgoing_rx.recv() => {
                match msg {
                    Some(pkt) => {
                        frame(&pkt.data, &mut frame_buf);
                        if let Some(n) = corrupt_every {
                            if CORRUPT_ACTIVE.load(Ordering::Relaxed) {
                                let count = maybe_corrupt(&mut frame_buf, n, &mut corrupt_rng);
                                if count > 0 {
                                    tracing::trace!(
                                        "TCP {} corrupted {} byte(s) in {} byte frame",
                                        name, count, frame_buf.len()
                                    );
                                }
                            }
                        }
                        if let Err(e) = writer.write_all(&frame_buf).await {
                            tracing::debug!("TCP interface {} write error: {}", name, e);
                            return outgoing_rx;
                        }
                        counters.tx_bytes.fetch_add(frame_buf.len() as u64, Ordering::Relaxed);
                    }
                    None => {
                        // Event loop dropped its sender, shut down
                        tracing::debug!("TCP interface {} outgoing channel closed", name);
                        return outgoing_rx;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_delay_schedule() {
        let base = Duration::from_secs(5);
        let max = Duration::from_secs(60);
        // Attempts 1..=3 heal at base rate.
        assert_eq!(backoff_delay(1, base, max), base);
        assert_eq!(backoff_delay(2, base, max), base);
        assert_eq!(backoff_delay(3, base, max), base);
        // Then doubling.
        assert_eq!(backoff_delay(4, base, max), Duration::from_secs(10));
        assert_eq!(backoff_delay(5, base, max), Duration::from_secs(20));
        assert_eq!(backoff_delay(6, base, max), Duration::from_secs(40));
        // Clamped at max, never exceeding it.
        assert_eq!(backoff_delay(7, base, max), max); // 80s -> 60s
        assert_eq!(backoff_delay(8, base, max), max); // 160s -> 60s
    }

    #[test]
    fn test_backoff_delay_bounds_and_monotonic() {
        let base = Duration::from_secs(5);
        let max = Duration::from_secs(60);
        let mut prev = Duration::ZERO;
        for attempt in 1..=200u64 {
            let d = backoff_delay(attempt, base, max);
            assert!(d <= max, "attempt {attempt}: {d:?} exceeds max {max:?}");
            assert!(
                d >= prev,
                "attempt {attempt}: {d:?} < prev {prev:?} (not monotonic)"
            );
            prev = d;
        }
        // A large attempt count saturates rather than overflowing.
        assert_eq!(backoff_delay(u64::MAX, base, max), max);
    }

    #[test]
    fn test_backoff_delay_base_above_max_clamps() {
        let base = Duration::from_secs(90);
        let max = Duration::from_secs(60);
        // Even the base-rate attempts never exceed the cap.
        assert_eq!(backoff_delay(1, base, max), max);
        assert_eq!(backoff_delay(4, base, max), max);
    }

    #[test]
    fn test_backoff_jitter_within_twenty_percent() {
        let delay = Duration::from_secs(40);
        let lo = Duration::from_secs(32); // 0.8x
        let hi = Duration::from_secs(48); // 1.2x
        for attempt in 1..=64u64 {
            let j = backoff_jitter("tcp_client_0", attempt, delay);
            assert!(
                j >= lo && j <= hi,
                "attempt {attempt}: {j:?} outside +/-20% of {delay:?}"
            );
        }
    }

    #[test]
    fn test_backoff_jitter_deterministic() {
        let delay = Duration::from_secs(40);
        for attempt in 1..=16u64 {
            let a = backoff_jitter("autoconnect/peer_A", attempt, delay);
            let b = backoff_jitter("autoconnect/peer_A", attempt, delay);
            assert_eq!(
                a, b,
                "jitter must be deterministic for the same (name, attempt)"
            );
        }
    }

    #[test]
    fn test_backoff_jitter_differs_across_names() {
        // Anti-lockstep: different interface names draw different offsets, so a
        // fleet retrying the same dead peer does not knock in unison.
        let delay = Duration::from_secs(40);
        let mut differed = false;
        for attempt in 1..=16u64 {
            let a = backoff_jitter("tcp_client_0", attempt, delay);
            let b = backoff_jitter("tcp_client_1", attempt, delay);
            if a != b {
                differed = true;
                break;
            }
        }
        assert!(
            differed,
            "distinct names must produce distinct jitter for at least one attempt"
        );
    }

    #[test]
    fn test_should_log_failure_throttle() {
        // Always log the first three (base-rate) attempts.
        assert!(should_log_failure(1));
        assert!(should_log_failure(2));
        assert!(should_log_failure(3));
        // Log exactly on the doubling boundaries, silent in between.
        assert!(should_log_failure(4));
        assert!(!should_log_failure(5));
        assert!(!should_log_failure(6));
        assert!(!should_log_failure(7));
        assert!(should_log_failure(8));
        for a in 9..=15u64 {
            assert!(!should_log_failure(a), "attempt {a} should be silent");
        }
        assert!(should_log_failure(16));
        // Once capped, logging spacing keeps widening: at most one line per
        // capped interval. Count the logging attempts in a wide window.
        let logged = (17..=1024u64).filter(|&a| should_log_failure(a)).count();
        // Only 32, 64, 128, 256, 512, 1024 -> 6 lines across ~1000 attempts.
        assert_eq!(logged, 6);
    }

    #[test]
    fn test_connect_log_line_single_variant() {
        let addr: SocketAddr = "127.0.0.1:9050".parse().unwrap();
        // Clean first connect: just the endpoint.
        let clean = connect_log_line("tcp_client_0", addr, 0, None);
        assert_eq!(clean, "tcp_client_0: connected to 127.0.0.1:9050");
        // A connect that succeeded on attempt N (after N-1... here N failures)
        // reports recovery with the count and elapsed outage as ONE line.
        let recovered = connect_log_line("tcp_client_0", addr, 3, Some(Duration::from_secs(12)));
        assert!(recovered.contains("reconnected to 127.0.0.1:9050"));
        assert!(recovered.contains("after 3 attempt(s)"));
        assert!(recovered.contains("12.0s"));
        // Exactly one line either way — no embedded newline.
        assert!(!clean.contains('\n'));
        assert!(!recovered.contains('\n'));
    }

    #[test]
    fn test_backoff_resets_after_success() {
        // The reset is enforced in the task by `attempt = 0` on a successful
        // connect. Model it here: after a success the counter is 0, so the next
        // failure is attempt 1 and waits the base delay again.
        let base = Duration::from_secs(5);
        let max = Duration::from_secs(60);
        // Deep into backoff...
        assert_eq!(backoff_delay(7, base, max), max);
        // ...success resets the counter, so the next failure is attempt 1.
        let after_reset = 1u64;
        assert_eq!(backoff_delay(after_reset, base, max), base);
    }

    #[test]
    fn test_tcp_interface_connect_refused() {
        // Connecting to a port with nothing listening should fail
        let result = spawn_tcp_interface(
            InterfaceId(0),
            "test".to_string(),
            "127.0.0.1:19999",
            Duration::from_millis(500),
            16,
            None,
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_spawn_tcp_interface() {
        // Start a listener, connect via spawn_tcp_interface
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = spawn_tcp_interface(
            InterfaceId(0),
            "test_tcp".to_string(),
            addr,
            Duration::from_secs(2),
            16,
            None,
        )
        .unwrap();

        assert_eq!(handle.info.name, "test_tcp");
        assert_eq!(handle.info.id, InterfaceId(0));

        // Accept the connection on the listener side
        let (_server_stream, _peer) = listener.accept().unwrap();

        // The handle is valid and channels are open
        assert!(!handle.outgoing.is_closed());
    }

    #[test]
    fn test_maybe_corrupt_zero_means_no_corruption() {
        let mut buf = vec![0xAA; 100];
        let original = buf.clone();
        let mut rng = Xorshift64::from_entropy();
        let count = maybe_corrupt(&mut buf, 0, &mut rng);
        assert_eq!(count, 0);
        assert_eq!(buf, original);
    }

    #[test]
    fn test_maybe_corrupt_every_one_corrupts_all() {
        let mut buf = vec![0xAA; 500];
        let original = buf.clone();
        let mut rng = Xorshift64::from_entropy();
        let count = maybe_corrupt(&mut buf, 1, &mut rng);
        assert_eq!(count, 500);
        // XOR with non-zero guarantees every byte changed
        for (i, &b) in buf.iter().enumerate() {
            assert_ne!(b, original[i], "byte {i} should have changed");
        }
    }

    #[test]
    fn test_maybe_corrupt_rare_practically_none() {
        let mut buf = vec![0xAA; 500];
        let original = buf.clone();
        let mut rng = Xorshift64::from_entropy();
        let count = maybe_corrupt(&mut buf, 1_000_000, &mut rng);
        // With 500 bytes and 1/1M probability, expect ~0 corruptions
        assert!(count <= 2, "expected near-zero corruption, got {count}");
        // Most bytes should be unchanged
        let unchanged = buf
            .iter()
            .zip(original.iter())
            .filter(|(a, b)| a == b)
            .count();
        assert!(unchanged >= 498);
    }

    #[test]
    fn test_maybe_corrupt_empty_buffer() {
        let mut buf: Vec<u8> = Vec::new();
        let mut rng = Xorshift64::from_entropy();
        let count = maybe_corrupt(&mut buf, 1, &mut rng);
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_tcp_server_accepts_connection() {
        let next_id = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = mpsc::channel::<InterfaceHandle>(4);

        // Bind on ephemeral port
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let std_listener = std::net::TcpListener::bind(addr).unwrap();
        let bound_addr = std_listener.local_addr().unwrap();
        drop(std_listener); // free the port for spawn_tcp_server

        spawn_tcp_server(
            bound_addr,
            next_id.clone(),
            tx,
            16,
            None,
            None,
            leviculum_core::traits::InterfaceMode::default(),
        )
        .unwrap();

        // Connect a raw TCP client
        let _client = tokio::net::TcpStream::connect(bound_addr).await.unwrap();

        // Verify an InterfaceHandle arrives on the channel
        let handle = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout waiting for handle")
            .expect("channel closed");

        assert!(handle.info.name.starts_with("tcp_server/"));
        assert_eq!(handle.info.id, InterfaceId(0));
        assert!(!handle.outgoing.is_closed());
    }

    #[tokio::test]
    async fn test_tcp_server_spawned_child_inherits_mode() {
        // Codeberg #104: an accepted (spawned-per-connection) interface inherits
        // the listener's propagation mode, mirroring Python
        // `spawned_interface.mode = self.mode` (TCPInterface.py:625).
        use leviculum_core::traits::{Interface, InterfaceMode};

        let next_id = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = mpsc::channel::<InterfaceHandle>(4);

        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let std_listener = std::net::TcpListener::bind(addr).unwrap();
        let bound_addr = std_listener.local_addr().unwrap();
        drop(std_listener);

        spawn_tcp_server(
            bound_addr,
            next_id.clone(),
            tx,
            16,
            None,
            None,
            InterfaceMode::AccessPoint,
        )
        .unwrap();

        let _client = tokio::net::TcpStream::connect(bound_addr).await.unwrap();

        let handle = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout waiting for handle")
            .expect("channel closed");

        assert_eq!(
            handle.info.mode,
            InterfaceMode::AccessPoint,
            "spawned child must carry the listener's configured mode in its info"
        );
        assert_eq!(
            Interface::mode(&handle),
            InterfaceMode::AccessPoint,
            "the Interface::mode() trait accessor must report the inherited mode"
        );
    }

    #[tokio::test]
    async fn test_tcp_client_reconnects_after_disconnect() {
        use leviculum_core::framing::hdlc;

        // 1. Start TCP listener on ephemeral port
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // 2. Spawn reconnecting client with short interval
        let mut handle = spawn_tcp_client_with_reconnect(TcpClientConfig {
            id: InterfaceId(0),
            name: "test_reconnect".to_string(),
            addr,
            buffer_size: 32,
            corrupt_every: None,
            reconnect_interval: Duration::from_millis(200),
            max_reconnect_tries: Some(10),
            reconnect_max_interval: DEFAULT_RECONNECT_MAX_INTERVAL,
            connect_timeout: DEFAULT_TCP_CONNECT_TIMEOUT,
            reconnect_notify: None,
            tunnel_notify: None,
        });

        // 3. Accept first connection, send an HDLC-framed packet
        let (mut conn, _peer) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .expect("timeout accepting first connection")
            .unwrap();

        let payload = b"hello-first";
        let mut frame_buf = Vec::new();
        hdlc::frame(payload, &mut frame_buf);
        tokio::io::AsyncWriteExt::write_all(&mut conn, &frame_buf)
            .await
            .unwrap();

        // Verify packet arrives on incoming channel
        let pkt = tokio::time::timeout(Duration::from_secs(2), handle.incoming.recv())
            .await
            .expect("timeout waiting for first packet")
            .expect("channel closed");
        assert_eq!(pkt.data, payload);

        // 4. Drop the connection (simulate disconnect)
        drop(conn);

        // 5. Accept the reconnection
        let (mut conn2, _peer2) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
            .await
            .expect("timeout accepting reconnection")
            .unwrap();

        // 6. Send another framed packet on the new connection
        let payload2 = b"hello-second";
        let mut frame_buf2 = Vec::new();
        hdlc::frame(payload2, &mut frame_buf2);
        tokio::io::AsyncWriteExt::write_all(&mut conn2, &frame_buf2)
            .await
            .unwrap();

        // Verify second packet arrives
        let pkt2 = tokio::time::timeout(Duration::from_secs(2), handle.incoming.recv())
            .await
            .expect("timeout waiting for second packet")
            .expect("channel closed");
        assert_eq!(pkt2.data, payload2);

        // 7. Outgoing channel should still be open
        assert!(!handle.outgoing.is_closed());
    }

    #[tokio::test]
    async fn test_tcp_client_gives_up_after_max_retries() {
        // Use a port that nothing is listening on (bind and immediately drop)
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener); // nobody listening

        let mut handle = spawn_tcp_client_with_reconnect(TcpClientConfig {
            id: InterfaceId(0),
            name: "test_giveup".to_string(),
            addr,
            buffer_size: 16,
            corrupt_every: None,
            reconnect_interval: Duration::from_millis(100),
            max_reconnect_tries: Some(2),
            reconnect_max_interval: DEFAULT_RECONNECT_MAX_INTERVAL,
            // Short, explicit bound so give-up is deterministic regardless of
            // how long the OS takes to refuse a dead loopback port (instant on
            // Linux, ~1s+ SYN-retransmit on Windows). 2 tries × (≤300ms connect
            // + 100ms interval) stays well under the 3s test budget everywhere.
            connect_timeout: Duration::from_millis(300),
            reconnect_notify: None,
            tunnel_notify: None,
        });

        // Wait for the reconnect task to give up (2 attempts * 100ms + overhead)
        let result = tokio::time::timeout(Duration::from_secs(3), handle.incoming.recv()).await;

        // The incoming channel should close (recv returns None) because
        // the reconnect task dropped incoming_tx after max retries
        match result {
            Ok(None) => {} // expected: channel closed
            Ok(Some(_)) => panic!("should not receive a packet"),
            Err(_) => panic!("timeout — reconnect task did not give up in time"),
        }
    }
}
