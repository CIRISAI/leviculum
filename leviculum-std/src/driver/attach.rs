//! Interfaces a host drives itself (+ciris): [`ReticulumNode::attach_interface`]
//! and [`ReticulumNode::attach_host_interface`].
//!
//! A built-in interface owns its carrier: the TCP client dials, the RNode
//! interface talks to its radio. An attached interface leaves the carrier
//! to the host. The host pushes the frames it received into the node through
//! an [`ExternalInbound`], takes the frames the node wants sent from an
//! [`ExternalOutbound`], and says when the carrier is up or down through the
//! [`ExternalInterfaceGuard`]. Dropping the guard detaches the interface.
//!
//! The handle the event loop registers is the same `InterfaceHandle` every
//! built-in interface hands it, through the same channel, so an attached
//! interface takes the same registration path as a runtime-spawned TCP
//! client or RNode: it shows in [`interface_stats`](ReticulumNode::interface_stats),
//! its IFAC is applied, its mode and kind reach the transport (announce
//! rebroadcast, the `ByMedium` resource window), and the interface-up
//! re-announce runs on it. See `docs/src/concepts/attached-interfaces.md`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use leviculum_core::ifac::IfacConfig;
use leviculum_core::traits::{InterfaceKind, InterfaceMode};
use leviculum_core::transport::InterfaceId;
use tokio::sync::{mpsc, oneshot, watch};

use super::lifecycle::Running;
use super::ReticulumNode;
use crate::error::Error;
use crate::interfaces::host::{self, HostCarrier, HostInterfaceConfig, HostInterfaceEnd};
use crate::interfaces::{
    IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket, ReadySignal,
};

/// Frames buffered in each direction when the config names no other bound:
/// the TCP interface's default.
pub const EXTERNAL_DEFAULT_BUFFER_SIZE: usize = 256;

/// What an attached interface is, as the node registers it.
///
/// Built with [`new`](Self::new); the other fields are public so a host can
/// set what it knows.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ExternalInterfaceConfig {
    /// The interface's name in stats and logs.
    pub name: String,
    /// The medium. It reaches the transport, which groups status by it and
    /// picks the resource window by it (`WindowPolicy::ByMedium`).
    pub kind: InterfaceKind,
    /// The Reticulum propagation mode. Default [`InterfaceMode::Full`].
    pub mode: InterfaceMode,
    /// The hardware MTU the interface signals into link MTU negotiation, or
    /// `None` to signal nothing, which keeps links over it at the base MTU
    /// (500). Must be at least 500 when set.
    pub hw_mtu: Option<u32>,
    /// The bitrate, in bits per second, registered for announce capping the
    /// way a configured `bitrate` key is. `None` leaves announces uncapped,
    /// as on TCP.
    pub bitrate: Option<u32>,
    /// Whether the carrier is up at attach. Default `true`.
    pub up: bool,
    /// The interface access code, applied inbound and outbound like a
    /// configured interface's.
    pub ifac: Option<IfacConfig>,
    /// Whether announce ingress control applies. Default `false`, as on a
    /// runtime-spawned TCP client.
    pub ingress_control: bool,
    /// Frames buffered in each direction. Default
    /// [`EXTERNAL_DEFAULT_BUFFER_SIZE`].
    pub buffer_size: usize,
}

impl ExternalInterfaceConfig {
    /// A config with the defaults above.
    pub fn new(name: impl Into<String>, kind: InterfaceKind) -> Self {
        Self {
            name: name.into(),
            kind,
            mode: InterfaceMode::Full,
            hw_mtu: None,
            bitrate: None,
            up: true,
            ifac: None,
            ingress_control: false,
            buffer_size: EXTERNAL_DEFAULT_BUFFER_SIZE,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        if self.name.is_empty() {
            return Err(Error::Config("an attached interface needs a name".into()));
        }
        if let Some(mtu) = self.hw_mtu {
            if (mtu as usize) < leviculum_core::constants::MTU {
                return Err(Error::Config(format!(
                    "interface {}: hw_mtu {mtu} is below the Reticulum MTU ({})",
                    self.name,
                    leviculum_core::constants::MTU
                )));
            }
        }
        if self.buffer_size == 0 {
            return Err(Error::Config(format!(
                "interface {}: buffer_size must be at least 1",
                self.name
            )));
        }
        Ok(())
    }
}

/// An interface attached by [`ReticulumNode::attach_interface`]: the inbound
/// sender, the outbound receiver and the guard. Take them apart with
/// [`into_parts`](Self::into_parts) to hand each to its own task or thread.
pub struct ExternalInterface {
    inbound: ExternalInbound,
    outbound: ExternalOutbound,
    guard: ExternalInterfaceGuard,
}

impl ExternalInterface {
    /// The id the node assigned.
    pub fn id(&self) -> InterfaceId {
        self.guard.id
    }

    /// A sender for received frames; clone it freely.
    pub fn inbound(&self) -> &ExternalInbound {
        &self.inbound
    }

    /// The receiver of frames to send.
    pub fn outbound(&mut self) -> &mut ExternalOutbound {
        &mut self.outbound
    }

    /// The up/down signals and the detach guard.
    pub fn guard(&self) -> &ExternalInterfaceGuard {
        &self.guard
    }

    /// The three parts. The interface stays attached while the guard lives.
    pub fn into_parts(self) -> (ExternalInbound, ExternalOutbound, ExternalInterfaceGuard) {
        (self.inbound, self.outbound, self.guard)
    }
}

/// Why a frame was not delivered. The frame comes back.
#[derive(Debug)]
pub enum DeliverError {
    /// The inbound buffer is full; try again, or use
    /// [`ExternalInbound::deliver`] to wait.
    Full(Vec<u8>),
    /// The interface is detached or the node stopped.
    Detached(Vec<u8>),
}

impl std::fmt::Display for DeliverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeliverError::Full(_) => f.write_str("the interface's inbound buffer is full"),
            DeliverError::Detached(_) => f.write_str("the interface is detached"),
        }
    }
}

impl std::error::Error for DeliverError {}

/// Pushes frames the host received into the node. `Clone`, `Send` and
/// `Sync`: any thread may hold one.
///
/// A frame is one Reticulum packet as it came off the carrier, IFAC bytes
/// included when the interface has IFAC; the host has already removed its
/// own framing.
#[derive(Clone)]
pub struct ExternalInbound {
    tx: mpsc::Sender<IncomingPacket>,
    counters: Arc<InterfaceCounters>,
}

impl ExternalInbound {
    fn packet(&self, frame: Vec<u8>) -> IncomingPacket {
        IncomingPacket {
            data: frame,
            peer: None,
            radio: None,
        }
    }

    fn count(&self, len: usize) {
        self.counters
            .rx_bytes
            .fetch_add(len as u64, Ordering::Relaxed);
    }

    /// Deliver without waiting.
    pub fn try_deliver(&self, frame: Vec<u8>) -> Result<(), DeliverError> {
        let len = frame.len();
        match self.tx.try_send(self.packet(frame)) {
            Ok(()) => {
                self.count(len);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(p)) => Err(DeliverError::Full(p.data)),
            Err(mpsc::error::TrySendError::Closed(p)) => Err(DeliverError::Detached(p.data)),
        }
    }

    /// Deliver, waiting while the buffer is full. Runs on any executor.
    pub async fn deliver(&self, frame: Vec<u8>) -> Result<(), DeliverError> {
        let len = frame.len();
        match self.tx.send(self.packet(frame)).await {
            Ok(()) => {
                self.count(len);
                Ok(())
            }
            Err(mpsc::error::SendError(p)) => Err(DeliverError::Detached(p.data)),
        }
    }

    /// Deliver from a thread that is not running async code, blocking while
    /// the buffer is full. Panics if called from inside a tokio runtime, as
    /// tokio's own blocking calls do.
    pub fn blocking_deliver(&self, frame: Vec<u8>) -> Result<(), DeliverError> {
        let len = frame.len();
        match self.tx.blocking_send(self.packet(frame)) {
            Ok(()) => {
                self.count(len);
                Ok(())
            }
            Err(mpsc::error::SendError(p)) => Err(DeliverError::Detached(p.data)),
        }
    }

    /// Whether the interface is gone (detached, or the node stopped).
    pub fn is_detached(&self) -> bool {
        self.tx.is_closed()
    }
}

/// One frame the node wants sent: a whole Reticulum packet, IFAC applied.
/// The host adds its carrier's framing.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct OutboundFrame {
    /// The bytes. Shared with every other interface a broadcast went to.
    pub data: Arc<[u8]>,
    /// Link requests, proofs and channel data; a host with a queue of its
    /// own may send these first.
    pub high_priority: bool,
}

/// Why [`ExternalOutbound::try_recv`] returned nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRecvError {
    /// Nothing to send now.
    Empty,
    /// The interface is detached or the node stopped; nothing more comes.
    Detached,
}

/// The frames the node wants sent on this interface.
///
/// While the interface is down the node keeps handing it frames, as it does
/// a TCP client across a reconnect; they wait here, up to the buffer size,
/// and the node sees a full buffer as backpressure. A host may send them
/// once the carrier is back or drop them.
pub struct ExternalOutbound {
    rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
}

impl ExternalOutbound {
    fn take(&self, p: OutgoingPacket) -> OutboundFrame {
        self.counters
            .tx_bytes
            .fetch_add(p.data.len() as u64, Ordering::Relaxed);
        OutboundFrame {
            data: p.data,
            high_priority: p.high_priority,
        }
    }

    /// The next frame; `None` once detached. Runs on any executor.
    pub async fn recv(&mut self) -> Option<OutboundFrame> {
        let p = self.rx.recv().await?;
        Some(self.take(p))
    }

    /// Poll for the next frame; `Ready(None)` once detached.
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<OutboundFrame>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(p)) => Poll::Ready(Some(self.take(p))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    /// The next frame without waiting.
    pub fn try_recv(&mut self) -> Result<OutboundFrame, OutboundRecvError> {
        match self.rx.try_recv() {
            Ok(p) => Ok(self.take(p)),
            Err(mpsc::error::TryRecvError::Empty) => Err(OutboundRecvError::Empty),
            Err(mpsc::error::TryRecvError::Disconnected) => Err(OutboundRecvError::Detached),
        }
    }

    /// The next frame, from a thread that is not running async code. Panics
    /// if called from inside a tokio runtime.
    pub fn blocking_recv(&mut self) -> Option<OutboundFrame> {
        let p = self.rx.blocking_recv()?;
        Some(self.take(p))
    }
}

/// The carrier signals of an attached interface, and its lifetime: dropping
/// the guard (or calling [`detach`](Self::detach)) removes the interface from
/// the node, through the same path as
/// [`remove_interface`](ReticulumNode::remove_interface). `Send` and `Sync`.
pub struct ExternalInterfaceGuard {
    id: InterfaceId,
    counters: Arc<InterfaceCounters>,
    ready: Arc<ReadySignal>,
    reconnect_tx: mpsc::Sender<InterfaceId>,
    /// Whether the carrier has been up before; see [`set_up`](Self::set_up).
    been_up: AtomicBool,
    /// Dropped with the guard, which wakes the watcher that removes the
    /// interface.
    _detach: oneshot::Sender<()>,
}

impl ExternalInterfaceGuard {
    /// The id the node assigned.
    pub fn id(&self) -> InterfaceId {
        self.id
    }

    /// The carrier came up.
    ///
    /// Marks the interface online. Every up after the first runs the
    /// interface-up path, which re-announces this node's destinations on it,
    /// as a TCP client's reconnect does. The first up of an interface
    /// attached down does not: the announces of the attach are still waiting
    /// in the outbound buffer.
    pub fn set_up(&self) {
        let changed = !self.counters.is_online();
        self.counters.set_online(true);
        self.ready.signal_ready();
        if changed && self.been_up.swap(true, Ordering::AcqRel) {
            let _ = self.reconnect_tx.try_send(self.id);
        }
    }

    /// The carrier went down. The interface stays attached and keeps its
    /// paths, as a TCP client's does while it redials.
    pub fn set_down(&self) {
        self.counters.set_online(false);
    }

    /// Whether the carrier is up, as last signalled.
    pub fn is_up(&self) -> bool {
        self.counters.is_online()
    }

    /// Detach now; the same as dropping the guard.
    pub fn detach(self) {}
}

impl Drop for ExternalInterfaceGuard {
    fn drop(&mut self) {
        self.counters.set_online(false);
    }
}

/// Register an external interface with the run `running` belongs to.
pub(crate) fn attach(
    running: &Running,
    cfg: ExternalInterfaceConfig,
) -> Result<ExternalInterface, Error> {
    cfg.validate()?;
    let up = cfg.up;
    let id = InterfaceId(running.iface_id_counter.fetch_add(1, Ordering::Relaxed));
    let (incoming_tx, incoming_rx) = mpsc::channel(cfg.buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(cfg.buffer_size);
    let counters = Arc::new(InterfaceCounters::new());
    counters.set_online(cfg.up);
    let ready = if cfg.up {
        ReadySignal::ready_immediate()
    } else {
        ReadySignal::new()
    };
    let handle = InterfaceHandle {
        next_tag: None,
        info: InterfaceInfo {
            transit: true,
            id,
            name: cfg.name,
            hw_mtu: cfg.hw_mtu,
            is_local_client: false,
            bitrate: cfg.bitrate,
            announce_cap_bitrate: cfg.bitrate,
            tx_jitter_max_ms: None,
            tx_hold_spread_max_ms: None,
            acquisition: None,
            frame_turnaround_ms: None,
            ifac: cfg.ifac,
            mode: cfg.mode,
            kind: cfg.kind,
            ingress_control: Some(cfg.ingress_control),
            path_choice: leviculum_core::transport::PathChoice::Hops,
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters: Arc::clone(&counters),
        credit: None,
        ready: Arc::clone(&ready),
        teardown: None,
    };

    // The watcher removes the interface when the guard goes. A removal that
    // overtakes the registration is held by the loop (`pending_removals`).
    let (detach_tx, detach_rx) = oneshot::channel::<()>();
    let remove_tx = running.remove_iface_tx.clone();
    running.runtime.spawn(async move {
        let _ = detach_rx.await;
        let _ = remove_tx.send(id).await;
    });

    running
        .new_iface_tx
        .try_send(handle)
        .map_err(|_| Error::NotRunning)?;

    Ok(ExternalInterface {
        inbound: ExternalInbound {
            tx: incoming_tx,
            counters: Arc::clone(&counters),
        },
        outbound: ExternalOutbound {
            rx: outgoing_rx,
            counters: Arc::clone(&counters),
        },
        guard: ExternalInterfaceGuard {
            id,
            counters,
            ready,
            reconnect_tx: running.reconnect_tx.clone(),
            // An interface attached down skips the re-announce on its
            // first up; one attached up has had that up.
            been_up: AtomicBool::new(up),
            _detach: detach_tx,
        },
    })
}

impl ReticulumNode {
    /// Attach an interface whose carrier the host drives.
    ///
    /// The node assigns the [`InterfaceId`] and registers the interface with
    /// the running event loop like a built-in one. The host moves frames with
    /// the returned [`ExternalInterface`]; dropping its guard detaches it.
    /// [`Error::NotRunning`] before [`start`](Self::start), [`Error::Config`]
    /// for an invalid config. Takes the run lock, not the core lock.
    pub fn attach_interface(
        &self,
        cfg: ExternalInterfaceConfig,
    ) -> Result<ExternalInterface, Error> {
        attach(&self.running()?, cfg)
    }

    /// Attach a [`HostInterface`](crate::interfaces::host) over a carrier the
    /// host supplies: a framed Reticulum interface (one packet per message,
    /// or the TCP HDLC byte stream) driven by the carrier's edge-triggered
    /// readiness, on the node's runtime.
    ///
    /// The interface registers when the carrier is connected (at once when
    /// `cfg.connected` says it already is) and detaches when the carrier
    /// closes or fails, or when the returned handle is dropped. It never
    /// redials: reconnecting is the host's policy, and a new connection is a
    /// new interface, as an accepted TCP connection is. [`Error::NotRunning`]
    /// before [`start`](Self::start). Takes the run lock, not the core lock.
    pub fn attach_host_interface<C>(
        &self,
        carrier: C,
        cfg: HostInterfaceConfig,
    ) -> Result<HostInterfaceHandle, Error>
    where
        C: HostCarrier + Send + 'static,
    {
        let running = self.running()?;
        let runtime = running.runtime.clone();
        let (state_tx, state_rx) = watch::channel(HostInterfaceState::Connecting);
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let attach_tx = state_tx.clone();
        let attacher = move |cfg: ExternalInterfaceConfig| {
            let iface = attach(&running, cfg)?;
            let _ = attach_tx.send(HostInterfaceState::Up(iface.id()));
            Ok(iface)
        };
        runtime.spawn(async move {
            let end = host::run(carrier, cfg, attacher, StopSignal(stop_rx)).await;
            let _ = state_tx.send(HostInterfaceState::Ended(end));
        });
        Ok(HostInterfaceHandle {
            state: state_rx,
            _stop: stop_tx,
        })
    }
}

/// Resolves when the handle drops.
struct StopSignal(oneshot::Receiver<()>);

impl Future for StopSignal {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.0).poll(cx).map(|_| ())
    }
}

/// Where a host interface is in its one connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostInterfaceState {
    /// Waiting for the carrier to connect.
    Connecting,
    /// Registered with the node under this id.
    Up(InterfaceId),
    /// Over, and why. The interface is detached.
    Ended(HostInterfaceEnd),
}

/// The host's handle to an interface from
/// [`ReticulumNode::attach_host_interface`]. Dropping it detaches the
/// interface and closes the carrier.
pub struct HostInterfaceHandle {
    state: watch::Receiver<HostInterfaceState>,
    _stop: oneshot::Sender<()>,
}

impl HostInterfaceHandle {
    /// Where the interface is now.
    pub fn state(&self) -> HostInterfaceState {
        self.state.borrow().clone()
    }

    /// The node's id for the interface, once it registered.
    pub fn interface_id(&self) -> Option<InterfaceId> {
        match &*self.state.borrow() {
            HostInterfaceState::Up(id) => Some(*id),
            _ => None,
        }
    }

    /// Wait until the interface registers or ends; the state it reached.
    pub async fn connected(&mut self) -> HostInterfaceState {
        let state = self
            .state
            .wait_for(|s| !matches!(s, HostInterfaceState::Connecting))
            .await
            .map(|s| s.clone());
        state.unwrap_or(HostInterfaceState::Ended(HostInterfaceEnd::Removed))
    }

    /// Wait until the connection is over: the host's cue to redial if its
    /// policy says so.
    pub async fn ended(&mut self) -> HostInterfaceEnd {
        let state = self
            .state
            .wait_for(|s| matches!(s, HostInterfaceState::Ended(_)))
            .await
            .map(|s| s.clone());
        match state {
            Ok(HostInterfaceState::Ended(end)) => end,
            _ => HostInterfaceEnd::Removed,
        }
    }

    /// Detach now; the same as dropping the handle.
    pub fn detach(self) {}
}

#[cfg(test)]
mod tests {
    //! The HostInterface run loop between two in-memory carriers, without a
    //! node: every frame each side is handed comes out the other, in order,
    //! under strictly edge-triggered readiness.

    use super::*;
    use crate::interfaces::host::{CarrierKind, Framing};
    use crate::test_support::mem_carrier::{mem_carrier_pair, MemCarrierConfig};
    use std::time::Duration;

    /// An interface with its node-side ends: what it delivers, and where
    /// the node's frames go in.
    fn test_interface(
        buffer: usize,
    ) -> (
        ExternalInterface,
        mpsc::Receiver<IncomingPacket>,
        mpsc::Sender<OutgoingPacket>,
    ) {
        let (incoming_tx, incoming_rx) = mpsc::channel(buffer);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(buffer);
        let (reconnect_tx, _) = mpsc::channel(1);
        let (detach_tx, _) = oneshot::channel();
        let counters = Arc::new(InterfaceCounters::new());
        let iface = ExternalInterface {
            inbound: ExternalInbound {
                tx: incoming_tx,
                counters: Arc::clone(&counters),
            },
            outbound: ExternalOutbound {
                rx: outgoing_rx,
                counters: Arc::clone(&counters),
            },
            guard: ExternalInterfaceGuard {
                id: InterfaceId(7),
                counters,
                ready: ReadySignal::ready_immediate(),
                reconnect_tx,
                been_up: AtomicBool::new(true),
                _detach: detach_tx,
            },
        };
        (iface, incoming_rx, outgoing_tx)
    }

    /// A frame of `len` bytes that names its index and contains the HDLC
    /// flag and escape octets, so the stream path escapes on every frame.
    fn numbered(i: usize, len: usize) -> Vec<u8> {
        let mut f = vec![0x7E, 0x7D];
        f.extend_from_slice(&(i as u32).to_be_bytes());
        while f.len() < len {
            f.push((f.len() * 31 + i) as u8);
        }
        f
    }

    async fn every_frame_crosses(
        cfg: MemCarrierConfig,
        frames: usize,
    ) -> crate::test_support::mem_carrier::MemCarrierStats {
        let (a, b) = mem_carrier_pair(cfg);
        let observer = a.observer();
        let (ia, rx_a, tx_a) = test_interface(8);
        let (ib, rx_b, tx_b) = test_interface(8);
        let mut slot_a = Some(ia);
        let mut slot_b = Some(ib);
        let (stop_a, stop_a_rx) = oneshot::channel::<()>();
        let (stop_b, stop_b_rx) = oneshot::channel::<()>();
        let run_a = tokio::spawn(host::run(
            a,
            HostInterfaceConfig::new("a"),
            move |_| Ok(slot_a.take().unwrap()),
            StopSignal(stop_a_rx),
        ));
        let run_b = tokio::spawn(host::run(
            b,
            HostInterfaceConfig::new("b"),
            move |_| Ok(slot_b.take().unwrap()),
            StopSignal(stop_b_rx),
        ));

        let send = move |tx: mpsc::Sender<OutgoingPacket>, salt: usize| async move {
            for i in 0..frames {
                let data: Arc<[u8]> = numbered(i, 20 + (i * 7 + salt) % 480).into();
                tx.send(OutgoingPacket {
                    data,
                    ..Default::default()
                })
                .await
                .unwrap();
            }
        };
        // The node keeps its sending end for as long as the interface is
        // registered; closing it would read as a removal.
        let (keep_a, keep_b) = (tx_a.clone(), tx_b.clone());
        let sends = tokio::spawn(async move {
            tokio::join!(send(tx_a, 0), send(tx_b, 3));
        });
        let collect = move |mut rx: mpsc::Receiver<IncomingPacket>, salt: usize| async move {
            for i in 0..frames {
                let Some(p) = rx.recv().await else {
                    panic!("frame {i} missing")
                };
                assert_eq!(p.data, numbered(i, 20 + (i * 7 + salt) % 480), "frame {i}");
            }
            rx
        };
        // What a sent lands at b, what b sent lands at a.
        let (mut rx_b, mut rx_a) = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(collect(rx_b, 0), collect(rx_a, 3))
        })
        .await
        .expect("every frame crosses without a stall");
        sends.await.unwrap();
        assert!(rx_a.try_recv().is_err() && rx_b.try_recv().is_err());

        let stats = observer.stats();
        drop(stop_a);
        let _keep = (keep_a, keep_b);
        assert_eq!(run_a.await.unwrap(), HostInterfaceEnd::Detached);
        assert!(observer.is_closed(), "detach closes the carrier");
        // The other side hears the close.
        assert_eq!(
            run_b.await.unwrap(),
            HostInterfaceEnd::Closed(crate::interfaces::host::CloseReason::PeerClose)
        );
        drop(stop_b);
        assert!(stats.recv_would_blocks > 0 && stats.readable_events > 0);
        // Every Readable after the first was earned by a WouldBlock.
        assert!(stats.readable_events <= stats.recv_would_blocks + 2);
        if stats.send_would_blocks > 0 {
            assert!(stats.writable_events > 0);
        }
        stats
    }

    #[tokio::test]
    async fn message_framing_carries_every_frame_in_order() {
        let mut cfg = MemCarrierConfig::new(CarrierKind::Ws, Framing::Message);
        cfg.capacity = 3;
        let stats = every_frame_crosses(cfg, 400).await;
        assert!(stats.send_would_blocks > 0, "{stats:?}");
    }

    #[tokio::test]
    async fn stream_framing_survives_short_writes_and_would_block() {
        let mut cfg = MemCarrierConfig::new(CarrierKind::Tcp, Framing::Stream);
        cfg.capacity = 64;
        cfg.max_chunk = 7;
        let stats = every_frame_crosses(cfg, 300).await;
        assert!(
            stats.short_writes > 0 && stats.send_would_blocks > 0,
            "{stats:?}"
        );
    }

    #[tokio::test]
    async fn a_carrier_whose_messages_cannot_hold_a_packet_is_unusable() {
        let mut cfg = MemCarrierConfig::new(CarrierKind::WebTransport, Framing::Message);
        cfg.max_message = Some(400);
        let (a, _b) = mem_carrier_pair(cfg);
        let observer = a.observer();
        let (_stop, stop_rx) = oneshot::channel::<()>();
        let end = host::run(
            a,
            HostInterfaceConfig::new("small"),
            |_| -> Result<ExternalInterface, Error> { panic!("must not attach") },
            StopSignal(stop_rx),
        )
        .await;
        assert!(matches!(end, HostInterfaceEnd::Unusable(_)), "{end:?}");
        assert!(observer.is_closed());
    }
}
