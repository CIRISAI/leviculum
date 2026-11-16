//! HostInterface (+ciris): a framed Reticulum interface over a carrier the
//! host supplies.
//!
//! The carrier is a connection the node does not own: a WebSocket the
//! browser holds, a TCP stream an embedding app accepted, a serial port the
//! host opened. [`HostCarrier`] mirrors the `net` interface's `stream`
//! resource in the `ciris:node` WIT (CIRISVerify `src/ciris-host-abi`): a
//! non-blocking `send` and `recv` that answer `would-block`, `info`,
//! `close`, and the connection's events. Readiness is edge-triggered: after
//! `Readable` the interface calls `recv` until it answers
//! [`CarrierError::WouldBlock`] before it expects another, and `Writable`
//! comes once after a `send` answered `WouldBlock`.
//!
//! Framing follows the connection's [`Framing`]:
//! - [`Framing::Message`]: one Reticulum packet per message (`rns.raw` on a
//!   WebSocket). The hardware MTU is the carrier's `max_message`, capped at
//!   the TCP interface's; a carrier whose messages cannot hold a 500-byte
//!   packet is refused.
//! - [`Framing::Stream`]: the TCP interface's HDLC byte stream (`rns.hdlc`),
//!   with the TCP interface's hardware MTU. A send may take a prefix; the
//!   rest waits for `Writable`.
//!
//! One HostInterface carries one connection. It registers with the node
//! when the carrier connects and detaches when the carrier closes or
//! fails. It never redials: reconnecting is the host's policy, and the next
//! connection is a new interface. See
//! [`ReticulumNode::attach_host_interface`](crate::driver::ReticulumNode::attach_host_interface).
//!
//! The run loop below uses no runtime: only `poll_fn` and the attached
//! interface's channels, so it runs on any executor.

use std::collections::VecDeque;
use std::future::{poll_fn, Future};
use std::sync::Arc;
use std::task::{Context, Poll};

use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::ifac::IfacConfig;
use leviculum_core::traits::{InterfaceKind, InterfaceMode};

use crate::driver::{
    ExternalInterface, ExternalInterfaceConfig, OutboundFrame, EXTERNAL_DEFAULT_BUFFER_SIZE,
};
use crate::error::Error;

/// The connection-shaped carriers (WIT `net.carrier`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CarrierKind {
    /// A TCP stream.
    Tcp,
    /// Connected UDP datagrams (message framing).
    Udp,
    /// A WebSocket.
    Ws,
    /// A WebTransport session.
    WebTransport,
    /// A WebRTC data channel.
    WebRtc,
    /// A serial port; its settings are host configuration.
    Serial,
    /// A BLE connection.
    Ble,
    /// HTTP fetch, as a carrier.
    HttpFetch,
}

impl CarrierKind {
    /// The interface kind an interface over this carrier reports. Carriers
    /// with no kind of their own report [`InterfaceKind::Unknown`], which
    /// keeps the conservative resource window under `ByMedium`.
    pub fn interface_kind(self) -> InterfaceKind {
        match self {
            CarrierKind::Tcp => InterfaceKind::Tcp,
            CarrierKind::Udp => InterfaceKind::Udp,
            CarrierKind::Ws => InterfaceKind::WebSocket,
            CarrierKind::Serial => InterfaceKind::Serial,
            CarrierKind::Ble => InterfaceKind::Ble,
            CarrierKind::WebTransport | CarrierKind::WebRtc | CarrierKind::HttpFetch => {
                InterfaceKind::Unknown
            }
        }
    }
}

/// How a connection delimits data (WIT `net.framing`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    /// An ordered byte stream; `send` may take a prefix.
    Stream,
    /// Discrete messages; `send` takes a whole message or nothing.
    Message,
}

/// What the host knows about a connection (WIT `net.conn-info`). Fields the
/// host cannot know before the connection is up read empty until then.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CarrierInfo {
    /// The carrier.
    pub carrier: CarrierKind,
    /// How data is delimited.
    pub framing: Framing,
    /// The largest message `send` takes, for message framing.
    pub max_message: Option<u32>,
    /// Whether delivered data is reliable and ordered.
    pub reliable: bool,
    /// The negotiated WebSocket subprotocol.
    pub subprotocol: Option<String>,
    /// The peer's address as the host reports it; opaque.
    pub peer: Option<String>,
    /// The host's estimate of the bitrate, bits per second.
    pub bitrate_hint: Option<u64>,
}

impl CarrierInfo {
    /// Info with every optional field empty and `reliable` true.
    pub fn new(carrier: CarrierKind, framing: Framing) -> Self {
        Self {
            carrier,
            framing,
            max_message: None,
            reliable: true,
            subprotocol: None,
            peer: None,
            bitrate_hint: None,
        }
    }
}

/// What a carrier call answered instead of data (WIT `types.errno`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CarrierError {
    /// No progress now; the matching event follows.
    WouldBlock,
    /// The connection is closed (for `recv`: and everything was read).
    Closed,
    /// The peer or the host refused.
    Refused,
    /// Not found.
    NotFound,
    /// Out of space.
    NoSpace,
    /// Denied.
    Denied,
    /// Malformed or out of range: a message larger than `max_message`, or
    /// one larger than the `max` given to `recv`.
    Invalid,
    /// The host does not offer it.
    Unsupported,
    /// The host's I/O failed.
    Io,
}

/// Why a connection closed (WIT `net.close-reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// The peer closed it.
    PeerClose,
    /// A transport error closed it.
    Error,
    /// The host is shutting down.
    HostShutdown,
}

/// Why a dial failed (WIT `net.fail-reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailReason {
    /// The peer refused.
    Refused,
    /// No route.
    Unreachable,
    /// Timed out.
    Timeout,
    /// The name did not resolve.
    NameNotResolved,
    /// Policy or the person denied it.
    Denied,
    /// Anything else.
    Error,
}

/// One connection event (the `net` arms of WIT `event`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarrierEvent {
    /// The dial completed.
    Connected,
    /// Data became readable after a `recv` answered `WouldBlock`.
    Readable,
    /// Sending may succeed again after a `send` answered `WouldBlock`.
    Writable,
    /// The connection closed.
    Closed(CloseReason),
    /// The dial failed.
    Failed(FailReason),
}

/// A connection the host supplies, shaped like the WIT's `net.stream`.
///
/// `send` and `recv` never block. `poll_event` hands over the connection's
/// events in order, registering the waker when there is none; a spurious
/// `Readable` or `Writable` is allowed and read as "try again".
///
/// The trait asks nothing of threads; the tokio driver needs the carrier to
/// be `Send` to spawn it, a single-threaded driver does not.
pub trait HostCarrier {
    /// What the host knows about the connection now.
    fn info(&self) -> CarrierInfo;
    /// Stream framing: the bytes accepted, a prefix, at least one.
    /// Message framing: `Ok` means the whole message was taken. Nothing
    /// accepted is [`CarrierError::WouldBlock`], never `Ok(0)`.
    fn send(&mut self, data: &[u8]) -> Result<usize, CarrierError>;
    /// Stream framing: up to `max` bytes. Message framing: one whole
    /// message, or [`CarrierError::Invalid`] when it is larger than `max`.
    /// [`CarrierError::Closed`] once the peer closed and all was read.
    fn recv(&mut self, max: usize) -> Result<Vec<u8>, CarrierError>;
    /// Close the connection. No events are expected after it.
    fn close(&mut self);
    /// The next event, or `Pending` with the waker registered.
    fn poll_event(&mut self, cx: &mut Context<'_>) -> Poll<CarrierEvent>;
}

/// The node-side settings of a HostInterface. The rest (framing, MTU, kind)
/// comes from the carrier's [`CarrierInfo`] once it is connected.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HostInterfaceConfig {
    /// The interface's name in stats and logs.
    pub name: String,
    /// The propagation mode. Default [`InterfaceMode::Full`].
    pub mode: InterfaceMode,
    /// The interface access code.
    pub ifac: Option<IfacConfig>,
    /// The bitrate registered for announce capping; `None` takes the
    /// carrier's `bitrate_hint`, and without one announces are uncapped.
    pub bitrate: Option<u32>,
    /// Whether the carrier is connected already (an accepted connection),
    /// so no [`CarrierEvent::Connected`] is awaited. Default `false`.
    pub connected: bool,
    /// Frames buffered in each direction.
    pub buffer_size: usize,
}

impl HostInterfaceConfig {
    /// A config with the defaults above.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mode: InterfaceMode::Full,
            ifac: None,
            bitrate: None,
            connected: false,
            buffer_size: EXTERNAL_DEFAULT_BUFFER_SIZE,
        }
    }
}

/// How a HostInterface's connection ended. The interface is detached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostInterfaceEnd {
    /// The carrier closed.
    Closed(CloseReason),
    /// The dial failed.
    Failed(FailReason),
    /// The carrier cannot carry Reticulum (its messages are too small), or
    /// the interface config was refused.
    Unusable(String),
    /// The host dropped the handle.
    Detached,
    /// The node removed the interface: it stopped, or `remove_interface`
    /// named it.
    Removed,
}

/// The hardware MTU a connection with this info gets, or why it is
/// unusable.
pub fn hw_mtu_for(info: &CarrierInfo) -> Result<u32, String> {
    let tcp = super::tcp::TCP_HW_MTU;
    match info.framing {
        Framing::Stream => Ok(tcp),
        Framing::Message => {
            let mtu = info.max_message.map_or(tcp, |m| m.min(tcp));
            if (mtu as usize) < leviculum_core::constants::MTU {
                Err(format!(
                    "the carrier's largest message ({mtu} bytes) cannot hold a {}-byte packet",
                    leviculum_core::constants::MTU
                ))
            } else {
                Ok(mtu)
            }
        }
    }
}

/// Bytes asked of one stream `recv`.
const STREAM_RECV_CHUNK: usize = 16 * 1024;
/// Room for the IFAC on a message carrier that names no `max_message`.
const IFAC_SLACK: usize = leviculum_core::constants::ED25519_SIGNATURE_SIZE;
/// Outbound stream bytes held before the interface stops taking frames.
const STREAM_TX_HIGH_WATER: usize = 64 * 1024;
/// Outbound messages held before the interface stops taking frames.
const MESSAGE_TX_HIGH_WATER: usize = 64;

/// What woke the run loop.
enum Wake {
    Stop,
    Event(CarrierEvent),
    Frame(OutboundFrame),
    Removed,
}

/// Bytes or messages waiting for the carrier.
enum TxQueue {
    Stream {
        buf: Vec<u8>,
        off: usize,
        scratch: Vec<u8>,
    },
    Message(VecDeque<Arc<[u8]>>),
}

impl TxQueue {
    fn has_room(&self) -> bool {
        match self {
            TxQueue::Stream { buf, off, .. } => buf.len() - off < STREAM_TX_HIGH_WATER,
            TxQueue::Message(q) => q.len() < MESSAGE_TX_HIGH_WATER,
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            TxQueue::Stream { buf, off, .. } => *off == buf.len(),
            TxQueue::Message(q) => q.is_empty(),
        }
    }

    fn push(&mut self, f: OutboundFrame) {
        match self {
            TxQueue::Stream { buf, off, scratch } => {
                if *off == buf.len() {
                    buf.clear();
                    *off = 0;
                }
                frame(&f.data, scratch);
                buf.extend_from_slice(scratch);
            }
            TxQueue::Message(q) => q.push_back(f.data),
        }
    }

    /// Send until done or `WouldBlock`; `Ok(true)` when the carrier blocked.
    fn flush<C: HostCarrier>(&mut self, carrier: &mut C, name: &str) -> Result<bool, CarrierError> {
        match self {
            TxQueue::Stream { buf, off, .. } => {
                while *off < buf.len() {
                    match carrier.send(&buf[*off..]) {
                        Ok(0) | Err(CarrierError::WouldBlock) => return Ok(true),
                        Ok(n) => *off += n.min(buf.len() - *off),
                        Err(e) => return Err(e),
                    }
                }
                Ok(false)
            }
            TxQueue::Message(q) => {
                while let Some(m) = q.front() {
                    match carrier.send(m) {
                        Ok(_) => {
                            q.pop_front();
                        }
                        Err(CarrierError::WouldBlock) => return Ok(true),
                        Err(CarrierError::Invalid) => {
                            tracing::debug!(
                                "host interface {name}: a {}-byte frame exceeds the carrier's \
                                 message size, dropped",
                                m.len()
                            );
                            q.pop_front();
                        }
                        Err(e) => return Err(e),
                    }
                }
                Ok(false)
            }
        }
    }
}

/// Close the carrier and say how it ended. The caller drops the attached
/// interface, which detaches it.
fn end<C: HostCarrier>(carrier: &mut C, end: HostInterfaceEnd) -> HostInterfaceEnd {
    carrier.close();
    end
}

/// Drive one connection: wait for it to connect, attach through `attach`,
/// then move frames until it closes, `stop` resolves or the node removes
/// the interface.
pub(crate) async fn run<C, A, S>(
    mut carrier: C,
    cfg: HostInterfaceConfig,
    attach: A,
    stop: S,
) -> HostInterfaceEnd
where
    C: HostCarrier,
    A: FnOnce(ExternalInterfaceConfig) -> Result<ExternalInterface, Error>,
    S: Future<Output = ()>,
{
    let mut stop = std::pin::pin!(stop);

    if !cfg.connected {
        loop {
            let ev = poll_fn(|cx| {
                if stop.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(None);
                }
                carrier.poll_event(cx).map(Some)
            })
            .await;
            match ev {
                None => return end(&mut carrier, HostInterfaceEnd::Detached),
                Some(CarrierEvent::Connected) => break,
                Some(CarrierEvent::Closed(r)) => {
                    return end(&mut carrier, HostInterfaceEnd::Closed(r))
                }
                Some(CarrierEvent::Failed(r)) => {
                    return end(&mut carrier, HostInterfaceEnd::Failed(r))
                }
                // Readiness before the connection is up: the first drain
                // after it covers whatever arrived.
                Some(CarrierEvent::Readable | CarrierEvent::Writable) => {}
            }
        }
    }

    let info = carrier.info();
    let hw_mtu = match hw_mtu_for(&info) {
        Ok(m) => m,
        Err(why) => {
            tracing::warn!("host interface {}: {why}", cfg.name);
            return end(&mut carrier, HostInterfaceEnd::Unusable(why));
        }
    };
    let name = cfg.name.clone();
    let mut ext = ExternalInterfaceConfig::new(cfg.name, info.carrier.interface_kind());
    ext.mode = cfg.mode;
    ext.hw_mtu = Some(hw_mtu);
    ext.bitrate = cfg.bitrate.or_else(|| {
        info.bitrate_hint
            .filter(|b| *b > 0)
            .map(|b| b.min(u64::from(u32::MAX)) as u32)
    });
    ext.ifac = cfg.ifac;
    ext.buffer_size = cfg.buffer_size;
    let iface = match attach(ext) {
        Ok(i) => i,
        Err(Error::NotRunning) => return end(&mut carrier, HostInterfaceEnd::Removed),
        Err(e) => return end(&mut carrier, HostInterfaceEnd::Unusable(e.to_string())),
    };
    tracing::info!(
        "host interface {name} up ({}): carrier={:?} framing={:?} hw_mtu={hw_mtu} peer={}",
        iface.id(),
        info.carrier,
        info.framing,
        info.peer.as_deref().unwrap_or("-"),
    );
    let (inbound, mut outbound, guard) = iface.into_parts();

    let mut deframer = match info.framing {
        Framing::Stream => Some(Deframer::with_max_frame(hw_mtu as usize)),
        Framing::Message => None,
    };
    let recv_max = match info.framing {
        Framing::Stream => STREAM_RECV_CHUNK,
        Framing::Message => info
            .max_message
            .map_or(hw_mtu as usize + IFAC_SLACK, |m| m as usize),
    };
    let mut tx = match info.framing {
        Framing::Stream => TxQueue::Stream {
            buf: Vec::new(),
            off: 0,
            scratch: Vec::new(),
        },
        Framing::Message => TxQueue::Message(VecDeque::new()),
    };
    // Edge-triggered: start by draining, since an edge may have passed
    // before the interface was listening, and start writable.
    let mut readable = true;
    let mut writable = true;
    // A close the carrier reported, held until what it left is read.
    let mut closing = None;

    let outcome = loop {
        if readable {
            let mut closed = None;
            loop {
                match carrier.recv(recv_max) {
                    Ok(bytes) => {
                        let frames: Vec<Vec<u8>> = match deframer.as_mut() {
                            Some(d) => d
                                .process(&bytes)
                                .into_iter()
                                .filter_map(|r| match r {
                                    DeframeResult::Frame(f) => Some(f),
                                    DeframeResult::Oversized => {
                                        tracing::trace!(
                                            "host interface {name}: frame exceeds HW_MTU, discarded"
                                        );
                                        None
                                    }
                                    _ => None,
                                })
                                .collect(),
                            None if bytes.is_empty() => Vec::new(),
                            None => vec![bytes],
                        };
                        for f in frames {
                            if inbound.deliver(f).await.is_err() {
                                closed = Some(HostInterfaceEnd::Removed);
                                break;
                            }
                        }
                        if closed.is_some() {
                            break;
                        }
                    }
                    Err(CarrierError::WouldBlock) => {
                        readable = false;
                        break;
                    }
                    Err(CarrierError::Closed) => {
                        closed = Some(HostInterfaceEnd::Closed(CloseReason::PeerClose));
                        break;
                    }
                    Err(e) => {
                        // An oversized message included: the native
                        // WebSocket interface closes on one too (1009).
                        tracing::debug!("host interface {name}: recv failed: {e:?}");
                        closed = Some(HostInterfaceEnd::Closed(CloseReason::Error));
                        break;
                    }
                }
            }
            if let Some(c) = closed {
                break c;
            }
        }
        if let Some(c) = closing.take() {
            break c;
        }

        if writable && !tx.is_empty() {
            match tx.flush(&mut carrier, &name) {
                Ok(blocked) => writable = !blocked,
                Err(CarrierError::Closed) => {
                    break HostInterfaceEnd::Closed(CloseReason::PeerClose)
                }
                Err(e) => {
                    tracing::debug!("host interface {name}: send failed: {e:?}");
                    break HostInterfaceEnd::Closed(CloseReason::Error);
                }
            }
        }

        let wake = poll_fn(|cx| {
            if stop.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Wake::Stop);
            }
            if let Poll::Ready(ev) = carrier.poll_event(cx) {
                return Poll::Ready(Wake::Event(ev));
            }
            if tx.has_room() {
                match outbound.poll_recv(cx) {
                    Poll::Ready(Some(f)) => return Poll::Ready(Wake::Frame(f)),
                    Poll::Ready(None) => return Poll::Ready(Wake::Removed),
                    Poll::Pending => {}
                }
            }
            Poll::Pending
        })
        .await;

        match wake {
            Wake::Stop => break HostInterfaceEnd::Detached,
            Wake::Removed => break HostInterfaceEnd::Removed,
            Wake::Event(CarrierEvent::Readable) => readable = true,
            Wake::Event(CarrierEvent::Writable) => writable = true,
            Wake::Event(CarrierEvent::Connected) => {}
            Wake::Event(CarrierEvent::Closed(r)) => {
                readable = true;
                closing = Some(HostInterfaceEnd::Closed(r));
            }
            Wake::Event(CarrierEvent::Failed(r)) => break HostInterfaceEnd::Failed(r),
            Wake::Frame(f) => {
                tx.push(f);
                // Take what else is waiting, so one flush carries it.
                while tx.has_room() {
                    match outbound.try_recv() {
                        Ok(f) => tx.push(f),
                        Err(_) => break,
                    }
                }
            }
        }
    };

    tracing::info!("host interface {name} down: {outcome:?}");
    guard.set_down();
    drop(guard);
    end(&mut carrier, outcome)
}
