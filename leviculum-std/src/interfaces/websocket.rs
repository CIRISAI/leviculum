//! +ciris (#109): WebSocket interfaces, server and client.
//!
//! A browser cannot open raw TCP or UDP, so a browser node reaches the mesh
//! over WebSocket; every node can therefore accept and dial WebSocket links.
//! Python RNS has no WebSocket interface, so these links are leviculum to
//! leviculum, and a node bridges them onto the rest of the mesh like any
//! other interface.
//!
//! # Wire framing
//!
//! Two framings, chosen per connection by WebSocket subprotocol
//! (`Sec-WebSocket-Protocol`); the server offers both, and a client that
//! asks for none gets `rns.raw`:
//!
//! - [`SUBPROTOCOL_RAW`] (`rns.raw`): one Reticulum packet (as the transport
//!   hands it to an interface, IFAC included when configured) is one
//!   **binary** message, unchanged, with no HDLC or KISS inside. A message
//!   longer than [`WS_HW_MTU`] ends the connection (close code 1009).
//! - [`SUBPROTOCOL_HDLC`] (`rns.hdlc`): the binary messages are a byte
//!   stream carrying the TCP interface's HDLC framing. Message boundaries
//!   mean nothing: a frame may span messages and a message may hold several
//!   frames. A frame longer than [`WS_HW_MTU`] is discarded, as on TCP.
//!
//! In both, a **text** message ends the connection with close code 1003
//! (unsupported data): a peer that sends text is not speaking this protocol,
//! and a close says so where silently dropping it would not. Ping and pong
//! are answered and count as liveness, never as traffic.
//!
//! # Structure
//!
//! Modelled on [`super::tcp`]: the server's accept loop sits under the same
//! admission tracker ([`super::tcp_admission`]: per-source and total caps, the
//! silent-client timeout) and registers every accepted connection as its own
//! interface; the client keeps one channel pair across reconnects and redials
//! with the TCP client's jittered backoff ([`super::tcp::backoff_delay`],
//! capped at 20 s). Both report the TCP interface's hardware MTU and bitrate.
//!
//! The server speaks plain `ws://`. For `wss://` it sits behind a reverse
//! proxy that terminates TLS; the client speaks both.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Bytes, Message};
use tokio_tungstenite::WebSocketStream;

use super::inventory::{InterfaceIdentity, ListenerRow, SharedInventory};
use super::tcp::{
    apply_liveness_options, backoff_delay, backoff_jitter, should_log_failure, TcpLiveness,
    TCP_BITRATE_GUESS, TCP_HW_MTU,
};
use super::tcp_admission::{
    effective_total_cap, fd_usage, open_fds, raise_nofile_limit, AcceptBackoff, Admission,
    AdmissionConfig, Closed, ConnTracker, Refusal, SourceKey, MIN_SERVER_FDS,
};
use super::{
    IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket, ReadySignal,
};
use crate::sync_ext::MutexRecover;
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::transport::InterfaceId;

/// The hardware MTU a WebSocket interface signals, and the largest message
/// it accepts: the TCP interface's, since both run over one TCP stream.
pub(crate) const WS_HW_MTU: u32 = TCP_HW_MTU;

/// Interval between the pings each side sends. A reverse proxy closes a
/// WebSocket that carries nothing for its read timeout (nginx: 60 s), and a
/// keepalive probe on the socket under it does not count there; a ping does.
pub(crate) const WS_PING_INTERVAL: Duration = Duration::from_secs(30);

/// A connection that delivered nothing at all (no message, no pong) for this
/// long is dead: three missed pings.
pub(crate) const WS_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Bound on the opening handshake (the HTTP upgrade, and TLS for `wss://`).
pub(crate) const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The subprotocol of one packet per binary message.
pub const SUBPROTOCOL_RAW: &str = "rns.raw";
/// The subprotocol of an HDLC byte stream over binary messages.
pub const SUBPROTOCOL_HDLC: &str = "rns.hdlc";

/// The largest message the WebSocket layer takes. Raw framing checks its own
/// bound ([`WS_HW_MTU`]) on top; an HDLC message is stream bytes and may
/// carry several escaped frames, so the layer's bound is wider.
const WS_MAX_MESSAGE: usize = 4 * WS_HW_MTU as usize;

/// How packets sit in the binary messages of one connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WsFraming {
    /// `rns.raw`: one packet per message.
    #[default]
    Raw,
    /// `rns.hdlc`: an HDLC byte stream, boundaries not significant.
    Hdlc,
}

impl WsFraming {
    /// The subprotocol name.
    pub fn subprotocol(self) -> &'static str {
        match self {
            WsFraming::Raw => SUBPROTOCOL_RAW,
            WsFraming::Hdlc => SUBPROTOCOL_HDLC,
        }
    }

    /// The framing a subprotocol name selects, `None` for any other name.
    pub fn from_subprotocol(name: &str) -> Option<Self> {
        match name.trim() {
            SUBPROTOCOL_RAW => Some(WsFraming::Raw),
            SUBPROTOCOL_HDLC => Some(WsFraming::Hdlc),
            _ => None,
        }
    }

    /// The config value (`framing = raw | hdlc`).
    pub(crate) fn from_config(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "raw" => Some(WsFraming::Raw),
            "hdlc" => Some(WsFraming::Hdlc),
            _ => None,
        }
    }
}

/// The server's pick from the client's `Sec-WebSocket-Protocol` offer: the
/// first name it knows, in the client's order; `None` when the client asked
/// for none or for none it knows, which is served as raw with no protocol
/// header in the answer.
fn pick_framing(offer: Option<&str>) -> Option<WsFraming> {
    offer?.split(',').find_map(WsFraming::from_subprotocol)
}

/// The WebSocket layer's limits.
fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(WS_MAX_MESSAGE))
        .max_frame_size(Some(WS_MAX_MESSAGE))
}

fn interface_info(id: InterfaceId, name: String) -> InterfaceInfo {
    InterfaceInfo {
        transit: true,
        id,
        name,
        hw_mtu: Some(WS_HW_MTU),
        is_local_client: false,
        bitrate: None,
        announce_cap_bitrate: None,
        tx_jitter_max_ms: None,
        tx_hold_spread_max_ms: None,
        acquisition: None,
        frame_turnaround_ms: None,
        ifac: None,
        mode: leviculum_core::traits::InterfaceMode::default(),
        kind: leviculum_core::traits::InterfaceKind::WebSocket,
        ingress_control: None,
        path_choice: leviculum_core::transport::PathChoice::Hops,
    }
}

/// Where a WebSocket client dials: `ws[s]://<host>:<port><path>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WsTarget {
    pub host: String,
    pub port: u16,
    /// Starts with `/`.
    pub path: String,
    pub tls: bool,
}

impl WsTarget {
    pub(crate) fn new(host: impl Into<String>, port: u16, path: &str, tls: bool) -> Self {
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        Self {
            host: host.into(),
            port,
            path,
            tls,
        }
    }

    /// Parse `ws://host[:port][/path]` or `wss://...`. The port defaults to
    /// 80 or 443, the path to `/`; an IPv6 host is written in brackets.
    pub(crate) fn parse_url(url: &str) -> Result<Self, String> {
        let url = url.trim();
        let (tls, rest) = if let Some(rest) = url.strip_prefix("wss://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("ws://") {
            (false, rest)
        } else {
            return Err(format!("{url:?} is not a ws:// or wss:// URL"));
        };
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], &rest[i..]),
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let path = if path.starts_with('?') {
            format!("/{path}")
        } else {
            path.to_string()
        };
        let default_port = if tls { 443 } else { 80 };
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, tail) = v6
                .split_once(']')
                .ok_or_else(|| format!("{url:?}: unclosed '[' in the host"))?;
            let port = match tail.strip_prefix(':') {
                Some(p) => p.parse().map_err(|_| format!("{url:?}: bad port {p:?}"))?,
                None if tail.is_empty() => default_port,
                None => return Err(format!("{url:?}: junk after the host")),
            };
            (host.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (
                    h.to_string(),
                    p.parse().map_err(|_| format!("{url:?}: bad port {p:?}"))?,
                ),
                None => (authority.to_string(), default_port),
            }
        };
        if host.is_empty() || host.contains('@') {
            return Err(format!("{url:?}: no host (or credentials in it)"));
        }
        Ok(Self::new(host, port, &path, tls))
    }

    /// The URL the opening handshake requests.
    pub(crate) fn url(&self) -> String {
        let scheme = if self.tls { "wss" } else { "ws" };
        let host = self.host.trim_start_matches('[').trim_end_matches(']');
        if host.contains(':') {
            format!("{scheme}://[{host}]:{}{}", self.port, self.path)
        } else {
            format!("{scheme}://{host}:{}{}", self.port, self.path)
        }
    }
}

impl std::fmt::Display for WsTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.url())
    }
}

/// The rustls client configuration a `wss://` client dials with: ring, the
/// webpki roots, TLS 1.2 and 1.3. Built here rather than left to the
/// process-wide default provider, which panics when a build links both of
/// rustls's providers.
fn tls_connector() -> io::Result<tokio_tungstenite::Connector> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(io::Error::other)?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(tokio_tungstenite::Connector::Rustls(Arc::new(config)))
}

/// Why a served connection ended.
enum ServeEnd {
    /// Closed, an error, silence past [`WS_IDLE_TIMEOUT`], or a closed
    /// channel: the ordinary loss.
    Lost,
    /// The host asked for a reconnect (leviculum#84).
    Kicked,
}

/// A server connection's place in its listener's [`ConnTracker`], as
/// [`super::tcp::ConnLease`] is for a TCP server's.
struct WsLease {
    tracker: Arc<Mutex<ConnTracker>>,
    id: u64,
}

impl Drop for WsLease {
    fn drop(&mut self) {
        self.tracker.lock_recover().remove(self.id);
    }
}

/// Serve one open WebSocket: packets in both directions until it ends.
///
/// Returns the `outgoing_rx` so the client's reconnect loop reuses it on the
/// next connection, as the TCP client does.
#[allow(clippy::too_many_arguments)]
async fn ws_interface_task<S>(
    name: String,
    ws: WebSocketStream<S>,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
    honour_kick: bool,
    lease: Option<WsLease>,
    mut close: Option<oneshot::Receiver<()>>,
    framing: WsFraming,
) -> (mpsc::Receiver<OutgoingPacket>, ServeEnd)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut sink, mut stream) = ws.split();
    let mut spoken = false;
    // `rns.hdlc` only: the TCP interface's deframer, bounded the same way,
    // and the buffer each outgoing frame is built in.
    let mut deframer =
        (framing == WsFraming::Hdlc).then(|| Deframer::with_max_frame(WS_HW_MTU as usize));
    let mut frame_buf = Vec::new();
    let mut last_rx = Instant::now();
    let mut ping = tokio::time::interval_at(
        tokio::time::Instant::now() + WS_PING_INTERVAL,
        WS_PING_INTERVAL,
    );
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = counters.reconnect_kicked(), if honour_kick => {
                tracing::debug!("WebSocket interface {} dropped on host request", name);
                let _ = sink.close().await;
                return (outgoing_rx, ServeEnd::Kicked);
            }
            // The listener's admission tracker closed this connection as
            // silent, or to make room for a newcomer.
            _ = async {
                match close.as_mut() {
                    Some(rx) => {
                        let _ = rx.await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {
                tracing::debug!("WebSocket interface {} closed by admission control", name);
                let _ = sink.close().await;
                return (outgoing_rx, ServeEnd::Lost);
            }
            _ = ping.tick() => {
                if last_rx.elapsed() >= WS_IDLE_TIMEOUT {
                    tracing::debug!(
                        "WebSocket interface {} silent for {:?}, dropped", name, last_rx.elapsed());
                    return (outgoing_rx, ServeEnd::Lost);
                }
                if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                    return (outgoing_rx, ServeEnd::Lost);
                }
            }
            msg = stream.next() => {
                let msg = match msg {
                    Some(Ok(msg)) => msg,
                    Some(Err(e)) => {
                        tracing::debug!("WebSocket interface {} read error: {}", name, e);
                        return (outgoing_rx, ServeEnd::Lost);
                    }
                    None => {
                        tracing::debug!("WebSocket interface {} disconnected", name);
                        return (outgoing_rx, ServeEnd::Lost);
                    }
                };
                last_rx = Instant::now();
                match msg {
                    Message::Binary(data) => {
                        counters.rx_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
                        let packets: Vec<Vec<u8>> = match deframer.as_mut() {
                            None if data.len() > WS_HW_MTU as usize => {
                                tracing::debug!(
                                    "WebSocket interface {}: {} byte message exceeds HW_MTU, closing",
                                    name, data.len());
                                let _ = sink.send(Message::Close(Some(CloseFrame {
                                    code: CloseCode::Size,
                                    reason: "message exceeds HW_MTU".into(),
                                }))).await;
                                return (outgoing_rx, ServeEnd::Lost);
                            }
                            None => vec![data.to_vec()],
                            Some(d) => d
                                .process(&data)
                                .into_iter()
                                .filter_map(|r| match r {
                                    DeframeResult::Frame(f) => Some(f),
                                    _ => None,
                                })
                                .collect(),
                        };
                        if !packets.is_empty() {
                            if let Some(l) = lease.as_ref().filter(|_| !spoken) {
                                // Spoken once is spoken for good, as on TCP.
                                l.tracker.lock_recover().mark_spoken(l.id);
                                spoken = true;
                            }
                        }
                        for data in packets {
                            let packet = IncomingPacket { data, peer: None, radio: None };
                            if incoming_tx.send(packet).await.is_err() {
                                return (outgoing_rx, ServeEnd::Lost);
                            }
                        }
                    }
                    Message::Text(_) => {
                        tracing::debug!(
                            "WebSocket interface {}: text message, closing (1003)", name);
                        let _ = sink.send(Message::Close(Some(CloseFrame {
                            code: CloseCode::Unsupported,
                            reason: "binary messages only".into(),
                        }))).await;
                        return (outgoing_rx, ServeEnd::Lost);
                    }
                    Message::Close(_) => {
                        tracing::debug!("WebSocket interface {} closed by peer", name);
                        return (outgoing_rx, ServeEnd::Lost);
                    }
                    // Pings are answered by the WebSocket layer; a pong and a
                    // ping only prove the peer alive.
                    Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                }
            }
            pkt = outgoing_rx.recv() => {
                let Some(pkt) = pkt else {
                    tracing::debug!("WebSocket interface {} outgoing channel closed", name);
                    let _ = sink.close().await;
                    return (outgoing_rx, ServeEnd::Lost);
                };
                let bytes = match framing {
                    WsFraming::Raw => Bytes::copy_from_slice(&pkt.data),
                    WsFraming::Hdlc => {
                        frame(&pkt.data, &mut frame_buf);
                        Bytes::copy_from_slice(&frame_buf)
                    }
                };
                // Counted before the write, as on TCP (Codeberg #389).
                counters.tx_bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                if let Err(e) = sink.send(Message::Binary(bytes)).await {
                    tracing::debug!("WebSocket interface {} write error: {}", name, e);
                    return (outgoing_rx, ServeEnd::Lost);
                }
            }
        }
    }
}

/// Configuration for a WebSocket server (`WebSocketServerInterface`).
pub(crate) struct WsServerConfig {
    pub bind_addr: SocketAddr,
    /// A listener the caller already bound; `bind_addr` is then only the
    /// fallback name (as [`super::tcp::TcpServerConfig::prebound`]).
    pub prebound: Option<std::net::TcpListener>,
    pub section: String,
    pub next_id: Arc<AtomicUsize>,
    pub new_interface_tx: mpsc::Sender<InterfaceHandle>,
    pub buffer_size: usize,
    pub ifac: Option<leviculum_core::ifac::IfacConfig>,
    pub mode: leviculum_core::traits::InterfaceMode,
    pub transit: bool,
    pub ingress_control: bool,
    pub listener_id: usize,
    pub inventory: SharedInventory,
    pub announce_rate: (Option<u32>, Option<u32>, Option<u32>),
    pub admission: AdmissionConfig,
    pub liveness: TcpLiveness,
}

/// How often the accept loop looks for connections past the silent timeout.
const SILENT_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// `WebSocketServerInterface[<section>/<ip>:<port>]`.
pub(crate) fn ws_listener_name(section: &str, bind: SocketAddr) -> String {
    format!("WebSocketServerInterface[{section}/{bind}]")
}

/// Start a WebSocket server. Binds synchronously, so a bind error reaches the
/// caller, then accepts in the background, as [`super::tcp::spawn_tcp_server`]
/// does. Each connection the tracker admits does its opening handshake in its
/// own task and becomes its own interface once that succeeds.
pub(crate) fn spawn_ws_server(config: WsServerConfig) -> Result<SocketAddr, io::Error> {
    let WsServerConfig {
        bind_addr,
        prebound,
        section,
        next_id,
        new_interface_tx,
        buffer_size,
        ifac,
        mode,
        transit,
        ingress_control,
        listener_id,
        inventory,
        announce_rate,
        admission,
        liveness,
    } = config;

    let nofile = raise_nofile_limit();
    let total_cap = match effective_total_cap(
        admission.max_connections,
        nofile.map(|l| l.soft),
        open_fds(),
    ) {
        Ok(cap) => cap,
        Err(free) => {
            return Err(io::Error::other(format!(
                "WebSocket server {section}: only {free} file descriptors are free, a server \
                 needs at least {MIN_SERVER_FDS}; raise LimitNOFILE / ulimit -n"
            )))
        }
    };
    let tracker = Arc::new(Mutex::new(ConnTracker::new(AdmissionConfig {
        max_connections: total_cap,
        ..admission
    })));

    let std_listener = match prebound {
        Some(listener) => listener,
        None => std::net::TcpListener::bind(bind_addr)?,
    };
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(std_listener)?;
    let bound = listener.local_addr().unwrap_or(bind_addr);
    tracing::info!("WebSocket server listening on {}", bound);

    inventory.lock_recover().add_listener(
        listener_id,
        ListenerRow {
            identity: InterfaceIdentity {
                name: ws_listener_name(&section, bound),
                short_name: section.clone(),
                type_name: "WebSocketServerInterface",
                parent: None,
            },
            bitrate: TCP_BITRATE_GUESS,
            hw_mtu: WS_HW_MTU as i64,
            mode,
            announce_rate,
            ifac_size_bits: ifac.as_ref().map(|c| (c.ifac_size() * 8) as i64),
            departed_rxb: 0,
            departed_txb: 0,
            bound_addr: Some(bound),
            admission: Some(Arc::clone(&tracker)),
        },
    );

    let child = Arc::new(ChildTemplate {
        section: section.clone(),
        next_id,
        new_interface_tx: new_interface_tx.clone(),
        buffer_size,
        ifac,
        mode,
        transit,
        ingress_control,
        listener_id,
        inventory,
    });

    tokio::spawn(async move {
        let mut backoff = AcceptBackoff::default();
        let mut janitor = tokio::time::interval(SILENT_CHECK_INTERVAL);
        janitor.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                result = listener.accept() => match result {
                    Ok((stream, peer)) => {
                        backoff.on_success();
                        let now = Instant::now();
                        let decision = tracker.lock_recover().admit(peer.ip(), now);
                        match decision {
                            Admission::Accept { id, close, evicted } => {
                                if let Some(victim) = evicted {
                                    report_silent_close(&tracker, &section, &victim, now);
                                    victim.close();
                                }
                                let lease = WsLease { tracker: Arc::clone(&tracker), id };
                                stream.set_nodelay(true).ok();
                                apply_liveness_options(&stream, &liveness).ok();
                                tokio::spawn(serve_accepted(
                                    Arc::clone(&child), stream, peer, lease, close,
                                ));
                            }
                            Admission::Refuse { reason, source, held, report } => {
                                refuse(stream, reason, source, held, report, &section, peer);
                            }
                        }
                    }
                    Err(e) => {
                        let (pause, report) = backoff.on_error(Instant::now());
                        if let Some(errors) = report {
                            tracing::warn!(
                                event = "TCP_ACCEPT_BACKOFF",
                                iface = %section,
                                errors = errors,
                                fds = %fd_usage(),
                                error = %e,
                            );
                        }
                        tokio::select! {
                            _ = tokio::time::sleep(pause) => {}
                            _ = new_interface_tx.closed() => break,
                        }
                    }
                },
                _ = janitor.tick() => {
                    let now = Instant::now();
                    let closed = tracker.lock_recover().expire(now);
                    for victim in closed {
                        report_silent_close(&tracker, &section, &victim, now);
                        victim.close();
                    }
                }
                _ = new_interface_tx.closed() => {
                    tracing::debug!("WebSocket server shutting down (event loop exited)");
                    break;
                }
            }
        }
    });

    Ok(bound)
}

/// What every accepted connection inherits from its listener.
struct ChildTemplate {
    section: String,
    next_id: Arc<AtomicUsize>,
    new_interface_tx: mpsc::Sender<InterfaceHandle>,
    buffer_size: usize,
    ifac: Option<leviculum_core::ifac::IfacConfig>,
    mode: leviculum_core::traits::InterfaceMode,
    transit: bool,
    ingress_control: bool,
    listener_id: usize,
    inventory: SharedInventory,
}

/// One admitted connection: the opening handshake, then its interface. The
/// lease is held from accept on, so a client that never finishes the
/// handshake counts against its source and is closed as silent like any
/// other.
async fn serve_accepted(
    child: Arc<ChildTemplate>,
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    lease: WsLease,
    mut close: oneshot::Receiver<()>,
) {
    // The subprotocol the client asked for decides the framing; the answer
    // names the one picked, and no protocol when the client named none it
    // knows (served raw).
    let picked = Arc::new(Mutex::new(WsFraming::Raw));
    let pick = Arc::clone(&picked);
    // The error type is tungstenite's `Callback` contract, not ours.
    #[allow(clippy::result_large_err)]
    let callback =
        move |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
              mut resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let offer = req
                .headers()
                .get("Sec-WebSocket-Protocol")
                .and_then(|v| v.to_str().ok());
            if let Some(framing) = pick_framing(offer) {
                *pick.lock_recover() = framing;
                if let Ok(value) = framing.subprotocol().parse() {
                    resp.headers_mut().insert("Sec-WebSocket-Protocol", value);
                }
            }
            Ok(resp)
        };
    let handshake = tokio::time::timeout(
        WS_HANDSHAKE_TIMEOUT,
        tokio_tungstenite::accept_hdr_async_with_config(stream, callback, Some(ws_config())),
    );
    let ws = tokio::select! {
        result = handshake => match result {
            Ok(Ok(ws)) => ws,
            Ok(Err(e)) => {
                tracing::debug!("WebSocket server {}: handshake failed: {}", child.section, e);
                return;
            }
            Err(_) => {
                tracing::debug!("WebSocket server {}: handshake timed out", child.section);
                return;
            }
        },
        _ = &mut close => return,
    };
    let framing = *picked.lock_recover();

    let id = InterfaceId(child.next_id.fetch_add(1, Ordering::Relaxed));
    // The driver's name carries the source's daily pseudonym, not its
    // address (order 1678), as a TCP server's does.
    let src_tag = crate::peer_tag::tag(peer.ip());
    let shown = crate::peer_tag::show_addr(peer);
    let name = format!("ws_server/{shown}");
    {
        let mut inv = child.inventory.lock_recover();
        inv.add_spawned(
            id.0,
            InterfaceIdentity {
                name: format!("WebSocketInterface[Client on {}/{}]", child.section, peer),
                short_name: format!("Client on {}", child.section),
                type_name: "WebSocketClientInterface",
                parent: Some(child.listener_id),
            },
        );
        inv.set_src_tag(id.0, src_tag.to_string());
    }

    let (incoming_tx, incoming_rx) = mpsc::channel(child.buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(child.buffer_size);
    let counters = Arc::new(InterfaceCounters::new());
    let mut info = interface_info(id, name.clone());
    info.ifac = child.ifac.clone();
    info.mode = child.mode;
    info.transit = child.transit;
    info.ingress_control = Some(child.ingress_control);
    let handle = InterfaceHandle {
        next_tag: None,
        info,
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters: Arc::clone(&counters),
        credit: None,
        ready: ReadySignal::ready_immediate(),
        teardown: None,
    };
    tracing::info!(
        "Accepted WebSocket connection: {} ({}) src={} framing={}",
        name,
        id,
        shown,
        framing.subprotocol()
    );
    if child.new_interface_tx.send(handle).await.is_err() {
        return;
    }
    let _ = ws_interface_task(
        name,
        ws,
        incoming_tx,
        outgoing_rx,
        counters,
        false,
        Some(lease),
        Some(close),
        framing,
    )
    .await;
}

/// Close a refused connection with a reset, and report it when the tracker
/// says a report is due, under the TCP server's event names (the `iface`
/// key names the WebSocket section).
fn refuse(
    stream: tokio::net::TcpStream,
    reason: Refusal,
    source: SourceKey,
    held: usize,
    report: bool,
    section: &str,
    peer: SocketAddr,
) {
    socket2::SockRef::from(&stream)
        .set_linger(Some(Duration::ZERO))
        .ok();
    drop(stream);
    if !report {
        return;
    }
    match reason {
        Refusal::Source => tracing::warn!(
            event = "TCP_SOURCE_CAPPED",
            iface = %section,
            source = %crate::peer_tag::show_ip(source.ip()),
            held = held,
        ),
        Refusal::Total => tracing::warn!(
            event = "TCP_TOTAL_CAPPED",
            iface = %section,
            source = %crate::peer_tag::show_ip(peer.ip()),
            held = held,
        ),
    }
}

fn report_silent_close(tracker: &Mutex<ConnTracker>, section: &str, closed: &Closed, now: Instant) {
    if let Some(suppressed) = tracker.lock_recover().silent_report(now) {
        tracing::info!(
            event = "TCP_SILENT_CLOSED",
            iface = %section,
            source = %crate::peer_tag::show_ip(closed.peer),
            after_s = closed.after.as_secs(),
            reason = closed.reason,
            suppressed = suppressed,
        );
    }
}

/// Configuration for a reconnecting WebSocket client
/// (`WebSocketClientInterface`).
pub(crate) struct WsClientConfig {
    pub id: InterfaceId,
    pub name: String,
    pub target: WsTarget,
    pub resolver: Option<crate::resolver::HostResolver>,
    pub buffer_size: usize,
    pub reconnect_interval: Duration,
    pub max_reconnect_tries: Option<u64>,
    pub reconnect_max_interval: Duration,
    pub connect_timeout: Duration,
    pub reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    pub outbound_socket_hook: Option<crate::socket_hook::OutboundSocketHook>,
    pub liveness: TcpLiveness,
    /// The framing this client asks for; it names it in the handshake.
    pub framing: WsFraming,
}

/// Spawn a WebSocket client that dials in the background and redials when
/// the connection ends, as [`super::tcp::spawn_tcp_client_with_reconnect`]
/// does: the handle is returned at once, and its channels live across
/// reconnects.
pub(crate) fn spawn_ws_client_with_reconnect(config: WsClientConfig) -> InterfaceHandle {
    let (incoming_tx, incoming_rx) = mpsc::channel(config.buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(config.buffer_size);
    let counters = Arc::new(InterfaceCounters::new());
    counters.set_online(false);
    let ready = ReadySignal::new();
    let info = interface_info(config.id, config.name.clone());
    let task_counters = Arc::clone(&counters);
    let task_ready = Arc::clone(&ready);
    tokio::spawn(ws_client_reconnect_loop(
        config,
        incoming_tx,
        outgoing_rx,
        task_counters,
        task_ready,
    ));
    InterfaceHandle {
        next_tag: None,
        info,
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
        ready,
        teardown: None,
    }
}

/// One dial: resolve afresh, TCP to the first address that answers, then the
/// opening handshake (with TLS for `wss://`).
async fn ws_dial(
    config: &WsClientConfig,
    connector: &tokio_tungstenite::Connector,
) -> io::Result<(
    WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    SocketAddr,
)> {
    let target = &config.target;
    let host = target.host.trim_start_matches('[').trim_end_matches(']');
    let addrs = tokio::time::timeout(
        config.connect_timeout,
        crate::resolver::resolve(config.resolver.as_ref(), host, target.port),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "lookup timed out"))??;
    let mut last = None;
    let mut connected = None;
    for addr in addrs {
        match tokio::time::timeout(
            config.connect_timeout,
            crate::socket_hook::connect_hooked(addr, config.outbound_socket_hook.as_ref()),
        )
        .await
        {
            Ok(Ok(stream)) => {
                connected = Some((stream, addr));
                break;
            }
            Ok(Err(e)) => last = Some(e),
            Err(_) => {
                last = Some(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "connect attempt timed out",
                ))
            }
        }
    }
    let Some((stream, addr)) = connected else {
        return Err(last.unwrap_or_else(|| io::Error::other(format!("no addresses for {target}"))));
    };
    stream.set_nodelay(true).ok();
    apply_liveness_options(&stream, &config.liveness).ok();
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
            target.url(),
        )
        .map_err(io::Error::other)?;
    if let Ok(value) = config.framing.subprotocol().parse() {
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", value);
    }
    let (ws, _response) = tokio::time::timeout(
        WS_HANDSHAKE_TIMEOUT,
        tokio_tungstenite::client_async_tls_with_config(
            request,
            stream,
            Some(ws_config()),
            Some(connector.clone()),
        ),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handshake timed out"))?
    .map_err(io::Error::other)?;
    Ok((ws, addr))
}

async fn ws_client_reconnect_loop(
    config: WsClientConfig,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
    ready: Arc<ReadySignal>,
) {
    let name = config.name.clone();
    let connector = if config.target.tls {
        match tls_connector() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("{}: TLS setup failed: {}", name, e);
                return;
            }
        }
    } else {
        tokio_tungstenite::Connector::Plain
    };
    let mut attempt = 0u64;
    let mut has_connected_before = false;
    counters.enable_reconnect();
    loop {
        counters.set_online(false);
        match ws_dial(&config, &connector).await {
            Ok((ws, addr)) => {
                let is_reconnect = has_connected_before;
                has_connected_before = true;
                attempt = 0;
                ready.signal_ready();
                counters.set_online(true);
                tracing::info!("{}: connected to {} ({})", name, config.target, addr);
                if is_reconnect {
                    if let Some(ref notify) = config.reconnect_notify {
                        let _ = notify.try_send(config.id);
                    }
                }
                let (rx, end) = ws_interface_task(
                    name.clone(),
                    ws,
                    incoming_tx.clone(),
                    outgoing_rx,
                    Arc::clone(&counters),
                    true,
                    None,
                    None,
                    config.framing,
                )
                .await;
                outgoing_rx = rx;
                match end {
                    ServeEnd::Lost => tracing::warn!("{}: connection lost, will reconnect", name),
                    ServeEnd::Kicked => {
                        tracing::info!("{}: reconnecting now (host request)", name);
                        if incoming_tx.is_closed() {
                            return;
                        }
                        continue;
                    }
                }
            }
            Err(e) => {
                if should_log_failure(attempt + 1) {
                    tracing::warn!(
                        "{}: connect to {} failed: {} (attempt {})",
                        name,
                        config.target,
                        e,
                        attempt + 1
                    );
                }
            }
        }
        attempt += 1;
        if let Some(max) = config.max_reconnect_tries {
            if attempt >= max {
                tracing::error!("{}: max reconnect attempts ({}) reached", name, max);
                return;
            }
        }
        if incoming_tx.is_closed() {
            return;
        }
        let delay = backoff_jitter(
            &name,
            attempt,
            backoff_delay(
                attempt,
                config.reconnect_interval,
                config.reconnect_max_interval,
            ),
        );
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = counters.reconnect_kicked() => attempt = 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_renders_its_url() {
        assert_eq!(
            WsTarget::new("example.org", 443, "/rns", true).url(),
            "wss://example.org:443/rns"
        );
        assert_eq!(
            WsTarget::new("::1", 8080, "mesh", false).url(),
            "ws://[::1]:8080/mesh"
        );
        assert_eq!(
            WsTarget::new("[::1]", 80, "/", false).url(),
            "ws://[::1]:80/"
        );
    }

    #[test]
    fn a_url_parses_into_a_target() {
        let t = |u| WsTarget::parse_url(u).expect(u);
        assert_eq!(
            t("wss://mesh.example.org/rns"),
            WsTarget::new("mesh.example.org", 443, "/rns", true)
        );
        assert_eq!(
            t("ws://127.0.0.1:8765"),
            WsTarget::new("127.0.0.1", 8765, "/", false)
        );
        assert_eq!(
            t("ws://[::1]:9/a/b?x=1"),
            WsTarget::new("::1", 9, "/a/b?x=1", false)
        );
        assert_eq!(
            t("wss://[2001:db8::1]"),
            WsTarget::new("2001:db8::1", 443, "/", true)
        );
        assert_eq!(t("ws://h?q"), WsTarget::new("h", 80, "/?q", false));
        for bad in [
            "http://h/",
            "ws://",
            "ws://h:port/",
            "ws://[::1/",
            "ws://u@h/",
        ] {
            assert!(WsTarget::parse_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_server_picks_the_first_subprotocol_it_knows() {
        assert_eq!(pick_framing(None), None);
        assert_eq!(pick_framing(Some("chat, other")), None);
        assert_eq!(pick_framing(Some("rns.hdlc")), Some(WsFraming::Hdlc));
        assert_eq!(
            pick_framing(Some("x, rns.raw, rns.hdlc")),
            Some(WsFraming::Raw)
        );
        assert_eq!(WsFraming::from_config("HDLC"), Some(WsFraming::Hdlc));
        assert_eq!(WsFraming::from_config("kiss"), None);
    }

    #[test]
    fn the_tls_connector_builds_on_ring_with_the_webpki_roots() {
        assert!(matches!(
            tls_connector().expect("connector"),
            tokio_tungstenite::Connector::Rustls(_)
        ));
    }
}
