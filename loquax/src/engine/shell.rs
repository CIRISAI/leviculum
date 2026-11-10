//! The engine's shell: the driver it runs inside, the channels to the
//! frontend, the mining thread and its runtime.
//!
//! [`Engine`] is the core and holds its own queues; what this module adds is
//! everything that needs a process: a [`CoreProcessor`] for the driver's
//! `StdNodeCore` that moves those queues onto `mpsc` channels around every
//! hook, the [`InProcessOutbox`] the frontend holds, [`attach`], and the
//! stamp worker thread that runs [`super::mine`] on a current-thread tokio
//! runtime with `OsRng`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use std::sync::Arc;

use leviculum_core::identity::Identity;
use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;
use leviculum_core::{DestinationHash, LinkId, RequestPolicy};
use leviculum_std::driver::{CoreProcessor, ReticulumNodeBuilder, StdNodeCore};
use leviculum_std::ReticulumNode;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use super::{Engine, StampAnswer, StampJob};
use crate::clock;
use crate::effects::{Effects, Terminal};
use crate::outbox::{Command, Outbox, OutboxEvent, OutboxGone};
use crate::ui::strings::{self, Lang, StrId};

/// The engine as the driver sees it, with the frontend's and the miner's
/// ends of its queues.
struct Processor {
    engine: Engine,
    commands: Receiver<Command>,
    events: Sender<OutboxEvent>,
    stamps: Sender<StampJob>,
    stamp_answers: Receiver<StampAnswer>,
    /// On a home node: its home destination, and where requests to it go.
    home: Option<(DestinationHash, UnboundedSender<HomeRequest>)>,
    /// Where the events of a lent node go, while one is lent ([`Lent`]).
    lend: Tap,
}

/// How long a stop waits for a lent node's fetch to let go of it: one
/// call's await on a link or a request, which the fetch abandons once its
/// lend ended.
const LEND_LET_GO: std::time::Duration = std::time::Duration::from_secs(2);

/// One lent end: its events, and its end's signal. Empty while nothing is
/// lent; emptied when the lend ends.
type Slot = Option<(UnboundedSender<NodeEvent>, oneshot::Sender<()>)>;

/// Who a node is lent to. Each has its own slot, so the Browse tab and the
/// Rooms (1379) hold the node side by side and neither's lend ends the
/// other's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lender {
    /// The Browse tab's page fetch.
    Browse,
    /// The Rooms' hub links (`crate::rooms_link`).
    Rooms,
}

/// The taps of a lent node, one slot per [`Lender`].
#[derive(Default)]
struct Taps {
    browse: Slot,
    rooms: Slot,
}

impl Taps {
    fn of(&mut self, lender: Lender) -> &mut Slot {
        match lender {
            Lender::Browse => &mut self.browse,
            Lender::Rooms => &mut self.rooms,
        }
    }
}

type Tap = Arc<std::sync::Mutex<Taps>>;

/// The engine's node lent to a page fetch beside the engine (1337 pass
/// two, `docs/src/concepts/lnomad-everywhere.md` slice 3): the Browse tab
/// fetches through the node the phone already runs, not a second one.
/// The engine keeps the node, so the lend holds it weakly; the events are
/// the ones a fetch reads, forwarded by the processor tap as the home
/// requests are (`lends`).
pub struct Lent {
    pub node: std::sync::Weak<ReticulumNode>,
    pub events: UnboundedReceiver<NodeEvent>,
    /// Resolves once the lend ends: the node lent again, or stopping.
    pub ended: oneshot::Receiver<()>,
}

/// Whether a lent node's fetch reads `event`: its links coming up and
/// closing, the answers to its requests, and the NomadNet nodes'
/// announces, which the Browse tab lists. Nothing else leaves the tap.
fn lends(event: &NodeEvent) -> bool {
    match event {
        NodeEvent::LinkEstablished { is_initiator, .. }
        | NodeEvent::LinkClosed { is_initiator, .. } => *is_initiator,
        NodeEvent::ResponseReceived { .. } | NodeEvent::RequestTimedOut { .. } => true,
        NodeEvent::AnnounceReceived { announce, .. } => lnomad::is_nomad_node_announce(announce),
        _ => false,
    }
}

/// Whether the Rooms read `event`: their links coming up and closing, and
/// the plain packets on links, where a hub's envelopes arrive. Each hub
/// link keeps the packets of its own link id.
fn rooms_read(event: &NodeEvent) -> bool {
    match event {
        NodeEvent::LinkEstablished { is_initiator, .. }
        | NodeEvent::LinkClosed { is_initiator, .. } => *is_initiator,
        NodeEvent::LinkDataReceived { .. } => true,
        _ => false,
    }
}

/// Whether `lender` reads `event`.
fn reads(lender: Lender, event: &NodeEvent) -> bool {
    match lender {
        Lender::Browse => lends(event),
        Lender::Rooms => rooms_read(event),
    }
}

/// The taps, poisoned or not: what they hold is a channel's end, and no
/// panic leaves one half-written.
fn slot(tap: &Tap) -> std::sync::MutexGuard<'_, Taps> {
    tap.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One request a client sent the home destination, with the identity the
/// link was identified with when it was. Taken off the processor tap, which
/// never drops an event, and answered by the home node's loop
/// (`crate::home::daemon`), off the core lock.
#[derive(Debug, Clone)]
pub struct HomeRequest {
    pub link_id: LinkId,
    pub request_id: [u8; 16],
    pub path: String,
    pub data: Vec<u8>,
    pub remote: Option<[u8; 16]>,
}

/// What the attached engine is.
pub enum Role {
    /// The messenger: our identity's delivery destination, the router.
    Messenger,
    /// A client of the home node whose LXMF address this is
    /// ([`Engine::client`]): no delivery destination at all.
    Client { address: [u8; 16] },
    /// The home node: the messenger's engine, handing every received message
    /// out packed for the archive, plus the home destination `loquax.home`
    /// with its five request handlers, whose requests go to `requests`.
    Home {
        requests: UnboundedSender<HomeRequest>,
    },
    /// A group the home node hosts (1307, `crate::home::group`): the
    /// group identity's delivery destination, announced with the group's
    /// name, handing every received message out packed as the home's does,
    /// its proof held until the archive keeps it. No home destination: the
    /// group is reached by its members' messages alone.
    Group,
}

impl Processor {
    /// A request to the home destination goes to the home node's loop. The
    /// remote identity is read here, under the lock the event came with.
    fn forward_home_request(&self, core: &StdNodeCore, event: &NodeEvent) {
        let Some((home, requests)) = &self.home else {
            return;
        };
        let NodeEvent::RequestReceived {
            link_id,
            destination_hash,
            request_id,
            path,
            data,
            ..
        } = event
        else {
            return;
        };
        if destination_hash != home {
            return;
        }
        let remote = core
            .get_remote_identity(link_id)
            .map(|identity| *identity.hash());
        let _ = requests.send(HomeRequest {
            link_id: *link_id,
            request_id: *request_id,
            path: path.clone(),
            data: data.clone(),
            remote,
        });
    }
}

impl Processor {
    /// An event a lent node's fetch reads goes to it. A lend whose end
    /// went away is over.
    fn forward_lent(&self, event: &NodeEvent) {
        let mut taps = slot(&self.lend);
        for lender in [Lender::Browse, Lender::Rooms] {
            if !reads(lender, event) {
                continue;
            }
            let slot = taps.of(lender);
            let gone = matches!(&*slot, Some((events, _)) if events.send(event.clone()).is_err());
            if gone {
                *slot = None;
            }
        }
    }

    /// Move what the frontend and the miner sent into the engine's queues.
    /// Neither blocks; a gone sender has simply nothing more to send.
    fn take_inputs(&mut self) {
        // An error is either empty or disconnected, and a disconnected
        // frontend means the node is being torn down: both end the drain.
        while let Ok(command) = self.commands.try_recv() {
            self.engine.submit(command);
        }
        while let Ok(answer) = self.stamp_answers.try_recv() {
            self.engine.stamp_answered(answer);
        }
    }

    /// Hand on what the hook produced.
    fn flush(&mut self) {
        while let Some(event) = self.engine.next_event() {
            // A dropped receiver means the frontend has already finished;
            // the engine keeps running until the node stops, and has nothing
            // to say about it.
            let _ = self.events.send(event);
        }
        while let Some(job) = self.engine.next_stamp_job() {
            if let Err(std::sync::mpsc::SendError(job)) = self.stamps.send(job) {
                self.engine.stamp_undelivered(&job);
            }
        }
    }
}

impl CoreProcessor for Processor {
    fn on_event(&mut self, core: &mut StdNodeCore, event: &NodeEvent) -> TickOutput {
        self.forward_home_request(core, event);
        self.forward_lent(event);
        let out = self.engine.on_event(core, event);
        self.flush();
        out
    }

    fn on_tick(&mut self, core: &mut StdNodeCore, now_ms: u64) -> TickOutput {
        self.take_inputs();
        let out = self.engine.on_tick(core, now_ms);
        self.flush();
        out
    }
}

/// The frontend's end of the in-process engine.
pub struct InProcessOutbox {
    commands: Sender<Command>,
    events: Receiver<OutboxEvent>,
}

impl InProcessOutbox {
    /// Cut this end off the engine, as the engine's death does: every call
    /// after this answers [`OutboxGone`]. For the tests of what a frontend
    /// does then, which cannot otherwise kill an engine on purpose.
    #[doc(hidden)]
    pub fn sever(&mut self) {
        self.commands = std::sync::mpsc::channel().0;
        self.events = std::sync::mpsc::channel().1;
    }

    /// An end attached to no engine: what a frontend holds in the outbox's
    /// place while a reader on another thread has the real one (1492).
    #[doc(hidden)]
    pub fn severed() -> Self {
        Self {
            commands: std::sync::mpsc::channel().0,
            events: std::sync::mpsc::channel().1,
        }
    }
}

impl Outbox for InProcessOutbox {
    fn submit(&self, command: Command) -> Result<(), OutboxGone> {
        self.commands.send(command).map_err(|_| OutboxGone)
    }

    fn try_next_event(&mut self) -> Result<Option<OutboxEvent>, OutboxGone> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(OutboxGone),
        }
    }
}

/// What attaching to a shared instance needs to know. `N` is where the stamp
/// worker's progress lines go: the CLI's [`Terminal`] writes them to stderr,
/// a full-screen frontend has to catch them before they land on its screen.
pub struct AttachConfig<N = Terminal> {
    /// The shared instance to join, i.e. which daemon.
    pub instance: String,
    /// That daemon's Reticulum config directory, whose key the node asks the
    /// daemon's RPC with
    /// ([`ReticulumNodeBuilder::shared_instance_config_dir`]): this node's
    /// own identity is not the daemon's key (1119). `None` is the platform
    /// default.
    pub config_dir: Option<PathBuf>,
    /// Where this client keeps its node state (learned identities, paths).
    ///
    /// Deliberately not the daemon's storage directory: `lncp` and `lnstatus`
    /// share that safely because a client with `enable_transport(false)`
    /// writes no paths, announces or packet hashes — this one registers a
    /// destination and learns identities, so it gets its own. The caller
    /// creates it: a directory is the shell's, and `main` names the path in
    /// the error when it cannot.
    pub storage_dir: PathBuf,
    /// The persistent identity whose delivery destination is our address.
    pub identity: Identity,
    /// The name recipients see as the sender: it goes out in the delivery
    /// announce `Engine::announce` sends, and a reference client shows it
    /// next to every message from us. Resolved by
    /// [`crate::display_name::from_process`], which never yields an empty one.
    pub display_name: Vec<u8>,
    /// Where the stamp worker's progress lines go.
    pub notices: N,
}

/// What attaching a leaf needs to know: [`AttachConfig`] without the
/// shared instance, and with the one node the leaf talks to instead.
pub struct LeafConfig<N = Terminal> {
    /// The servers the leaf dials, `host:port` each, one at a time in this
    /// order with failover (`crate::home::rendezvous`): the rendezvous
    /// list on their access point ports, or one fixed address. Each an
    /// lnsd `TCPServerInterface` or an rnsd one; a name is looked up again
    /// on every reconnect.
    pub servers: Vec<String>,
    /// The home destination whose path the leaf asks for each time a
    /// server comes up (1196, rule 2), `None` for none.
    pub home_destination: Option<[u8; 16]>,
    /// How the failover waits.
    pub timing: crate::home::rendezvous::Timing,
    /// Where this leaf keeps its node state: as [`AttachConfig::storage_dir`],
    /// and here also the node's transport identity, since no daemon holds one.
    pub storage_dir: PathBuf,
    /// The identity the engine runs on.
    pub identity: Identity,
    /// The name recipients see, as [`AttachConfig::display_name`].
    pub display_name: Vec<u8>,
    /// Where the stamp worker's progress lines go.
    pub notices: N,
    /// The keepalive of the connection to the home node, from the config
    /// file's `tcp_*` keys (1176); the default is the reference's.
    pub tcp: crate::config::TcpLiveness,
    /// The platform's word on the network (1298): a change re-evaluates
    /// the rotation. [`Network::default`](crate::home::rendezvous::Network)
    /// where nothing says one.
    pub network: crate::home::rendezvous::Network,
}

/// What attaching a mailbox that runs its own node needs: a phone's
/// mailbox, with no daemon on the device (1152;
/// `docs/drafts/loquax-onboarding-2026-10.md`, section 3).
pub struct MeshConfig<N = Terminal> {
    /// The entry servers the node dials, `host:port` each, on their
    /// ordinary ports: one at a time in this order with failover, as a
    /// peer (`crate::home::rendezvous`), the node's way into the network.
    pub servers: Vec<String>,
    /// How the failover waits.
    pub timing: crate::home::rendezvous::Timing,
    /// The listener the device's own screens dial, bound by the caller on
    /// the loopback address: one TCP server interface, so the screens reach
    /// the mailbox the way a phone reaches an external one.
    pub screens: std::net::TcpListener,
    /// As [`LeafConfig::storage_dir`].
    pub storage_dir: PathBuf,
    /// The mailbox identity.
    pub identity: Identity,
    /// The name recipients see, as [`AttachConfig::display_name`].
    pub display_name: Vec<u8>,
    /// Where the stamp worker's progress lines go.
    pub notices: N,
    /// The keepalive of the connections to the entry servers, as
    /// [`LeafConfig::tcp`].
    pub tcp: crate::config::TcpLiveness,
    /// The platform's word on the network, as [`LeafConfig::network`].
    pub network: crate::home::rendezvous::Network,
}

/// `host:port` as the leaf dials it: the host as written (a name, an IPv4
/// address, or an IPv6 one in brackets, which come off) and the port. The
/// error says what is wrong in words a user typed the address in.
pub fn parse_server(text: &str) -> Result<(String, u16), String> {
    let text = text.trim();
    let Some((host, port)) = text.rsplit_once(':') else {
        return Err(format!(
            "'{text}' has no port: write the home node's address as host:port, e.g. \
             192.168.1.20:4242"
        ));
    };
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return Err(format!("'{text}' has no host before the port"));
    }
    if host.contains(':') && !text.starts_with('[') {
        return Err(format!(
            "'{text}': an IPv6 address goes in brackets, as [{host}]:port"
        ));
    }
    match port.parse::<u16>() {
        Ok(port) if port > 0 => Ok((host.to_string(), port)),
        _ => Err(format!("'{port}' is not a port: a number from 1 to 65535")),
    }
}

/// Where the attached node's traffic goes.
enum Upstream {
    /// A running daemon's shared instance, by name, and its config directory.
    Shared {
        instance: String,
        config_dir: Option<PathBuf>,
    },
    /// No interface at the start: the rendezvous rotation dials the
    /// servers one at a time once the node runs ([`Attached::dial`]).
    Tcp,
    /// A TCP server on the screens' listener; the entry servers are dialled
    /// by the rotation as for [`Upstream::Tcp`].
    Mesh { screens: std::net::TcpListener },
}

/// Why attaching failed.
#[derive(Debug)]
pub enum AttachError {
    /// No daemon answered. The message names the instance, because the usual
    /// cause is the right daemon running under a different name.
    ///
    /// Both the build and the start of a shared-instance client fail this way,
    /// and they are one error to a user: the IPC socket is the only thing
    /// either step reaches for. Which of the two noticed is in `detail`.
    NoDaemon { instance: String, detail: String },
    /// A leaf's own node did not come up. `server` is what it was to dial.
    NoServer { server: String, detail: String },
}

impl core::fmt::Display for AttachError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoDaemon { instance, detail } => write!(
                f,
                "could not join the Reticulum shared instance named '{instance}'.\n  \
                 Loquax talks to a daemon; it starts lnsd when none answers, unless \
                 --no-start forbids it or there is no lnsd to start.\n  \
                 Start one with `lnsd` (or Python's `rnsd`), or name another instance \
                 with --instance / --config.\n  \
                 The stack said: {detail}"
            ),
            Self::NoServer { server, detail } => write!(
                f,
                "could not start a node that talks to the home node at {server}.\n  \
                 The stack said: {detail}"
            ),
        }
    }
}

impl core::error::Error for AttachError {}

impl AttachError {
    /// What a person reads when attaching failed (1265, walk finding 5):
    /// for a missing daemon one sentence and the one command that starts
    /// it, `start` from [`start_command`]; the [`Display`](core::fmt::Display)
    /// text, which names the instance, the socket and rnsd, is the detail a
    /// `--verbose` run prints instead. A leaf's failed node has no such
    /// command and says what it said before.
    pub fn brief(&self, lang: Lang, start: &str) -> String {
        match self {
            Self::NoDaemon { .. } => strings::format(
                StrId::NeedsService.text(lang),
                &[strings::Arg::Str(start.to_string())],
                lang,
            ),
            Self::NoServer { .. } => self.to_string(),
        }
    }
}

/// The packaged unit that runs the daemon, where systemd looks for a
/// package's units on Debian (`/lib` is `/usr/lib` merged, both named for
/// an unmerged system).
pub const LNSD_UNITS: [&str; 2] = [
    "/usr/lib/systemd/system/lnsd.service",
    "/lib/systemd/system/lnsd.service",
];

/// The command that starts the daemon for this loquax: inside a cargo
/// `target` directory it is a checkout's build, which starts lnsd from the
/// same checkout; otherwise the packaged unit when it is installed (the
/// `loquax` package depends on `leviculum`, which ships it), else plain
/// `lnsd` for an install from source.
pub fn start_command(exe: Option<&std::path::Path>, unit_installed: bool) -> &'static str {
    let in_checkout = exe.is_some_and(|exe| {
        exe.components()
            .any(|part| part.as_os_str() == std::ffi::OsStr::new("target"))
    });
    match (in_checkout, unit_installed) {
        (true, _) => "cargo run --release --bin lnsd",
        (false, true) => "sudo systemctl start lnsd",
        (false, false) => "lnsd",
    }
}

/// [`start_command`] for the running process.
pub fn start_command_here() -> &'static str {
    let exe = std::env::current_exe().ok();
    let unit = LNSD_UNITS
        .iter()
        .any(|unit| std::path::Path::new(unit).exists());
    start_command(exe.as_deref(), unit)
}

/// The language the locale names (`LC_ALL`, `LC_MESSAGES`, `LANG`, the
/// first one set and not empty), English without one.
pub fn locale() -> Lang {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .map_or(Lang::En, |tag| Lang::of_tag(&tag))
}

/// A running client node and the outbox that talks to its engine.
pub struct Attached {
    node: Arc<ReticulumNode>,
    /// The frontend's end of the seam.
    pub outbox: InProcessOutbox,
    /// The rendezvous rotation, when this node dials one.
    rendezvous: Option<crate::home::rendezvous::Rendezvous>,
    /// The network word the rotation listens to (1298).
    network: crate::home::rendezvous::Network,
    /// The processor's tap for a lent node ([`Attached::lend`]).
    lend: Tap,
}

/// A group's engine is driven as the home's is, through its outbox.
impl Outbox for Attached {
    fn submit(&self, command: Command) -> Result<(), crate::outbox::OutboxGone> {
        self.outbox.submit(command)
    }

    fn try_next_event(&mut self) -> Result<Option<OutboxEvent>, crate::outbox::OutboxGone> {
        self.outbox.try_next_event()
    }
}

impl Attached {
    /// The node, for what talks to the network beside the engine: the home
    /// node's answers and a client's pulls (`crate::home`).
    pub fn node(&self) -> Arc<ReticulumNode> {
        Arc::clone(&self.node)
    }

    /// Lend the node to a page fetch beside the engine ([`Lent`]). A lend
    /// before ends: one fetch reads the events at a time.
    pub fn lend(&self) -> Lent {
        self.lend_to(Lender::Browse)
    }

    /// Lend the node to `lender` ([`Lent`]). A lend before to the same
    /// lender ends; the other lender's goes on.
    pub fn lend_to(&self, lender: Lender) -> Lent {
        let (events, events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (end, ended) = oneshot::channel();
        *slot(&self.lend).of(lender) = Some((events, end));
        Lent {
            node: Arc::downgrade(&self.node),
            events: events_rx,
            ended,
        }
    }

    /// Where the rendezvous rotation stands, when this node dials one.
    pub fn rendezvous(&self) -> Option<crate::home::rendezvous::State> {
        self.rendezvous.as_ref().map(|r| r.state())
    }

    /// The network word this node's rotation listens to (1298): what
    /// [`UiEffect::Network`](crate::ui::UiEffect::Network) is said to, and
    /// shared with every other rotation that was handed the same one.
    pub fn network(&self) -> &crate::home::rendezvous::Network {
        &self.network
    }

    /// Dial `servers` (`host:port` each) one at a time with failover, as
    /// `side` ([`crate::home::rendezvous`]), replacing a rotation already
    /// running. An empty list dials nothing. A shared instance's client
    /// may dial too: the mailbox beside a daemon keeps its own peer
    /// connection to the rendezvous that way, with transport off, so it
    /// relays nothing between the two. `network` is the platform's word on
    /// the network the rotation re-evaluates on (1298).
    pub fn dial(
        &mut self,
        servers: &[String],
        side: crate::home::rendezvous::Side,
        tcp: crate::config::TcpLiveness,
        timing: crate::home::rendezvous::Timing,
        network: crate::home::rendezvous::Network,
    ) -> Result<(), AttachError> {
        let entries = servers
            .iter()
            .map(|server| {
                parse_server(server).map_err(|detail| AttachError::NoServer {
                    server: server.clone(),
                    detail,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.rendezvous = None;
        self.network = network.clone();
        if entries.is_empty() {
            return Ok(());
        }
        self.rendezvous = Some(crate::home::rendezvous::Rendezvous::start(
            crate::home::rendezvous::Dial {
                node: Arc::clone(&self.node),
                entries,
                side,
                tcp,
                timing,
                network,
            },
        ));
        Ok(())
    }

    /// Stop the node. Best effort: the send outcome is already decided by the
    /// time anything calls this. A task still holding the node (a pull cut
    /// off at exit) leaves it to be torn down with the process.
    pub async fn stop(mut self) {
        if let Some(rendezvous) = self.rendezvous.take() {
            rendezvous.stop().await;
        }
        // A lent node's fetch lets go of it once told; one in the middle
        // of a call holds it for that call.
        let node = self.node;
        let lent = {
            let mut taps = slot(&self.lend);
            let browse = taps.browse.take().is_some();
            let rooms = taps.rooms.take().is_some();
            browse || rooms
        };
        if lent {
            let deadline = tokio::time::Instant::now() + LEND_LET_GO;
            while Arc::strong_count(&node) > 1 && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
        if let Ok(mut node) = Arc::try_unwrap(node) {
            let _ = node.stop().await;
        }
    }
}

/// Attach to a running shared instance with an LXMF engine installed.
pub async fn attach<N: Effects + Send + 'static>(
    config: AttachConfig<N>,
) -> Result<Attached, AttachError> {
    attach_as(config, Role::Messenger).await
}

/// [`attach`] in a given [`Role`].
pub async fn attach_as<N: Effects + Send + 'static>(
    config: AttachConfig<N>,
    role: Role,
) -> Result<Attached, AttachError> {
    let AttachConfig {
        instance,
        config_dir,
        storage_dir,
        identity,
        display_name,
        notices,
    } = config;
    let joined = Joined {
        storage_dir,
        identity,
        display_name,
        notices,
    };
    attach_through(
        joined,
        role,
        Upstream::Shared {
            instance,
            config_dir,
        },
    )
    .await
}

/// Attach as a leaf: an in-process node with transport off and one TCP
/// client interface to `config.server`, no daemon and no shared instance.
/// The phone's way to its home node (`docs/drafts/loquax-android-2026-10.md`,
/// option A). The node is up once this returns; the TCP connection comes up
/// on its own and reconnects when it drops, so an unreachable server is no
/// error here but a home node request that finds no path.
pub async fn attach_leaf<N: Effects + Send + 'static>(
    config: LeafConfig<N>,
    role: Role,
) -> Result<Attached, AttachError> {
    let LeafConfig {
        servers,
        home_destination,
        timing,
        storage_dir,
        identity,
        display_name,
        notices,
        tcp,
        network,
    } = config;
    for server in &servers {
        parse_server(server).map_err(|detail| AttachError::NoServer {
            server: server.clone(),
            detail,
        })?;
    }
    let joined = Joined {
        storage_dir,
        identity,
        display_name,
        notices,
    };
    let mut attached = attach_through(joined, role, Upstream::Tcp).await?;
    // A device's dial to its mailbox takes the long TCP idle (1346); a
    // group's leaf is a mesh connection and keeps the reference's.
    let (side, tcp) = match home_destination {
        Some(hash) => (
            crate::home::rendezvous::Side::Phone(leviculum_core::DestinationHash::new(hash)),
            tcp.for_device(),
        ),
        None => (crate::home::rendezvous::Side::Peer, tcp),
    };
    attached.dial(&servers, side, tcp, timing, network)?;
    Ok(attached)
}

/// Attach a mailbox with a node of its own: transport off, a TCP client
/// interface to every entry server, and a TCP server on `config.screens`
/// for the device's own screens. No daemon and no shared instance; as for
/// a leaf, an entry server that does not answer is no error here, its
/// interface reconnects on its own.
pub async fn attach_mesh<N: Effects + Send + 'static>(
    config: MeshConfig<N>,
    role: Role,
) -> Result<Attached, AttachError> {
    let MeshConfig {
        servers,
        timing,
        screens,
        storage_dir,
        identity,
        display_name,
        notices,
        tcp,
        network,
    } = config;
    for server in &servers {
        parse_server(server).map_err(|detail| AttachError::NoServer {
            server: server.clone(),
            detail,
        })?;
    }
    let joined = Joined {
        storage_dir,
        identity,
        display_name,
        notices,
    };
    let mut attached = attach_through(joined, role, Upstream::Mesh { screens }).await?;
    attached.dial(
        &servers,
        crate::home::rendezvous::Side::Peer,
        tcp,
        timing,
        network,
    )?;
    Ok(attached)
}

/// `section` with the config file's `tcp_*` keys (1176) on it: the same
/// keys an lnsd section takes, so the node checks their range exactly as
/// lnsd does.
pub(crate) fn tcp_section(
    section: leviculum_std::config::InterfaceConfig,
    tcp: crate::config::TcpLiveness,
) -> leviculum_std::config::InterfaceConfig {
    leviculum_std::config::InterfaceConfig {
        tcp_keepalive_idle_secs: tcp.idle_secs,
        tcp_keepalive_interval_secs: tcp.interval_secs,
        tcp_keepalive_probes: tcp.probes,
        tcp_user_timeout_secs: tcp.user_timeout_secs,
        ..section
    }
}

/// What both attaches hand the engine and the node.
struct Joined<N> {
    storage_dir: PathBuf,
    identity: Identity,
    display_name: Vec<u8>,
    notices: N,
}

async fn attach_through<N: Effects + Send + 'static>(
    config: Joined<N>,
    role: Role,
    upstream: Upstream,
) -> Result<Attached, AttachError> {
    // What a failure names, taken before the build consumes the upstream.
    let shared = match &upstream {
        Upstream::Shared { instance, .. } => Some(instance.clone()),
        Upstream::Tcp | Upstream::Mesh { .. } => None,
    };
    let dialled = String::from("the rendezvous list");
    let fail = |detail: String| match &shared {
        Some(instance) => AttachError::NoDaemon {
            instance: instance.clone(),
            detail,
        },
        None => AttachError::NoServer {
            server: dialled.clone(),
            detail,
        },
    };
    // The home destination is the identity's too, so it is minted before
    // the engine takes the identity.
    let home = match &role {
        Role::Home { requests } => {
            let copy = config
                .identity
                .private_key_bytes()
                .ok()
                .and_then(|bytes| Identity::from_private_key_bytes(&bytes).ok())
                .ok_or_else(|| fail("the home identity has no private key".to_string()))?;
            let mut destination = crate::home::destination(copy).map_err(fail)?;
            destination.set_accepts_links(true);
            Some((destination, requests.clone()))
        }
        _ => None,
    };
    let (commands_tx, commands_rx) = std::sync::mpsc::channel::<Command>();
    let (events_tx, events_rx) = std::sync::mpsc::channel::<OutboxEvent>();
    let (stamps_tx, stamps_rx) = std::sync::mpsc::channel::<StampJob>();
    let (answers_tx, answers_rx) = std::sync::mpsc::channel::<StampAnswer>();

    // Proof-of-work, off the core lock and off the runtime's workers: mining
    // is pure CPU that can run for minutes at a cost the *peer* chooses. Its
    // own thread with its own current-thread runtime, exactly as
    // `leviculum-lxmf-node` does it (`leviculum-lxmf-node/src/main.rs:224-255`),
    // because the generator is async. Dormant unless a peer advertises a stamp
    // cost, which most do not.
    let notices = config.notices;
    std::thread::spawn(move || run_stamp_worker(&stamps_rx, &answers_tx, notices));

    let engine = match role {
        Role::Messenger => Engine::new(config.identity, config.display_name),
        Role::Client { address } => Engine::client(address),
        Role::Home { .. } => Engine::new(config.identity, config.display_name)
            .handing_out_packed()
            .saying_home(),
        Role::Group => Engine::new(config.identity, config.display_name).handing_out_packed(),
    };
    let lend = Tap::default();
    let processor = Processor {
        lend: Arc::clone(&lend),
        engine,
        commands: commands_rx,
        events: events_tx,
        stamps: stamps_tx,
        stamp_answers: answers_rx,
        home: home
            .as_ref()
            .map(|(destination, requests)| (*destination.hash(), requests.clone())),
    };

    let builder = ReticulumNodeBuilder::new().enable_transport(false);
    let builder = match upstream {
        Upstream::Shared {
            instance,
            config_dir,
        } => {
            let builder = builder.connect_to_shared_instance(&instance);
            match config_dir {
                Some(dir) => builder.shared_instance_config_dir(dir),
                None => builder,
            }
        }
        // No interface in the config: the rendezvous rotation spawns one
        // TCP client interface at a time once the node runs, as a config
        // section and not `add_tcp_client`, since that takes a socket
        // address and a home node behind a dynamic DNS name has to be
        // looked up again on each reconnect, which the TCP client does for
        // a name in `target_host`. No shared instance to serve, no
        // discovery, no other interface. A leaf is never a transport node.
        Upstream::Tcp => builder
            .config(leviculum_std::Config::default())
            .share_instance(false)
            .discover_interfaces(false),
        // As a leaf's, the entry servers dialled by the rotation; the
        // screens' server on the listener the caller bound. Neither
        // discovery nor a shared instance: the phone's mailbox is a
        // destination on the network, not a transport node.
        Upstream::Mesh { screens } => builder
            .config(leviculum_std::Config::default())
            .add_tcp_server_listener(screens)
            .share_instance(false)
            .discover_interfaces(false),
    };
    let mut node = builder
        .storage_path(config.storage_dir)
        .core_processor(processor)
        // The engine consumes events on the processor tap; nothing here ever
        // takes the driver's application receiver, so enabling it would only
        // fill a queue with no reader (Codeberg #419).
        .without_events()
        .build()
        .await
        .map_err(|e| fail(e.to_string()))?;
    node.start().await.map_err(|e| fail(e.to_string()))?;

    // Registered after the start, as a page server registers its own
    // (`lblogd/src/node.rs`): the handlers admit every link, and the home
    // node's loop checks the identity on each request itself.
    if let Some((destination, _)) = home {
        let hash = *destination.hash();
        node.register_destination(destination);
        for path in [
            crate::home::JOIN_PATH,
            crate::home::SYNC_PATH,
            crate::home::SEND_PATH,
            crate::home::MARK_PATH,
            crate::home::PETNAME_PATH,
            crate::home::PEERSTATE_PATH,
            crate::home::WATCH_PATH,
            crate::home::KEY_PATH,
            crate::home::MOVE_PATH,
            crate::home::MOVED_PATH,
            crate::home::GROUP_PATH,
            crate::home::GROUP_ADD_PATH,
            crate::home::GROUP_DELETE_PATH,
            crate::home::PAGES_PATH,
            crate::home::BOOKMARK_PATH,
            crate::home::ABOUT_PATH,
        ] {
            node.register_request_handler(hash, path, RequestPolicy::AllowAll);
        }
    }

    Ok(Attached {
        node: Arc::new(node),
        outbox: InProcessOutbox {
            commands: commands_tx,
            events: events_rx,
        },
        rendezvous: None,
        network: crate::home::rendezvous::Network::default(),
        lend,
    })
}

/// The terminal's miner: one job at a time, each on tokio's clock with a
/// fresh `OsRng` draw source.
fn run_stamp_worker(
    jobs: &Receiver<StampJob>,
    answers: &Sender<StampAnswer>,
    mut notices: impl Effects,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    let clock = clock::Tokio::new();
    while let Ok(job) = jobs.recv() {
        let answer = runtime.block_on(super::mine(job, rand_core::OsRng, &clock, &mut notices));
        if answers.send(answer).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1337 pass two: a lent node's fetch hears its own links come up and
    /// close, and every answer and timeout; the controls, a link a peer
    /// opened to us and what is no fetch's, stay at the tap. The announces
    /// it hears are `lnomad::is_nomad_node_announce`'s, tested there and
    /// end to end in the phone's facade (no announce is made here: the
    /// core parses them only off the wire).
    #[test]
    fn a_lent_fetch_hears_its_own_links_and_its_answers_alone() {
        let link_id = LinkId::new([0x11; 16]);
        let destination_hash = DestinationHash::new([0x22; 16]);
        let link = |is_initiator| NodeEvent::LinkEstablished {
            link_id,
            is_initiator,
            destination_hash,
        };
        let closed = |is_initiator| NodeEvent::LinkClosed {
            link_id,
            reason: leviculum_core::LinkCloseReason::Normal,
            is_initiator,
            destination_hash,
        };
        assert!(lends(&link(true)));
        assert!(lends(&closed(true)));
        assert!(lends(&NodeEvent::RequestTimedOut {
            link_id,
            request_id: [0x33; 16],
        }));
        // The controls.
        assert!(!lends(&link(false)), "a link a peer opened");
        assert!(!lends(&closed(false)), "a link a peer opened");
        assert!(!lends(&NodeEvent::InterfaceDown(0)));
    }

    /// 1379: the Rooms hear their links and the plain packets a hub sends
    /// on them; a page fetch's answers and a peer's link are not theirs,
    /// and a page fetch hears no packets (the controls).
    #[test]
    fn the_rooms_hear_their_links_and_the_hubs_packets() {
        let link_id = LinkId::new([0x11; 16]);
        let destination_hash = DestinationHash::new([0x22; 16]);
        let link = |is_initiator| NodeEvent::LinkEstablished {
            link_id,
            is_initiator,
            destination_hash,
        };
        let data = NodeEvent::LinkDataReceived {
            link_id,
            data: vec![0xa0],
        };
        assert!(reads(Lender::Rooms, &link(true)));
        assert!(reads(Lender::Rooms, &data));
        assert!(!reads(Lender::Rooms, &link(false)));
        assert!(!reads(
            Lender::Rooms,
            &NodeEvent::RequestTimedOut {
                link_id,
                request_id: [0x33; 16],
            }
        ));
        assert!(!reads(Lender::Browse, &data));
        assert!(reads(Lender::Browse, &link(true)));
    }

    #[test]
    fn a_server_is_host_and_port() {
        let ok = |text: &str, host: &str, port: u16| {
            assert_eq!(parse_server(text), Ok((host.to_string(), port)), "{text}");
        };
        ok("192.168.1.20:4242", "192.168.1.20", 4242);
        ok(" home.example.org:4965 ", "home.example.org", 4965);
        ok("[fd00::1]:4242", "fd00::1", 4242);
        let refused = |text: &str, words: &str| {
            let error = parse_server(text).expect_err(text);
            assert!(error.contains(words), "{text}: {error}");
        };
        refused("192.168.1.20", "has no port");
        refused(":4242", "has no host");
        refused("fd00::1:4242", "in brackets");
        refused("home:0", "not a port");
        refused("home:65536", "not a port");
        refused("home:http", "not a port");
    }

    /// The no-daemon start says one sentence and one command in the
    /// locale's language (1265); the instance and the stack's words are
    /// the verbose text, and a leaf's error keeps its own.
    #[test]
    fn a_missing_daemon_is_one_sentence_and_one_command() {
        let error = AttachError::NoDaemon {
            instance: "walk".to_string(),
            detail: "no local Leviculum daemon reachable".to_string(),
        };
        assert_eq!(
            error.brief(Lang::En, "sudo systemctl start lnsd"),
            "Loquax needs its network service. Start it with: sudo systemctl start lnsd"
        );
        assert_eq!(
            error.brief(Lang::De, "sudo systemctl start lnsd"),
            "Loquax braucht seinen Netzwerkdienst. Starte ihn mit: sudo systemctl start lnsd"
        );
        let verbose = error.to_string();
        assert!(
            verbose.contains("'walk'") && verbose.contains("rnsd"),
            "{verbose}"
        );
        let leaf = AttachError::NoServer {
            server: "home:4242".to_string(),
            detail: "refused".to_string(),
        };
        assert_eq!(leaf.brief(Lang::En, "lnsd"), leaf.to_string());
    }

    #[test]
    fn the_command_is_the_checkouts_the_packages_or_lnsd() {
        use std::path::Path;
        let checkout = Path::new("/home/u/src/leviculum/target/release/loquax");
        let packaged = Path::new("/usr/bin/loquax");
        assert_eq!(
            start_command(Some(checkout), true),
            "cargo run --release --bin lnsd"
        );
        assert_eq!(
            start_command(Some(checkout), false),
            "cargo run --release --bin lnsd"
        );
        assert_eq!(
            start_command(Some(packaged), true),
            "sudo systemctl start lnsd"
        );
        assert_eq!(start_command(Some(packaged), false), "lnsd");
        assert_eq!(start_command(None, true), "sudo systemctl start lnsd");
        // A directory that only starts with the word is no target directory.
        let near = Path::new("/opt/targets/loquax");
        assert_eq!(start_command(Some(near), false), "lnsd");
    }
}
