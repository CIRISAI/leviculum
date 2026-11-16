//! Interfaces a host drives itself (+ciris): [`ReticulumNode::attach_interface`].
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use leviculum_core::ifac::IfacConfig;
use leviculum_core::traits::{InterfaceKind, InterfaceMode};
use leviculum_core::transport::InterfaceId;
use tokio::sync::{mpsc, oneshot};

use super::lifecycle::Running;
use super::ReticulumNode;
use crate::error::Error;
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
}
