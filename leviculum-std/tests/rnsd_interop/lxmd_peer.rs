//! The `lxmd_peer` fixture: the reference `lxmd` daemon, stock and
//! unmodified, as a propagation-node peer of `lnpnd` (leviculum order 671).
//!
//! Every other harness in this suite that speaks LXMF propagation runs the
//! Python side as a *client* of a node, or runs Python's `LXMRouter` inside
//! `scripts/test_daemon.py`. This one starts the real daemon,
//! `reference/LXMF/LXMF/Utilities/lxmd.py` at the pinned 795fdaa, with a
//! config file the test writes in lxmd's own format, so whatever lxmd
//! decides about peering, offers and syncs is the stock daemon's decision.
//!
//! The topology is one shared instance and its local clients:
//!
//! ```text
//!   SharedInstance (in-process leviculum node, share_instance = yes)
//!     ├── Lnpnd    (lnpnd's production Engine, attached as lnpnd/src/main.rs
//!     │             attaches it: lnpnd::node_builder)
//!     └── LxmdPeer (python3 -m LXMF.Utilities.lxmd, attached as a Python
//!                   shared-instance client, same instance_name)
//! ```
//!
//! What a test reads:
//! - lxmd's own log (stdout at the config's `loglevel`, here DEBUG):
//!   `Peered with <hash>` (`LXMRouter.py:2046`), `Syncing N messages to
//!   peer` (`LXMPeer.py:513`), `Postponing sync ... peering key`
//!   (`LXMPeer.py:290`), and the store index line at start;
//! - lxmd's message store directory, `storage/lxmf/messagestore`
//!   (`LXMRouter.py:130`, `:555`), one file per message, named
//!   `<transient id>_<received>[_<stamp value>]` (`:2502-2504`);
//! - lnpnd's `EngineEvent`s, the same channel `main.rs` reads, and its
//!   remote-management stats, fetched over the control destination exactly
//!   as `lnpnd --status` fetches them, so `Peers : N total` is
//!   `ControlNodeStats::total_peers` of a real `/pn/get/stats` answer.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use leviculum_core::identity::Identity;
use leviculum_core::{Destination, DestinationHash};
use leviculum_lxmf::control::{ControlNodeStats, ControlResponse, CONTROL_ASPECTS, STATS_GET_PATH};
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::{MemoryPeerStore, PeeringConfig, PropagationNodeConfig, PropagationStore};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::process::spawn_supervised;
use leviculum_std::ReticulumNode;
use lnpnd::engine::{Engine, EngineConfig, EngineEvent};

/// The reference trees the daemon runs from (the same pins the rest of
/// the suite uses).
const REFERENCE_RNS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../reference/Reticulum");
const REFERENCE_LXMF: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../reference/LXMF");

/// How long lxmd may take from spawn to `LXMF Propagation Node started`.
/// On the coder host (2026-10-12) every run logged it in the second of its
/// `Substantiating Reticulum` line; the margin is for a loaded gate host,
/// not for a slow daemon.
const LXMD_START_TIMEOUT: Duration = Duration::from_secs(30);

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn unique(tag: &str) -> String {
    format!(
        "{tag}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// The `lxmf.propagation` destination of an identity.
pub fn propagation_hash_of(identity: &Identity) -> [u8; 16] {
    let name_hash = Destination::compute_name_hash(APP_NAME, &["propagation"]);
    *Destination::compute_destination_hash(&name_hash, identity.hash()).as_bytes()
}

/// The shared instance every party attaches to: transport on, so it relays
/// announces and links between its local clients as `lnsd` does.
pub struct SharedInstance {
    node: ReticulumNode,
    pub name: String,
    identity_bytes: [u8; 64],
    _storage: tempfile::TempDir,
}

impl SharedInstance {
    pub async fn start(tag: &str) -> Self {
        let name = unique(tag);
        let identity = Identity::generate(&mut rand_core::OsRng);
        let identity_bytes = identity
            .private_key_bytes()
            .expect("a generated identity has private keys");
        let storage = crate::common::temp_storage(tag, "instance");
        let mut node = ReticulumNodeBuilder::new()
            .identity(identity)
            .enable_transport(true)
            .share_instance(true)
            .instance_name(name.clone())
            .storage_path(storage.path().to_path_buf())
            .build()
            .await
            .expect("build the shared instance");
        node.start().await.expect("start the shared instance");
        Self {
            node,
            name,
            identity_bytes,
            _storage: storage,
        }
    }

    pub async fn stop(mut self) {
        let _ = self.node.stop().await;
    }
}

/// What the test writes into lxmd's `[propagation]` section. Every field is
/// an lxmd config key (`lxmd.py:107-235`); `None` leaves the key out, so
/// lxmd's own default applies.
#[derive(Debug, Clone)]
pub struct LxmdConfig {
    pub display_name: String,
    pub autopeer: bool,
    /// `propagation_stamp_cost_target`; lxmd clamps it to at least 13
    /// (`PROPAGATION_COST_MIN`, `lxmd.py:188-189`).
    pub stamp_cost_target: u8,
    /// `propagation_stamp_cost_flexibility`: the peer accepts (and is
    /// offered) stamps worth at least target minus this
    /// (`LXMRouter.py:2242`, `LXMPeer.py:331`).
    pub stamp_cost_flexibility: u8,
    /// `peering_cost`: what lnpnd mines to offer this node anything.
    pub peering_cost: u8,
    pub max_peers: Option<usize>,
    /// `[propagation] announce_at_start`: a second node announce, 10 s
    /// (`DEFFERED_JOBS_DELAY`, `lxmd.py:33`) plus 20 s
    /// (`NODE_ANNOUNCE_DELAY`, `LXMRouter.py:41`) after start. Off by
    /// default because lxmd announces its node anyway: `enable_propagation`
    /// ends with `announce_propagation_node()` (`LXMRouter.py:685`), which
    /// sends 20 s after start whatever this key says.
    pub announce_at_start: bool,
}

impl Default for LxmdConfig {
    fn default() -> Self {
        Self {
            display_name: "lxmd-peer".to_string(),
            autopeer: true,
            stamp_cost_target: 13,
            stamp_cost_flexibility: 3,
            // Low so lnpnd's key for this peer mines in milliseconds; the
            // stock 18 works too, it only costs wall time.
            peering_cost: 4,
            max_peers: None,
            announce_at_start: false,
        }
    }
}

impl LxmdConfig {
    fn render(&self) -> String {
        let yes_no = |flag: bool| if flag { "yes" } else { "no" };
        let mut out = String::new();
        out.push_str("[propagation]\n");
        out.push_str("  enable_node = yes\n");
        out.push_str(&format!("  autopeer = {}\n", yes_no(self.autopeer)));
        out.push_str(&format!(
            "  propagation_stamp_cost_target = {}\n",
            self.stamp_cost_target
        ));
        out.push_str(&format!(
            "  propagation_stamp_cost_flexibility = {}\n",
            self.stamp_cost_flexibility
        ));
        out.push_str(&format!("  peering_cost = {}\n", self.peering_cost));
        if let Some(max) = self.max_peers {
            out.push_str(&format!("  max_peers = {max}\n"));
        }
        out.push_str(&format!(
            "  announce_at_start = {}\n",
            yes_no(self.announce_at_start)
        ));
        out.push_str("\n[lxmf]\n");
        out.push_str(&format!("  display_name = {}\n", self.display_name));
        out.push_str("  announce_at_start = no\n");
        // DEBUG (6): the postponement and offer lines live there.
        out.push_str("\n[logging]\n  loglevel = 6\n");
        out
    }
}

/// One message to place in lxmd's store before it starts, in the
/// reference's own file format, so lxmd's start-up index takes it in
/// (`LXMRouter.py:566-590`, which needs the value component).
pub struct PreloadedMessage {
    pub transient_id: [u8; 32],
    pub body: Vec<u8>,
    pub stamp_value: u8,
}

/// A running stock lxmd. Killed on drop; [`LxmdPeer::stop`] ends it with
/// SIGTERM first, which runs lxmd's own exit handler.
pub struct LxmdPeer {
    child: Option<Child>,
    dir: tempfile::TempDir,
    pub name: String,
    pub propagation_hash: [u8; 16],
    started: Instant,
}

impl LxmdPeer {
    /// Start lxmd on `instance` as a propagation node with a fixed identity
    /// (generated here, written to lxmd's `identity` file before start) and
    /// the given config; returns once lxmd logs that the node runs.
    pub async fn start(
        instance: &SharedInstance,
        config: &LxmdConfig,
        preload: &[PreloadedMessage],
    ) -> Self {
        let dir = tempfile::Builder::new()
            .prefix(&format!("lxmd_peer_{}_", config.display_name))
            .tempdir()
            .expect("lxmd temp dir");
        let rns_dir = dir.path().join("rns");
        let lxmd_dir = dir.path().join("lxmd");
        std::fs::create_dir_all(rns_dir.join("storage")).expect("rns storage dir");
        // The instance's transport identity, so the Python client derives
        // the instance's RPC key (`rpc_interop_tests.rs` does the same).
        std::fs::write(
            rns_dir.join("storage").join("transport_identity"),
            instance.identity_bytes,
        )
        .expect("write transport_identity");
        std::fs::write(
            rns_dir.join("config"),
            format!(
                "[reticulum]\n  enable_transport = no\n  share_instance = Yes\n  \
                 instance_name = {}\n\n[logging]\n  loglevel = 4\n\n[interfaces]\n",
                instance.name
            ),
        )
        .expect("write rns config");

        let identity = Identity::generate(&mut rand_core::OsRng);
        let messagestore = lxmd_dir.join("storage").join("lxmf").join("messagestore");
        std::fs::create_dir_all(&messagestore).expect("lxmd messagestore dir");
        std::fs::write(
            lxmd_dir.join("identity"),
            identity
                .private_key_bytes()
                .expect("a generated identity has private keys"),
        )
        .expect("write lxmd identity");
        std::fs::write(lxmd_dir.join("config"), config.render()).expect("write lxmd config");
        let received = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(1.0);
        for message in preload {
            std::fs::write(
                messagestore.join(format!(
                    "{}_{received}_{}",
                    hex::encode(message.transient_id),
                    message.stamp_value
                )),
                &message.body,
            )
            .expect("preload a message");
        }

        let log = std::fs::File::create(dir.path().join("lxmd.log")).expect("lxmd log file");
        let log_err = log.try_clone().expect("clone log handle");
        let mut cmd = Command::new("python3");
        cmd.arg("-u")
            .args(["-m", "LXMF.Utilities.lxmd", "--config"])
            .arg(&lxmd_dir)
            .arg("--rnsconfig")
            .arg(&rns_dir)
            .env("PYTHONPATH", format!("{REFERENCE_RNS}:{REFERENCE_LXMF}"))
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        let child = spawn_supervised(cmd).expect("spawn lxmd");
        let started = Instant::now();

        let propagation_hash = propagation_hash_of(&identity);
        let peer = Self {
            child: Some(child),
            dir,
            name: config.display_name.clone(),
            propagation_hash,
            started,
        };
        let expected = format!(
            "LXMF Propagation Node started on <{}>",
            hex::encode(propagation_hash)
        );
        if peer
            .wait_for_log(&expected, LXMD_START_TIMEOUT)
            .await
            .is_none()
        {
            panic!(
                "lxmd {} did not report its propagation node within {:?}; log:\n{}",
                peer.name,
                LXMD_START_TIMEOUT,
                peer.log()
            );
        }
        peer
    }

    pub fn log_path(&self) -> PathBuf {
        self.dir.path().join("lxmd.log")
    }

    /// Everything lxmd has written so far.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    /// Log lines containing `needle`.
    pub fn lines(&self, needle: &str) -> Vec<String> {
        self.log()
            .lines()
            .filter(|line| line.contains(needle))
            .map(str::to_string)
            .collect()
    }

    /// Wait until a log line contains `needle`; the elapsed time since
    /// lxmd's spawn, or `None` at the timeout.
    pub async fn wait_for_log(&self, needle: &str, timeout: Duration) -> Option<Duration> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.log().contains(needle) {
                return Some(self.started.elapsed());
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn messagestore(&self) -> PathBuf {
        self.dir
            .path()
            .join("lxmd")
            .join("storage")
            .join("lxmf")
            .join("messagestore")
    }

    /// Transient ids (hex) of the messages in lxmd's store directory.
    pub fn stored(&self) -> Vec<String> {
        stored_in(&self.messagestore())
    }

    /// Wait until lxmd's store holds `transient_id`.
    pub async fn wait_for_stored(&self, transient_id: &[u8; 32], timeout: Duration) -> bool {
        let wanted = hex::encode(transient_id);
        let deadline = Instant::now() + timeout;
        loop {
            if self.stored().contains(&wanted) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// SIGTERM, then wait up to five seconds; a daemon that ignores it is
    /// killed. Returns the full log.
    pub async fn stop(mut self) -> String {
        if let Some(mut child) = self.child.take() {
            let _ = Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        self.log()
    }
}

impl Drop for LxmdPeer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn stored_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.split('_').next().map(str::to_string))
        })
        .filter(|id| id.len() == 64)
        .collect();
    ids.sort();
    ids
}

/// What the test sets on lnpnd beside its store.
pub struct LnpndOptions {
    pub node_config: PropagationNodeConfig,
    pub peering: PeeringConfig,
    /// Seconds from start to lnpnd's first node announce; `None` keeps it
    /// silent for the whole test (the interval is set past any test).
    pub announce_delay_secs: Option<u64>,
    pub sync_interval_secs: Option<u64>,
    pub rotation_interval_secs: Option<u64>,
}

impl Default for LnpndOptions {
    fn default() -> Self {
        Self {
            node_config: PropagationNodeConfig::default(),
            peering: PeeringConfig::default(),
            announce_delay_secs: Some(0),
            sync_interval_secs: None,
            rotation_interval_secs: None,
        }
    }
}

/// lnpnd's production engine on a shared instance, and its event channel.
pub struct Lnpnd {
    node: ReticulumNode,
    events: Receiver<EngineEvent>,
    /// Every event seen so far, in arrival order.
    pub seen: Vec<EngineEvent>,
    pub identity: Identity,
    pub propagation_hash: [u8; 16],
    instance: String,
    _storage: tempfile::TempDir,
}

impl Lnpnd {
    pub async fn start<S>(instance: &SharedInstance, store: S, options: LnpndOptions) -> Self
    where
        S: PropagationStore + Send + 'static,
    {
        let identity = leviculum_std::generate_identity();
        let (announce_delay_secs, announce_interval_secs) = match options.announce_delay_secs {
            Some(delay) => (delay, 3600),
            None => (3600, 3600),
        };
        let (mut engine, events) = Engine::new(EngineConfig {
            identity: identity.clone(),
            node_config: options.node_config,
            store,
            announce_interval_secs,
            announce_delay_secs,
            peering: options.peering,
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 64_000_000,
            delivery_limit_kb: 1000,
        });
        if let Some(secs) = options.sync_interval_secs {
            engine = engine.with_sync_interval(Duration::from_secs(secs));
        }
        if let Some(secs) = options.rotation_interval_secs {
            engine = engine.with_rotation_interval(Duration::from_secs(secs));
        }
        let storage = crate::common::temp_storage("lnpnd", "daemon");
        let mut node = lnpnd::node_builder(engine, storage.path().to_path_buf())
            .connect_to_shared_instance(&instance.name)
            .build()
            .await
            .expect("build lnpnd");
        node.start().await.expect("start lnpnd");
        let propagation_hash = propagation_hash_of(&identity);
        Self {
            node,
            events,
            seen: Vec::new(),
            identity,
            propagation_hash,
            instance: instance.name.clone(),
            _storage: storage,
        }
    }

    fn drain(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.seen.push(event);
        }
    }

    /// Wait for the first event (seen before or arriving later) matching
    /// `pred`.
    pub async fn wait_event<F>(&mut self, timeout: Duration, pred: F) -> Option<EngineEvent>
    where
        F: Fn(&EngineEvent) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain();
            if let Some(event) = self.seen.iter().find(|event| pred(event)) {
                return Some(event.clone());
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Every event matching `pred` so far.
    pub fn events_matching<F>(&mut self, pred: F) -> Vec<EngineEvent>
    where
        F: Fn(&EngineEvent) -> bool,
    {
        self.drain();
        self.seen.iter().filter(|e| pred(e)).cloned().collect()
    }

    /// The remote-management stats, asked for the way `lnpnd --status`
    /// asks (`lnpnd/src/client.rs::query`): a separate local client with
    /// the node's own identity resolves the control destination, links,
    /// identifies and requests `/pn/get/stats`.
    pub async fn stats(&self) -> ControlNodeStats {
        let storage = crate::common::temp_storage("lnpnd", "status");
        let mut client = ReticulumNodeBuilder::new()
            .enable_transport(false)
            .connect_to_shared_instance(&self.instance)
            .storage_path(storage.path().to_path_buf())
            .build()
            .await
            .expect("build the status client");
        client.start().await.expect("start the status client");
        let name_hash = Destination::compute_name_hash(APP_NAME, &CONTROL_ASPECTS);
        let control: DestinationHash =
            Destination::compute_destination_hash(&name_hash, self.identity.hash());
        let budget = Duration::from_secs(20);
        assert!(
            lnpnd::client::resolve_control_path(&client, &control, budget).await,
            "no path to lnpnd's control destination"
        );
        let signing_key = self.identity.ed25519_verifying().to_bytes();
        let (link, established) = client
            .connect_awaited(&control, &signing_key)
            .await
            .expect("link to the control destination");
        tokio::time::timeout(budget, established)
            .await
            .expect("control link timed out")
            .expect("control link established");
        client
            .identify_link(link.link_id(), &self.identity)
            .await
            .expect("identify");
        let (_, response) = client
            .send_request_awaited(link.link_id(), STATS_GET_PATH, None, Some(10_000))
            .await
            .expect("stats request");
        let info = tokio::time::timeout(budget, response)
            .await
            .expect("stats response timed out")
            .expect("stats response");
        let _ = client.close_link(link.link_id()).await;
        let _ = client.stop().await;
        match ControlResponse::decode(&info.response_data).expect("decode stats") {
            ControlResponse::Stats(stats) => *stats,
            other => panic!("stats request answered {other:?}"),
        }
    }

    /// The `Peers   : N total (peer limit is M)` line of `lnpnd --status`,
    /// formatted from a live stats answer as `client.rs` formats it.
    pub async fn peers_line(&self) -> (u64, String) {
        let stats = self.stats().await;
        let limit = stats
            .max_peers
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unlimited".to_string());
        (
            stats.total_peers,
            format!(
                "Peers   : {} total (peer limit is {limit})",
                stats.total_peers
            ),
        )
    }

    pub async fn stop(mut self) {
        let _ = self.node.stop().await;
    }
}

/// `PN_PEER action=add` for `peer`.
pub fn is_peer_add(peer: &[u8; 16]) -> impl Fn(&EngineEvent) -> bool + '_ {
    move |event| {
        matches!(event, EngineEvent::Peer { action: "add", destination_hash, .. }
            if destination_hash == peer)
    }
}

/// `PN_PEER action=drop` for `peer`.
pub fn is_peer_drop(peer: &[u8; 16]) -> impl Fn(&EngineEvent) -> bool + '_ {
    move |event| {
        matches!(event, EngineEvent::Peer { action: "drop", destination_hash, .. }
            if destination_hash == peer)
    }
}

/// An outbound sync round toward `peer` that concluded.
pub fn is_sync_out_to(peer: &[u8; 16]) -> impl Fn(&EngineEvent) -> bool + '_ {
    move |event| matches!(event, EngineEvent::SyncDone { dir: "out", peer: p, .. } if p == peer)
}

/// Mine a propagation stamp of at least `cost` over a fresh random body and
/// return `(transient_id, stamped body, true value)`: the shape a client
/// upload leaves in a store.
pub fn stamped_message(cost: u8) -> ([u8; 32], Vec<u8>, u8) {
    use leviculum_lxmf::constants::{LXMF_OVERHEAD, WORKBLOCK_EXPAND_ROUNDS_PN};
    use leviculum_lxmf::{CooperativeStamper, StampCancel};
    use rand_core::RngCore;

    let mut unstamped = vec![0u8; LXMF_OVERHEAD + 64];
    rand_core::OsRng.fill_bytes(&mut unstamped);
    let transient_id = leviculum_core::crypto::full_hash(&unstamped);
    let mut stamper = CooperativeStamper::cooperative(rand_core::OsRng);
    let stamp = futures::executor::block_on(stamper.generate(
        &transient_id,
        cost,
        WORKBLOCK_EXPAND_ROUNDS_PN,
        &StampCancel::new(),
    ))
    .expect("mine a stamp");
    let value = futures::executor::block_on(stamper.measure_stamp(
        &transient_id,
        &stamp,
        WORKBLOCK_EXPAND_ROUNDS_PN,
    ));
    let mut body = unstamped;
    body.extend_from_slice(&stamp);
    (transient_id, body, value.min(u8::MAX as u16) as u8)
}
