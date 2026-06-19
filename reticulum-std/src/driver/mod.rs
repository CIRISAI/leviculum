//! Sans-I/O driver for Reticulum
//!
//! This module provides `ReticulumNode`, the async I/O driver that bridges the
//! pure state machine (`NodeCore` from reticulum-core) with actual network
//! interfaces. It owns the interfaces and dispatches `Action` values.
//!
//! # Architecture (Sans-I/O)
//!
//! `NodeCore` from reticulum-core is a pure state machine that never performs I/O
//! directly. Instead, it returns `Action` values (SendPacket, Broadcast) that this
//! driver dispatches to the actual network interfaces.
//!
//! The event loop awaits interface readability via `select!`:
//! 1. Wakes immediately when any interface has data (no polling delay)
//! 2. Feeds packets to `NodeCore::handle_packet()` → gets `TickOutput`
//! 3. Dispatches `TickOutput` from external callers (connect, send, close)
//! 4. Wakes on timer deadline for periodic maintenance
//! 5. Dispatches `Action`s from `TickOutput` to interfaces
//! 6. Forwards `NodeEvent`s to the application
//!
//! # Example
//!
//! ```no_run
//! use reticulum_std::driver::ReticulumNodeBuilder;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create a node with a TCP interface
//!     let mut node = ReticulumNodeBuilder::new()
//!         .add_tcp_client("127.0.0.1:4242".parse()?)
//!         .build()
//!         .await?;
//!
//!     // Start the node
//!     node.start().await?;
//!
//!     // Take event receiver to handle events
//!     let mut events = node.take_event_receiver().unwrap();
//!
//!     // Process events
//!     while let Some(event) = events.recv().await {
//!         println!("Event: {:?}", event);
//!     }
//!
//!     Ok(())
//! }
//! ```

mod builder;
mod sender;
mod stream;

pub use builder::ReticulumNodeBuilder;
pub use sender::PacketSender;
pub use stream::LinkHandle;

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use tokio::sync::mpsc::{
    self,
    error::{TryRecvError, TrySendError},
};
use tokio::sync::watch;

use crate::interfaces::IncomingPacket;
use reticulum_core::constants::TRUNCATED_HASHBYTES;
use reticulum_core::link::LinkId;
use reticulum_core::node::{EventClass, NodeCore, NodeEvent};
use reticulum_core::traits::{InterfaceError, Storage as StorageTrait};
use reticulum_core::transport::{InterfaceId, TickOutput};
use reticulum_core::{Destination, DestinationHash};

use crate::clock::SystemClock;
use crate::config::InterfaceConfig;
use crate::error::Error;
use crate::interfaces::auto_interface::orchestrator::spawn_auto_interface;
use crate::interfaces::auto_interface::AutoInterfaceConfig;
use crate::interfaces::tcp::{
    spawn_tcp_client_with_reconnect, spawn_tcp_server, TcpClientConfig,
    DEFAULT_TCP_CONNECT_TIMEOUT, TCP_DEFAULT_BUFFER_SIZE,
};
use crate::interfaces::udp::spawn_udp_interface;
use crate::interfaces::{
    InterfaceHandle, InterfaceOnlineMap, InterfaceRegistry, InterfaceStatsMap,
};
use crate::storage::Storage;

/// Type alias for the concrete NodeCore used by std platforms
pub(crate) type StdNodeCore = NodeCore<rand_core::OsRng, SystemClock, Storage>;

/// Capacity of the internal action-dispatch channel that carries
/// `TickOutput`s produced outside the event loop (connect, send_on_link,
/// close_link, announce) into it. Each such call produces exactly one
/// `TickOutput` and the loop drains them every iteration, so this only
/// backs up if the loop is already blocked.
const ACTION_DISPATCH_CAPACITY: usize = 256;

/// Maximum packets per interface in the retry queue.
/// Sized to absorb announce-burst fan-out from transit peers; observed
/// peak >500 packets in a single event-loop tick on transit-active lnsd.
/// When full, oldest is dropped.
const RETRY_QUEUE_CAP: usize = 1024;

/// Depth at which `push_retry_with_warn` emits a one-shot tracing::warn
/// to flag that first-order backpressure may be mis-tuned. Held at
/// 12.5 % of `RETRY_QUEUE_CAP` so the warn fires well before drops do.
const RETRY_QUEUE_DEPTH_WARN: usize = 128;

/// Sender half of the split control/data node-event channels (Codeberg #71).
///
/// Lives in the event loop only (single owner, so `&mut self` is enough for
/// the dropped-counter — no atomics needed). [`emit`](EventSink::emit)
/// classifies each [`NodeEvent`] with [`NodeEvent::event_class`] and routes
/// it:
///
/// * **Control** plane — lossless by default. When the bounded control
///   channel is full the event is dropped but counted, and the loss is made
///   visible by delivering one [`NodeEvent::ControlPlaneOverflow`] as soon as
///   the channel has room (see [`flush_overflow`](EventSink::flush_overflow)).
///   The marker itself is only enqueued when there is room, so it is never
///   lost.
/// * **Data** plane — droppable. A full data channel drops silently; that is
///   the intended backpressure.
struct EventSink {
    /// Lossless-by-default control plane.
    control_tx: mpsc::Sender<NodeEvent>,
    /// Droppable data plane (backpressure).
    data_tx: mpsc::Sender<NodeEvent>,
    /// Configured control-channel capacity, for the overflow warn log.
    control_capacity: usize,
    /// Control events dropped since the last `ControlPlaneOverflow` marker
    /// was delivered. Surfaced (and reset) by `flush_overflow` once the
    /// control channel has room.
    control_dropped: u64,
}

impl EventSink {
    /// Route one event to the control or data plane by its class.
    fn emit(&mut self, event: NodeEvent) {
        match event.event_class() {
            EventClass::Control => self.emit_control(event),
            EventClass::Data => self.emit_data(event),
        }
    }

    /// Deliver a control-plane event losslessly, or count it as dropped and
    /// surface the loss via `ControlPlaneOverflow`.
    ///
    /// The real event is tried first so a freed slot is never starved by the
    /// overflow marker; only when the event lands (proving the channel has
    /// room) do we try to flush any pending overflow marker behind it.
    fn emit_control(&mut self, event: NodeEvent) {
        match self.control_tx.try_send(event) {
            Ok(()) => self.flush_overflow(),
            Err(TrySendError::Full(ev)) => {
                self.control_dropped += 1;
                tracing::warn!(
                    event = "EVENT_CHANNEL_FULL",
                    queue_capacity = self.control_capacity,
                    dropped_event_type = ev.variant_name(),
                    pending_dropped = self.control_dropped,
                    "control event dropped, will surface via ControlPlaneOverflow"
                );
            }
            Err(TrySendError::Closed(ev)) => {
                tracing::warn!(
                    event = "EVENT_CHANNEL_CLOSED",
                    dropped_event_type = ev.variant_name(),
                    "control channel closed (receiver dropped), dropping event"
                );
            }
        }
    }

    /// If control events were previously dropped, try to deliver one
    /// `ControlPlaneOverflow` marker reporting the count. It is only enqueued
    /// when the channel has room, so the marker is never itself dropped; the
    /// counter is reset only on a successful send.
    fn flush_overflow(&mut self) {
        if self.control_dropped == 0 {
            return;
        }
        let dropped_count = self.control_dropped;
        match self
            .control_tx
            .try_send(NodeEvent::ControlPlaneOverflow { dropped_count })
        {
            Ok(()) => {
                tracing::warn!(
                    event = "CONTROL_PLANE_OVERFLOW",
                    dropped_count,
                    "surfaced dropped control events via ControlPlaneOverflow"
                );
                self.control_dropped = 0;
            }
            // Still full: keep the count and try again on the next emit.
            Err(TrySendError::Full(_)) => {}
            // Receiver gone: nothing can observe the marker anyway.
            Err(TrySendError::Closed(_)) => self.control_dropped = 0,
        }
    }

    /// Deliver a data-plane event, dropping silently when full (backpressure).
    fn emit_data(&mut self, event: NodeEvent) {
        match self.data_tx.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(ev)) => {
                // Silent by design: data-plane drops are normal backpressure.
                tracing::trace!(
                    dropped_event_type = ev.variant_name(),
                    "data event dropped (backpressure)"
                );
            }
            Err(TrySendError::Closed(ev)) => {
                tracing::warn!(
                    event = "EVENT_CHANNEL_CLOSED",
                    dropped_event_type = ev.variant_name(),
                    "data channel closed (receiver dropped), dropping event"
                );
            }
        }
    }
}

/// Receiver half handed to the application by
/// [`ReticulumNode::take_event_receiver`] (Codeberg #71).
///
/// Merges the split control/data channels into a single stream, draining the
/// control plane with strict priority over the data plane so a flood of data
/// events can never starve discovery- or lifecycle-critical control events.
pub struct EventReceiver {
    /// Lossless-by-default control plane (drained first).
    control: mpsc::Receiver<NodeEvent>,
    /// Droppable data plane.
    data: mpsc::Receiver<NodeEvent>,
}

impl EventReceiver {
    /// Receive the next event, control plane first.
    ///
    /// Returns `None` only once both planes are closed and drained. Drop-safe
    /// for use in `tokio::select!`: a buffered control event is returned
    /// synchronously, otherwise both channels are awaited with the control
    /// plane biased, and `tokio::sync::mpsc::Receiver::recv` is cancel-safe.
    pub async fn recv(&mut self) -> Option<NodeEvent> {
        // Strict priority: return any already-buffered control event first.
        match self.control.try_recv() {
            Ok(ev) => return Some(ev),
            Err(TryRecvError::Empty) => {}
            // Control closed and drained: serve the data plane to completion.
            Err(TryRecvError::Disconnected) => return self.data.recv().await,
        }
        // Nothing buffered on control; wait on both, control biased so a
        // control event that races in still wins.
        tokio::select! {
            biased;
            ev = self.control.recv() => match ev {
                Some(e) => Some(e),
                None => self.data.recv().await, // control closed
            },
            ev = self.data.recv() => match ev {
                Some(e) => Some(e),
                None => self.control.recv().await, // data closed
            },
        }
    }

    /// Non-blocking receive, control plane first. Mirrors
    /// [`tokio::sync::mpsc::Receiver::try_recv`].
    pub fn try_recv(&mut self) -> Result<NodeEvent, TryRecvError> {
        match self.control.try_recv() {
            Ok(ev) => Ok(ev),
            Err(TryRecvError::Empty) => self.data.try_recv(),
            // Control closed: fall back to whatever the data plane reports.
            Err(TryRecvError::Disconnected) => self.data.try_recv(),
        }
    }
}

/// Build an IfacConfig from interface configuration, if IFAC params are present.
fn build_ifac_config(config: &InterfaceConfig) -> Option<reticulum_core::ifac::IfacConfig> {
    if config.networkname.is_none() && config.passphrase.is_none() {
        return None;
    }
    let default_size = match config.interface_type.as_str() {
        "RNodeInterface" => reticulum_core::constants::IFAC_DEFAULT_SIZE_SERIAL,
        _ => reticulum_core::constants::IFAC_DEFAULT_SIZE_NETWORK,
    };
    let size = config.ifac_size.unwrap_or(default_size);
    match reticulum_core::ifac::IfacConfig::new(
        config.networkname.as_deref(),
        config.passphrase.as_deref(),
        size,
    ) {
        Ok(ifac) => Some(ifac),
        Err(e) => {
            tracing::warn!("Failed to create IFAC config: {:?}", e);
            None
        }
    }
}

/// Channels consumed by the event loop.
struct EventLoopChannels {
    /// Split control/data application event sink. `None` when the node was
    /// built with `without_events()`; in that case `dispatch_output` skips
    /// event-forwarding and `output.events` falls out of scope, exactly
    /// like the `reticulum-nrf` daemon binaries.
    event_sink: Option<EventSink>,
    action_dispatch_rx: mpsc::Receiver<TickOutput>,
    new_interface_rx: mpsc::Receiver<InterfaceHandle>,
    reconnect_rx: mpsc::Receiver<InterfaceId>,
    shutdown: watch::Receiver<bool>,
}

/// Event received from any interface
enum RecvEvent {
    /// A complete packet from an interface
    Packet(InterfaceId, IncomingPacket),
    /// An interface disconnected (its incoming channel closed)
    Disconnected(InterfaceId),
}

/// Reason a `wait_for_interface_ready` call did not return `Ok(())`.
///
/// Returned by [`ReticulumNode::wait_for_interface_ready`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterfaceReadyError {
    /// The interface index did not match any registered interface.
    Unknown { idx: usize },
    /// The readiness deadline elapsed before the interface signalled
    /// ready.
    TimedOut { idx: usize },
    /// `start()` has not been called yet, so no interfaces are
    /// registered.
    NotStarted,
}

impl std::fmt::Display for InterfaceReadyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InterfaceReadyError::Unknown { idx } => write!(f, "unknown interface index {idx}"),
            InterfaceReadyError::TimedOut { idx } => {
                write!(f, "interface {idx} did not become ready in time")
            }
            InterfaceReadyError::NotStarted => write!(f, "node not started"),
        }
    }
}

impl std::error::Error for InterfaceReadyError {}

/// Per-interface readiness state reported by
/// [`ReticulumNode::wait_for_interfaces_ready`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadyState {
    /// The interface did not signal ready before the shared deadline.
    TimedOut,
    /// `start()` had not been called when the wait began.
    NotStarted,
}

/// High-level async Reticulum node
///
/// `ReticulumNode` provides an async API for interacting with the Reticulum
/// network. It manages the internal event loop and provides methods for sending
/// data, establishing links, and handling incoming messages.
pub struct ReticulumNode {
    /// Handle to the core node
    inner: Arc<Mutex<StdNodeCore>>,
    /// Interface configurations
    interfaces: Vec<InterfaceConfig>,
    /// Control-plane event sender, cloned into the runner's `EventSink`.
    /// `None` when built with `without_events()` (daemon-mode); the loop
    /// then never forwards `NodeEvent`s. Kept here so the channel stays open.
    control_tx: Option<mpsc::Sender<NodeEvent>>,
    /// Data-plane event sender, cloned into the runner's `EventSink`.
    data_tx: Option<mpsc::Sender<NodeEvent>>,
    /// Capacity of the control channel, needed to build the runner's
    /// `EventSink` (used for the overflow warn log).
    control_channel_capacity: usize,
    /// Merged event receiver for consuming events. `None` either because the
    /// node was built with `without_events()`, or because
    /// `take_event_receiver()` already handed it out.
    event_rx: Option<EventReceiver>,
    /// Shutdown sender
    shutdown_tx: Option<watch::Sender<bool>>,
    /// Runner task handle
    runner_handle: Option<tokio::task::JoinHandle<()>>,
    /// Channel for dispatching TickOutput from outside the event loop
    /// (used by connect, send_on_link, close_link, announce)
    action_dispatch_tx: mpsc::Sender<TickOutput>,
    /// Fault injection: corrupt ~1 byte per N bytes on TCP write
    corrupt_every: Option<u64>,
    /// Interval between periodic storage flushes (seconds).
    /// Crash protection only, normal shutdown calls flush() via signal handler.
    /// Lost data from a crash is recovered via fresh announces.
    flush_interval_secs: u64,
    /// Peer count from AutoInterface orchestrator (if configured)
    auto_peer_count_rx: Option<watch::Receiver<usize>>,
    /// Shared instance name (if enabled). When Some, the daemon listens on
    /// abstract Unix socket `\0rns/{name}` for local IPC clients.
    share_instance_name: Option<String>,
    /// Shared instance to connect to as client. When Some, the node connects
    /// to abstract Unix socket `\0rns/{name}` instead of starting its own
    /// interfaces from config.
    connect_instance_name: Option<String>,
    /// Time when the node was created (for RPC uptime reporting).
    start_time: std::time::Instant,
    /// Shared interface I/O counters, populated by the event loop.
    iface_stats_map: InterfaceStatsMap,
    /// Per-interface online status, keyed by interface index. Inserted
    /// `true` on registration, removed on disconnect. Read by the RPC
    /// handler so the `interface_stats.status` field reflects the real
    /// `is_online()` of each interface (Codeberg #56).
    iface_online_map: InterfaceOnlineMap,
    /// Per-interface readiness signals, keyed by interface index.
    /// Populated by `start()` once interfaces are spawned.  Read by
    /// [`wait_for_interface_ready`](Self::wait_for_interface_ready)
    /// and [`wait_for_interfaces_ready`](Self::wait_for_interfaces_ready).
    iface_ready_map: crate::interfaces::InterfaceReadyMap,
    /// Dedicated, time-enabled runtime that hosts the event loop and every
    /// interface task. Owning our own runtime means the node works regardless
    /// of how the *embedding* application built its runtime — e.g. a PyO3 host
    /// that constructed a current-thread runtime without `enable_time()`, which
    /// previously panicked the timer-driven event loop (`sleep_until`) and the
    /// interface timers. Torn down via `shutdown_background()` in `Drop` so the
    /// runtime is never dropped blocking inside a host async context.
    runtime: Option<tokio::runtime::Runtime>,
}

impl Drop for ReticulumNode {
    fn drop(&mut self) {
        // Tear the node's runtime down without blocking. Dropping a tokio
        // `Runtime` directly performs a blocking shutdown, which panics if the
        // drop happens inside another runtime's async context (e.g. the PyO3
        // host dropping the node from one of its own tasks).
        // `shutdown_background` aborts the event loop + interface tasks and
        // returns immediately.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl ReticulumNode {
    /// Create a new ReticulumNode (internal use - use ReticulumNodeBuilder)
    pub(crate) fn new(
        core: StdNodeCore,
        interfaces: Vec<InterfaceConfig>,
        corrupt_every: Option<u64>,
        events_enabled: bool,
        flush_interval_secs: u64,
        control_channel_capacity: usize,
        data_channel_capacity: usize,
    ) -> Self {
        // When events are disabled (daemon-mode), no channels are constructed
        // at all — neither senders nor receiver. The event loop's
        // `dispatch_output` then skips its event-forwarding branch and
        // `output.events` falls out of scope unread, mirroring the NRF
        // daemon binaries.
        //
        // Codeberg #71: the single bounded channel is split into a lossless
        // control plane and a droppable data plane, merged back for the
        // application by `EventReceiver`.
        let (control_tx, data_tx, event_rx) = if events_enabled {
            let (control_tx, control_rx) = mpsc::channel(control_channel_capacity);
            let (data_tx, data_rx) = mpsc::channel(data_channel_capacity);
            (
                Some(control_tx),
                Some(data_tx),
                Some(EventReceiver {
                    control: control_rx,
                    data: data_rx,
                }),
            )
        } else {
            (None, None, None)
        };
        // Create dummy channel; real one is created in start()
        let (action_dispatch_tx, _) = mpsc::channel(1);

        Self {
            inner: Arc::new(Mutex::new(core)),
            interfaces,
            control_tx,
            data_tx,
            control_channel_capacity,
            event_rx,
            shutdown_tx: None,
            runner_handle: None,
            action_dispatch_tx,
            corrupt_every,
            flush_interval_secs,
            auto_peer_count_rx: None,
            share_instance_name: None,
            connect_instance_name: None,
            start_time: std::time::Instant::now(),
            iface_stats_map: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            iface_online_map: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
            iface_ready_map: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
            runtime: None,
        }
    }

    /// Start the node
    ///
    /// This spawns the internal event loop and initializes interfaces.
    /// The node will process incoming packets and emit events until `stop()` is called.
    pub async fn start(&mut self) -> Result<(), Error> {
        if self.runner_handle.is_some() {
            return Err(Error::Config("node already running".to_string()));
        }

        // Build a dedicated, time-enabled runtime to host the event loop and
        // all interface tasks. Entering it here routes every `tokio::spawn`
        // performed by the rest of `start()` — and transitively the child tasks
        // those spawn — onto this runtime, so the timer-driven event loop and
        // interface timers work even when the *embedding* runtime was built
        // without `enable_time()` (the PyO3/edge case that panicked at
        // `sleep_until`). `start()`'s body is synchronous up to the spawns, so
        // holding the enter guard across it (no await) is sound.
        //
        // Single worker thread: the node's work is async-I/O bound (network +
        // light per-packet crypto), so one cooperatively-scheduled worker is
        // sufficient, and it keeps the node from adding `num_cpus` threads on
        // top of an embedding host's own runtime — that oversubscription, plus
        // the genuine parallelism a multi-worker pool introduced between the
        // event loop and the public API, is the kind of thing that surfaces
        // latent ordering races in a cohabiting host.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("reticulum-node")
            .build()
            .map_err(|e| Error::Config(format!("failed to build node runtime: {e}")))?;
        let enter_guard = runtime.enter();

        // Shared monotonic counter for interface IDs.
        // Initialized at interfaces.len() so static and dynamic IDs never collide.
        let next_id = Arc::new(AtomicUsize::new(self.interfaces.len()));

        // Channel for dynamically registering interfaces (e.g. from TCP server accept loop)
        let (new_iface_tx, new_iface_rx) = mpsc::channel::<InterfaceHandle>(32);

        // Channel for TCP client reconnection notifications (Block D).
        // When a reconnecting TCP client re-establishes its connection, it sends
        // its InterfaceId here so the event loop can call handle_interface_up()
        // to re-announce destinations on the recovered link.
        let (reconnect_tx, reconnect_rx) = mpsc::channel::<InterfaceId>(16);

        // Initialize interfaces, the driver owns them, NOT NodeCore.
        // Interface init is the one fallible step after the runtime exists
        // (e.g. a TCPServerInterface bind failure). On error, tear the runtime
        // down with shutdown_background() before propagating — a bare `?` would
        // drop the live Runtime here, and a blocking Runtime drop inside the
        // caller's async context panics, masking the real interface error.
        let registry = match self.initialize_interfaces(&next_id, &new_iface_tx, &reconnect_tx) {
            Ok(registry) => registry,
            Err(e) => {
                drop(enter_guard);
                runtime.shutdown_background();
                return Err(e);
            }
        };

        {
            let mut core = self.inner.lock().unwrap();

            // Register human-readable interface names, HW_MTU, and counters with core
            {
                let mut stats = self.iface_stats_map.lock().unwrap();
                let mut online = self.iface_online_map.lock().unwrap();
                let mut ready = self.iface_ready_map.lock().unwrap();
                for handle in registry.handles() {
                    core.set_interface_name(handle.info.id.0, handle.info.name.clone());
                    if let Some(hw_mtu) = handle.info.hw_mtu {
                        core.set_interface_hw_mtu(handle.info.id.0, hw_mtu);
                    }
                    if let Some(bitrate) = handle.info.bitrate {
                        tracing::info!("Interface {} bitrate: {} bps", handle.info.name, bitrate);
                    }
                    stats.insert(handle.info.id.0, Arc::clone(&handle.counters));
                    online.insert(handle.info.id.0, true);
                    ready.insert(handle.info.id.0, Arc::clone(&handle.ready));
                }
            }

            // Register IFAC configurations for static interfaces (TCP client, UDP, RNode).
            // TCPServerInterface IFAC is handled via spawn_tcp_server → InterfaceInfo.ifac,
            // because the server listener itself doesn't register as an interface, only
            // accepted connections do, and they get dynamic interface IDs.
            for (idx, iface_config) in self.interfaces.iter().enumerate() {
                if !iface_config.enabled {
                    continue;
                }
                if iface_config.interface_type == "TCPServerInterface" {
                    continue; // IFAC passed to spawn_tcp_server in initialize_interfaces
                }
                if let Some(ifac) = build_ifac_config(iface_config) {
                    core.set_ifac_config(idx, ifac);
                    tracing::info!(
                        "IFAC enabled on interface {} (size={})",
                        idx,
                        iface_config
                            .ifac_size
                            .unwrap_or(reticulum_core::constants::IFAC_DEFAULT_SIZE_NETWORK)
                    );
                }
            }

            let transport_enabled = core.transport_config().enable_transport;
            let iface_count = self.interfaces.iter().filter(|c| c.enabled).count();
            tracing::info!(
                "Node started with {} interface(s), transport {}",
                iface_count,
                if transport_enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            );
        }

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        self.shutdown_tx = Some(shutdown_tx);

        // Channel for dispatching TickOutput from outside the event loop.
        // connect(), send_on_link(), close_link(), and
        // announce_destination() produce actions that must reach the event loop
        // for interface dispatch.  Capacity 256 is generous, each call
        // produces exactly one TickOutput, and the event loop drains them on
        // every iteration, so the queue only backs up if the event loop is
        // blocked (which also stalls all other I/O).
        let (action_dispatch_tx, action_dispatch_rx) = mpsc::channel(ACTION_DISPATCH_CAPACITY);
        self.action_dispatch_tx = action_dispatch_tx;

        // Clone handles for the runner. The event loop owns a single
        // `EventSink` built from clones of both plane senders; it is the only
        // writer, so the dropped-counter needs no synchronisation.
        let inner = Arc::clone(&self.inner);
        let event_sink = match (self.control_tx.clone(), self.data_tx.clone()) {
            (Some(control_tx), Some(data_tx)) => Some(EventSink {
                control_tx,
                data_tx,
                control_capacity: self.control_channel_capacity,
                control_dropped: 0,
            }),
            // `without_events()` leaves both senders None.
            _ => None,
        };
        let iface_stats_map = Arc::clone(&self.iface_stats_map);
        let iface_online_map = Arc::clone(&self.iface_online_map);
        let flush_interval_secs = self.flush_interval_secs;

        // Spawn the runner
        let runner_handle = tokio::spawn(async move {
            run_event_loop(
                inner,
                registry,
                EventLoopChannels {
                    event_sink,
                    action_dispatch_rx,
                    new_interface_rx: new_iface_rx,
                    reconnect_rx,
                    shutdown: shutdown_rx,
                },
                iface_stats_map,
                iface_online_map,
                flush_interval_secs,
            )
            .await;
        });

        self.runner_handle = Some(runner_handle);

        // Release the runtime context now that all tasks are spawned, then keep
        // the runtime alive in the node so its worker thread keeps driving them.
        drop(enter_guard);
        self.runtime = Some(runtime);

        Ok(())
    }

    /// Initialize interfaces from configuration
    ///
    /// Static interfaces (TCP clients) are connected and registered directly.
    /// Server listeners spawn accept loops that send new handles via `new_iface_tx`.
    fn initialize_interfaces(
        &mut self,
        next_id: &Arc<AtomicUsize>,
        new_iface_tx: &mpsc::Sender<InterfaceHandle>,
        reconnect_tx: &mpsc::Sender<InterfaceId>,
    ) -> Result<InterfaceRegistry, Error> {
        if self.share_instance_name.is_some() && self.connect_instance_name.is_some() {
            return Err(Error::Config(
                "cannot both share_instance and connect_to_shared_instance".to_string(),
            ));
        }

        let mut registry = InterfaceRegistry::new();
        let is_client_mode = self.connect_instance_name.is_some();

        // Only load config interfaces if NOT in shared-instance client mode.
        // Client mode routes everything through the daemon's Unix socket.
        if is_client_mode {
            tracing::info!("Shared instance client mode — skipping config interfaces");
        }

        if !is_client_mode {
            for (idx, config) in self.interfaces.iter().enumerate() {
                if !config.enabled {
                    continue;
                }

                match config.interface_type.as_str() {
                    "TCPClientInterface" => {
                        let target_host = config.target_host.as_ref().ok_or_else(|| {
                            Error::Config("TCPClientInterface requires target_host".to_string())
                        })?;
                        let target_port = config.target_port.ok_or_else(|| {
                            Error::Config("TCPClientInterface requires target_port".to_string())
                        })?;

                        let addr_str = format!("{}:{}", target_host, target_port);
                        let addr: SocketAddr = addr_str
                            .as_str()
                            .to_socket_addrs()
                            .map_err(|e| {
                                Error::Config(format!("cannot resolve {}: {}", addr_str, e))
                            })?
                            .next()
                            .ok_or_else(|| {
                                Error::Config(format!("no addresses for {}", addr_str))
                            })?;

                        let iface_name = format!("tcp_client_{}", idx);
                        let id = InterfaceId(idx);
                        let buffer_size = config.buffer_size.unwrap_or(TCP_DEFAULT_BUFFER_SIZE);
                        let reconnect_interval =
                            Duration::from_secs(config.reconnect_interval_secs.unwrap_or(5));

                        // TCP interfaces don't register a bitrate cap
                        // (bitrate=0 means unlimited). Future LoRa/serial interfaces
                        // should call transport.register_interface_bitrate(id, bitrate)
                        // after registration to enable per-interface announce caps.
                        let handle = spawn_tcp_client_with_reconnect(TcpClientConfig {
                            id,
                            name: iface_name,
                            addr,
                            buffer_size,
                            corrupt_every: self.corrupt_every,
                            reconnect_interval,
                            max_reconnect_tries: config.max_reconnect_tries,
                            connect_timeout: DEFAULT_TCP_CONNECT_TIMEOUT,
                            reconnect_notify: Some(reconnect_tx.clone()),
                        });
                        tracing::info!("TCP client interface for {} (reconnect enabled)", addr);
                        registry.register(handle);
                    }
                    "TCPServerInterface" => {
                        let listen_ip = config.listen_ip.as_deref().unwrap_or("0.0.0.0");
                        let listen_port = config.listen_port.ok_or_else(|| {
                            Error::Config("TCPServerInterface requires listen_port".to_string())
                        })?;

                        let addr: SocketAddr = format!("{}:{}", listen_ip, listen_port)
                            .parse()
                            .map_err(|e| Error::Config(format!("invalid listen address: {}", e)))?;

                        let buffer_size = config.buffer_size.unwrap_or(TCP_DEFAULT_BUFFER_SIZE);
                        let ifac = build_ifac_config(config);
                        spawn_tcp_server(
                            addr,
                            next_id.clone(),
                            new_iface_tx.clone(),
                            buffer_size,
                            self.corrupt_every,
                            ifac,
                        )?;
                    }
                    "UDPInterface" => {
                        let listen_ip = config.listen_ip.as_deref().unwrap_or("0.0.0.0");
                        let listen_port = config.listen_port.ok_or_else(|| {
                            Error::Config("UDPInterface requires listen_port".to_string())
                        })?;
                        let forward_ip = config.forward_ip.as_ref().ok_or_else(|| {
                            Error::Config("UDPInterface requires forward_ip".to_string())
                        })?;
                        let forward_port = config.forward_port.ok_or_else(|| {
                            Error::Config("UDPInterface requires forward_port".to_string())
                        })?;

                        let listen_addr: SocketAddr = format!("{}:{}", listen_ip, listen_port)
                            .parse()
                            .map_err(|e| {
                                Error::Config(format!("UDPInterface invalid listen address: {}", e))
                            })?;
                        let forward_addr: SocketAddr = format!("{}:{}", forward_ip, forward_port)
                            .parse()
                            .map_err(|e| {
                                Error::Config(format!(
                                    "UDPInterface invalid forward address: {}",
                                    e
                                ))
                            })?;

                        let iface_name = format!("udp_{}", idx);
                        let id = InterfaceId(idx);
                        let handle =
                            spawn_udp_interface(id, iface_name, listen_addr, forward_addr)?;
                        tracing::info!(
                            "UDP interface listening on {}, forwarding to {}",
                            listen_addr,
                            forward_addr
                        );
                        registry.register(handle);
                    }
                    "AutoInterface" => {
                        let auto_config = AutoInterfaceConfig {
                            group_id: config
                                .group_id
                                .as_deref()
                                .map(|s| s.as_bytes().to_vec())
                                .unwrap_or_else(|| {
                                    crate::interfaces::auto_interface::DEFAULT_GROUP_ID.to_vec()
                                }),
                            discovery_port: config.discovery_port.unwrap_or(
                                crate::interfaces::auto_interface::DEFAULT_DISCOVERY_PORT,
                            ),
                            data_port: config
                                .data_port
                                .unwrap_or(crate::interfaces::auto_interface::DEFAULT_DATA_PORT),
                            discovery_scope: config
                                .discovery_scope
                                .clone()
                                .unwrap_or_else(|| "link".to_string()),
                            allowed_devices: config.devices.clone(),
                            ignored_devices: config.ignored_devices.clone(),
                            multicast_loopback: config.multicast_loopback.unwrap_or(false),
                        };
                        let peer_count_rx = spawn_auto_interface(
                            next_id.clone(),
                            new_iface_tx.clone(),
                            auto_config,
                        );
                        self.auto_peer_count_rx = Some(peer_count_rx);
                        tracing::info!("AutoInterface: starting orchestrator");
                    }
                    "RNodeInterface" => {
                        let port_path = config
                            .port
                            .as_ref()
                            .ok_or_else(|| {
                                Error::Config("RNodeInterface requires port".to_string())
                            })?
                            .clone();
                        let frequency: u32 = config
                            .frequency
                            .ok_or_else(|| {
                                Error::Config("RNodeInterface requires frequency".to_string())
                            })
                            .and_then(|f| {
                                u32::try_from(f).map_err(|_| {
                                    Error::Config(format!("frequency {} exceeds u32 range", f))
                                })
                            })?;
                        let bandwidth = config.bandwidth.ok_or_else(|| {
                            Error::Config("RNodeInterface requires bandwidth".to_string())
                        })?;
                        let sf = config.spreading_factor.ok_or_else(|| {
                            Error::Config("RNodeInterface requires spreading_factor".to_string())
                        })?;
                        let cr = config.coding_rate.ok_or_else(|| {
                            Error::Config("RNodeInterface requires coding_rate".to_string())
                        })?;
                        let tx_power: u8 =
                            config.tx_power.unwrap_or(0).try_into().map_err(|_| {
                                Error::Config(format!(
                                    "tx_power {} out of range (0-37)",
                                    config.tx_power.unwrap_or(0)
                                ))
                            })?;

                        reticulum_core::rnode::validate_config(
                            frequency, bandwidth, tx_power, sf, cr,
                        )
                        .map_err(|e| Error::Config(format!("RNodeInterface: {}", e)))?;

                        let st_alock = config.airtime_limit_short.map(|p| (p * 100.0) as u16);
                        let lt_alock = config.airtime_limit_long.map(|p| (p * 100.0) as u16);
                        let flow_control = config.flow_control.unwrap_or(false);
                        let buffer_size = config
                            .buffer_size
                            .unwrap_or(crate::interfaces::rnode::RNODE_DEFAULT_BUFFER_SIZE);

                        let iface_name = format!("rnode_{}", idx);
                        let id = InterfaceId(idx);

                        let handle = crate::interfaces::rnode::spawn_rnode_interface(
                            crate::interfaces::rnode::RNodeInterfaceConfig {
                                id,
                                name: iface_name,
                                port_path: port_path.clone(),
                                frequency,
                                bandwidth,
                                tx_power,
                                sf,
                                cr,
                                st_alock,
                                lt_alock,
                                flow_control,
                                buffer_size,
                                reconnect_notify: Some(reconnect_tx.clone()),
                            },
                        );

                        tracing::info!(
                        "RNode interface on {} (freq={} Hz, sf={}, bw={} Hz, cr={}, txp={} dBm)",
                        port_path,
                        frequency,
                        sf,
                        bandwidth,
                        cr,
                        tx_power,
                    );
                        registry.register(handle);
                    }
                    "SerialInterface" => {
                        let port_path = config
                            .port
                            .as_ref()
                            .ok_or_else(|| {
                                Error::Config("SerialInterface requires port".to_string())
                            })?
                            .clone();
                        let speed = config.speed.unwrap_or(9600);
                        let data_bits = crate::interfaces::serial::parse_data_bits(
                            config.databits.unwrap_or(8),
                        );
                        let parity = crate::interfaces::serial::parse_parity(
                            config.parity.as_deref().unwrap_or("N"),
                        );
                        let stop_bits = crate::interfaces::serial::parse_stop_bits(
                            config.stopbits.unwrap_or(1),
                        );
                        let buffer_size = config
                            .buffer_size
                            .unwrap_or(crate::interfaces::serial::SERIAL_DEFAULT_BUFFER_SIZE);

                        let iface_name = format!("serial_{}", idx);
                        let id = InterfaceId(idx);

                        let radio_config = if config.frequency.is_some() {
                            Some(crate::interfaces::serial::SerialRadioConfig {
                                frequency: config.frequency.unwrap_or(869_525_000),
                                bandwidth: config.bandwidth.unwrap_or(125_000),
                                spreading_factor: config.spreading_factor.unwrap_or(7),
                                coding_rate: config.coding_rate.unwrap_or(5),
                                tx_power: config.tx_power.unwrap_or(17),
                                preamble_len: 24,
                                csma_enabled: config.csma_enabled.unwrap_or(true),
                            })
                        } else {
                            None
                        };

                        let mut handle = crate::interfaces::serial::spawn_serial_interface(
                            crate::interfaces::serial::SerialInterfaceConfig {
                                id,
                                name: iface_name.clone(),
                                port: port_path.clone(),
                                speed,
                                data_bits,
                                parity,
                                stop_bits,
                                buffer_size,
                                reconnect_notify: Some(reconnect_tx.clone()),
                                radio_config,
                            },
                        );
                        handle.info.bitrate = Some(speed);

                        tracing::info!("Serial interface on {} (speed={} baud)", port_path, speed,);
                        registry.register(handle);
                    }
                    other => {
                        tracing::warn!("Unknown interface type: {}", other);
                    }
                }
            }
        } // end if !is_client_mode

        // Connect to shared instance daemon as client
        if let Some(ref instance_name) = self.connect_instance_name {
            let id = InterfaceId(next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
            let handle = crate::interfaces::local::spawn_local_client(
                id,
                instance_name,
                crate::interfaces::local::LOCAL_DEFAULT_BUFFER_SIZE,
            )?;
            tracing::info!("Connected to shared instance '{}'", instance_name);
            registry.register(handle);
        }

        // Start local (shared instance) server if enabled
        if let Some(ref instance_name) = self.share_instance_name {
            crate::interfaces::local::spawn_local_server(
                instance_name,
                next_id.clone(),
                new_iface_tx.clone(),
                crate::interfaces::local::LOCAL_DEFAULT_BUFFER_SIZE,
            )?;

            // Start RPC server for Python CLI tool compatibility (rnstatus, rnpath, rnprobe)
            let authkey = {
                let core = self.inner.lock().unwrap();
                match core.identity().private_key_bytes() {
                    Ok(prv) => {
                        use sha2::Digest;
                        let hash = sha2::Sha256::digest(prv);
                        let mut key = [0u8; 32];
                        key.copy_from_slice(&hash);
                        key
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Cannot derive RPC authkey (no private key: {}), RPC server disabled",
                            e
                        );
                        return Ok(registry);
                    }
                }
            };
            if let Err(e) = crate::rpc::spawn_rpc_server(
                instance_name,
                Arc::clone(&self.inner),
                authkey,
                self.start_time,
                Arc::clone(&self.iface_stats_map),
                Arc::clone(&self.iface_online_map),
                self.auto_peer_count_rx.as_ref().cloned(),
            ) {
                tracing::warn!("Failed to start RPC server: {}", e);
            }
        }

        // Spawn background traffic counter (matches Python Transport.count_traffic_loop)
        crate::interfaces::spawn_traffic_counter(Arc::clone(&self.iface_stats_map));

        Ok(registry)
    }

    /// Stop the node
    ///
    /// This signals the event loop to stop, waits for completion, and persists
    /// known destinations to disk.
    pub async fn stop(&mut self) -> Result<(), Error> {
        // Signal shutdown
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }

        // Wait for runner to finish
        if let Some(handle) = self.runner_handle.take() {
            handle
                .await
                .map_err(|e| Error::Config(format!("runner panicked: {}", e)))?;
        }

        // Persist state to disk
        self.save_persistent_state();

        // Tear down the node's runtime (non-blocking) now that the event loop
        // has exited. Clearing it means a subsequent start() builds a fresh
        // runtime instead of overwriting (and blocking-dropping) a live one in
        // this async context.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }

        tracing::info!("ReticulumNode stopped");
        Ok(())
    }

    /// Persist all state to disk on shutdown.
    ///
    /// Delegates to `Storage::flush()` which saves known_destinations
    /// and packet_hashlist in Python-compatible formats.
    fn save_persistent_state(&self) {
        use reticulum_core::traits::Storage as _;
        let mut core = self.inner.lock().unwrap();
        core.storage_mut().flush();
    }

    /// Enable shared instance with the given instance name.
    ///
    /// Called by the builder when `share_instance = true`.
    pub(crate) fn set_share_instance(&mut self, name: String) {
        self.share_instance_name = Some(name);
    }

    /// Connect to a shared instance daemon as a client.
    ///
    /// Called by the builder when `connect_to_shared_instance` is set.
    pub(crate) fn set_connect_instance(&mut self, name: String) {
        self.connect_instance_name = Some(name);
    }

    /// Check if the node is running
    pub fn is_running(&self) -> bool {
        self.runner_handle
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Register a destination for incoming links
    pub fn register_destination(&self, destination: Destination) {
        let mut inner = self.inner.lock().unwrap();
        inner.register_destination(destination);
    }

    /// Connect to a remote destination
    ///
    /// Sends a link request to the destination and returns a `LinkHandle`
    /// for async read/write operations. The returned handle is usable
    /// immediately, but the link is not yet established, watch for
    /// `NodeEvent::LinkEstablished` on the event channel before sending data.
    ///
    /// Returns `Err` only if the event loop is down (the request could not
    /// be dispatched). Link-level failures arrive as `NodeEvent::LinkClosed`.
    ///
    /// # Arguments
    /// * `dest_hash` - The destination hash to connect to
    /// * `dest_signing_key` - The destination's signing key (from announce)
    pub async fn connect(
        &self,
        dest_hash: &DestinationHash,
        dest_signing_key: &[u8; 32],
    ) -> Result<LinkHandle, Error> {
        // Request link from NodeCore
        let (link_id, _was_routed, output) = {
            let mut inner = self.inner.lock().unwrap();
            inner.connect(*dest_hash, dest_signing_key)
        };
        // Send output to event loop for dispatch (backpressure, waits if full)
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;

        Ok(LinkHandle::new(
            link_id,
            Arc::clone(&self.inner),
            self.action_dispatch_tx.clone(),
        ))
    }

    /// Accept an incoming link request
    ///
    /// Accepts a link request identified by `link_id` (from a `LinkRequest`
    /// event) and returns a `LinkHandle` for async read/write operations.
    /// The link proof packet is queued and dispatched by the event loop.
    ///
    /// # Arguments
    /// * `link_id` - The link ID from the `LinkRequest` event
    pub async fn accept_link(&self, link_id: &LinkId) -> Result<LinkHandle, Error> {
        let output = {
            let mut inner = self.inner.lock().unwrap();
            inner.accept_link(link_id)?
        };

        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;

        Ok(LinkHandle::new(
            *link_id,
            Arc::clone(&self.inner),
            self.action_dispatch_tx.clone(),
        ))
    }

    /// Take the event receiver
    ///
    /// This allows consuming node events directly. Can only be called once.
    ///
    /// The returned [`EventReceiver`] merges the split control/data planes
    /// (Codeberg #71), draining control events with strict priority over data
    /// events. Use [`EventReceiver::recv`] exactly like a
    /// `tokio::sync::mpsc::Receiver`.
    pub fn take_event_receiver(&mut self) -> Option<EventReceiver> {
        self.event_rx.take()
    }

    /// Wait until the interface at index `idx` has reached its readiness
    /// condition, or return `Err(InterfaceReadyError)` after `timeout`.
    ///
    /// # Readiness contract (per interface type)
    ///
    /// - **TCP client (`add_tcp_client`):** ready once the kernel-level
    ///   TCP three-way handshake has succeeded
    ///   (`TcpStream::connect` returned Ok).  This is Option α
    ///   semantics from the Codeberg #49 audit: it does **not**
    ///   guarantee that the remote peer has completed any
    ///   post-accept registration steps it may run.  Tests that
    ///   need the daemon-side peer-registration acknowledgement
    ///   should pair this call with a daemon-side check (e.g. the
    ///   test harness's `TestDaemon::wait_for_peer_count`).
    /// - **TCP server (`add_tcp_server`):** the listener is bound
    ///   before the handle is registered; the API returns
    ///   immediately as ready.
    /// - **UDP, RNode, AutoInterface, Local IPC:** ready once the
    ///   underlying socket / port is bound or the IPC stream is
    ///   connected — currently signalled at handle construction
    ///   time, so the API returns immediately as ready.
    ///
    /// Returns `Err(InterfaceReadyError::Unknown)` if `idx` does not
    /// match any registered interface; `Err(InterfaceReadyError::TimedOut)`
    /// if the readiness deadline elapsed before the signal fired;
    /// `Err(InterfaceReadyError::NotStarted)` if `start()` has not
    /// yet been called.
    pub async fn wait_for_interface_ready(
        &self,
        idx: usize,
        timeout: std::time::Duration,
    ) -> Result<(), InterfaceReadyError> {
        if self.runner_handle.is_none() {
            return Err(InterfaceReadyError::NotStarted);
        }
        let signal = {
            let map = self.iface_ready_map.lock().unwrap();
            map.get(&idx).cloned()
        };
        match signal {
            Some(s) => s
                .wait(timeout)
                .await
                .map_err(|_| InterfaceReadyError::TimedOut { idx }),
            None => Err(InterfaceReadyError::Unknown { idx }),
        }
    }

    /// Wait until **all** registered interfaces are ready, or return
    /// `Err` listing the ones that timed out.
    ///
    /// The deadline is shared across all interfaces; each
    /// individual wait gets the remaining budget rather than the
    /// full `timeout`.  See [`wait_for_interface_ready`](Self::wait_for_interface_ready)
    /// for the per-interface readiness contract.
    pub async fn wait_for_interfaces_ready(
        &self,
        timeout: std::time::Duration,
    ) -> Result<(), Vec<(usize, ReadyState)>> {
        if self.runner_handle.is_none() {
            return Err(vec![(0, ReadyState::NotStarted)]);
        }
        let signals: Vec<(usize, std::sync::Arc<crate::interfaces::ReadySignal>)> = {
            let map = self.iface_ready_map.lock().unwrap();
            map.iter()
                .map(|(k, v)| (*k, std::sync::Arc::clone(v)))
                .collect()
        };
        if signals.is_empty() {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let mut failures = Vec::new();
        for (idx, sig) in signals {
            let now = tokio::time::Instant::now();
            let remaining = if deadline > now {
                deadline - now
            } else {
                std::time::Duration::ZERO
            };
            if sig.wait(remaining).await.is_err() {
                failures.push((idx, ReadyState::TimedOut));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures)
        }
    }

    /// Get the number of active (established) links
    pub fn active_link_count(&self) -> usize {
        self.inner.lock().unwrap().active_link_count()
    }

    /// Get the number of pending (not yet established) links
    pub fn pending_link_count(&self) -> usize {
        self.inner.lock().unwrap().pending_link_count()
    }

    /// Get the node's identity hash (16 bytes)
    pub fn identity_hash(&self) -> [u8; 16] {
        *self.inner.lock().unwrap().identity().hash()
    }

    /// Get the negotiated MTU for a link
    ///
    /// Returns `None` if the link does not exist.
    pub fn link_negotiated_mtu(&self, link_id: &LinkId) -> Option<u32> {
        self.inner
            .lock()
            .unwrap()
            .link(link_id)
            .map(|l| l.negotiated_mtu())
    }

    /// Get the encrypted link MDU (maximum data unit) for a link
    ///
    /// Returns `None` if the link does not exist.
    pub fn link_mdu(&self, link_id: &LinkId) -> Option<usize> {
        self.inner.lock().unwrap().link(link_id).map(|l| l.mdu())
    }

    /// Register a known identity for a destination
    ///
    /// Identities learned from received announces are cached automatically.    /// call this only for out-of-band identity registration or testing.
    pub fn remember_identity(
        &self,
        dest_hash: DestinationHash,
        identity: reticulum_core::Identity,
    ) {
        self.inner
            .lock()
            .unwrap()
            .remember_identity(dest_hash, identity);
    }

    /// Get a handle to the inner NodeCore
    ///
    /// Use this for direct access to the core API.
    #[cfg(test)]
    pub(crate) fn inner(&self) -> Arc<Mutex<StdNodeCore>> {
        Arc::clone(&self.inner)
    }

    /// Check if a path to a destination is known
    pub fn has_path(&self, dest_hash: &reticulum_core::DestinationHash) -> bool {
        self.inner.lock().unwrap().has_path(dest_hash)
    }

    /// Look up a known identity for a destination hash.
    ///
    /// Returns the identity if it was previously learned from an announce.
    /// The Ed25519 verifying key (bytes 32..64 of `public_key_bytes()`)
    /// is the `dest_signing_key` required by `connect()`.
    pub fn get_identity(
        &self,
        dest_hash: &reticulum_core::DestinationHash,
    ) -> Option<reticulum_core::Identity> {
        self.inner
            .lock()
            .unwrap()
            .storage()
            .get_identity(dest_hash.as_bytes())
            .cloned()
    }

    /// Request a path to a destination.
    ///
    /// Sends a PATH_REQUEST. The result will arrive as a `PathFound` event
    /// and `has_path()` will return true.
    pub async fn request_path(
        &self,
        dest_hash: &reticulum_core::DestinationHash,
    ) -> Result<(), Error> {
        let output = {
            let mut inner = self.inner.lock().unwrap();
            inner.request_path(dest_hash)
        };
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;
        Ok(())
    }

    /// Get hop count to a destination
    pub fn hops_to(&self, dest_hash: &reticulum_core::DestinationHash) -> Option<u8> {
        self.inner.lock().unwrap().hops_to(dest_hash)
    }

    /// Returns the current ratchet public key for a registered destination.
    pub fn destination_ratchet_public(
        &self,
        dest_hash: &reticulum_core::DestinationHash,
    ) -> Option<[u8; 32]> {
        self.inner
            .lock()
            .unwrap()
            .destination_ratchet_public(dest_hash)
    }

    /// Get the number of known paths
    pub fn path_count(&self) -> usize {
        self.inner.lock().unwrap().path_count()
    }

    /// Read the current monotonic-clock value (milliseconds since
    /// NodeCore construction).
    ///
    /// Exposed to let observability surfaces convert
    /// `PathTableExport.expires_ms` / `RateTableExport.blocked_until_ms`
    /// (both monotonic) into wall-clock projections by anchoring
    /// against `std::time::SystemTime::now()` at call time.
    pub fn now_ms(&self) -> u64 {
        self.inner.lock().unwrap().now_ms()
    }

    /// Snapshot every known path-table entry.
    ///
    /// Returns owned `PathTableExport` clones — the inner storage map
    /// is unlocked before the result returns to the caller, so no
    /// mutex-borrowed references escape. Intended for downstream
    /// observability surfaces (routing-table inspectors, federation
    /// diagnostics). Snapshot reflects the table at call time; entries
    /// may be evicted by subsequent expiry sweeps.
    pub fn path_table_entries(&self) -> Vec<reticulum_core::transport::PathTableExport> {
        self.inner.lock().unwrap().path_table_entries()
    }

    /// Snapshot every announce-rate-table entry.
    ///
    /// Returns owned `RateTableExport` clones; same deep-clone /
    /// mutex-release contract as [`Self::path_table_entries`]. Used by
    /// operator tools that need to inspect per-source announce
    /// frequency / ban state.
    pub fn rate_table_entries(&self) -> Vec<reticulum_core::transport::RateTableExport> {
        self.inner.lock().unwrap().rate_table_entries()
    }

    /// Look up a single path entry by destination hash.
    ///
    /// Returns a cloned `PathEntry` (no mutex-borrowed reference
    /// escapes) or `None` when the destination is unknown.
    pub fn get_path_clone(
        &self,
        dest_hash: &reticulum_core::DestinationHash,
    ) -> Option<reticulum_core::storage_types::PathEntry> {
        self.inner
            .lock()
            .unwrap()
            .get_path_clone(dest_hash.as_bytes())
    }

    /// Drop a specific path entry by destination hash.
    ///
    /// Returns `true` if the entry existed and was removed, `false`
    /// when it was not present. The local path cache only — does
    /// not emit any wire-level invalidation packet.
    pub fn remove_path(&self, dest_hash: &reticulum_core::DestinationHash) -> bool {
        self.inner.lock().unwrap().remove_path(dest_hash.as_bytes())
    }

    /// Drop every path whose `next_hop` matches the supplied transport
    /// identity hash.
    ///
    /// Local cache surgery only, mirroring the rnsd RPC drop-all-via
    /// semantics: no wire-level invalidation is emitted.
    ///
    /// Returns the count of paths removed. Useful when a transport
    /// peer is known to be down: the caller bulk-evicts every path
    /// routed via that peer in a single call rather than iterating
    /// the table and calling [`Self::remove_path`] per entry.
    pub fn drop_all_paths_via(&self, via_hash: &reticulum_core::DestinationHash) -> usize {
        self.inner
            .lock()
            .unwrap()
            .drop_all_paths_via(via_hash.as_bytes())
    }

    /// Get transport statistics (packets sent, received, forwarded, dropped)
    pub fn transport_stats(&self) -> reticulum_core::transport::TransportStats {
        self.inner.lock().unwrap().transport_stats()
    }

    /// Get link statistics for a link
    pub fn link_stats(
        &self,
        link_id: &reticulum_core::link::LinkId,
    ) -> Option<reticulum_core::node::LinkStats> {
        self.inner.lock().unwrap().link_stats(link_id)
    }

    /// Announce a registered destination on all interfaces
    ///
    /// Builds the announce packet and queues it as a Broadcast action.
    /// The event loop dispatches the action on the next iteration.
    ///
    /// # Arguments
    /// * `dest_hash` - Hash of the registered destination to announce
    /// * `app_data` - Optional application data to include in the announce
    pub async fn announce_destination(
        &self,
        dest_hash: &reticulum_core::DestinationHash,
        app_data: Option<&[u8]>,
    ) -> Result<(), Error> {
        let output = self
            .inner
            .lock()
            .unwrap()
            .announce_destination(dest_hash, app_data)?;
        // Send output to event loop for dispatch (backpressure, waits if full)
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;
        Ok(())
    }

    /// Close a link gracefully
    ///
    /// Sends a LINKCLOSE packet to the peer and removes the link.
    ///
    /// # Arguments
    /// * `link_id` - The link ID of the link to close
    pub async fn close_link(&self, link_id: &LinkId) -> Result<(), Error> {
        let output = {
            let mut inner = self.inner.lock().unwrap();
            inner.close_link(link_id)
        };
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;
        Ok(())
    }

    /// Identify our identity to the link peer.
    ///
    /// See [`NodeCore::identify_link()`] for protocol details.
    pub async fn identify_link(
        &self,
        link_id: &LinkId,
        identity: &reticulum_core::Identity,
    ) -> Result<(), Error> {
        let output = {
            let mut inner = self.inner.lock().unwrap();
            inner.identify_link(link_id, identity)?
        };
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;
        Ok(())
    }

    /// Get the remote identity for a link, if the peer has identified.
    pub fn get_remote_identity(&self, link_id: &LinkId) -> Option<reticulum_core::Identity> {
        let inner = self.inner.lock().unwrap();
        inner.get_remote_identity(link_id).cloned()
    }

    // Request/Response API
    /// Register a request handler for a given path on a destination.
    pub fn register_request_handler(
        &self,
        destination_hash: reticulum_core::DestinationHash,
        path: &str,
        policy: reticulum_core::RequestPolicy,
    ) {
        let mut inner = self.inner.lock().unwrap();
        inner.register_request_handler(destination_hash, path, policy);
    }

    /// Send a request on an established link.
    ///
    /// Returns the request_id identifying this request.
    pub async fn send_request(
        &self,
        link_id: &LinkId,
        path: &str,
        data: Option<&[u8]>,
        timeout_ms: Option<u64>,
    ) -> Result<[u8; 16], Error> {
        let (request_id, output) = {
            let mut inner = self.inner.lock().unwrap();
            inner
                .send_request(link_id, path, data, timeout_ms)
                .map_err(Error::Request)?
        };
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;
        Ok(request_id)
    }

    /// Send a response to a received request.
    ///
    /// `response_data` must be exactly one valid msgpack-encoded value.
    pub async fn send_response(
        &self,
        link_id: &LinkId,
        request_id: &[u8; 16],
        response_data: &[u8],
    ) -> Result<(), Error> {
        let output = {
            let mut inner = self.inner.lock().unwrap();
            inner
                .send_response(link_id, request_id, response_data)
                .map_err(Error::Request)?
        };
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;
        Ok(())
    }

    // Resource Transfer API
    /// Initiate a resource transfer on an established link.
    ///
    /// Returns the resource hash identifying this transfer. The ADV packet is
    /// queued and dispatched by the event loop immediately.
    ///
    /// # Arguments
    /// * `link_id` - The link to send over (must be Active)
    /// * `data` - The application data to transfer
    /// * `metadata` - Optional metadata bytes, must be msgpack-encoded by the
    ///   caller (Python's Resource constructor calls `umsgpack.packb(metadata)`)
    pub async fn send_resource(
        &self,
        link_id: &LinkId,
        data: &[u8],
        metadata: Option<&[u8]>,
        auto_compress: bool,
    ) -> Result<[u8; 32], Error> {
        let (resource_hash, output) = {
            let mut inner = self.inner.lock().unwrap();
            inner
                .send_resource(link_id, data, metadata, auto_compress)
                .map_err(Error::Resource)?
        };
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;
        Ok(resource_hash)
    }

    /// Set the resource acceptance strategy for a link.
    ///
    /// # Arguments
    /// * `link_id` - The link to configure
    /// * `strategy` - The acceptance strategy (AcceptNone, AcceptAll, AcceptApp)
    pub fn set_resource_strategy(
        &self,
        link_id: &LinkId,
        strategy: reticulum_core::resource::ResourceStrategy,
    ) -> Result<(), Error> {
        self.inner
            .lock()
            .unwrap()
            .set_resource_strategy(link_id, strategy)
            .map_err(Error::Resource)
    }

    /// Accept a pending resource advertisement on a link.
    ///
    /// Call this after receiving a `NodeEvent::ResourceAdvertised` event.
    pub async fn accept_resource(&self, link_id: &LinkId) -> Result<(), Error> {
        let output = {
            let mut inner = self.inner.lock().unwrap();
            inner.accept_resource(link_id).map_err(Error::Resource)?
        };
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;
        Ok(())
    }

    /// Reject a pending resource advertisement on a link.
    ///
    /// Call this after receiving a `NodeEvent::ResourceAdvertised` event.
    pub async fn reject_resource(&self, link_id: &LinkId) -> Result<(), Error> {
        let output = {
            let mut inner = self.inner.lock().unwrap();
            inner.reject_resource(link_id).map_err(Error::Resource)?
        };
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;
        Ok(())
    }

    /// Send a single (fire-and-forget) packet to a destination
    ///
    /// Builds an unreliable data packet addressed to `dest_hash` and queues it
    /// for dispatch. A path to the destination must already be known.
    ///
    /// # Arguments
    /// * `dest_hash` - The destination hash to send to
    /// * `data` - The data to send (must fit in a single packet)
    ///
    /// # Returns
    /// The truncated packet hash, usable for tracking delivery proofs.
    pub async fn send_single_packet(
        &self,
        dest_hash: &DestinationHash,
        data: &[u8],
    ) -> Result<[u8; TRUNCATED_HASHBYTES], Error> {
        let (packet_hash, output) = {
            let mut inner = self.inner.lock().unwrap();
            inner.send_single_packet(dest_hash, data)?
        };
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;
        Ok(packet_hash)
    }

    /// Send a delivery proof for a previously received packet, after a
    /// `PacketProofRequested` event under `ProofStrategy::App`. Additive: built
    /// on the core `send_proof`, dispatched like the other send paths.
    pub async fn send_proof(
        &self,
        packet_hash: &[u8; 32],
        dest_hash: &DestinationHash,
    ) -> Result<(), Error> {
        let output = {
            let mut inner = self.inner.lock().unwrap();
            inner
                .send_proof(packet_hash, dest_hash)
                .map_err(|e| match e {
                    reticulum_core::transport::TransportError::NoPath => {
                        Error::Send(reticulum_core::SendError::NoPath)
                    }
                    other => Error::Config(format!("proof send failed: {other:?}")),
                })?
        };
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;
        Ok(())
    }

    /// Create a PacketSender for a destination
    ///
    /// Returns a self-contained handle for sending single packets.
    /// No path or destination validation, errors are reported on send().
    pub fn packet_sender(&self, dest_hash: &DestinationHash) -> PacketSender {
        PacketSender::new(
            *dest_hash,
            Arc::clone(&self.inner),
            self.action_dispatch_tx.clone(),
        )
    }

    /// Return a diagnostic dump of all protocol state memory usage
    pub fn diagnostic_dump(&self) -> String {
        self.inner.lock().unwrap().diagnostic_dump()
    }

    /// Check if transport mode (relay/routing) is enabled
    pub fn is_transport_enabled(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .transport_config()
            .enable_transport
    }

    /// Get the number of discovered AutoInterface peers
    ///
    /// Returns 0 if no AutoInterface is configured.
    pub fn auto_interface_peer_count(&self) -> usize {
        self.auto_peer_count_rx
            .as_ref()
            .map(|rx| *rx.borrow())
            .unwrap_or(0)
    }
}

// Sans-I/O Event Loop
/// Poll all interface channels with round-robin fairness
///
/// Returns `RecvEvent::Packet` when a complete packet is available, or
/// `RecvEvent::Disconnected` when an interface's incoming channel closes.
/// Returns `Poll::Pending` when no interface has data ready.
async fn recv_any(registry: &mut InterfaceRegistry) -> RecvEvent {
    if registry.is_empty() {
        // No interfaces, pend forever (timer branch will still fire)
        std::future::pending().await
    } else {
        std::future::poll_fn(|cx| {
            let (handles, poll_start) = registry.handles_mut();
            let len = handles.len();

            for offset in 0..len {
                let idx = (*poll_start + offset) % len;
                let handle = &mut handles[idx];
                let id = handle.info.id;

                match handle.incoming.poll_recv(cx) {
                    Poll::Ready(Some(pkt)) => {
                        *poll_start = (idx + 1) % len;
                        return Poll::Ready(RecvEvent::Packet(id, pkt));
                    }
                    Poll::Ready(None) => {
                        *poll_start = (idx + 1) % len;
                        return Poll::Ready(RecvEvent::Disconnected(id));
                    }
                    Poll::Pending => {}
                }
            }
            Poll::Pending
        })
        .await
    }
}

/// Run the internal event loop (sans-I/O architecture)
///
/// The driver owns the interfaces and acts as the I/O bridge between the
/// pure state machine (`NodeCore`) and the actual network. Uses `select!`
/// to wake immediately on socket readability, outgoing data, or timer expiry.
async fn run_event_loop(
    inner: Arc<Mutex<StdNodeCore>>,
    mut registry: InterfaceRegistry,
    channels: EventLoopChannels,
    iface_stats_map: InterfaceStatsMap,
    iface_online_map: InterfaceOnlineMap,
    flush_interval_secs: u64,
) {
    let mut event_sink = channels.event_sink;
    let mut action_dispatch_rx = channels.action_dispatch_rx;
    let mut new_interface_rx = channels.new_interface_rx;
    let mut reconnect_rx = channels.reconnect_rx;
    let mut shutdown = channels.shutdown;
    let mut next_poll = tokio::time::Instant::now();
    let mut next_flush = tokio::time::Instant::now() + Duration::from_secs(flush_interval_secs);
    let mut retry_queues: BTreeMap<usize, VecDeque<Vec<u8>>> = BTreeMap::new();
    // Track which per-interface queues have already emitted the
    // depth-high warning so we don't spam once the queue is deep.
    // Cleared when the queue drops back below RETRY_QUEUE_DEPTH_WARN.
    let mut retry_queue_warned: std::collections::BTreeSet<usize> =
        std::collections::BTreeSet::new();
    // Monotonic high-watermark of each retry_queue's depth since
    // process start. Logged at info! when it increases so hardware
    // benchmarks can read it out of the capture without extra
    // instrumentation.
    let mut retry_queue_max_depth: BTreeMap<usize, usize> = BTreeMap::new();

    // Clone IFAC configs from core so dispatch_output can apply IFAC outside the lock.
    // This is the canonical source of truth for "what IFAC config does interface N have
    // according to the INI config". On reconnect, we re-apply from this map.
    let mut ifac_configs: BTreeMap<usize, reticulum_core::ifac::IfacConfig> = {
        let core = inner.lock().unwrap();
        core.clone_ifac_configs()
    };

    loop {
        // Event-driven retry-queue drain. Any non-empty queue whose
        // front packet is currently ineligible for a slot contributes a
        // wake deadline; the earliest of those becomes the
        // tokio::time::sleep_until arm below. When all queues are empty
        // or the head is already ready, no sleep arm is activated.
        let retry_wake_instant: Option<tokio::time::Instant> = {
            let now_ms = inner.lock().unwrap().now_ms();
            compute_retry_wake_deadline_ms(&retry_queues, &registry, now_ms)
                .and_then(|slot_ms| slot_ms.checked_sub(now_ms))
                .map(|delta_ms| tokio::time::Instant::now() + Duration::from_millis(delta_ms))
        };

        tokio::select! {
            // Fires exactly when the earliest retry-queue head becomes
            // eligible. The arm only exists when retry_wake_instant is
            // Some; otherwise the select skips it. Inside, we call
            // drain + push_interface_state to get the packets out and
            // refresh Transport's caches.
            _ = async {
                match retry_wake_instant {
                    Some(t) => tokio::time::sleep_until(t).await,
                    None => core::future::pending::<()>().await,
                }
            } => {
                let now_ms = inner.lock().unwrap().now_ms();
                drain_retry_queues(&mut retry_queues, &mut registry, now_ms);
                push_interface_state(&mut registry, &inner);
            }

            // Branch 1: Packet from any interface
            event = recv_any(&mut registry) => {
                match event {
                    RecvEvent::Packet(iface_id, pkt) => {
                        tracing::debug!(
                            "driver: received {} bytes from iface {} ({})",
                            pkt.data.len(),
                            iface_id,
                            registry.name_of(iface_id),
                        );
                        let (output, now_ms) = {
                            let mut core = inner.lock().unwrap();
                            let output = core.handle_packet(iface_id, &pkt.data);
                            let now_ms = core.now_ms();
                            (output, now_ms)
                        };
                        tracing::debug!(
                            "driver: handle_packet produced {} actions, {} events",
                            output.actions.len(),
                            output.events.len(),
                        );
                        // Packet handling may schedule new deadlines (e.g. announce
                        // rebroadcast retries), advance next_poll if sooner.
                        if let Some(deadline_ms) = output.next_deadline_ms {
                            let delta = deadline_ms.saturating_sub(now_ms);
                            let wake_at = tokio::time::Instant::now()
                                + Duration::from_millis(delta);
                            if wake_at < next_poll {
                                next_poll = wake_at;
                            }
                        }
                        dispatch_output(
                            output,
                            &mut registry,
                            event_sink.as_mut(),
                            &inner,
                            &mut retry_queues, &mut retry_queue_warned, &mut retry_queue_max_depth, &ifac_configs,
                        );
                    }
                    RecvEvent::Disconnected(iface_id) => {
                        tracing::warn!("Interface {} ({}) disconnected", iface_id, registry.name_of(iface_id));
                        let output = {
                            let mut core = inner.lock().unwrap();
                            core.handle_interface_down(iface_id)
                        };
                        dispatch_output(
                            output,
                            &mut registry,
                            event_sink.as_mut(),
                            &inner,
                            &mut retry_queues, &mut retry_queue_warned, &mut retry_queue_max_depth, &ifac_configs,
                        );
                        // Clear retry queue for disconnected interface. The legacy
                        // is_interface_congested flag was removed in Phase F;
                        // Transport's interface_next_slot_ms falls back to
                        // now_ms once the interface is removed from the
                        // backchannel, which happens naturally.
                        retry_queues.remove(&iface_id.0);
                        registry.remove(iface_id);
                        {
                            let mut stats = iface_stats_map.lock().unwrap();
                            stats.remove(&iface_id.0);
                        }
                        {
                            let mut online = iface_online_map.lock().unwrap();
                            online.remove(&iface_id.0);
                        }
                    }
                }
            }

            // Branch 2: Dispatch TickOutput from outside the event loop
            // (connect, send_on_link, close_link, announce send here)
            Some(output) = action_dispatch_rx.recv() => {
                dispatch_output(
                    output,
                    &mut registry,
                    event_sink.as_mut(),
                    &inner,
                    &mut retry_queues, &mut retry_queue_warned, &mut retry_queue_max_depth, &ifac_configs,
                );
            }

            // Branch 3: Timer, persistent deadline, not recomputed per iteration
            _ = tokio::time::sleep_until(next_poll) => {
                let (output, now_ms) = {
                    let mut core = inner.lock().unwrap();
                    let output = core.handle_timeout();
                    let now_ms = core.now_ms();
                    (output, now_ms)
                };
                let next = output.next_deadline_ms;
                dispatch_output(
                    output,
                    &mut registry,
                    event_sink.as_mut(),
                    &inner,
                    &mut retry_queues, &mut retry_queue_warned, &mut retry_queue_max_depth, &ifac_configs,
                );

                // Advance next_poll based on next_deadline_ms
                let interval = match next {
                    Some(deadline_ms) => {
                        let delta = deadline_ms.saturating_sub(now_ms);
                        Duration::from_millis(delta.clamp(1, 1000))
                    }
                    None => Duration::from_secs(1),
                };
                next_poll = tokio::time::Instant::now() + interval;
            }

            // Branch 4: Shutdown
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("Node shutdown requested");
                    break;
                }
            }

            // Branch 5: Dynamic interface registration (TCP server, local server accept loops)
            Some(handle) = new_interface_rx.recv() => {
                tracing::info!("New connection: {} ({})", handle.info.name, handle.info.id);
                let is_local = handle.info.is_local_client;
                let iface_idx = handle.info.id.0;
                let inherited_ifac = handle.info.ifac.clone();
                {
                    let mut core = inner.lock().unwrap();
                    core.set_interface_name(iface_idx, handle.info.name.clone());
                    if let Some(hw_mtu) = handle.info.hw_mtu {
                        core.set_interface_hw_mtu(iface_idx, hw_mtu);
                    }
                    if is_local {
                        core.set_interface_local_client(iface_idx, true);
                    }
                    // Inherit IFAC config from parent interface (e.g., TCP server listener).
                    // Removal path: handle_interface_down removes ifac_config when connection drops.
                    if let Some(ifac) = &inherited_ifac {
                        core.set_ifac_config(iface_idx, ifac.clone());
                    }
                }
                // Mirror inherited IFAC in driver-local ifac_configs for dispatch_actions.
                if let Some(ifac) = inherited_ifac {
                    ifac_configs.insert(iface_idx, ifac);
                }
                {
                    let mut stats = iface_stats_map.lock().unwrap();
                    stats.insert(iface_idx, Arc::clone(&handle.counters));
                }
                {
                    let mut online = iface_online_map.lock().unwrap();
                    online.insert(iface_idx, true);
                }
                registry.register(handle);

                // Send cached local-destination announces on the new interface
                // so the new peer learns about our destinations even if the
                // original announce was sent before the connection was established.
                if !is_local {
                    let output = {
                        let mut core = inner.lock().unwrap();
                        core.handle_interface_up(iface_idx)
                    };
                    dispatch_output(output, &mut registry, event_sink.as_mut(), &inner, &mut retry_queues, &mut retry_queue_warned, &mut retry_queue_max_depth, &ifac_configs);
                }
            }

            // Branch 6: TCP client reconnection (Block D)
            //
            // When a reconnecting TCP client re-establishes its connection, it
            // sends a notification here. We call handle_interface_up() to
            // re-announce all local destinations (daemon-owned get fresh announces,
            // client-cached get rebroadcast) so the remote peer re-learns paths.
            Some(iface_id) = reconnect_rx.recv() => {
                tracing::info!("Interface {} reconnected, re-announcing destinations", iface_id);
                // Re-apply IFAC config to core (E29: handle_interface_down removed it)
                if let Some(cfg) = ifac_configs.get(&iface_id.0) {
                    let mut core = inner.lock().unwrap();
                    core.set_ifac_config(iface_id.0, cfg.clone());
                }
                let output = {
                    let mut core = inner.lock().unwrap();
                    core.handle_interface_up(iface_id.0)
                };
                dispatch_output(output, &mut registry, event_sink.as_mut(), &inner, &mut retry_queues, &mut retry_queue_warned, &mut retry_queue_max_depth, &ifac_configs);
            }

            // Branch 7: Periodic storage flush (persist identities + packet hashes)
            _ = tokio::time::sleep_until(next_flush) => {
                {
                    use reticulum_core::traits::Storage as _;
                    let mut core = inner.lock().unwrap();
                    core.storage_mut().flush();
                }
                next_flush = tokio::time::Instant::now() + Duration::from_secs(flush_interval_secs);
            }
        }
    }
}

/// Dispatch a TickOutput: drain retry queues, route Actions to interfaces, forward Events.
///
/// `event_sink` is `None` when the node was built with `without_events()`;
/// in that case, `output.events` is dropped at the end of this function
/// without being forwarded — identical to the NRF daemon path, where
/// the events vector simply falls out of scope.
#[allow(clippy::too_many_arguments)]
fn dispatch_output(
    output: TickOutput,
    registry: &mut InterfaceRegistry,
    event_sink: Option<&mut EventSink>,
    inner: &Arc<Mutex<StdNodeCore>>,
    retry_queues: &mut BTreeMap<usize, VecDeque<Vec<u8>>>,
    retry_queue_warned: &mut std::collections::BTreeSet<usize>,
    retry_queue_max_depth: &mut BTreeMap<usize, usize>,
    ifac_configs: &BTreeMap<usize, reticulum_core::ifac::IfacConfig>,
) {
    // Drain retry queues before dispatching new actions
    let drain_now_ms = inner.lock().unwrap().now_ms();
    drain_retry_queues(retry_queues, registry, drain_now_ms);

    // Dispatch new actions to interfaces (protocol logic in core)
    let mut ifaces: Vec<&mut dyn reticulum_core::traits::Interface> = registry
        .handles_mut_slice()
        .iter_mut()
        .map(|h| h as &mut dyn reticulum_core::traits::Interface)
        .collect();
    let result =
        reticulum_core::transport::dispatch_actions(&mut ifaces, output.actions, ifac_configs);

    // Log dispatch errors
    for (iface_id, error) in &result.errors {
        match error {
            InterfaceError::BufferFull => {
                tracing::trace!("Interface {} buffer full", iface_id);
            }
            InterfaceError::Disconnected => {
                tracing::warn!("Interface {} disconnected during dispatch", iface_id);
            }
        }
    }

    // Queue SendPacket retries (with cap enforcement)
    for retry in result.retries {
        let iface_idx = retry.iface_idx;
        let queue = retry_queues.entry(iface_idx).or_default();
        if queue.len() >= RETRY_QUEUE_CAP {
            queue.pop_front();
            tracing::warn!(
                "Retry queue full for iface {}, dropping oldest packet",
                iface_idx,
            );
        }
        push_retry_with_warn(
            queue,
            iface_idx,
            retry.data,
            retry_queue_warned,
            retry_queue_max_depth,
        );
    }

    // Remove empty queues to avoid accumulating stale entries.
    // Transport reads per-interface readiness from the
    // interface_next_slot_ms backchannel.
    retry_queues.retain(|_, queue| !queue.is_empty());

    // Clear the per-queue warned flag when the queue drops back
    // below RETRY_QUEUE_DEPTH_WARN so a future re-crossing re-emits
    // the warning. Also drop entries for queues that no longer exist.
    retry_queue_warned.retain(|idx| {
        retry_queues
            .get(idx)
            .map(|q| q.len() >= RETRY_QUEUE_DEPTH_WARN)
            .unwrap_or(false)
    });

    // Push per-interface next_slot_ms + max_airtime_ms into the
    // Transport backchannels. Transport can't hold handles
    // sans-I/O), so the driver mirrors both quantities here.
    // next_slot_ms is read by the announce-retry / direct-send
    // path; max_airtime_ms feeds the jitter-window helper that
    // scales announce retry randomness with the slowest link's
    // airtime.
    push_interface_state(registry, inner);

    // Forward events to the application via the split-plane EventSink:
    // control events lossless-by-default (overflow surfaced via
    // ControlPlaneOverflow), data events droppable under load (Codeberg #71).
    // When event_sink is None (daemon-mode, built via `without_events()`),
    // events are dropped here without forwarding — the events vector
    // simply falls out of scope at the end of this function.
    if let Some(event_sink) = event_sink {
        for event in output.events {
            if let NodeEvent::LinkEstablished { link_id, .. } = &event {
                tracing::debug!("Link established: {:?}", link_id);
            }
            event_sink.emit(event);
        }
    }
}

/// Append `data` to the per-interface retry queue. Emit a single
/// tracing::warn when the queue depth first crosses
/// `RETRY_QUEUE_DEPTH_WARN`; update the monotonic max-depth high-
/// watermark and log at info! whenever it increases.
fn push_retry_with_warn(
    queue: &mut VecDeque<Vec<u8>>,
    iface_idx: usize,
    data: Vec<u8>,
    warned: &mut std::collections::BTreeSet<usize>,
    max_depth: &mut BTreeMap<usize, usize>,
) {
    let len_before = queue.len();
    queue.push_back(data);
    if len_before < RETRY_QUEUE_DEPTH_WARN
        && queue.len() == RETRY_QUEUE_DEPTH_WARN
        && !warned.contains(&iface_idx)
    {
        tracing::warn!(
            iface = iface_idx,
            depth = queue.len(),
            "retry queue depth high, first-order backpressure may be mis-tuned"
        );
        warned.insert(iface_idx);
    }
    // E2: monotonic max-depth watermark. Log at info! only when the
    // watermark actually advances, benchmarks can grep for this.
    let prev = max_depth.get(&iface_idx).copied().unwrap_or(0);
    if queue.len() > prev {
        max_depth.insert(iface_idx, queue.len());
        tracing::info!(
            iface = iface_idx,
            max_depth = queue.len(),
            "retry_queue max depth increased"
        );
    }
}

/// Compute the next wall-clock deadline at which any packet in the
/// retry queues becomes eligible to drain. Returns the MINIMUM over
/// all non-empty queues of `handle.next_slot_ms(front.len(), now)`.
/// `None` iff every retry queue is empty.
///
/// Used by run_event_loop to schedule a sleep_until arm so idle nodes
/// with retry-queued packets still drain at the right moment, no
/// polling, no fixed 500 ms fallback.
fn compute_retry_wake_deadline_ms(
    retry_queues: &BTreeMap<usize, VecDeque<Vec<u8>>>,
    registry: &InterfaceRegistry,
    now_ms: u64,
) -> Option<u64> {
    use reticulum_core::traits::Interface;
    let mut min_slot: Option<u64> = None;
    for (&iface_idx, queue) in retry_queues.iter() {
        let Some(front) = queue.front() else { continue };
        if let Some(handle) = registry.handles().iter().find(|h| h.id().0 == iface_idx) {
            let slot = handle.next_slot_ms(front.len(), now_ms);
            // Only count slots strictly in the future; ready slots don't
            // need waking, they'd drain at the next normal dispatch tick.
            if slot > now_ms {
                match min_slot {
                    Some(current) if slot < current => min_slot = Some(slot),
                    None => min_slot = Some(slot),
                    _ => {}
                }
            } else {
                // A ready queue head means we can drain NOW, return
                // None so the caller doesn't sleep at all.
                return None;
            }
        }
    }
    min_slot
}

/// Drain per-interface retry queues in-place, honouring per-packet
/// airtime gating. Before calling try_send, ask the handle's
/// `next_slot_ms` for the actual packet size. Transport's MTU-sized
/// backchannel cache is conservative for smaller packets, and the
/// drain's finer granularity recovers that headroom. Extracted so it
/// is unit-testable without spinning up the full driver.
fn drain_retry_queues(
    retry_queues: &mut BTreeMap<usize, VecDeque<Vec<u8>>>,
    registry: &mut InterfaceRegistry,
    now_ms: u64,
) {
    use reticulum_core::traits::Interface;
    for (iface_idx, queue) in retry_queues.iter_mut() {
        let iface_id = InterfaceId(*iface_idx);
        while let Some(data) = queue.front() {
            if let Some(handle) = registry
                .handles_mut_slice()
                .iter_mut()
                .find(|h| h.id() == iface_id)
            {
                if handle.next_slot_ms(data.len(), now_ms) > now_ms {
                    // Interface not yet ready for THIS packet size, leave
                    // it at the front, try next dispatch tick (driver-local
                    // wake in E3 will fire at the computed slot).
                    break;
                }
                // Retry queue only holds SendPacket data (directed traffic),
                // which is always high priority.
                match handle.try_send_prioritized(data, true) {
                    Ok(()) => {
                        queue.pop_front();
                    }
                    Err(InterfaceError::BufferFull) => break,
                    Err(InterfaceError::Disconnected) => {
                        queue.clear();
                        break;
                    }
                }
            } else {
                // Interface removed, clear queue
                queue.clear();
                break;
            }
        }
    }
}

/// Mirror each interface's per-tick state into Transport's
/// backchannels. Pushes `next_slot_ms(MTU, now_ms)` for the
/// readiness cache and, for LoRa-Serial interfaces with an airtime
/// credit bucket, the worst-case airtime that drives the jitter
/// window for announce retries. Non-LoRa interfaces have
/// `credit == None` and are simply skipped for the airtime push;
/// Transport's getter falls back to the legacy floor when no
/// interface contributes.
///
/// Extracted so it is unit-testable without spinning up the full
/// driver; called from `dispatch_output`.
fn push_interface_state(registry: &mut InterfaceRegistry, inner: &Arc<Mutex<StdNodeCore>>) {
    use reticulum_core::traits::Interface;
    let now_ms = inner.lock().unwrap().now_ms();
    let mut core = inner.lock().unwrap();
    for handle in registry.handles_mut_slice().iter_mut() {
        let mtu = handle.mtu();
        let iface_idx = handle.id().0;
        let slot = handle.next_slot_ms(mtu, now_ms);
        core.set_interface_next_slot_ms(iface_idx, slot);
        if let Some(credit) = handle.credit.as_ref() {
            let max_airtime = credit
                .lock()
                .expect("airtime credit mutex poisoned")
                .max_airtime_ms();
            core.set_interface_max_airtime_ms(iface_idx, max_airtime);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default builder leaves the event channel enabled. The first
    /// `take_event_receiver()` returns the receiver; second call returns
    /// `None` (already taken).
    #[test]
    fn builder_default_events_enabled() {
        let td = tempfile::tempdir().expect("tempdir");
        let mut node = ReticulumNodeBuilder::new()
            .storage_path(td.path().to_path_buf())
            .build_sync()
            .expect("build_sync failed");

        assert!(
            node.control_tx.is_some() && node.data_tx.is_some(),
            "default build must keep both event planes on"
        );
        assert!(
            node.take_event_receiver().is_some(),
            "default build must hand out a receiver"
        );
        assert!(
            node.take_event_receiver().is_none(),
            "second take must return None"
        );
    }

    /// `without_events()` skips construction of the event channel; the
    /// node has neither sender nor receiver, so daemon-mode build never
    /// queues `NodeEvent`s.
    #[test]
    fn builder_without_events_disables_event_channel() {
        let td = tempfile::tempdir().expect("tempdir");
        let mut node = ReticulumNodeBuilder::new()
            .storage_path(td.path().to_path_buf())
            .without_events()
            .build_sync()
            .expect("build_sync failed");

        assert!(
            node.control_tx.is_none() && node.data_tx.is_none(),
            "daemon-mode build must not have event senders"
        );
        assert!(
            node.take_event_receiver().is_none(),
            "daemon-mode build must not hand out a receiver"
        );
    }

    /// `dispatch_output` with `event_tx = None` accepts a TickOutput
    /// containing events and consumes them silently (no panic, no try_send,
    /// no warn). Mirrors the NRF daemon path where `output.events` simply
    /// falls out of scope.
    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_output_skips_event_forwarding_when_disabled() {
        use reticulum_core::node::{NodeCoreBuilder, NodeEvent};
        use reticulum_core::transport::TickOutput;
        use reticulum_core::DestinationHash;

        let tmp =
            std::env::temp_dir().join(format!("without-events-dispatch-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let core: Arc<Mutex<StdNodeCore>> = {
            let node = NodeCoreBuilder::new().enable_transport(true).build(
                rand_core::OsRng,
                SystemClock::new(),
                crate::storage::Storage::new(&tmp).unwrap(),
            );
            Arc::new(Mutex::new(node))
        };

        let mut registry = InterfaceRegistry::new();
        let mut retry_queues: BTreeMap<usize, VecDeque<Vec<u8>>> = BTreeMap::new();
        let mut retry_queue_warned: std::collections::BTreeSet<usize> =
            std::collections::BTreeSet::new();
        let mut retry_queue_max_depth: BTreeMap<usize, usize> = BTreeMap::new();
        let ifac_configs: BTreeMap<usize, reticulum_core::ifac::IfacConfig> = BTreeMap::new();

        let mut output = TickOutput::empty();
        output.events.push(NodeEvent::PathLost {
            destination_hash: DestinationHash::new([0xAA; 16]),
        });
        output.events.push(NodeEvent::InterfaceDown(7));

        // event_tx = None, the function must accept this and simply drop
        // the events. No panic, no channel send.
        dispatch_output(
            output,
            &mut registry,
            None,
            &core,
            &mut retry_queues,
            &mut retry_queue_warned,
            &mut retry_queue_max_depth,
            &ifac_configs,
        );
    }

    /// Build a connected control/data sink + merged receiver for the
    /// split-channel tests (Codeberg #71).
    fn sink_and_receiver(control_cap: usize, data_cap: usize) -> (EventSink, EventReceiver) {
        let (control_tx, control_rx) = mpsc::channel(control_cap);
        let (data_tx, data_rx) = mpsc::channel(data_cap);
        (
            EventSink {
                control_tx,
                data_tx,
                control_capacity: control_cap,
                control_dropped: 0,
            },
            EventReceiver {
                control: control_rx,
                data: data_rx,
            },
        )
    }

    fn path_found(i: usize) -> NodeEvent {
        NodeEvent::PathFound {
            destination_hash: reticulum_core::DestinationHash::new([0xAB; 16]),
            hops: (i % 256) as u8,
            interface_index: i,
        }
    }

    /// Adapted from emoore's PR #71 repro
    /// (`control_plane_burst_lossless_to_draining_consumer`). Their unbounded
    /// channel accepted a burst of `EVENT_CHANNEL_CAPACITY * 4`; our bounded
    /// control plane is lossless *up to its configured capacity*. So we burst
    /// exactly capacity control events into an empty channel and require all
    /// of them, in order, at a draining consumer — the property the old single
    /// bounded `try_send` channel violated by silently dropping once full.
    #[tokio::test]
    async fn control_plane_burst_lossless_to_draining_consumer() {
        let cap = crate::config::DEFAULT_CONTROL_CHANNEL_CAPACITY;
        // Tiny data plane to prove the control plane is independent of it.
        let (mut sink, mut rx) = sink_and_receiver(cap, 16);

        for i in 0..cap {
            sink.emit(path_found(i));
        }

        for i in 0..cap {
            match rx.recv().await {
                Some(NodeEvent::PathFound {
                    hops,
                    interface_index,
                    ..
                }) => {
                    assert_eq!(
                        interface_index, i,
                        "control events must arrive in order with none dropped"
                    );
                    assert_eq!(hops, (i % 256) as u8, "event payload must be intact");
                }
                other => panic!("expected PathFound #{i}, got {other:?}"),
            }
        }
    }

    /// The property emoore's unbounded channel broke: the DATA plane must stay
    /// bounded and drop under load rather than grow without limit. Emitting
    /// far more data events than the data capacity, with no concurrent drain,
    /// must leave at most `data_cap` buffered.
    #[tokio::test]
    async fn data_plane_stays_bounded_and_drops_under_load() {
        let data_cap = 8;
        let (mut sink, mut rx) = sink_and_receiver(16, data_cap);

        let burst = data_cap * 8;
        for i in 0..burst {
            sink.emit(NodeEvent::PacketReceived {
                destination: reticulum_core::DestinationHash::new([0x11; 16]),
                data: vec![i as u8],
                interface_index: i,
            });
        }

        let mut count = 0;
        while let Ok(ev) = rx.try_recv() {
            assert!(
                matches!(ev, NodeEvent::PacketReceived { .. }),
                "only data events expected"
            );
            count += 1;
        }
        assert_eq!(
            count, data_cap,
            "data plane must be bounded at its capacity (backpressure preserved)"
        );
    }

    /// Overflowing the bounded control channel must be VISIBLE: the dropped
    /// events are counted and surfaced as a single
    /// `ControlPlaneOverflow {{ dropped_count }}` once the channel has room.
    /// The marker itself is never lost, and the counter resets after delivery.
    #[tokio::test]
    async fn control_overflow_delivers_visible_marker() {
        let cap = 4;
        let (mut sink, mut rx) = sink_and_receiver(cap, 4);

        // Fill the control channel to capacity (all delivered)...
        for i in 0..cap {
            sink.emit(path_found(i));
        }
        // ...then emit three more that cannot fit: dropped and counted.
        let dropped = 3usize;
        for i in 0..dropped {
            sink.emit(path_found(100 + i));
        }

        // Drain everything currently buffered so the channel has headroom for
        // both the next real event and the overflow marker behind it.
        for _ in 0..cap {
            assert!(matches!(rx.try_recv(), Ok(NodeEvent::PathFound { .. })));
        }

        // One more control event lands AND carries the overflow marker behind
        // it (emit_control flushes the marker once an event proves there's room).
        sink.emit(path_found(200));
        assert!(
            matches!(
                rx.try_recv(),
                Ok(NodeEvent::PathFound {
                    interface_index: 200,
                    ..
                })
            ),
            "the real event is delivered first"
        );
        match rx.try_recv() {
            Ok(NodeEvent::ControlPlaneOverflow { dropped_count }) => {
                assert_eq!(
                    dropped_count, dropped as u64,
                    "marker must report exactly the number of dropped control events"
                );
            }
            other => panic!("expected ControlPlaneOverflow {{{dropped}}}, got {other:?}"),
        }

        // Counter reset: no spurious second marker.
        sink.emit(path_found(201));
        assert!(matches!(
            rx.try_recv(),
            Ok(NodeEvent::PathFound {
                interface_index: 201,
                ..
            })
        ));
        assert!(
            rx.try_recv().is_err(),
            "no second overflow marker after the count was reset"
        );
    }

    /// Strict priority: a backlog of data events must never delay a control
    /// event. With both planes non-empty, `recv` returns control first.
    #[tokio::test]
    async fn control_plane_drained_before_data_plane() {
        let (mut sink, mut rx) = sink_and_receiver(8, 8);
        // Queue data first, then a single control event.
        for i in 0..4 {
            sink.emit(NodeEvent::PacketReceived {
                destination: reticulum_core::DestinationHash::new([0x22; 16]),
                data: vec![i as u8],
                interface_index: i,
            });
        }
        sink.emit(path_found(7));

        // Despite arriving last, the control event is delivered first.
        match rx.recv().await {
            Some(NodeEvent::PathFound {
                interface_index: 7, ..
            }) => {}
            other => panic!("control event must come first, got {other:?}"),
        }
        // Then the data backlog follows.
        for i in 0..4 {
            match rx.recv().await {
                Some(NodeEvent::PacketReceived {
                    interface_index, ..
                }) => assert_eq!(interface_index, i),
                other => panic!("expected data #{i}, got {other:?}"),
            }
        }
    }

    /// Regression: the node's timer-driven event loop (`sleep_until`) and
    /// interface timers must work even when the *embedding* runtime was built
    /// without `enable_time()` — the PyO3/edge case that previously panicked
    /// the event-loop task on its first poll. The node owns its own
    /// time-enabled, single-worker runtime, so `start()` is independent of how
    /// the host configured its runtime.
    #[test]
    fn event_loop_survives_host_runtime_without_time_driver() {
        let td = tempfile::tempdir().expect("tempdir");
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(true)
            .storage_path(td.path().to_path_buf())
            .build_sync()
            .expect("build_sync");

        // Host runtime deliberately WITHOUT enable_time() (IO only) — mirrors an
        // embedder that built its runtime without timers.
        let host = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("host runtime");
        host.block_on(async {
            node.start().await.expect("start");
            // Let the event loop tick on the node's own runtime. OS sleep — the
            // host runtime has no timer to drive a tokio sleep.
            std::thread::sleep(std::time::Duration::from_millis(250));
            // Pre-fix the event loop panicked on its first `sleep_until` poll,
            // so its JoinHandle resolved to a JoinError and stop() returned Err.
            node.stop()
                .await
                .expect("stop — event loop must not have panicked");
        });
    }

    /// Regression for the runtime-cleanup-on-error path: when interface init
    /// fails *after* the node runtime is built, start() must return the error —
    /// not panic by blocking-dropping the Runtime inside the host's async
    /// context.
    #[test]
    fn start_surfaces_interface_init_error_without_panicking() {
        // Occupy a port so the node's TCP server bind fails during init.
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe");
        let busy: std::net::SocketAddr = occupied.local_addr().expect("local_addr");

        let td = tempfile::tempdir().expect("tempdir");
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(true)
            .add_tcp_server(busy)
            .storage_path(td.path().to_path_buf())
            .build_sync()
            .expect("build_sync");

        let host = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("host runtime");
        host.block_on(async {
            // Pre-fix this panicked (blocking Runtime drop in async context);
            // post-fix it returns the bind error cleanly.
            let result = node.start().await;
            assert!(
                result.is_err(),
                "start() should surface the TCP bind failure, got {result:?}"
            );
        });
    }

    /// Regression for the `Drop` teardown path: dropping a started node from
    /// *inside* another runtime's async context must not panic. emoore's
    /// other tests exercise the `stop()` teardown; this one drops the node
    /// without calling `stop()`, so the node's owned `Runtime` is torn down by
    /// the `Drop` impl. A blocking `Runtime` drop inside an async context
    /// panics; the `Drop` impl uses `shutdown_background()` to avoid it.
    #[test]
    fn node_drops_cleanly_within_host_runtime_async_context() {
        let host = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("host runtime");
        host.block_on(async {
            let td = tempfile::tempdir().expect("tempdir");
            let mut node = ReticulumNodeBuilder::new()
                .enable_transport(true)
                .storage_path(td.path().to_path_buf())
                .build_sync()
                .expect("build_sync");
            node.start().await.expect("start");
            // Drop the live node (and its owned runtime) here, inside the host
            // runtime's async context. Pre-fix the blocking Runtime drop panicked;
            // post-fix `Drop`'s shutdown_background() returns without blocking.
            drop(node);
        });
    }

    #[test]
    fn test_reticulum_node_builder_creates_node() {
        let td = tempfile::tempdir().expect("tempdir");
        let node = ReticulumNodeBuilder::new()
            .enable_transport(true)
            .storage_path(td.path().to_path_buf())
            .build_sync()
            .expect("build_sync failed");

        assert!(node.is_transport_enabled());
        assert!(!node.is_running());
        assert_eq!(node.path_count(), 0);

        let fake_hash = reticulum_core::DestinationHash::new([0xFF; 16]);
        assert!(!node.has_path(&fake_hash));
        assert!(node.hops_to(&fake_hash).is_none());
    }

    /// push_retry_with_warn inserts an entry into the `warned` set
    /// the first time queue depth reaches RETRY_QUEUE_DEPTH_WARN.
    /// Subsequent pushes beyond the threshold do NOT re-insert.
    #[test]
    fn push_retry_warns_once_when_crossing_warn_depth() {
        let mut q: VecDeque<Vec<u8>> = VecDeque::new();
        let mut warned: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        let mut max_depth: BTreeMap<usize, usize> = BTreeMap::new();
        // Fill up to one below the warn threshold → never warns.
        for _ in 0..(RETRY_QUEUE_DEPTH_WARN - 1) {
            push_retry_with_warn(&mut q, 1, vec![0u8; 8], &mut warned, &mut max_depth);
        }
        assert!(
            !warned.contains(&1),
            "below-threshold depth must not trigger warn"
        );
        // Push one more → crosses threshold.
        push_retry_with_warn(&mut q, 1, vec![0u8; 8], &mut warned, &mut max_depth);
        assert!(
            warned.contains(&1),
            "reaching RETRY_QUEUE_DEPTH_WARN must trigger warn"
        );
        // Push past threshold → already warned, set membership unchanged (idempotent).
        push_retry_with_warn(&mut q, 1, vec![0u8; 8], &mut warned, &mut max_depth);
        assert!(warned.contains(&1));
        assert_eq!(warned.len(), 1, "no duplicate entries");
    }

    /// Clearing the warned flag (as dispatch_output does after the
    /// retain loop) allows a future re-crossing of the warn depth
    /// to re-emit.
    #[test]
    fn push_retry_rewarns_after_queue_drains_below_warn_depth() {
        let mut q: VecDeque<Vec<u8>> = VecDeque::new();
        let mut warned: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        let mut max_depth: BTreeMap<usize, usize> = BTreeMap::new();
        for _ in 0..RETRY_QUEUE_DEPTH_WARN {
            push_retry_with_warn(&mut q, 2, vec![0u8; 8], &mut warned, &mut max_depth);
        }
        assert!(warned.contains(&2));
        // Drain below the warn threshold (simulate: clear queue,
        // clear warned per the retain-clause in dispatch_output).
        q.clear();
        warned.retain(|idx| {
            let _ = idx;
            // Mirror dispatch_output's clause:
            // keep only if queue.len() >= RETRY_QUEUE_DEPTH_WARN
            false // queue is empty now
        });
        assert!(!warned.contains(&2));
        // Rebuild to threshold → warn re-emitted.
        for _ in 0..RETRY_QUEUE_DEPTH_WARN {
            push_retry_with_warn(&mut q, 2, vec![0u8; 8], &mut warned, &mut max_depth);
        }
        assert!(warned.contains(&2));
    }

    /// max_depth is monotonic and tracks the high-watermark per
    /// interface index.
    #[test]
    fn push_retry_tracks_monotonic_max_depth() {
        let mut q: VecDeque<Vec<u8>> = VecDeque::new();
        let mut warned: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        let mut max_depth: BTreeMap<usize, usize> = BTreeMap::new();
        for _ in 0..5 {
            push_retry_with_warn(&mut q, 3, vec![0u8; 4], &mut warned, &mut max_depth);
        }
        assert_eq!(max_depth.get(&3), Some(&5));
        // Drain the queue manually; max_depth must NOT regress.
        q.clear();
        // A single push after drain puts len=1, watermark stays at 5.
        push_retry_with_warn(&mut q, 3, vec![0u8; 4], &mut warned, &mut max_depth);
        assert_eq!(max_depth.get(&3), Some(&5), "watermark must be monotonic");
        // Re-fill past the old watermark → grows.
        for _ in 0..10 {
            push_retry_with_warn(&mut q, 3, vec![0u8; 4], &mut warned, &mut max_depth);
        }
        assert_eq!(max_depth.get(&3), Some(&11));
    }

    /// compute_retry_wake_deadline_ms returns `None` when every retry
    /// queue is empty, no wake needed.
    #[tokio::test(flavor = "current_thread")]
    async fn compute_retry_wake_none_when_queues_empty() {
        let registry = InterfaceRegistry::new();
        let retry_queues: BTreeMap<usize, VecDeque<Vec<u8>>> = BTreeMap::new();
        assert_eq!(
            compute_retry_wake_deadline_ms(&retry_queues, &registry, 1_000),
            None
        );
    }

    /// Queues with a ready head → return None so the caller doesn't
    /// sleep (drain would already happen on the next normal tick).
    #[tokio::test(flavor = "current_thread")]
    async fn compute_retry_wake_none_when_any_head_ready() {
        use crate::interfaces::airtime::AirtimeCredit;
        use crate::interfaces::{InterfaceCounters, InterfaceHandle, InterfaceInfo};
        use reticulum_core::transport::InterfaceId;

        let mut registry = InterfaceRegistry::new();
        let (_inc_tx, inc_rx) = tokio::sync::mpsc::channel(4);
        let (out_tx, _out_rx) = tokio::sync::mpsc::channel(4);
        registry.register(InterfaceHandle {
            info: InterfaceInfo {
                id: InterfaceId(0),
                name: "ready".into(),
                hw_mtu: None,
                is_local_client: false,
                bitrate: None,
                ifac: None,
            },
            incoming: inc_rx,
            outgoing: out_tx,
            counters: Arc::new(InterfaceCounters::new()),
            credit: None, // always-ready
            ready: crate::interfaces::ReadySignal::ready_immediate(),
        });
        let mut retry_queues = BTreeMap::new();
        retry_queues
            .entry(0usize)
            .or_insert_with(VecDeque::new)
            .push_back(vec![1, 2, 3]);
        assert_eq!(
            compute_retry_wake_deadline_ms(&retry_queues, &registry, 1_000),
            None,
            "ready interface should short-circuit to None"
        );
        // Silence unused-import warning on non-LoRa path.
        let _ = AirtimeCredit::new(125_000, 10, 8, 500);
    }

    /// When a queue head is NOT ready, return the MINIMUM over all
    /// not-ready heads' slot times.
    #[tokio::test(flavor = "current_thread")]
    async fn compute_retry_wake_returns_min_future_slot() {
        use crate::interfaces::airtime::AirtimeCredit;
        use crate::interfaces::{InterfaceCounters, InterfaceHandle, InterfaceInfo};
        use reticulum_core::transport::InterfaceId;

        let mut registry = InterfaceRegistry::new();
        let now_ms = 1_000;

        // Two LoRa handles with different saturation, both have
        // not-ready heads; the earlier slot should win.
        for (idx, payload_charge) in [(0usize, 500u32), (1usize, 100u32)] {
            let mut credit = AirtimeCredit::new(125_000, 10, 8, 500);
            credit.try_charge(payload_charge, now_ms).unwrap();
            let (_inc_tx, inc_rx) = tokio::sync::mpsc::channel(4);
            let (out_tx, _out_rx) = tokio::sync::mpsc::channel(4);
            registry.register(InterfaceHandle {
                info: InterfaceInfo {
                    id: InterfaceId(idx),
                    name: format!("lora-{idx}"),
                    hw_mtu: None,
                    is_local_client: false,
                    bitrate: None,
                    ifac: None,
                },
                incoming: inc_rx,
                outgoing: out_tx,
                counters: Arc::new(InterfaceCounters::new()),
                credit: Some(Arc::new(Mutex::new(credit))),
                ready: crate::interfaces::ReadySignal::ready_immediate(),
            });
        }
        // Both queues carry a full-MTU packet, both heads are
        // definitely not-ready because the buckets were charged at
        // different magnitudes.
        let mut retry_queues = BTreeMap::new();
        retry_queues
            .entry(0usize)
            .or_insert_with(VecDeque::new)
            .push_back(vec![0u8; 500]);
        retry_queues
            .entry(1usize)
            .or_insert_with(VecDeque::new)
            .push_back(vec![0u8; 500]);

        let iface0_slot = {
            let handles = registry.handles();
            use reticulum_core::traits::Interface;
            handles[0].next_slot_ms(500, now_ms)
        };
        let iface1_slot = {
            let handles = registry.handles();
            use reticulum_core::traits::Interface;
            handles[1].next_slot_ms(500, now_ms)
        };
        let expected_min = iface0_slot.min(iface1_slot);
        assert!(expected_min > now_ms);

        assert_eq!(
            compute_retry_wake_deadline_ms(&retry_queues, &registry, now_ms),
            Some(expected_min)
        );
    }

    /// drain_retry_queues honors next_slot_ms. A ready interface
    /// drains its packet; a saturated interface leaves the packet at
    /// the queue front.
    #[tokio::test(flavor = "current_thread")]
    async fn drain_retry_queues_skips_saturated_and_drains_ready() {
        use crate::interfaces::airtime::AirtimeCredit;
        use crate::interfaces::{InterfaceCounters, InterfaceHandle, InterfaceInfo};
        use reticulum_core::transport::InterfaceId;

        let mut registry = InterfaceRegistry::new();

        // LoRa handle (iface_idx=1), saturated bucket.
        let mut saturated = AirtimeCredit::new(125_000, 10, 8, 500);
        saturated.try_charge(500, 0).unwrap();
        let (_li, l_inc_rx) = tokio::sync::mpsc::channel(4);
        let (l_out_tx, mut l_out_rx) = tokio::sync::mpsc::channel(4);
        registry.register(InterfaceHandle {
            info: InterfaceInfo {
                id: InterfaceId(1),
                name: "lora".into(),
                hw_mtu: Some(500),
                is_local_client: false,
                bitrate: None,
                ifac: None,
            },
            incoming: l_inc_rx,
            outgoing: l_out_tx,
            counters: Arc::new(InterfaceCounters::new()),
            credit: Some(Arc::new(Mutex::new(saturated))),
            ready: crate::interfaces::ReadySignal::ready_immediate(),
        });

        // Plain handle (iface_idx=2), credit = None (always ready).
        let (_pi, p_inc_rx) = tokio::sync::mpsc::channel(4);
        let (p_out_tx, mut p_out_rx) = tokio::sync::mpsc::channel(4);
        registry.register(InterfaceHandle {
            info: InterfaceInfo {
                id: InterfaceId(2),
                name: "plain".into(),
                hw_mtu: None,
                is_local_client: false,
                bitrate: None,
                ifac: None,
            },
            incoming: p_inc_rx,
            outgoing: p_out_tx,
            counters: Arc::new(InterfaceCounters::new()),
            credit: None,
            ready: crate::interfaces::ReadySignal::ready_immediate(),
        });

        // Queue one packet on each interface.
        let mut retry_queues: BTreeMap<usize, VecDeque<Vec<u8>>> = BTreeMap::new();
        retry_queues
            .entry(1)
            .or_default()
            .push_back(vec![0xAA; 100]);
        retry_queues
            .entry(2)
            .or_default()
            .push_back(vec![0xBB; 100]);

        drain_retry_queues(&mut retry_queues, &mut registry, 0);

        // Saturated LoRa: packet still at front.
        assert_eq!(retry_queues.get(&1).map(|q| q.len()), Some(1));
        // Plain: packet drained.
        assert_eq!(retry_queues.get(&2).map(|q| q.len()), Some(0));
        // And the plain interface's outgoing channel received the packet.
        assert!(p_out_rx.try_recv().is_ok());
        // Saturated: nothing went to outgoing.
        assert!(l_out_rx.try_recv().is_err());
    }

    /// A ready interface (no credit) drains repeatedly across retries.
    #[tokio::test(flavor = "current_thread")]
    async fn drain_retry_queues_drains_all_ready_packets() {
        use crate::interfaces::{InterfaceCounters, InterfaceHandle, InterfaceInfo};
        use reticulum_core::transport::InterfaceId;

        let mut registry = InterfaceRegistry::new();
        let (_pi, p_inc_rx) = tokio::sync::mpsc::channel(4);
        let (p_out_tx, mut p_out_rx) = tokio::sync::mpsc::channel(4);
        registry.register(InterfaceHandle {
            info: InterfaceInfo {
                id: InterfaceId(0),
                name: "tcp".into(),
                hw_mtu: None,
                is_local_client: false,
                bitrate: None,
                ifac: None,
            },
            incoming: p_inc_rx,
            outgoing: p_out_tx,
            counters: Arc::new(InterfaceCounters::new()),
            credit: None,
            ready: crate::interfaces::ReadySignal::ready_immediate(),
        });
        let mut retry_queues: BTreeMap<usize, VecDeque<Vec<u8>>> = BTreeMap::new();
        let queue = retry_queues.entry(0).or_default();
        queue.push_back(vec![1, 2, 3]);
        queue.push_back(vec![4, 5, 6]);
        queue.push_back(vec![7, 8, 9]);

        drain_retry_queues(&mut retry_queues, &mut registry, 0);

        assert_eq!(retry_queues.get(&0).map(|q| q.len()), Some(0));
        let mut received = 0;
        while p_out_rx.try_recv().is_ok() {
            received += 1;
        }
        assert_eq!(received, 3);
    }

    /// push_interface_state copies per-interface next_slot_ms into
    /// Transport's backchannel. Build a synthetic registry with one
    /// LoRa (saturated bucket → future slot) and one non-LoRa (default
    /// → now_ms), run the push, assert Transport reflects both.
    #[tokio::test(flavor = "current_thread")]
    async fn push_interface_state_mirrors_per_handle_values() {
        use crate::interfaces::airtime::AirtimeCredit;
        use crate::interfaces::{InterfaceCounters, InterfaceHandle, InterfaceInfo};
        use reticulum_core::transport::InterfaceId;
        use std::sync::atomic::Ordering;
        let _ = Ordering::Relaxed; // silences unused-import on minor builds

        // Minimal StdNodeCore in Arc<Mutex>.
        let tmp = std::env::temp_dir().join(format!("bug3-phase2a-c3-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let core: Arc<Mutex<StdNodeCore>> = {
            let node = reticulum_core::node::NodeCoreBuilder::new()
                .enable_transport(true)
                .build(
                    rand_core::OsRng,
                    SystemClock::new(),
                    crate::storage::Storage::new(&tmp).unwrap(),
                );
            Arc::new(Mutex::new(node))
        };

        // Construct two synthetic handles directly. Channel receivers
        // are dropped at end of test, that's fine since we don't call
        // try_send here, only next_slot_ms (which is &self).
        let mut registry = InterfaceRegistry::new();

        let (_lora_inc_tx, lora_inc_rx) = tokio::sync::mpsc::channel(4);
        let (lora_out_tx, _lora_out_rx) = tokio::sync::mpsc::channel(4);
        let mut lora_credit = AirtimeCredit::new(125_000, 10, 8, 500);
        // Exhaust to guarantee earliest_fit_time > 0.
        lora_credit.try_charge(500, 0).unwrap();
        let lora_handle = InterfaceHandle {
            info: InterfaceInfo {
                id: InterfaceId(1),
                name: "lora-test".into(),
                hw_mtu: Some(500),
                is_local_client: false,
                bitrate: None,
                ifac: None,
            },
            incoming: lora_inc_rx,
            outgoing: lora_out_tx,
            counters: Arc::new(InterfaceCounters::new()),
            credit: Some(Arc::new(Mutex::new(lora_credit))),
            ready: crate::interfaces::ReadySignal::ready_immediate(),
        };

        let (_plain_inc_tx, plain_inc_rx) = tokio::sync::mpsc::channel(4);
        let (plain_out_tx, _plain_out_rx) = tokio::sync::mpsc::channel(4);
        let plain_handle = InterfaceHandle {
            info: InterfaceInfo {
                id: InterfaceId(2),
                name: "plain-test".into(),
                hw_mtu: None,
                is_local_client: false,
                bitrate: None,
                ifac: None,
            },
            incoming: plain_inc_rx,
            outgoing: plain_out_tx,
            counters: Arc::new(InterfaceCounters::new()),
            credit: None,
            ready: crate::interfaces::ReadySignal::ready_immediate(),
        };
        registry.register(lora_handle);
        registry.register(plain_handle);

        // Run the push.
        push_interface_state(&mut registry, &core);

        // LoRa (idx=1, saturated): slot must be in the future relative to now_ms.
        let now_ms = core.lock().unwrap().now_ms();
        let lora_slot = core.lock().unwrap().next_slot_ms_for_interface(1, now_ms);
        assert!(
            lora_slot > now_ms,
            "saturated LoRa should map to future slot, got {lora_slot} vs now {now_ms}"
        );
        // Plain (idx=2, no credit): slot equals now_ms (trait default).
        let plain_slot = core.lock().unwrap().next_slot_ms_for_interface(2, now_ms);
        assert_eq!(plain_slot, now_ms, "non-LoRa should map to now_ms");
    }

    /// One LoRa-Serial handle at SF7 → Transport's
    /// announce_jitter_max_ms() reflects the SF7 airtime (which at
    /// 500 B is well below 167 ms, so the legacy 500 ms floor wins).
    /// Verifies the airtime push runs and the helper composes
    /// correctly. Use SF10 for a value the helper actually amplifies.
    #[tokio::test(flavor = "current_thread")]
    async fn push_interface_state_pushes_max_airtime_for_lora() {
        use crate::interfaces::airtime::AirtimeCredit;
        use crate::interfaces::{InterfaceCounters, InterfaceHandle, InterfaceInfo};
        use reticulum_core::transport::InterfaceId;

        let tmp = std::env::temp_dir().join(format!("bug19-a2-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let core: Arc<Mutex<StdNodeCore>> = {
            let node = reticulum_core::node::NodeCoreBuilder::new()
                .enable_transport(true)
                .build(
                    rand_core::OsRng,
                    SystemClock::new(),
                    crate::storage::Storage::new(&tmp).unwrap(),
                );
            Arc::new(Mutex::new(node))
        };

        let mut registry = InterfaceRegistry::new();
        let (_inc_tx, inc_rx) = tokio::sync::mpsc::channel(4);
        let (out_tx, _out_rx) = tokio::sync::mpsc::channel(4);
        let credit = AirtimeCredit::new(125_000, 10, 8, 500);
        let expected_airtime = credit.max_airtime_ms();
        let handle = InterfaceHandle {
            info: InterfaceInfo {
                id: InterfaceId(1),
                name: "lora-sf10".into(),
                hw_mtu: Some(500),
                is_local_client: false,
                bitrate: None,
                ifac: None,
            },
            incoming: inc_rx,
            outgoing: out_tx,
            counters: Arc::new(InterfaceCounters::new()),
            credit: Some(Arc::new(Mutex::new(credit))),
            ready: crate::interfaces::ReadySignal::ready_immediate(),
        };
        registry.register(handle);

        push_interface_state(&mut registry, &core);

        let jitter = core.lock().unwrap().announce_jitter_max_ms();
        assert_eq!(
            jitter,
            (3 * expected_airtime).max(500),
            "jitter window should track SF10 airtime"
        );
    }

    /// A non-LoRa registry leaves the airtime map empty; the helper
    /// returns the legacy floor.
    #[tokio::test(flavor = "current_thread")]
    async fn push_interface_state_skips_airtime_for_non_lora() {
        use crate::interfaces::{InterfaceCounters, InterfaceHandle, InterfaceInfo};
        use reticulum_core::transport::InterfaceId;

        let tmp = std::env::temp_dir().join(format!("bug19-a2-non-lora-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let core: Arc<Mutex<StdNodeCore>> = {
            let node = reticulum_core::node::NodeCoreBuilder::new()
                .enable_transport(true)
                .build(
                    rand_core::OsRng,
                    SystemClock::new(),
                    crate::storage::Storage::new(&tmp).unwrap(),
                );
            Arc::new(Mutex::new(node))
        };

        let mut registry = InterfaceRegistry::new();
        let (_inc_tx, inc_rx) = tokio::sync::mpsc::channel(4);
        let (out_tx, _out_rx) = tokio::sync::mpsc::channel(4);
        let handle = InterfaceHandle {
            info: InterfaceInfo {
                id: InterfaceId(2),
                name: "tcp-test".into(),
                hw_mtu: None,
                is_local_client: false,
                bitrate: None,
                ifac: None,
            },
            incoming: inc_rx,
            outgoing: out_tx,
            counters: Arc::new(InterfaceCounters::new()),
            credit: None,
            ready: crate::interfaces::ReadySignal::ready_immediate(),
        };
        registry.register(handle);

        push_interface_state(&mut registry, &core);

        let jitter = core.lock().unwrap().announce_jitter_max_ms();
        assert_eq!(jitter, 500, "no LoRa interface ⇒ legacy floor");
    }

    /// Reconfiguring the bucket's radio params (SF7 → SF10) is picked
    /// up on the next push. Mirrors the live `send_radio_config` flow:
    /// the bucket's `update_radio_params` swaps params atomically; the
    /// next dispatch tick mirrors the new airtime into Transport.
    #[tokio::test(flavor = "current_thread")]
    async fn push_interface_state_picks_up_runtime_radio_reconfig() {
        use crate::interfaces::airtime::AirtimeCredit;
        use crate::interfaces::{InterfaceCounters, InterfaceHandle, InterfaceInfo};
        use reticulum_core::transport::InterfaceId;

        let tmp = std::env::temp_dir().join(format!("bug19-a2-reconfig-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let core: Arc<Mutex<StdNodeCore>> = {
            let node = reticulum_core::node::NodeCoreBuilder::new()
                .enable_transport(true)
                .build(
                    rand_core::OsRng,
                    SystemClock::new(),
                    crate::storage::Storage::new(&tmp).unwrap(),
                );
            Arc::new(Mutex::new(node))
        };

        let mut registry = InterfaceRegistry::new();
        let (_inc_tx, inc_rx) = tokio::sync::mpsc::channel(4);
        let (out_tx, _out_rx) = tokio::sync::mpsc::channel(4);
        let credit = Arc::new(Mutex::new(AirtimeCredit::new(125_000, 7, 5, 500)));
        let handle = InterfaceHandle {
            info: InterfaceInfo {
                id: InterfaceId(1),
                name: "lora-reconfig".into(),
                hw_mtu: Some(500),
                is_local_client: false,
                bitrate: None,
                ifac: None,
            },
            incoming: inc_rx,
            outgoing: out_tx,
            counters: Arc::new(InterfaceCounters::new()),
            credit: Some(credit.clone()),
            ready: crate::interfaces::ReadySignal::ready_immediate(),
        };
        registry.register(handle);

        push_interface_state(&mut registry, &core);
        let sf7_jitter = core.lock().unwrap().announce_jitter_max_ms();

        credit.lock().unwrap().update_radio_params(125_000, 10, 8);
        push_interface_state(&mut registry, &core);
        let sf10_jitter = core.lock().unwrap().announce_jitter_max_ms();

        assert!(
            sf10_jitter > sf7_jitter,
            "SF10 jitter ({sf10_jitter}) must exceed SF7 ({sf7_jitter}) after reconfig"
        );
    }
}
