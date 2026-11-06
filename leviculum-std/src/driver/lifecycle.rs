//! The state a run of the node owns, behind one lock so `start` and `stop`
//! work through a shared handle (+ciris, leviculum#90: mobile suspend and
//! resume, where the host holds the node as an `Arc` across many tasks).
//!
//! Everything `start` creates and `stop` tears down lives in [`RunState`]:
//! the shutdown signal, the event-loop task, the node's runtime, the senders
//! runtime-attached interfaces need, and the traffic-counter thread. The
//! node's identity, destinations, configuration and event channel stay on
//! the node itself, so a restarted node is the same node and an event
//! receiver taken before a stop keeps receiving after the next start.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use super::{InterfaceHandle, InterfaceId, ReticulumNode, TickOutput};
use crate::error::Error;
use crate::sync_ext::MutexRecover;

/// What one run of the node owns. Empty (all `None`) while stopped.
pub(crate) struct RunState {
    pub(crate) shutdown_tx: Option<watch::Sender<StopSignal>>,
    pub(crate) runner_handle: Option<tokio::task::JoinHandle<()>>,
    /// The node's own runtime, hosting the event loop and every interface
    /// task. Shut down by `stop`, which is what closes every socket.
    pub(crate) runtime: Option<tokio::runtime::Runtime>,
    /// Commits outside the event loop (connect, send, close, announce)
    /// dispatch through here. Before the first start it is a channel whose
    /// receiver is already gone, so a call fails as not running.
    pub(crate) action_dispatch_tx: mpsc::Sender<TickOutput>,
    pub(crate) running: Option<Running>,
    pub(crate) traffic_counter: Option<TrafficCounter>,
}

impl RunState {
    pub(crate) fn new(action_dispatch_tx: mpsc::Sender<TickOutput>) -> Self {
        RunState {
            shutdown_tx: None,
            runner_handle: None,
            runtime: None,
            action_dispatch_tx,
            running: None,
            traffic_counter: None,
        }
    }
}

/// The senders and allocator a runtime-attached interface needs, valid for
/// one run. Cloned out of the lock by each caller.
#[derive(Clone)]
pub(crate) struct Running {
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) new_iface_tx: mpsc::Sender<InterfaceHandle>,
    pub(crate) iface_id_counter: Arc<AtomicUsize>,
    pub(crate) reconnect_tx: mpsc::Sender<InterfaceId>,
    pub(crate) tunnel_notify_tx: mpsc::Sender<InterfaceId>,
    pub(crate) peer_event_tx: mpsc::Sender<(InterfaceId, crate::interfaces::PeerEvent)>,
    pub(crate) remove_iface_tx: mpsc::Sender<InterfaceId>,
}

/// The once-a-second traffic sampler of one run, and the flag that ends it.
/// Before leviculum#90 each `start` spawned one that ended only when the
/// node was dropped, so every suspend/resume cycle leaked a thread.
pub(crate) struct TrafficCounter {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl TrafficCounter {
    pub(crate) fn new(stop: Arc<AtomicBool>, thread: std::thread::JoinHandle<()>) -> Self {
        TrafficCounter { stop, thread }
    }

    /// Ask the thread to end and wait for it. It parks between samples, so
    /// the wait is a wake-up, not a sleep.
    pub(crate) fn end(self) {
        self.stop.store(true, Ordering::Release);
        self.thread.thread().unpark();
        let _ = self.thread.join();
    }
}

impl ReticulumNode {
    /// The per-run senders, or `NotRunning`.
    pub(crate) fn running(&self) -> Result<Running, Error> {
        self.run
            .lock_recover()
            .running
            .clone()
            .ok_or(Error::NotRunning)
    }

    /// The commit channel of the current run (see [`RunState`]).
    pub(crate) fn action_dispatch_tx(&self) -> mpsc::Sender<TickOutput> {
        self.run.lock_recover().action_dispatch_tx.clone()
    }
}

/// What the event loop is told about stopping (leviculum#97).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopSignal {
    /// Keep running.
    Run,
    /// Stop; links stay in the table, as [`ReticulumNode::stop`] always left
    /// them.
    ///
    /// [`ReticulumNode::stop`]: super::ReticulumNode::stop
    Stop,
    /// Stop, closing every link last thing (`stop_shared`): the loop is the
    /// only thing that can still land one, so once it has closed them
    /// nothing is left frozen for a host to find on a stopped node.
    StopClosingLinks,
}
