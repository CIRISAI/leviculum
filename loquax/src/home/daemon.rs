//! `loquax --daemon`: the home node (`docs/src/concepts/loquax-home-node.md`).
//!
//! Headless, the 2026-08-08 option C (`docs/src/concepts/loquax-architecture.md`,
//! "process model"): the engine, the store and the router against the local
//! shared instance, and no screen. It
//!
//! - receives as the messenger does: the delivery destination of the home
//!   identity, announced at start and every [`ANNOUNCE_EVERY`], so senders
//!   deliver DIRECT and get their proof;
//! - archives every received message, packed as it came, under a sequence
//!   ([`crate::store::Store::archive_append`]), within the configured cap,
//!   and proves it to its sender only once it is archived (1239);
//! - answers its admitted clients on `loquax.home` ([`super::server`]);
//! - collects from its propagation node on the messenger's own schedule
//!   ([`crate::sync::Schedule`]), so mail that senders propagated arrives
//!   too;
//! - delivers what its clients hand in with `/send`, by the outbox policy
//!   of [`super::outgoing`]: direct at once, a propagated copy on failure,
//!   direct retries on every announce of the recipient and on a slow
//!   cadence, each state change an archive entry;
//! - wakes every client that keeps a watching link ([`super::wake`]) when
//!   the archive grows, so it pulls at once rather than on its schedule,
//!   and when another process grew it: an import, a name or a block set
//!   beside it, seen within [`OUTSIDE_LOOK`] (1164);
//! - archives the announced name of a peer the user has a relation to
//!   ([`super::relation`]), and of no other, and wakes nobody for it: a
//!   name rides along with the next pull (1136);
//! - announces its `loquax.home` destination with every delivery announce,
//!   so every relay between a phone and the mailbox holds its path (1196,
//!   [`super::APP_NAME`] says why);
//! - keeps a peer connection to one rendezvous at a time, the first of its
//!   list that answers ([`super::rendezvous`]), so a phone on the same
//!   rendezvous reaches it with no configuration;
//! - hosts every group of its store whose key it holds ([`super::group`],
//!   1307): one engine a group on the group's identity, beside its own,
//!   reached as the home's own is (the shared instance, or the same entry
//!   servers as a leaf). A member's message to the group is archived, then
//!   proven, then fanned out through the same outbox policy as a client's
//!   `/send`, and each copy's proof is booked in `group_fanout`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use leviculum_core::{LinkId, RequestError, SendError};
use leviculum_lxmf::router::{AttemptFailure, MessageState};
use leviculum_std::ReticulumNode;
use tokio::sync::mpsc::unbounded_channel;

use crate::clock::Tokio;
use crate::effects::{Effects, Terminal};
use crate::engine::{
    attach_as, attach_leaf, attach_mesh, AttachConfig, Attached, HomeRequest, LeafConfig,
    MeshConfig, Role,
};
use crate::fanout::Fanout;
use crate::fetch::{run_fetch, FetchOptions};
use crate::home::group::{self, Hosted};
use crate::home::keepalive::Ledger;
use crate::home::outgoing::{Act, Change, Heard, Leg, Outcome, Outgoing, Policy};
use crate::home::relation::Relations;
use crate::home::wake::{Due, Wakes};
use crate::mailbox_state::{MailboxRun, Stored};
use crate::outbox::{Command, Outbox, OutboxEvent, Via};
use crate::seen::{SeenStore, SeenWrite};
use crate::store::Store;
use crate::sync::{Schedule, SyncEvent};

/// How often the home node announces its delivery destination again: the
/// interval lxmd's config template suggests for the same announce
/// (`reference/LXMF/LXMF/Utilities/lxmd.py:1140`, `announce_interval = 360`,
/// in minutes).
pub const ANNOUNCE_EVERY: Duration = Duration::from_secs(360 * 60);

/// How often it announces instead for [`AFTER_MOVE`] after its identity
/// moved in from another program (`crate::identity::MOVED_FILE`, written by
/// an import that adopted the identity or brought its ratchets): every peer
/// that heard the old program announce remembers its ratchet for 30 days
/// (`RNS/Identity.py:69`) and replaces it only with the next ratchet it
/// hears announced (leviculum 1185). Thirty minutes is the reference's
/// ratchet interval (`RNS/Destination.py:90`), so each of these announces
/// carries a new ratchet, and it stays far inside what the reference asks
/// of an announcing destination: no default rate limit applies to it
/// (`announce_rate_target` is off unless an interface sets one), and 48
/// announces of about 200 bytes in a day average 0.9 bit/s, under 4 % of
/// the 2 % announce cap (`RNS/Reticulum.py:114`) on a 1200 bit/s link.
pub const ANNOUNCE_EVERY_AFTER_MOVE: Duration = Duration::from_secs(30 * 60);

/// How often a hosted group announces its own destination (1307 G2), on
/// its own clock, beside the one at its engine's start: a stock member
/// that lacks the group's key finds it by path request meanwhile (study
/// 5.2). At 585 ms an announce at SF8 that is 0.016 % of the hour.
pub const GROUP_ANNOUNCE_EVERY: Duration = Duration::from_secs(60 * 60);

/// How long the shorter cadence lasts after a move.
pub const AFTER_MOVE: Duration = Duration::from_secs(24 * 60 * 60);

/// The announce interval at Unix time `now` for an identity that moved in
/// at `moved_at`, if it ever did.
pub fn announce_period(moved_at: Option<i64>, now: i64) -> Duration {
    let window = i64::try_from(AFTER_MOVE.as_secs()).unwrap_or(i64::MAX);
    match moved_at {
        Some(at) if (0..window).contains(&now.saturating_sub(at)) => ANNOUNCE_EVERY_AFTER_MOVE,
        _ => ANNOUNCE_EVERY,
    }
}

/// How long a mailbox whose identity moved away (1281) waits after its
/// `/moved` answer before it closes its node, so the answer leaves first.
const GONE_GRACE: Duration = Duration::from_secs(2);

/// The file under the node's storage with the identities it knows,
/// written when the node shuts down (`leviculum_std::storage`).
const KNOWN_FILE: &str = "known_destinations";

/// The open move for a `/move` or `/moved` (1281): the token the owner
/// opened, the delivery destination's ratchets and the node's known
/// identities as the node keeps them (empty when it has none), and the
/// mailbox's name. Read per request, since the owner opens a move while the
/// mailbox runs and the node rotates its ratchets.
fn handover(
    home: &std::path::Path,
    [ratchet_file, known_file]: [&std::path::Path; 2],
    name: &str,
    path: &str,
) -> Option<super::server::Handover> {
    if path != super::MOVE_PATH && path != super::MOVED_PATH {
        return None;
    }
    let token = super::handover::token(home, unix_now())?;
    let read = |file: &std::path::Path| match std::fs::read(file) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Vec::new()),
        Err(error) => {
            tracing::warn!("HOME move: {}: {error}", file.display());
            None
        }
    };
    Some(super::server::Handover {
        token,
        ratchets: read(ratchet_file)?,
        known: read(known_file)?,
        name: name.to_string(),
    })
}

/// How often the engine's events are read while there is work.
const PUMP: Duration = Duration::from_millis(20);
/// How often they are read once [`QUIET_PUMPS`] reads in a row found
/// nothing (order 1585): the 20 ms read was 70 % of an idle mailbox's CPU
/// (1.0 % of a core against 0.3 % at 250 ms, measured by
/// `tests/mailbox_census.rs`), and on the field host the mailbox burned
/// 23 % of a core with nothing to do. A device's request is not read by the
/// pump and is answered at once; what waits up to this long is an engine
/// event after a quiet spell, an arrival from the mesh among them.
const PUMP_QUIET: Duration = Duration::from_millis(250);
/// Reads in a row that found nothing before the pump slows: one second.
const QUIET_PUMPS: u32 = 50;
/// How often the outbox looks for a due retry or a passed horizon. Both are
/// counted in seconds.
const OUTBOX_TICK: Duration = Duration::from_secs(1);
/// How often the device links' keepalive totals are read (1346): a link
/// that closes between two readings loses at most this much of its count.
const COSTS_LOOK: Duration = Duration::from_secs(60);
/// The window of the `WAKE_SUMMARY` lines, one per device.
const SUMMARY_EVERY: Duration = Duration::from_millis(super::keepalive::SUMMARY_EVERY_MS);
/// How often the home node looks whether another process wrote the store
/// ([`crate::store::Store::data_version`]): the most a watching client
/// waits for an import on the mailbox before its wake goes. One look is a
/// lock and a read of the file's header, so an idle mailbox pays one such
/// read a second.
pub const OUTSIDE_LOOK: Duration = Duration::from_secs(1);

/// Where the home node's node reaches the network.
pub enum DaemonUpstream {
    /// The device's daemon (`loquax --daemon` beside lnsd): its shared
    /// instance, its Reticulum config directory for the RPC key, and how
    /// each collection selects its propagation node without a preference
    /// (`main` reads the daemon's path table for it).
    Shared {
        instance: String,
        config_dir: PathBuf,
        mailbox: MailboxSource,
        /// The rendezvous the mailbox's own node dials as a peer, on their
        /// ordinary ports, one at a time ([`super::rendezvous::peer_list`]);
        /// empty for none, the daemon's own interfaces then being its only
        /// way out.
        rendezvous: Vec<String>,
        /// The rendezvous connection's keepalive, from the `tcp_*` keys.
        tcp: crate::config::TcpLiveness,
    },
    /// A node of its own, for a device with no daemon: the phone's mailbox
    /// (1152). It dials the entry servers, `host:port` each, and serves its
    /// own screens on `screens`, a listener on the loopback address; a
    /// collection selects from the node's own path table.
    Mesh {
        servers: Vec<String>,
        screens: std::net::TcpListener,
        /// The entry connections' keepalive, from the `tcp_*` keys (1176).
        tcp: crate::config::TcpLiveness,
        /// How the failover between the servers waits.
        timing: super::rendezvous::Timing,
    },
}

/// What starting the home node needs.
pub struct DaemonOptions {
    pub home: PathBuf,
    pub upstream: DaemonUpstream,
    pub storage_dir: PathBuf,
    pub identity: leviculum_core::Identity,
    pub display_name: Vec<u8>,
    /// The archive's cap in bytes.
    pub archive_cap_bytes: u64,
    /// How long an admission token stays good, in seconds.
    pub token_ttl_secs: i64,
    /// The outbox's retry cadence and horizon.
    pub policy: Policy,
    /// The propagation node to collect from, as `--pn` and the config name
    /// it, with the word `LNMSG_PN` logs for where it came from.
    pub preferred_pn: Option<[u8; 16]>,
    pub pn_source: &'static str,
    /// The collection schedule's bound, `None` for no collection.
    pub sync_bound_ms: Option<u64>,
    /// One collection's budget.
    pub fetch_budget: Duration,
    /// Whether watching clients are woken when the archive grows
    /// (`home_wake` in the config, on by default). Off, a `/watch` is still
    /// answered, and the clients pull on their schedule.
    pub wake: bool,
    /// What ends the home node besides SIGINT and SIGTERM. `None` for the
    /// process of its own, which ends on the signals; `Some` for a home node
    /// inside an app, which installs no signal handler and ends when the
    /// sender says so or is dropped.
    pub stop: Option<tokio::sync::oneshot::Receiver<()>>,
    /// The platform's word on the network its rendezvous rotation
    /// re-evaluates on (1298): the phone's mailbox hears the app's;
    /// [`Network::default`](super::rendezvous::Network) elsewhere.
    pub network: super::rendezvous::Network,
    /// Where the home node's own node is lent once it runs, one end per
    /// lender: the phone's Browse tab fetches pages (1337 pass two) and
    /// its Rooms link to their hub (1384) through the mailbox's node,
    /// which is the phone's way onto the network. Empty lends it to
    /// nobody, as on a computer, where lnomad has a shared instance and
    /// the screens lend their own node to the Rooms.
    pub lends: Lends,
}

/// Where a node is lent, by [`crate::engine::Lender`].
pub type Lends = Vec<(
    crate::engine::Lender,
    tokio::sync::mpsc::UnboundedSender<crate::engine::Lent>,
)>;

/// A source of [`MailboxRun`]s, read once per collection.
pub type MailboxSource =
    Box<dyn Fn() -> Pin<Box<dyn Future<Output = MailboxRun> + Send>> + Send + Sync>;

/// The runners' effects on the home node: the terminal's files, its notices
/// on stderr (the journal under systemd), and a fetched message never
/// printed, because the archive has it and a journal is no place for mail.
struct Quiet(Terminal);

impl Effects for Quiet {
    fn notice(&mut self, line: &str) {
        self.0.notice(line);
    }

    fn print(&mut self, _rendered: &[u8]) -> Result<(), String> {
        Ok(())
    }

    fn save_mailbox(&mut self, stored: &Stored) -> Result<(), String> {
        self.0.save_mailbox(stored)
    }

    fn write_seen(&mut self, write: &SeenWrite) -> Result<(), String> {
        self.0.write_seen(write)
    }

    fn unix_now_ms(&self) -> u64 {
        self.0.unix_now_ms()
    }
}

/// Announce the home destination (1196): with no app data, the identity's
/// public key is all it carries, which the delivery announce publishes
/// already. A failure is said and passed over; the next period tries again.
async fn announce_home(node: &ReticulumNode, home_destination: &[u8; 16]) {
    let hash = leviculum_core::DestinationHash::new(*home_destination);
    match node.announce_destination(&hash, None).await {
        Ok(()) => tracing::info!(
            "HOME announced home={}",
            crate::address::to_hex(home_destination)
        ),
        Err(error) => tracing::warn!("HOME announce of the home destination: {error}"),
    }
}

/// Run the home node until SIGINT or SIGTERM, or until `stop` says so.
pub async fn run(options: DaemonOptions) -> Result<(), String> {
    let DaemonOptions {
        home,
        upstream,
        storage_dir,
        identity,
        display_name,
        archive_cap_bytes,
        token_ttl_secs,
        policy,
        preferred_pn,
        pn_source,
        sync_bound_ms,
        fetch_budget,
        wake,
        stop,
        network,
        lends,
    } = options;
    if let Some(at) = super::handover::gone_at(&home) {
        return Err(format!(
            "this mailbox moved to another device at {at} (unix time): it does not start \
             here again until its owner takes it back"
        ));
    }
    let _lock = super::lock::DeliveryLock::acquire(&home, super::lock::HOME_NODE)?;
    let address = crate::engine::delivery_address(&identity)?;
    let ratchet_file = leviculum_std::dest_ratchet_keys_path(&storage_dir, &address);
    let known_file = storage_dir.join(KNOWN_FILE);
    let own_name = match String::from_utf8_lossy(&display_name).into_owned() {
        name if name == crate::display_name::FALLBACK => String::new(),
        name => name,
    };
    let home_destination = crate::home::destination_hash(&identity.public_key_bytes())?;
    let mut store = Store::open_in(&home, address).map_err(|e| e.to_string())?;
    backfill_contacts(&mut store, &home, archive_cap_bytes)?;
    archive_own_name(&mut store, address, &own_name, archive_cap_bytes);
    let mut relations = relations_at_start(&store, &home)?;
    let signer = identity.clone();
    let mut outgoing = Outgoing::new(policy);
    // What the propagation node is still to be asked about (1409): the
    // uploads before a restart are asked about after it.
    let mut asker = crate::pn_status::Asker::restore(store.pn_asks().map_err(|e| e.to_string())?);
    for kept in store.outbox_open().map_err(|e| e.to_string())? {
        outgoing.add(
            kept.id,
            kept.destination,
            kept.packed,
            kept.created_at,
            kept.propagated,
        );
    }
    let group_upstream = match &upstream {
        DaemonUpstream::Shared {
            instance,
            config_dir,
            ..
        } => GroupUpstream::Shared {
            instance: instance.clone(),
            config_dir: config_dir.clone(),
        },
        DaemonUpstream::Mesh {
            servers,
            tcp,
            timing,
            ..
        } => GroupUpstream::Leaf {
            servers: servers.clone(),
            tcp: *tcp,
            timing: *timing,
        },
    };
    let group_storage = storage_dir.join(group::GROUPS_DIR);
    let group_network = network.clone();
    let (requests_tx, mut requests) = unbounded_channel::<HomeRequest>();
    let role = Role::Home {
        requests: requests_tx,
    };
    let (instance, attached, mailbox) = match upstream {
        DaemonUpstream::Shared {
            instance,
            config_dir,
            mailbox,
            rendezvous,
            tcp,
        } => {
            let mut attached = attach_as(
                AttachConfig {
                    instance: instance.clone(),
                    config_dir: Some(config_dir),
                    storage_dir,
                    identity,
                    display_name,
                    notices: Terminal::new(home.clone()),
                },
                role,
            )
            .await
            .map_err(|e| e.to_string())?;
            attached
                .dial(
                    &rendezvous,
                    super::rendezvous::Side::Peer,
                    tcp,
                    super::rendezvous::Timing::default(),
                    network,
                )
                .map_err(|e| e.to_string())?;
            (instance, attached, mailbox)
        }
        DaemonUpstream::Mesh {
            servers,
            screens,
            tcp,
            timing,
        } => {
            let attached = attach_mesh(
                MeshConfig {
                    servers,
                    timing,
                    screens,
                    storage_dir,
                    identity,
                    display_name,
                    notices: Terminal::new(home.clone()),
                    tcp,
                    network,
                },
                role,
            )
            .await
            .map_err(|e| e.to_string())?;
            let source = own_routes_source(attached.node(), home.clone());
            ("in-process".to_string(), attached, source)
        }
    };
    let mut attached = attached;
    let collector = Collector {
        home: home.clone(),
        instance,
        budget: fetch_budget,
        preferred: preferred_pn,
        source: pn_source,
        mailbox,
    };
    let node = attached.node();
    for (lender, to) in &lends {
        // A screen that went away borrows nothing.
        let _ = to.send(attached.lend_to(*lender));
    }
    // The groups' open copies are counted beside the home's own outbox:
    // they go out on the groups' engines, which start below, and the line
    // said `outbox=9` where 27 went out within two seconds (1406).
    tracing::warn!(
        "HOME up address={} home={} archive_cap={}B outbox={} group_copies={}",
        crate::address::to_hex(&address),
        crate::address::to_hex(&home_destination),
        archive_cap_bytes,
        outgoing.len(),
        store.group_copies_open_count().map_err(|e| e.to_string())?
    );
    // The name the owner goes by in the groups it makes (1307): its
    // announced name, else the start of its address.
    let owner_name = match own_name.trim() {
        "" => crate::address::to_hex(&address)[..8].to_string(),
        name => name.to_string(),
    };
    let mut serving = super::server::Home {
        identity: &signer,
        address,
        token_ttl_secs,
        cap_bytes: archive_cap_bytes,
        handover: None,
        groups: Some((&home, &owner_name)),
        shown: super::server::Shown::here(&own_name),
    };
    let mut sender = Sender {
        cap_bytes: archive_cap_bytes,
        preferred_pn,
        collecting: sync_bound_ms.is_some(),
        told: std::collections::HashSet::new(),
        newest: 0,
        nodes: std::collections::HashMap::new(),
        proven: Vec::new(),
        lines: Default::default(),
    };
    let mut groups = Vec::new();
    for hosted in group::hosted(&home, &store)? {
        let name = hosted.group.name.clone();
        match attach_group(
            &group_upstream,
            &group_storage,
            &home,
            hosted,
            &group_network,
            policy,
        )
        .await
        {
            Ok(mut engine) => {
                knows_owner(&engine, address, &signer);
                // The copies a restart cut off go again; the outbox's next
                // tick hands them to the engine.
                resume_group(&mut store, &mut engine, unix_now());
                groups.push(engine);
            }
            Err(error) => tracing::warn!("GROUP {name} not hosted: {error}"),
        }
    }
    let mut wakes: Wakes<LinkId> = Wakes::default();
    let mut costs: Ledger<LinkId> = Ledger::default();
    let mut costs_look = tokio::time::interval(COSTS_LOOK);
    let mut next_summary = tokio::time::Instant::now() + SUMMARY_EVERY;
    wakes.appended(store.archive_newest_waking().map_err(|e| e.to_string())?);
    let mut outside_version = store.data_version().map_err(|e| e.to_string())?;

    let mut fanout = Fanout::default();
    let mut schedule = sync_bound_ms.map(|bound| Schedule::new(Some(bound)));
    let mut fetch: Option<tokio::task::JoinHandle<Option<usize>>> = None;
    // The sync rounds finished, and the `/lev/status` questions in flight,
    // one a node (1409).
    let mut pn_round: u64 = 0;
    let mut pn_asks: Vec<([u8; 16], tokio::task::JoinHandle<Option<PnAnswer>>)> = Vec::new();
    // The node the engine's router has selected, as its PnSelected said:
    // not `--pn` itself, which the router learns only from a SelectPn, and
    // with collections off nothing else sends one.
    let mut selected_pn = None;
    // Whether the engine said Ready: what a later `Broken` means hangs on it.
    let mut engine_ready = false;
    let origin = tokio::time::Instant::now();
    let now_ms = || u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut next_pump = tokio::time::Instant::now();
    let mut quiet_pumps: u32 = 0;
    let mut outbox_tick = tokio::time::interval(OUTBOX_TICK);
    let mut outside = tokio::time::interval(OUTSIDE_LOOK);
    // The first announce goes out with the engine's registration; the next
    // one after the period, read again at every announce so the shorter
    // cadence ends a day after the move.
    let moved_at = crate::identity::moved_at(&home);
    if announce_period(moved_at, unix_now()) == ANNOUNCE_EVERY_AFTER_MOVE {
        tracing::warn!(
            "HOME identity moved in {} s ago: announcing every {} min for the first day",
            unix_now().saturating_sub(moved_at.unwrap_or(0)),
            ANNOUNCE_EVERY_AFTER_MOVE.as_secs() / 60
        );
    }
    let mut next_announce = tokio::time::Instant::now() + announce_period(moved_at, unix_now());
    // Each group announced with its engine's registration; the next ones
    // on the groups' own hour (1307 G2).
    let mut next_group_announce = tokio::time::Instant::now() + GROUP_ANNOUNCE_EVERY;
    // The delivery announce went out with the registration; the home
    // destination's goes beside it, and beside every one that follows.
    announce_home(&node, &home_destination).await;
    // A home node inside an app leaves the process's signals to the app.
    let mut terminate = match stop {
        Some(_) => None,
        None => Some(
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|e| format!("SIGTERM handler: {e}"))?,
        ),
    };
    let mut stop = stop;
    // The census (order 1585): every CENSUS_PERIOD into the log, and on
    // SIGUSR1 with every node's transport census on stderr, once the
    // engines answered or ANSWER_WAIT passed. The home engine's answer;
    // the groups' are on their engines.
    let mut census: Option<crate::outbox::EngineCensus> = None;
    let mut next_census = tokio::time::Instant::now() + super::census::CENSUS_PERIOD;
    let mut census_owed: Option<(tokio::time::Instant, bool)> = None;
    let mut usr1 = match terminate {
        Some(_) => Some(
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
                .map_err(|e| format!("SIGUSR1 handler: {e}"))?,
        ),
        None => None,
    };
    // From here a SIGUSR1 is the census's, not the end of the process
    // (1691): before this line its default disposition still kills the
    // mailbox, and `HOME up` and `HOME upstream` come earlier, so a
    // supervisor that signals on either can lose the process.
    if usr1.is_some() {
        tracing::warn!("HOME signals SIGUSR1=census SIGTERM=stop");
    }
    #[cfg(feature = "heap-accounting")]
    if usr1.is_some() {
        tracing::info!(
            "Heap accounting: on, SIGUSR1 reports measured live bytes beside the estimate"
        );
        leviculum_std::heap_accounting::configure_sampler_from_env();
    }

    // Every wake of the loop below, for the census (1723).
    let mut turns: u64 = 0;
    let outcome = loop {
        turns = turns.saturating_add(1);
        tokio::select! {
            Some(request) = requests.recv() => {
                // A device is talking: its answers' events are read at once.
                quiet_pumps = 0;
                next_pump = tokio::time::Instant::now() + PUMP;
                let (link, remote) = (request.link_id, request.remote);
                serving.handover =
                    handover(&home, [&ratchet_file, &known_file], &own_name, &request.path);
                let (accepted, watching, gone, (made, added, deleted), size) =
                    answer(&node, &mut store, &serving, request).await;
                if let Some(device) = remote {
                    // A device's link takes the long keepalive the device
                    // asks of it (1346), or it goes stale at 20 s.
                    if costs.asked(link, device, size as u64) {
                        node.set_link_keepalive(&link, super::keepalive::LINK_KEEPALIVE_SECS);
                    }
                }
                if let Some((address, told)) = deleted {
                    // The owner deleted the group (1307 pass four): its
                    // engine sends the last membership, then refuses all.
                    if let Some(engine) = groups.iter_mut().find(|g| g.hosted.group.address == address) {
                        engine.ended = true;
                        let now = unix_now();
                        for out in told {
                            engine.outgoing.add(out.id, out.destination, out.packed, now, false);
                        }
                        let acts = engine.outgoing.tick(now);
                        sender.carry_out(&mut engine.engine, &mut store, &mut engine.outgoing, acts, selected_pn, engine.selected_pn);
                    }
                }
                if let Some((address, told)) = added {
                    // Members added (1307 G2): the new membership goes out
                    // on the group's own engine.
                    if let Some(engine) = groups.iter_mut().find(|g| g.hosted.group.address == address) {
                        let now = unix_now();
                        for out in told {
                            engine.outgoing.add(out.id, out.destination, out.packed, now, false);
                        }
                        let acts = engine.outgoing.tick(now);
                        sender.carry_out(&mut engine.engine, &mut store, &mut engine.outgoing, acts, selected_pn, engine.selected_pn);
                    }
                }
                if let Some(made) = made {
                    // A group a device made (1307 pass two): hosted from
                    // now on, its membership to the admin on its engine.
                    let name = made.hosted.group.name.clone();
                    match attach_group(
                        &group_upstream,
                        &group_storage,
                        &home,
                        made.hosted,
                        &group_network,
                        policy,
                    )
                    .await
                    {
                        Ok(mut engine) => {
                            knows_owner(&engine, address, &signer);
                            let now = unix_now();
                            for out in made.welcome {
                                engine.outgoing.add(out.id, out.destination, out.packed, now, false);
                            }
                            let acts = engine.outgoing.tick(now);
                            sender.carry_out(&mut engine.engine, &mut store, &mut engine.outgoing, acts, selected_pn, engine.selected_pn);
                            groups.push(engine);
                        }
                        Err(error) => tracing::warn!("GROUP {name} not hosted: {error}"),
                    }
                }
                if gone {
                    // The new device holds everything (1281): this mailbox
                    // ends here, once its answer had a moment to leave.
                    if let Err(error) = super::handover::mark_gone(&home, unix_now()) {
                        tracing::warn!("HOME moved: {error}");
                    }
                    tokio::time::sleep(GONE_GRACE).await;
                    break Ok(());
                }
                if let (Some(told), Some(device), true) = (watching, remote, wake) {
                    wakes.watch(link, device, told);
                }
                // A send and a mark append; a failed read is said by the
                // next append that reads it.
                if let Ok(newest) = store.archive_newest_waking() {
                    wakes.appended(newest);
                }
                for accepted in accepted {
                    related(&mut store, &mut relations, accepted.destination, archive_cap_bytes);
                    outgoing.add(
                        accepted.id,
                        accepted.destination,
                        accepted.packed,
                        accepted.created_at,
                        false,
                    );
                    let acts = outgoing.tick(unix_now());
                    sender.carry_out(&mut attached.outbox, &mut store, &mut outgoing, acts, selected_pn, selected_pn);
                }
            }
            _ = costs_look.tick() => {
                for link in costs.links() {
                    costs.keepalives(&link, node.link_keepalive(&link).map(|(_, n)| n));
                }
            }
            _ = tokio::time::sleep_until(next_summary) => {
                next_summary = tokio::time::Instant::now() + SUMMARY_EVERY;
                for (device, window) in costs.take() {
                    tracing::info!(
                        "{}",
                        super::keepalive::mailbox_line(&crate::address::to_hex(&device), &window)
                    );
                }
            }
            _ = outside.tick() => {
                if let Some(newest) = written_outside(&mut store, &mut outside_version) {
                    wakes.appended(newest);
                }
            }
            _ = outbox_tick.tick() => {
                let acts = outgoing.tick(unix_now());
                sender.carry_out(&mut attached.outbox, &mut store, &mut outgoing, acts, selected_pn, selected_pn);
                for hosted in &mut groups {
                    digests(&mut store, hosted, unix_now());
                    let acts = hosted.outgoing.tick(unix_now());
                    sender.carry_out(&mut hosted.engine, &mut store, &mut hosted.outgoing, acts, selected_pn, hosted.selected_pn);
                }
            }
            _ = tokio::time::sleep_until(next_group_announce) => {
                next_group_announce = tokio::time::Instant::now() + GROUP_ANNOUNCE_EVERY;
                for hosted in groups.iter().filter(|g| !g.ended) {
                    // A group whose engine went away is said by its pump.
                    let _ = hosted.engine.submit(Command::Announce);
                }
            }
            _ = tokio::time::sleep_until(next_pump) => {
                let events = match fanout.pump(&mut attached.outbox) {
                    Ok(events) => events,
                    Err(gone) => break Err(gone.to_string()),
                };
                // Busy while anything came or is in flight; quiet after a
                // second of nothing (1585).
                let busy = !events.is_empty()
                    || fetch.is_some()
                    || !pn_asks.is_empty()
                    || census_owed.is_some();
                quiet_pumps = if busy { 0 } else { quiet_pumps.saturating_add(1) };
                next_pump = tokio::time::Instant::now()
                    + if quiet_pumps >= QUIET_PUMPS { PUMP_QUIET } else { PUMP };
                let mut unstarted = None;
                for event in events {
                    match event {
                        OutboxEvent::Ready { .. } => {
                            engine_ready = true;
                            if let Some(schedule) = &mut schedule {
                                schedule.on(SyncEvent::Start, now_ms());
                            }
                        }
                        OutboxEvent::Packed { message_id, packed, arrived } => {
                            let Ok(new) = archive(&mut store, &message_id, &packed, archive_cap_bytes)
                            else {
                                // Not archived, so not proven: the sender
                                // tries again, or propagates.
                                continue;
                            };
                            // A receipt names ours: their node is asked no
                            // more (1409).
                            for id in super::server::settles(&packed) {
                                asker.settled(&id);
                            }
                            crate::events::home_accept(
                                &message_id,
                                &source_of(&packed).unwrap_or([0; 16]),
                                arrived.as_str(),
                                &address,
                            );
                            // Archived, now or before: the sender may be
                            // told so (1239).
                            // An engine that went away is said by the next pump.
                            let proving = attached.outbox.submit(Command::Prove { message_id }).is_ok();
                            // A propagated message has no proof to release:
                            // the propagation node took it, not its sender.
                            if proving && arrived != crate::outbox::Arrived::Propagated {
                                crate::events::home_prove(&message_id, &address);
                            }
                            if let Some(seq) = new {
                                wakes.appended(seq);
                                if let Some(source) = source_of(&packed) {
                                    related(&mut store, &mut relations, source, archive_cap_bytes);
                                }
                                learn_invited_key(&node, &packed);
                            }
                        }
                        OutboxEvent::PnSelected { destination, .. } => {
                            selected_pn = Some(destination);
                        }
                        OutboxEvent::Census(answer) => census = Some(answer),
                        // A node proved an upload (1409): asked about at the
                        // end of the next sync rounds.
                        OutboxEvent::Uploaded { message_id, transient_id, node: proved_by } => {
                            if let Some(pn) = proved_by.or(selected_pn) {
                                asker.uploaded(pn, message_id, transient_id, unix_now());
                            }
                        }
                        OutboxEvent::PnUnavailable { detail } => {
                            tracing::info!("HOME no propagation node: {detail}");
                        }
                        OutboxEvent::HubAnnounced { hub } => {
                            // For every device of the home (1599); it wakes nobody.
                            archive_hub(&mut store, &hub, archive_cap_bytes);
                        }
                        OutboxEvent::PageAnnounced { node } => {
                            // For every device of the home (1579): its `/pages`.
                            keep_page(&mut store, &node);
                        }
                        OutboxEvent::Announced { destination, name, path_response } => {
                            if let Some(name) = name.filter(|_| destination != address) {
                                heard_name(
                                    &mut store,
                                    &mut relations,
                                    destination,
                                    &name,
                                    archive_cap_bytes,
                                );
                            }
                            let acts = outgoing.announced(destination, heard(path_response), unix_now());
                            sender.carry_out(&mut attached.outbox, &mut store, &mut outgoing, acts, selected_pn, selected_pn);
                        }
                        OutboxEvent::Left { message_id, last, failure } => {
                            if last == Some(MessageState::Delivered) {
                                asker.settled(&message_id);
                            }
                            let (outcome, reason) = left_outcome(last, failure);
                            sender.left(&mut attached.outbox, &mut store, &mut outgoing, &message_id, outcome, reason.as_deref(), selected_pn, selected_pn);
                        }
                        OutboxEvent::Queued { message_id } => {
                            sender.queued(&outgoing, &message_id);
                        }
                        OutboxEvent::State { message_id, state } => {
                            if state == MessageState::Delivered {
                                asker.settled(&message_id);
                            }
                            // A sign of the leg: the stall bound counts from now.
                            leg_sign(&mut outgoing, &message_id, state);
                        }
                        OutboxEvent::Unsent { message_id, detail } => {
                            sender.left(&mut attached.outbox, &mut store, &mut outgoing, &message_id, Outcome::Failed, Some(&format!("unsent: {detail}")), selected_pn, selected_pn);
                        }
                        OutboxEvent::KeyMissing { message_id, destination } => {
                            sender.left(&mut attached.outbox, &mut store, &mut outgoing, &message_id, Outcome::NoKey, Some(&no_key_reason(&destination)), selected_pn, selected_pn);
                        }
                        OutboxEvent::Refused { detail } => {
                            tracing::info!("HOME outbox refused: {detail}");
                        }
                        OutboxEvent::NodeAnnounced { destination, status, .. } => {
                            // Whether it answers `/lev/status` (1409).
                            asker.announced(destination, status);
                            if Some(destination) == selected_pn {
                                if let Some(schedule) = &mut schedule {
                                    schedule.on(SyncEvent::Announce, now_ms());
                                }
                            }
                        }
                        OutboxEvent::Broken { detail } => match engine_error(engine_ready, &detail) {
                            Ok(line) => tracing::warn!("{line}"),
                            Err(fatal) => unstarted = Some(fatal),
                        },
                        _ => {}
                    }
                }
                if let Some(fatal) = unstarted {
                    break Err(fatal);
                }
                for hosted in &mut groups {
                    pump_group(&mut store, &mut sender, hosted, selected_pn);
                }
                if let Some(task) = fetch.take_if(|task| task.is_finished()) {
                    let pulled = task.await.ok().flatten();
                    let event = match pulled {
                        Some(new) => SyncEvent::Done { new },
                        None => SyncEvent::Failed,
                    };
                    if let Some(schedule) = &mut schedule {
                        schedule.on(event, now_ms());
                        if let Some(n) = pulled {
                            tracing::info!("{}", pn_pull_line(n, selected_pn, schedule, now_ms()));
                        }
                    }
                    // The end of a round that synced: what became of our
                    // uploads (1409). One that failed found the node silent.
                    if pulled.is_some() {
                        pn_round += 1;
                        ask_pns(&node, &mut asker, &mut pn_asks, pn_round);
                    }
                }
                for (pn, answer) in pn_answered(&mut pn_asks).await {
                    let (answers, line) = asker.answered(pn, answer.as_deref());
                    tracing::info!("{line}");
                    if let Some(seq) =
                        super::server::keep_pn_status(&mut store, archive_cap_bytes, &answers, unix_now())
                    {
                        wakes.appended(seq);
                    }
                }
                if let Some(state) = asker.take_changed() {
                    if let Err(error) = store.save_pn_asks(&state) {
                        tracing::warn!("HOME pn asks not kept: {error}");
                    }
                }
                wakes.appended(sender.newest);
                for due in wakes.due(now_ms()) {
                    send_wake(&node, &mut store, &mut wakes, &mut costs, due).await;
                }
                if let Some((due, dump)) = census_owed {
                    let answered = census.is_some() && groups.iter().all(|g| g.census.is_some());
                    if answered || tokio::time::Instant::now() >= due {
                        census_owed = None;
                        let held = Held {
                            outgoing: &outgoing,
                            groups: &groups,
                            sender: &sender,
                            relations: &relations,
                            asker: &asker,
                            wakes: &wakes,
                            costs: &costs,
                            pn_asks: pn_asks.len(),
                            home: census.take(),
                            store: &store,
                            turns,
                        };
                        let line = held.census();
                        line.log(if dump { "signal" } else { "period" });
                        if dump {
                            eprint!("{}", census_dump(&node, &groups, &line));
                        }
                        for hosted in &mut groups {
                            hosted.census = None;
                        }
                    }
                }
                if fetch.is_none() {
                    if let Some(schedule) = &mut schedule {
                        if schedule.due(now_ms()).is_some() {
                            schedule.begin(now_ms());
                            fetch = Some(collector.collect(&mut fanout));
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(next_announce) => {
                next_announce = tokio::time::Instant::now() + announce_period(moved_at, unix_now());
                if attached.outbox.submit(Command::Announce).is_err() {
                    break Err("the message engine stopped".to_string());
                }
                announce_home(&node, &home_destination).await;
            }
            _ = tokio::time::sleep_until(next_census) => {
                next_census = tokio::time::Instant::now() + super::census::CENSUS_PERIOD;
                ask_census(&attached, &groups);
                census_owed.get_or_insert((tokio::time::Instant::now() + super::census::ANSWER_WAIT, false));
            }
            _ = user_signal(usr1.as_mut()) => {
                ask_census(&attached, &groups);
                census_owed = Some((tokio::time::Instant::now() + super::census::ANSWER_WAIT, true));
            }
            _ = signalled(terminate.as_mut()) => break Ok(()),
            _ = stopped(stop.as_mut()) => break Ok(()),
        }
    };
    if let Some(task) = fetch {
        task.abort();
    }
    for (_, task) in pn_asks {
        task.abort();
    }
    drop(node);
    attached.stop().await;
    for hosted in groups {
        hosted.engine.stop().await;
    }
    outcome
}

/// What a propagation node answered to `/lev/status`, by `transient_id`.
type PnAnswer = Vec<([u8; 32], leviculum_lxmf::lev_status::MessageStatus)>;

/// How long one `/lev/status` question may take, path, link and answer: a
/// node that does not answer in it is asked again the next round.
const PN_ASK_BUDGET: Duration = Duration::from_secs(30);

/// Ask every node that holds an open upload, once in sync round `round`,
/// each on a task of its own over a link it never identifies (1409); say
/// why a node is not asked. A node with a question still in flight is not
/// asked again (`rate`).
fn ask_pns(
    node: &std::sync::Arc<ReticulumNode>,
    asker: &mut crate::pn_status::Asker,
    asks: &mut Vec<([u8; 16], tokio::task::JoinHandle<Option<PnAnswer>>)>,
    round: u64,
) {
    for (pn, asked) in asker.round(round, unix_now()) {
        if asks.iter().any(|(busy, _)| *busy == pn) {
            tracing::info!("{}", crate::pn_status::Skip::Rate.line());
            continue;
        }
        match asked {
            Ok(ids) => {
                let reticulum = std::sync::Arc::clone(node);
                asks.push((
                    pn,
                    tokio::spawn(async move {
                        super::client::pn_answer(&reticulum, pn, &ids, PN_ASK_BUDGET).await
                    }),
                ));
            }
            Err(skip) => tracing::info!("{}", skip.line()),
        }
    }
}

/// The questions that came back, each with its node; a task that failed
/// says nothing, as silence does.
async fn pn_answered(
    asks: &mut Vec<([u8; 16], tokio::task::JoinHandle<Option<PnAnswer>>)>,
) -> Vec<([u8; 16], Option<PnAnswer>)> {
    let mut done = Vec::new();
    let mut i = 0;
    while i < asks.len() {
        if asks[i].1.is_finished() {
            let (pn, task) = asks.remove(i);
            done.push((pn, task.await.ok().flatten()));
        } else {
            i += 1;
        }
    }
    done
}

/// What the engine's `Broken` means to the home: the line to log while it
/// runs on, or why the service ends.
///
/// Before Ready the engine could not register and never will, so the
/// service ends and its unit restarts it. After Ready it is one router call
/// that failed while the engine went on (each hook puts its state back,
/// `crate::engine`), and the line says so. On miauhaus the home logged
/// "engine broken" on 2026-11-04 and kept archiving for seven minutes
/// (1412).
fn engine_error(engine_ready: bool, detail: &str) -> Result<String, String> {
    if engine_ready {
        Ok(format!(
            "HOME engine error, the engine keeps running: {detail}"
        ))
    } else {
        Err(format!("the message engine could not start: {detail}"))
    }
}

/// How a group's engine reaches the network: as the home's own does.
enum GroupUpstream {
    Shared {
        instance: String,
        config_dir: PathBuf,
    },
    /// The mailbox with a node of its own: the group is a leaf on the same
    /// entry servers, and the home's own node reaches it through them.
    Leaf {
        servers: Vec<String>,
        tcp: crate::config::TcpLiveness,
        timing: super::rendezvous::Timing,
    },
}

/// A hosted group, its engine and its copies' outbox.
struct GroupEngine<O> {
    hosted: Hosted,
    engine: O,
    outgoing: Outgoing,
    /// The propagation node this engine has selected: its own, since the
    /// home's engine selecting one selects nothing here (1328).
    selected_pn: Option<[u8; 16]>,
    /// Its admin deleted it (1307 pass four): the store and the key are
    /// gone, the engine stays until the daemon stops to send the last
    /// membership and to prove and drop every later message, as a
    /// stranger's.
    ended: bool,
    /// What its engine answered to the last census request (order 1585).
    census: Option<crate::outbox::EngineCensus>,
}

/// Start the engine of one hosted group, announced with the group's name,
/// its node's state under `storage` in a directory of the group's own.
async fn attach_group(
    upstream: &GroupUpstream,
    storage: &Path,
    home: &Path,
    hosted: Hosted,
    network: &super::rendezvous::Network,
    policy: Policy,
) -> Result<GroupEngine<Attached>, String> {
    let storage_dir = storage.join(crate::address::to_hex(&hosted.group.address));
    let display_name = hosted.group.name.as_bytes().to_vec();
    let identity = hosted.identity.clone();
    let engine = match upstream {
        GroupUpstream::Shared {
            instance,
            config_dir,
        } => attach_as(
            AttachConfig {
                instance: instance.clone(),
                config_dir: Some(config_dir.clone()),
                storage_dir,
                identity,
                display_name,
                notices: Terminal::new(home.to_path_buf()),
            },
            Role::Group,
        )
        .await
        .map_err(|e| e.to_string())?,
        GroupUpstream::Leaf {
            servers,
            tcp,
            timing,
        } => attach_leaf(
            LeafConfig {
                servers: servers.clone(),
                home_destination: None,
                timing: *timing,
                storage_dir,
                identity,
                display_name,
                notices: Terminal::new(home.to_path_buf()),
                tcp: *tcp,
                network: network.clone(),
            },
            Role::Group,
        )
        .await
        .map_err(|e| e.to_string())?,
    };
    tracing::warn!(
        "GROUP hosted name={} address={}",
        hosted.group.name,
        crate::address::to_hex(&hosted.group.address)
    );
    Ok(GroupEngine {
        outgoing: Outgoing::new(policy),
        hosted,
        engine,
        selected_pn: None,
        ended: false,
        census: None,
    })
}

/// Take every event a group's engine has: a member's message is archived
/// as [`crate::home::KIND_GROUP`], proven, and when new fanned out; the
/// copies' legs end as the home's outbox messages do, and each copy's
/// change is booked by [`Sender::record`] in `group_fanout`.
fn pump_group<O: Outbox>(
    store: &mut Store,
    sender: &mut Sender,
    group: &mut GroupEngine<O>,
    selected_pn: Option<[u8; 16]>,
) {
    loop {
        let event = match group.engine.try_next_event() {
            Ok(Some(event)) => event,
            Ok(None) => {
                if send_statuses(store, sender, group, selected_pn) {
                    continue;
                }
                return;
            }
            Err(gone) => {
                tracing::warn!("GROUP {} engine: {gone}", group.hosted.group.name);
                return;
            }
        };
        match event {
            OutboxEvent::Census(answer) => group.census = Some(answer),
            OutboxEvent::Packed {
                message_id, packed, ..
            } if group.ended => {
                // A deleted group keeps nothing and sends nothing on: the
                // writer's client stops retrying (as for a stranger).
                tracing::info!(
                    "GROUP {} ended: refused message={}",
                    group.hosted.group.name,
                    crate::address::to_hex(&message_id)
                );
                drop(packed);
                let _ = group.engine.submit(Command::Prove { message_id });
            }
            OutboxEvent::Packed {
                message_id, packed, ..
            } => {
                let now = unix_now();
                let new = match store.archive_append(
                    crate::home::KIND_GROUP,
                    &message_id,
                    &packed,
                    now,
                    sender.cap_bytes,
                ) {
                    Ok(new) => new,
                    Err(error) => {
                        // Not archived, so not proven: the member tries again.
                        tracing::warn!("GROUP archive: {error}");
                        continue;
                    }
                };
                let _ = group.engine.submit(Command::Prove { message_id });
                if new.is_none() {
                    continue;
                }
                let message = match leviculum_lxmf::Message::unpack(
                    &packed,
                    None,
                    None,
                    leviculum_lxmf::DeliveryMethod::Direct,
                ) {
                    Ok(message) => message,
                    Err(error) => {
                        tracing::warn!("GROUP unreadable message: {error:?}");
                        continue;
                    }
                };
                let arrival = match group::accept(store, &group.hosted, &message, now as f64) {
                    Ok(arrival) => arrival,
                    Err(error) => {
                        tracing::warn!("GROUP fan-out: {error}");
                        continue;
                    }
                };
                for out in arrival.outgoing() {
                    group
                        .outgoing
                        .add(out.id, out.destination, out.packed, now, false);
                }
                let acts = group.outgoing.tick(now);
                sender.carry_out(
                    &mut group.engine,
                    store,
                    &mut group.outgoing,
                    acts,
                    selected_pn,
                    group.selected_pn,
                );
            }
            OutboxEvent::Announced {
                destination,
                path_response,
                ..
            } => {
                let acts = group
                    .outgoing
                    .announced(destination, heard(path_response), unix_now());
                sender.carry_out(
                    &mut group.engine,
                    store,
                    &mut group.outgoing,
                    acts,
                    selected_pn,
                    group.selected_pn,
                );
            }
            OutboxEvent::Left {
                message_id,
                last,
                failure,
            } => {
                let (outcome, reason) = left_outcome(last, failure);
                sender.left(
                    &mut group.engine,
                    store,
                    &mut group.outgoing,
                    &message_id,
                    outcome,
                    reason.as_deref(),
                    selected_pn,
                    group.selected_pn,
                );
            }
            OutboxEvent::Queued { message_id } => {
                sender.queued(&group.outgoing, &message_id);
            }
            OutboxEvent::State { message_id, state } => {
                leg_sign(&mut group.outgoing, &message_id, state);
            }
            OutboxEvent::Unsent { message_id, detail } => {
                sender.left(
                    &mut group.engine,
                    store,
                    &mut group.outgoing,
                    &message_id,
                    Outcome::Failed,
                    Some(&format!("unsent: {detail}")),
                    selected_pn,
                    group.selected_pn,
                );
            }
            OutboxEvent::KeyMissing {
                message_id,
                destination,
            } => {
                sender.left(
                    &mut group.engine,
                    store,
                    &mut group.outgoing,
                    &message_id,
                    Outcome::NoKey,
                    Some(&no_key_reason(&destination)),
                    selected_pn,
                    group.selected_pn,
                );
            }
            OutboxEvent::PnSelected { destination, .. } => {
                group.selected_pn = Some(destination);
            }
            OutboxEvent::Refused { detail } => {
                tracing::info!("GROUP {} outbox refused: {detail}", group.hosted.group.name);
            }
            _ => {}
        }
    }
}

/// Send the statuses the proofs booked since the last call made due for
/// `group`'s writers (1307 pass four, [`group::status_after_proof`]).
/// Whether anything was sent, so the pump looks for events again.
fn send_statuses<O: Outbox>(
    store: &mut Store,
    sender: &mut Sender,
    group: &mut GroupEngine<O>,
    selected_pn: Option<[u8; 16]>,
) -> bool {
    let address = group.hosted.group.address;
    let (ours, others): (Vec<_>, Vec<_>) =
        sender.proven.drain(..).partition(|(g, _)| *g == address);
    sender.proven = others;
    if ours.is_empty() || group.ended {
        return false;
    }
    let now = unix_now();
    let mut sent = false;
    for (_, original) in ours {
        match group::status_after_proof(store, &group.hosted, &original, now as f64) {
            Ok(Some(out)) => {
                group
                    .outgoing
                    .add(out.id, out.destination, out.packed, now, false);
                sent = true;
            }
            Ok(None) => {}
            Err(error) => tracing::warn!("GROUP {} status: {error}", group.hosted.group.name),
        }
    }
    if sent {
        let acts = group.outgoing.tick(now);
        sender.carry_out(
            &mut group.engine,
            store,
            &mut group.outgoing,
            acts,
            selected_pn,
            group.selected_pn,
        );
    }
    sent
}

/// A group's copies a restart cut off (1307 G2, study 2.4): every copy
/// sent alone with no proof yet is made again from the archived original,
/// byte for byte, or under the text rule this program has when the rule
/// changed since (1406, [`group::rebuild`]), and handed to the group's
/// outbox with its acceptance time, so the outbox's retry bound counts from
/// then; a copy that cannot be made again (its original evicted, its member
/// gone) is ended. Every
/// copy that went out in a digest with no proof is held again, and the
/// next digest carries it.
fn resume_group<O>(store: &mut Store, group: &mut GroupEngine<O>, now: i64) -> usize {
    let address = group.hosted.group.address;
    let name = group.hosted.group.name.clone();
    match store.group_digests_reopen(&address) {
        Ok(0) => {}
        Ok(n) => tracing::info!("GROUP {name} holds {n} lines again for their digest"),
        Err(error) => tracing::warn!("GROUP {name} digests: {error}"),
    }
    let open = match store.group_copies_open(&address) {
        Ok(open) => open,
        Err(error) => {
            tracing::warn!("GROUP {name} open copies: {error}");
            return 0;
        }
    };
    let mut resumed = 0;
    for row in open {
        match group::rebuild(store, &group.hosted, &row) {
            Ok(Some(out)) => {
                if out.id != row.copy {
                    tracing::warn!(
                        "GROUP {name} copy {} made again under this program's text rule as {}",
                        crate::address::to_hex(&row.copy),
                        crate::address::to_hex(&out.id)
                    );
                }
                // A copy a node took before the restart is not uploaded
                // again (1307 pass four, the 1330 side finding).
                group.outgoing.add(
                    out.id,
                    out.destination,
                    out.packed,
                    row.sent_at.min(now),
                    row.propagated,
                );
                resumed += 1;
            }
            Ok(None) => {
                tracing::warn!(
                    "GROUP {name} copy {} cannot be made again: ended",
                    crate::address::to_hex(&row.copy)
                );
                if let Err(error) = store.group_copy_ended(&row.copy) {
                    tracing::warn!("GROUP {name} copy record: {error}");
                }
            }
            Err(error) => tracing::warn!("GROUP {name} copy rebuild: {error}"),
        }
    }
    if resumed > 0 {
        tracing::warn!("GROUP {name} resumed {resumed} copies from the archive");
    }
    resumed
}

/// Send every digest due for `group`'s slow members (study 5.3) through
/// its outbox; the held copies are booked as sent in it.
fn digests<O>(store: &mut Store, group: &mut GroupEngine<O>, now: i64) {
    let due = match group::due_digests(store, &group.hosted, now) {
        Ok(due) => due,
        Err(error) => {
            tracing::warn!("GROUP {} digest: {error}", group.hosted.group.name);
            return;
        }
    };
    for (out, copies) in due {
        if let Err(error) = store.group_digest_sent(&copies, &out.id, now) {
            tracing::warn!("GROUP {} digest: {error}", group.hosted.group.name);
            continue;
        }
        tracing::info!(
            "GROUP {} digest of {} lines message={}",
            group.hosted.group.name,
            copies.len(),
            crate::address::to_hex(&out.id)
        );
        group
            .outgoing
            .add(out.id, out.destination, out.packed, now, false);
    }
}

/// Whether another connection committed to the store since `version` was
/// read: then the archive's newest sequence of every kind, which the next
/// pump wakes the watching clients for (1164). An import beside the
/// running mailbox appends its history, names and blocks there, and no
/// device hears of them otherwise until something else makes it pull.
/// Names and blocks wake too here: on the mailbox no device has them yet,
/// unlike the ones a device sent with `/petname` or `/peerstate`.
///
/// The store comes `&mut`, though only read, so the loop's future stays
/// `Send`.
fn written_outside(store: &mut Store, version: &mut i64) -> Option<u64> {
    let now = match store.data_version() {
        Ok(now) => now,
        Err(error) => {
            tracing::warn!("HOME store: {error}");
            return None;
        }
    };
    if now == *version {
        return None;
    }
    *version = now;
    match store.archive_newest() {
        Ok(newest) => {
            tracing::info!("HOME archive written outside newest={newest}");
            Some(newest)
        }
        Err(error) => {
            tracing::warn!("HOME store: {error}");
            None
        }
    }
}

/// SIGINT or SIGTERM, for a home node that listens for them; never for one
/// that does not.
async fn signalled(terminate: Option<&mut tokio::signal::unix::Signal>) {
    match terminate {
        Some(terminate) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
        }
        None => std::future::pending().await,
    }
}

/// The app said stop, or dropped its end; never for a home node with none.
/// SIGUSR1, for a home node that listens for it; never for one inside an
/// app, whose signals are the app's.
async fn user_signal(usr1: Option<&mut tokio::signal::unix::Signal>) {
    match usr1 {
        Some(usr1) => {
            usr1.recv().await;
        }
        None => std::future::pending().await,
    }
}

/// Ask every engine for its census (order 1585); the answers come back as
/// [`OutboxEvent::Census`] through the pumps. An engine that went away is
/// said by its pump.
fn ask_census(home: &Attached, groups: &[GroupEngine<Attached>]) {
    let _ = home.outbox.submit(Command::Census);
    for hosted in groups {
        let _ = hosted.engine.submit(Command::Census);
    }
}

/// What the daemon's loop holds, for one census (order 1585).
struct Held<'a, O> {
    outgoing: &'a Outgoing,
    groups: &'a [GroupEngine<O>],
    sender: &'a Sender,
    relations: &'a Relations,
    asker: &'a crate::pn_status::Asker,
    wakes: &'a Wakes<LinkId>,
    costs: &'a Ledger<LinkId>,
    pn_asks: usize,
    home: Option<crate::outbox::EngineCensus>,
    store: &'a Store,
    turns: u64,
}

impl<O> Held<'_, O> {
    fn census(&self) -> super::census::MailboxCensus {
        let (told_n, sender) = self.sender.census();
        let (related_n, heard_n, relations) = self.relations.census();
        let (asker_n, asker) = self.asker.census();
        let (watchers_n, wakes) = self.wakes.census();
        let (devices_n, costs) = self.costs.census();
        let mut census = super::census::MailboxCensus {
            outbox: self.outgoing.heap_bytes(),
            outbox_n: self.outgoing.len(),
            group_outbox: self.groups.iter().map(|g| g.outgoing.heap_bytes()).sum(),
            group_outbox_n: self.groups.iter().map(|g| g.outgoing.len()).sum(),
            sender,
            told_n,
            relations,
            related_n,
            heard_n,
            asker,
            asker_n,
            wakes,
            watchers_n,
            costs,
            devices_n,
            pn_asks: self.pn_asks * std::mem::size_of::<([u8; 16], usize)>(),
            rss_kb: leviculum_std::census::rss_kb(),
            nodes: 1 + self.groups.len(),
            archive_kb: self.store.archive_bytes().unwrap_or(0) / 1024,
            turns: self.turns,
            ..Default::default()
        };
        for answer in self
            .home
            .iter()
            .chain(self.groups.iter().filter_map(|g| g.census.as_ref()))
        {
            census.engine(answer);
        }
        census
    }
}

/// The SIGUSR1 dump: the census line, every node's transport census under
/// its name, and lnsd's process sections over all of it, then the
/// measured live line in a build that can measure it.
fn census_dump(
    node: &ReticulumNode,
    groups: &[GroupEngine<Attached>],
    census: &super::census::MailboxCensus,
) -> String {
    let mut nodes = vec![("home".to_string(), node.diagnostic_dump())];
    for hosted in groups {
        nodes.push((
            format!(
                "group {}",
                crate::address::to_hex(&hosted.hosted.group.address)
            ),
            hosted.engine.node().diagnostic_dump(),
        ));
    }
    let own = format!("{}\n", census.line());
    #[allow(unused_mut)]
    let mut dump =
        leviculum_std::census::compose_multi_node_dump(&nodes, &own, census.tables() as u64);
    #[cfg(feature = "heap-accounting")]
    dump.push_str(&leviculum_std::heap_accounting::measured_live_line(
        "system",
    ));
    dump
}

async fn stopped(stop: Option<&mut tokio::sync::oneshot::Receiver<()>>) {
    match stop {
        Some(stop) => {
            let _ = stop.await;
        }
        None => std::future::pending().await,
    }
}

/// How a home node with a node of its own selects its propagation node
/// without a preference: from that node's path table, as `main` does from
/// the daemon's. No route is a local client's here, since the node serves
/// only the device's own screens.
fn own_routes_source(node: std::sync::Arc<ReticulumNode>, home: PathBuf) -> MailboxSource {
    Box::new(move || {
        let now_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            });
        let routes = own_routes(&node, now_unix_ms);
        let previous = crate::mailbox_state::load(&home);
        Box::pin(async move {
            MailboxRun {
                input: Some(crate::outbox::MailboxInput {
                    routes: Ok(routes),
                    previous,
                    excluded: Vec::new(),
                }),
            }
        })
    })
}

/// The node's path table as the selection rule reads it, each entry's
/// time turned from the node's clock into Unix milliseconds at `now_unix_ms`.
fn own_routes(node: &ReticulumNode, now_unix_ms: u64) -> Vec<crate::mailbox_state::Route> {
    let now = node.now_ms();
    node.path_table_entries()
        .into_iter()
        .map(|entry| crate::mailbox_state::Route {
            node: entry.hash,
            hops: entry.hops,
            own_device: false,
            seen_ms: now_unix_ms.saturating_sub(now.saturating_sub(entry.timestamp_ms)),
        })
        .collect()
}

/// The relations at start: the conversations the store holds and the
/// petnames of the home node's contacts file. A contacts file that cannot
/// be read is said and counts no petname.
fn relations_at_start(store: &Store, home: &std::path::Path) -> Result<Relations, String> {
    let mut related = store.archive_peers().map_err(|e| e.to_string())?;
    match crate::names::load_contacts(home) {
        Ok(contacts) => related.extend(contacts.petnames.into_keys()),
        Err(error) => tracing::warn!("HOME contacts: {error}"),
    }
    Ok(Relations::new(related))
}

/// The source of an LXMF message as packed: its second sixteen bytes.
/// The group an archived invitation names (1307 pass two): the node
/// learns its key, so a Join from any device of this home reaches it
/// before the group's announce is heard.
fn learn_invited_key(node: &ReticulumNode, packed: &[u8]) {
    let Ok(message) =
        leviculum_lxmf::Message::unpack(packed, None, None, leviculum_lxmf::DeliveryMethod::Direct)
    else {
        return;
    };
    let Some((group, public_key)) = group::invited_key(&message.fields) else {
        return;
    };
    match leviculum_core::Identity::from_public_key_bytes(&public_key) {
        Ok(identity) => {
            tracing::info!("HOME invited to group={}", crate::address::to_hex(&group));
            node.remember_identity(leviculum_core::DestinationHash::new(group), identity);
        }
        Err(error) => tracing::warn!("HOME invitation key: {error:?}"),
    }
}

/// A group's node knows the home's own identity from the start: the
/// membership and the admin's copies go to it before its announce is heard.
fn knows_owner(
    engine: &GroupEngine<Attached>,
    address: [u8; 16],
    signer: &leviculum_core::Identity,
) {
    if let Ok(identity) =
        leviculum_core::Identity::from_public_key_bytes(&signer.public_key_bytes())
    {
        engine
            .engine
            .node()
            .remember_identity(leviculum_core::DestinationHash::new(address), identity);
    }
}

fn source_of(packed: &[u8]) -> Option<[u8; 16]> {
    packed.get(16..32)?.try_into().ok()
}

/// `peer` announced `name`: archived when the user has a relation to the
/// peer, only remembered when not. Its sequence, which wakes nobody.
fn heard_name(
    store: &mut Store,
    relations: &mut Relations,
    peer: [u8; 16],
    name: &str,
    cap: u64,
) -> Option<u64> {
    let name = relations.announced(peer, name)?;
    archive_name(store, peer, &name, cap)
}

/// A conversation with `peer` went on: when it is a new one, the newest
/// name heard from the peer goes into the archive at once.
fn related(store: &mut Store, relations: &mut Relations, peer: [u8; 16], cap: u64) -> Option<u64> {
    let name = relations.relate(peer)?;
    archive_name(store, peer, &name, cap)
}

/// Keep a hub the home heard announcing in the archive (1599), for the
/// devices of the home: its sequence, when the entry says something new.
fn archive_hub(store: &mut Store, hub: &crate::ui::hubs::HeardHub, cap: u64) -> Option<u64> {
    match store.archive_hub(hub, cap) {
        Ok(Some(seq)) => {
            tracing::info!(
                "HOME hub archived hub={} hops={} seq={seq}",
                crate::address::to_hex(&hub.hash),
                hub.hops.map_or(-1, i32::from)
            );
            Some(seq)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!("HOME archive: {error}");
            None
        }
    }
}

/// Keep a NomadNet node the home heard announcing (1579,
/// [`super::pages`]): its row's new stamp, when the hearing said something
/// new; a hearing that only counted says nothing in the log.
fn keep_page(store: &mut Store, node: &lnomad::discovery::DiscoveredNode) -> Option<u64> {
    match store.page_heard(node) {
        Ok(Some(stamp)) => {
            tracing::info!(
                "HOME page kept node={} hops={} stamp={stamp}",
                crate::address::to_hex(&node.dest_hash),
                node.hops.map_or(-1, i64::from)
            );
            Some(stamp)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!("HOME pages: {error}");
            None
        }
    }
}

/// The owner's own name, for its devices (1449 item 2, walk 1 finding 8):
/// a device never hears its identity's announce (its own node holds the
/// destination), so the archive hands the name as it hands a peer's, and
/// the pull's name entry names the avatar and the profile. Archived once
/// per change: the same name again is no new entry, and no name is none.
fn archive_own_name(store: &mut Store, address: [u8; 16], own_name: &str, cap: u64) -> Option<u64> {
    match own_name.trim().is_empty() {
        true => None,
        false => archive_name(store, address, own_name, cap),
    }
}

/// Keep a name a peer announced in the archive, for the clients that hear
/// no announces: its sequence, when the name is new for the peer.
fn archive_name(store: &mut Store, peer: [u8; 16], name: &str, cap: u64) -> Option<u64> {
    let now = leviculum_std::local_time::unix_now_secs();
    match store.archive_name(peer, name, now, cap) {
        Ok(Some(seq)) => {
            tracing::info!(
                "HOME name archived peer={} seq={seq}",
                crate::address::to_hex(&peer)
            );
            Some(seq)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!("HOME archive: {error}");
            None
        }
    }
}

/// Keep one received message in the archive: its sequence when it is new,
/// `None` when the archive held it already, an error when it could not be
/// kept (said here).
fn archive(
    store: &mut Store,
    message_id: &[u8; 32],
    packed: &[u8],
    cap: u64,
) -> Result<Option<u64>, ()> {
    let now = leviculum_std::local_time::unix_now_secs();
    match store.archive_append(crate::home::KIND_RECEIVED, message_id, packed, now, cap) {
        Ok(Some(seq)) => {
            tracing::info!(
                "HOME archived message={} seq={seq} bytes={}",
                crate::address::to_hex(message_id),
                packed.len()
            );
            Ok(Some(seq))
        }
        Ok(None) => {
            tracing::info!(
                "HOME archived already message={}",
                crate::address::to_hex(message_id)
            );
            Ok(None)
        }
        Err(error) => {
            tracing::warn!("HOME archive: {error}");
            Err(())
        }
    }
}

/// One wake, on the link that watches, to a device still admitted. A link
/// that is busy owes the wake again; one that is gone, or whose device was
/// revoked, is forgotten, and the revoked device's link is closed.
///
/// The store comes `&mut`, though only read, so the loop's future stays
/// `Send`: a shared borrow of the store is not.
async fn send_wake(
    node: &ReticulumNode,
    store: &mut Store,
    wakes: &mut Wakes<LinkId>,
    costs: &mut Ledger<LinkId>,
    due: Due<LinkId>,
) {
    let device = crate::address::to_hex(&due.device);
    match store.is_admitted(&due.device) {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!("HOME wake refused device={device} reason=not-admitted");
            wakes.forget(&due.link);
            let _ = node.close_link(&due.link).await;
            return;
        }
        Err(error) => {
            tracing::warn!("HOME wake: {error}");
            wakes.unsent(&due.link, due.before);
            return;
        }
    }
    let wake = super::wire::wake(due.newest);
    let sent = node.link_handle(&due.link).try_send(&wake).await;
    match sent {
        Ok(()) => {
            costs.woke(due.device, wake.len() as u64);
            tracing::info!("HOME wake device={device} newest={}", due.newest);
        }
        Err(leviculum_std::Error::Send(SendError::Busy | SendError::PacingDelay { .. })) => {
            wakes.unsent(&due.link, due.before);
        }
        Err(error) => {
            tracing::info!("HOME wake device={device} link gone: {error}");
            wakes.forget(&due.link);
        }
    }
}

/// The unix time in seconds, the outbox's clock.
/// The contacts file's names the archive has no petname entry for, kept
/// as petname entries (1159), and its blocks the archive has no peer state
/// for, kept as blocked peer states (1162): an import before names and
/// blocks travelled wrote them to the file alone, so no device ever
/// received them. A second start finds each one there and appends nothing.
/// How many names and blocks were appended.
fn backfill_contacts(
    store: &mut Store,
    home: &Path,
    cap_bytes: u64,
) -> Result<(usize, usize), String> {
    let contacts = crate::names::load_contacts(home).map_err(|e| e.to_string())?;
    let names = contacts
        .petnames
        .iter()
        .map(|(peer, name)| (peer, name.as_str()));
    let now = unix_now();
    let appended =
        super::server::archive_petnames(store, names, now, cap_bytes).map_err(|e| e.to_string())?;
    let blocks = super::server::archive_blocks(store, &contacts.blocked, now, cap_bytes)
        .map_err(|e| e.to_string())?;
    tracing::info!(
        "HOME petnames backfilled={appended} contacts={} blocks={blocks}",
        contacts.petnames.len()
    );
    Ok((appended, blocks))
}

fn unix_now() -> i64 {
    leviculum_std::local_time::unix_now_secs()
}

/// Answer one request, as a packet when it fits and as a resource when not.
/// The messages a `/send` or a seen `/mark` put in the outbox come back,
/// the new ones, and what a `/watch` told its client, when it took it.
async fn answer(
    node: &ReticulumNode,
    store: &mut Store,
    serving: &super::server::Home<'_>,
    request: HomeRequest,
) -> (
    Vec<crate::store::Outgoing>,
    Option<u64>,
    bool,
    (
        Option<super::server::NewGroup>,
        Option<([u8; 16], Vec<group::Outgoing>)>,
        Option<([u8; 16], Vec<group::Outgoing>)>,
    ),
    usize,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64());
    let answer = super::server::answer(
        store,
        serving,
        &request.path,
        &request.data,
        request.remote,
        now,
    );
    if let Some((peer, public_key)) = answer.learned {
        match leviculum_core::Identity::from_public_key_bytes(&public_key) {
            Ok(identity) => {
                node.remember_identity(leviculum_core::DestinationHash::new(peer), identity)
            }
            Err(error) => tracing::warn!("HOME key: {error:?}"),
        }
    }
    let bytes = &answer.bytes;
    let size = request.data.len() + bytes.len();
    let sent = match node
        .send_response(&request.link_id, &request.request_id, bytes)
        .await
    {
        Err(leviculum_std::Error::Request(RequestError::PayloadTooLarge)) => {
            node.send_response_resource_with(
                &request.link_id,
                &request.request_id,
                bytes,
                super::ANSWERS_COMPRESSED,
            )
            .await
        }
        other => other,
    };
    if let Err(error) = sent {
        // The outbox has the message whether the answer arrived or not: a
        // client that retries with the same id is answered from it.
        tracing::warn!("HOME answer to {}: {error}", request.path);
        return (
            answer.accepted,
            None,
            false,
            (answer.group, answer.added, answer.deleted),
            size,
        );
    }
    (
        answer.accepted,
        answer.watching,
        answer.gone,
        (answer.group, answer.added, answer.deleted),
        size,
    )
}

/// What carrying out the outbox's acts needs besides the engine and the
/// store.
struct Sender {
    cap_bytes: u64,
    preferred_pn: Option<[u8; 16]>,
    /// Whether collections run, the only thing that selects a propagation
    /// node when none is named.
    collecting: bool,
    /// The messages already told they have no propagation node: said once
    /// each, not on every failed direct retry.
    told: std::collections::HashSet<[u8; 32]>,
    /// The newest archive sequence a state change took, for the wakes.
    newest: u64,
    /// The node each propagated leg in flight went to, for its lines
    /// (1330: "handed-on" did not say to whom).
    nodes: std::collections::HashMap<[u8; 32], [u8; 16]>,
    /// The originals a copy's first proof was booked for, by group, whose
    /// writer's status the group's pump sends when due (1307 pass four).
    proven: Vec<([u8; 16], [u8; 32])>,
    /// The `direct unanswered` lines, once a peer an hour (order 1585).
    lines: super::outgoing::PeerLines,
}

impl Sender {
    /// The messages told they have no node, the propagated legs' nodes and
    /// the proofs owed, by count and estimated heap bytes (order 1585).
    fn census(&self) -> (usize, usize) {
        use leviculum_std::census::hash_table_bytes;
        (
            self.told.len(),
            hash_table_bytes(self.told.capacity(), 32)
                + hash_table_bytes(self.nodes.capacity(), 48)
                + self.proven.capacity() * 48
                + self.lines.census().1,
        )
    }

    /// The router let go of `id`: tell the outbox, and carry out what it
    /// says. `reason` is why a failed leg failed, as the engine said it
    /// (1328: a propagated leg that failed in 100 ms said nothing).
    #[allow(clippy::too_many_arguments)]
    fn left(
        &mut self,
        engine: &mut impl Outbox,
        store: &mut Store,
        outgoing: &mut Outgoing,
        id: &[u8; 32],
        outcome: Outcome,
        reason: Option<&str>,
        selected_pn: Option<[u8; 16]>,
        held_pn: Option<[u8; 16]>,
    ) {
        if let Some(leg) = outgoing.in_flight(id) {
            let node = match leg {
                Leg::Propagated => self.nodes.remove(id),
                Leg::Direct => None,
            };
            let unanswered = outgoing.unanswered(id).then_some(!outgoing.propagated(id));
            // A recipient that does not answer is said at a message's first
            // miss, and its retries once a peer an hour, not once an
            // attempt (1585); every other end is said as it comes.
            let held = match (leg, outcome, unanswered) {
                (Leg::Direct, Outcome::Failed, Some(_)) if outgoing.misses(id) > 0 => {
                    let peer = outgoing.destination(id).unwrap_or([0; 16]);
                    self.lines.admit(peer, unix_now())
                }
                _ => Some(0),
            };
            if let Some(held) = held {
                tracing::info!(
                    "{}{}",
                    left_line(leg, outcome, unanswered, id, node, reason),
                    held_back(held)
                );
            }
        }
        let acts = outgoing.left(id, outcome, unix_now());
        self.carry_out(engine, store, outgoing, acts, selected_pn, held_pn);
    }

    /// Hand each message to the router on its leg, and record each state
    /// change. A leg that cannot start is ended as failed at once, so the
    /// outbox moves on. `selected_pn` is the node the home's collections
    /// selected, `held_pn` the one `engine` itself has selected: a group's
    /// engine is not the home's.
    fn carry_out(
        &mut self,
        engine: &mut impl Outbox,
        store: &mut Store,
        outgoing: &mut Outgoing,
        acts: Vec<Act>,
        selected_pn: Option<[u8; 16]>,
        held_pn: Option<[u8; 16]>,
    ) {
        let mut acts = std::collections::VecDeque::from(acts);
        while let Some(act) = acts.pop_front() {
            match act {
                Act::Record { id, change } => self.record(store, &id, change),
                Act::Unanswered { id, offer } => {
                    // A message's first miss is said; its retries once a
                    // peer an hour (1585).
                    let peer = outgoing.destination(&id).unwrap_or([0; 16]);
                    let held = match outgoing.misses(&id) {
                        0 | 1 => Some(0),
                        _ => self.lines.admit(peer, unix_now()),
                    };
                    if let Some(held) = held {
                        tracing::info!("{}{}", unanswered_line(&id, offer), held_back(held));
                    }
                    // As a stall's call-off: the router's end comes back as
                    // a Left the outbox ignores.
                    let _ = engine.submit(Command::Cancel { message_id: id });
                }
                Act::GiveUp { id, leg } => {
                    tracing::info!(
                        "HOME outbox {} stalled message={}: no end after {} s, trying again",
                        leg_word(leg),
                        crate::address::to_hex(&id),
                        super::outgoing::STALL_SECS
                    );
                    // The router's end of it comes back as a Left the outbox
                    // ignores; an engine that went away is said by the next
                    // pump.
                    let _ = engine.submit(Command::Cancel { message_id: id });
                }
                Act::Send { id, leg } => {
                    let Some(packed) = outgoing.packed(&id).map(<[u8]>::to_vec) else {
                        continue;
                    };
                    let mut node_used = None;
                    let started = match leg {
                        Leg::Direct => engine
                            .submit(Command::SendPacked {
                                packed,
                                via: Via::Link,
                            })
                            .is_ok(),
                        Leg::Propagated => match selected_pn.or(self.preferred_pn) {
                            Some(node) => {
                                // The node the collections use, selected on
                                // this engine unless it holds it already: one
                                // command queue, so the selection is in before
                                // the send. A group's engine never ran a
                                // collection, and taking the home's selection
                                // for its own failed every group copy's
                                // propagated leg in its router, unsent
                                // (1328).
                                let selected = held_pn == Some(node)
                                    || engine
                                        .submit(Command::SelectPn {
                                            preferred: Some(node),
                                            mailbox: None,
                                        })
                                        .is_ok();
                                node_used = Some(node);
                                selected
                                    && engine
                                        .submit(Command::SendPacked {
                                            packed,
                                            via: Via::Propagated,
                                        })
                                        .is_ok()
                            }
                            // Not selected here by the mailbox rule: that
                            // reads the daemon's path table and may wait for
                            // announce replays, which this path cannot. A
                            // collection selects one, or --pn names it.
                            None => {
                                if self.told.insert(id) {
                                    tracing::info!(
                                        "HOME outbox no propagation node message={}; direct \
                                         retries only{}",
                                        crate::address::to_hex(&id),
                                        match self.collecting {
                                            true => " until a collection selects one",
                                            false =>
                                                ", since collections are off \
                                                      (--sync-interval off) and no --pn \
                                                      names one",
                                        }
                                    );
                                }
                                false
                            }
                        },
                    };
                    if started {
                        // The leg's line waits for the router to take it
                        // ([`Sender::queued`]): an engine that refuses the
                        // leg says unsent, and the line said a leg that
                        // never was (1406: `propagated pn=` every hour for a
                        // recipient whose key the home never held).
                        match node_used {
                            Some(node) => self.nodes.insert(id, node),
                            None => self.nodes.remove(&id),
                        };
                    } else {
                        acts.extend(outgoing.left(&id, Outcome::Failed, unix_now()));
                    }
                }
            }
        }
    }

    /// The router took `id`: the line of the leg it is on, with the node a
    /// propagated leg went to. A message on no leg is not the outbox's.
    fn queued(&self, outgoing: &Outgoing, id: &[u8; 32]) {
        if let Some(leg) = outgoing.in_flight(id) {
            tracing::info!("{}", leg_line(leg, id, self.nodes.get(id).copied()));
        }
    }

    fn record(&mut self, store: &mut Store, id: &[u8; 32], change: Change) {
        if change != Change::Propagated {
            self.told.remove(id);
        }
        // A group's copy is the group's bookkeeping, not an entry the
        // owner's devices see.
        match group::book(store, id, change, unix_now()) {
            Ok(Some((group, originals))) => {
                for original in originals {
                    if !self.proven.contains(&(group, original)) {
                        self.proven.push((group, original));
                    }
                }
                tracing::info!(
                    "GROUP copy {} message={}",
                    change.word(),
                    crate::address::to_hex(id)
                );
                return;
            }
            Ok(None) => {}
            Err(error) => tracing::warn!("GROUP copy record: {error}"),
        }
        match store.outbox_record(id, change, unix_now(), self.cap_bytes) {
            Ok(seq) => {
                self.newest = self.newest.max(seq.unwrap_or(0));
                tracing::info!(
                    "HOME outbox {} message={} seq={}",
                    change.word(),
                    crate::address::to_hex(id),
                    seq.map_or_else(|| "-".to_string(), |seq| seq.to_string())
                )
            }
            Err(error) => tracing::warn!("HOME outbox record: {error}"),
        }
    }
}

/// The line a leg the router took leaves: `HOME outbox direct message=…`,
/// or `propagated` with the node.
fn leg_line(leg: Leg, id: &[u8; 32], node: Option<[u8; 16]>) -> String {
    format!(
        "HOME outbox {} message={}{}",
        leg_word(leg),
        crate::address::to_hex(id),
        node_field(node)
    )
}

/// The line a direct leg the outbox's answer bound called off leaves. It
/// offers the propagation node the copy only when `offer`: a message a node
/// took already is tried direct again on the announce and the hour, and
/// the line said otherwise for every one (1406).
fn unanswered_line(id: &[u8; 32], offer: bool) -> String {
    format!(
        "HOME outbox direct unanswered message={}: no link after {} s, {}",
        crate::address::to_hex(id),
        super::outgoing::ANSWER_SECS,
        unanswered_next(offer)
    )
}

/// The tail of a peer's line that was held back (1585): how many like it
/// went unsaid since the last one.
fn held_back(held: u32) -> String {
    match held {
        0 => String::new(),
        n => format!(" ({n} more for this peer in the last hour not logged)"),
    }
}

/// What follows an unanswered direct leg, as its lines say it.
fn unanswered_next(offer: bool) -> &'static str {
    match offer {
        true => "offering the propagation node",
        false => "the propagation node holds a copy",
    }
}

/// Why a propagated leg to `destination` did not go, as its `failed` line
/// says it (1406): the home holds no key for the recipient, so the copy
/// cannot be encrypted for the node, and the recipient's path was asked.
fn no_key_reason(destination: &[u8; 16]) -> String {
    format!(
        "no key for {}, path requested",
        crate::address::to_hex(&destination[..4])
    )
}

/// The ` pn=` a propagated leg's lines carry: the node's first four bytes,
/// as `PN_PULL` names it, so the two read side by side (1330).
fn node_field(node: Option<[u8; 16]>) -> String {
    node.map_or_else(String::new, |node| {
        format!(" pn={}", crate::address::to_hex(&node[..4]))
    })
}

/// The line a collection from the propagation node leaves (1293): how many
/// messages it brought, from which node, and when the next one is due, so
/// the cadence shows in the log when nothing comes.
fn pn_pull_line(n: usize, pn: Option<[u8; 16]>, schedule: &Schedule, now_ms: u64) -> String {
    let pn = pn.map_or_else(|| "-".to_string(), |pn| crate::address::to_hex(&pn[..4]));
    let next = schedule.next_ms().map_or_else(
        || "-".to_string(),
        |next| (next.saturating_sub(now_ms) / 1000).to_string(),
    );
    format!("PN_PULL n={n} pn={pn} next_in_s={next}")
}

/// The line a leg the router let go of leaves. A direct leg that failed
/// before the recipient answered is told as the outbox's own answer bound
/// tells it, `unanswered`, with the router's reason (1331): the two are one
/// fact, and which of them ends the leg first is no matter for the log.
/// `unanswered` is `Some(offer)` for such a leg, `offer` as in
/// [`unanswered_line`].
fn left_line(
    leg: Leg,
    outcome: Outcome,
    unanswered: Option<bool>,
    id: &[u8; 32],
    node: Option<[u8; 16]>,
    reason: Option<&str>,
) -> String {
    let id = crate::address::to_hex(id);
    if let (Leg::Direct, Outcome::Failed, Some(offer)) = (leg, outcome, unanswered) {
        return format!(
            "HOME outbox direct unanswered message={id}: reason=router: {}, {}",
            reason.unwrap_or("failed"),
            unanswered_next(offer)
        );
    }
    format!(
        "HOME outbox {} {} message={id}{}{}",
        leg_word(leg),
        match outcome {
            Outcome::Delivered => "delivered",
            Outcome::Sent => "handed-on",
            Outcome::Failed | Outcome::NoKey => "failed",
        },
        node_field(node),
        reason.map_or_else(String::new, |reason| format!(" reason={reason}"))
    )
}

/// The router reported `state` for `id`: a sign of its leg, and from
/// `Sending` on the recipient's answer, which lifts the direct leg's answer
/// bound (1328).
fn leg_sign(outgoing: &mut Outgoing, id: &[u8; 32], state: MessageState) {
    match state {
        MessageState::Sending
        | MessageState::Sent
        | MessageState::Delivered
        | MessageState::AwaitingCollection => outgoing.answered(id, unix_now()),
        _ => outgoing.heard(id, unix_now()),
    }
}

/// How a leg the router let go of ended, and why when it failed: the last
/// state the router reported for it, which tells a propagation node's
/// refusal (`rejected`) from attempts run out (`failed`) or a call-off
/// (`cancelled`), and after it the transport's word on the last failed
/// attempt (1330: `reason=failed` alone did not say whether the link died
/// mid-transfer or never came). Attempts run out with no attempt failed
/// say so: no attempt got a link, since a transfer on one that fails says why.
fn left_outcome(
    last: Option<MessageState>,
    failure: Option<AttemptFailure>,
) -> (Outcome, Option<String>) {
    let state = match last {
        Some(MessageState::Delivered) => return (Outcome::Delivered, None),
        Some(MessageState::Sent | MessageState::AwaitingCollection) => {
            return (Outcome::Sent, None)
        }
        Some(state) => format!("{state:?}").to_ascii_lowercase(),
        None => "no state seen".to_string(),
    };
    let reason = match (last, failure) {
        (_, Some(failure)) => format!("{state}: {failure}"),
        (Some(MessageState::Failed), None) => format!("{state}: no attempt got a link"),
        (_, None) => state,
    };
    (Outcome::Failed, Some(reason))
}

/// How the router heard a peer, for the outbox (1330).
fn heard(path_response: bool) -> Heard {
    match path_response {
        true => Heard::PathResponse,
        false => Heard::Announce,
    }
}

fn leg_word(leg: Leg) -> &'static str {
    match leg {
        Leg::Direct => "direct",
        Leg::Propagated => "propagated",
    }
}

/// What a collection from the propagation node needs.
struct Collector {
    home: PathBuf,
    instance: String,
    budget: Duration,
    preferred: Option<[u8; 16]>,
    source: &'static str,
    mailbox: MailboxSource,
}

impl Collector {
    /// One collection, on a tap of its own, the way `loquax fetch` runs it:
    /// how many messages it brought, `None` when it failed.
    fn collect(&self, fanout: &mut Fanout) -> tokio::task::JoinHandle<Option<usize>> {
        let mut tap = fanout.tap();
        let home = self.home.clone();
        let mut options = FetchOptions::new(self.instance.clone(), self.budget);
        let (preferred, source) = (self.preferred, self.source);
        let mailbox = (self.mailbox)();
        tokio::spawn(async move {
            options.mailbox = mailbox.await;
            let mut seen = match SeenStore::load(&home) {
                Ok(seen) => seen,
                Err(error) => {
                    tracing::warn!("HOME collect: seen store: {error}");
                    return None;
                }
            };
            let mut effects = Quiet(Terminal::new(home));
            match run_fetch(
                &mut tap,
                preferred,
                source,
                &options,
                &mut effects,
                &mut seen,
                &Tokio::new(),
            )
            .await
            {
                Ok(fetched) => Some(fetched.received),
                Err(error) => {
                    tracing::info!("HOME collect: {error}");
                    None
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1412: a router call that failed after Ready is an error the engine
    /// runs on through, and the line says so; one before Ready ends the
    /// service instead of leaving a home that never registered.
    #[test]
    fn an_engine_error_after_ready_is_logged_and_one_before_ends_the_service() {
        let detail = "router release_proofs: Node(ProofFailed)";
        assert_eq!(
            engine_error(true, detail),
            Ok(format!(
                "HOME engine error, the engine keeps running: {detail}"
            ))
        );
        assert_eq!(
            engine_error(false, "hold proofs: x"),
            Err("the message engine could not start: hold proofs: x".to_string())
        );
    }

    /// 1330, miauhaus 12:15:37: `direct failed ... reason=failed` named
    /// the router's state and nothing else. A link that died mid-transfer
    /// says so, a transfer that timed out says so, and attempts run out
    /// with none failing on a link say that; a call-off stays a call-off.
    #[test]
    fn a_failed_legs_reason_says_how_its_last_attempt_failed() {
        use leviculum_core::{resource::ResourceError, LinkCloseReason};
        use leviculum_lxmf::{DeliveryFailure, PropagationUploadFailure};
        let reason = |last, failure| left_outcome(Some(last), failure).1.expect("a reason");
        assert_eq!(
            reason(
                MessageState::Failed,
                Some(AttemptFailure::Delivery(DeliveryFailure::LinkClosed(
                    LinkCloseReason::Stale
                )))
            ),
            "failed: delivery link closed (Stale)"
        );
        assert_eq!(
            reason(
                MessageState::Failed,
                Some(AttemptFailure::Delivery(DeliveryFailure::Resource(
                    ResourceError::Timeout
                )))
            ),
            format!("failed: delivery transfer: {}", ResourceError::Timeout)
        );
        assert_eq!(
            reason(
                MessageState::Failed,
                Some(AttemptFailure::Upload(
                    PropagationUploadFailure::PacketTimeout
                ))
            ),
            "failed: upload packet was not acknowledged"
        );
        assert_eq!(
            reason(MessageState::Failed, None),
            "failed: no attempt got a link"
        );
        assert_eq!(reason(MessageState::Cancelled, None), "cancelled");
        assert_eq!(
            left_outcome(Some(MessageState::Sent), None),
            (Outcome::Sent, None)
        );
    }

    /// 1185: half an hour for a day after the identity moved in, the
    /// ordinary six hours before a move, after the day and for a mark from
    /// the future.
    #[test]
    fn the_announce_cadence_is_shorter_for_a_day_after_a_move() {
        let at = 1_793_000_000;
        assert_eq!(announce_period(None, at), ANNOUNCE_EVERY);
        assert_eq!(announce_period(Some(at), at), ANNOUNCE_EVERY_AFTER_MOVE);
        assert_eq!(
            announce_period(Some(at), at + 86_399),
            ANNOUNCE_EVERY_AFTER_MOVE
        );
        assert_eq!(announce_period(Some(at), at + 86_400), ANNOUNCE_EVERY);
        assert_eq!(announce_period(Some(at), at - 10), ANNOUNCE_EVERY);
    }

    /// 1331: the router ending a direct leg nobody answered logs the line
    /// the outbox's own bound logs, `unanswered`, with the router's reason;
    /// a direct leg the recipient answered and a propagated leg log as
    /// they did.
    #[test]
    fn a_router_failure_of_an_unanswered_direct_leg_is_told_as_unanswered() {
        let id = [0xab; 32];
        let hex = crate::address::to_hex(&id);
        assert_eq!(
            left_line(
                Leg::Direct,
                Outcome::Failed,
                Some(true),
                &id,
                None,
                Some("failed")
            ),
            format!(
                "HOME outbox direct unanswered message={hex}: reason=router: failed, \
                 offering the propagation node"
            )
        );
        assert_eq!(
            left_line(
                Leg::Direct,
                Outcome::Failed,
                Some(false),
                &id,
                None,
                Some("failed")
            ),
            format!(
                "HOME outbox direct unanswered message={hex}: reason=router: failed, \
                 the propagation node holds a copy"
            )
        );
        assert_eq!(
            left_line(
                Leg::Direct,
                Outcome::Failed,
                None,
                &id,
                None,
                Some("failed")
            ),
            format!("HOME outbox direct failed message={hex} reason=failed")
        );
        assert_eq!(
            left_line(Leg::Direct, Outcome::Delivered, Some(true), &id, None, None),
            format!("HOME outbox direct delivered message={hex}")
        );
        assert_eq!(
            left_line(
                Leg::Propagated,
                Outcome::Failed,
                Some(true),
                &id,
                Some([0x63, 0x52, 0x98, 0xc8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]),
                Some("rejected")
            ),
            format!("HOME outbox propagated failed message={hex} pn=635298c8 reason=rejected")
        );
    }

    /// 1406: a propagated leg refused for a missing key names the recipient
    /// and the path request in its `failed` line, in place of the
    /// `propagated pn=` line that promised an upload and the 779 s of
    /// silence after it on the field mailbox.
    #[test]
    fn a_leg_without_a_key_names_the_recipient() {
        let id = [0xab; 32];
        let hex = crate::address::to_hex(&id);
        let destination = [0x63, 0x52, 0x98, 0xc8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        assert_eq!(
            left_line(
                Leg::Propagated,
                Outcome::NoKey,
                None,
                &id,
                Some([0xca, 0xfa, 0x58, 0x3f, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
                Some(&no_key_reason(&destination))
            ),
            format!(
                "HOME outbox propagated failed message={hex} pn=cafa583f reason=no key for \
                 635298c8, path requested"
            )
        );
    }

    /// 1406, the field mailbox at the 13:56 restart: 22 of 27
    /// `unanswered` lines offered the node a copy it had held for days. A
    /// leg of a message on the node says so; one never handed on offers.
    #[test]
    fn an_unanswered_line_offers_the_node_only_what_it_lacks() {
        let id = [0xab; 32];
        let hex = crate::address::to_hex(&id);
        assert_eq!(
            unanswered_line(&id, true),
            format!(
                "HOME outbox direct unanswered message={hex}: no link after 45 s, \
                 offering the propagation node"
            )
        );
        assert_eq!(
            unanswered_line(&id, false),
            format!(
                "HOME outbox direct unanswered message={hex}: no link after 45 s, \
                 the propagation node holds a copy"
            )
        );
    }

    /// 1293: a collection that found nothing says so, with the node and
    /// the next one at the bound; one that found mail names the count and
    /// the activity cadence.
    #[test]
    fn a_pull_names_its_count_its_node_and_the_next_one() {
        let mut schedule = Schedule::new(Some(crate::sync::DEFAULT_BOUND_MS));
        schedule.begin(1_000);
        schedule.on(SyncEvent::Done { new: 0 }, 1_000);
        assert_eq!(
            pn_pull_line(0, Some([0xab; 16]), &schedule, 1_000),
            "PN_PULL n=0 pn=abababab next_in_s=900"
        );
        schedule.begin(2_000);
        schedule.on(SyncEvent::Done { new: 3 }, 2_000);
        assert_eq!(
            pn_pull_line(3, None, &schedule, 2_000),
            "PN_PULL n=3 pn=- next_in_s=120"
        );
    }

    const US: [u8; 16] = [0x11; 16];
    const FRIEND: [u8; 16] = [0x22; 16];
    const STRANGER: [u8; 16] = [0x33; 16];

    fn names_archived(store: &Store) -> Vec<([u8; 16], String)> {
        store
            .archive_since(0, usize::MAX)
            .expect("page")
            .entries
            .iter()
            .filter(|entry| entry.kind == crate::home::KIND_NAME)
            .map(|entry| crate::home::wire::read_name_entry(&entry.packed).expect("a name"))
            .collect()
    }

    fn petnames_archived(store: &Store) -> Vec<([u8; 16], Option<String>)> {
        store
            .archive_since(0, usize::MAX)
            .expect("page")
            .entries
            .iter()
            .filter(|entry| entry.kind == crate::home::KIND_PETNAME)
            .map(|entry| crate::home::wire::read_petname_entry(&entry.packed).expect("a petname"))
            .collect()
    }

    /// 1449 item 2 (walk 1 finding 8): the start archives the owner's own
    /// name for its devices, once per change; a home without a name
    /// archives none.
    #[test]
    fn the_start_archives_the_owners_own_name_for_its_devices() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        assert_eq!(archive_own_name(&mut store, US, "  ", u64::MAX), None);
        assert!(names_archived(&store).is_empty(), "no name, no entry");
        assert!(archive_own_name(&mut store, US, "Lew", u64::MAX).is_some());
        assert_eq!(
            archive_own_name(&mut store, US, "Lew", u64::MAX),
            None,
            "once"
        );
        assert!(archive_own_name(&mut store, US, "Lew P.", u64::MAX).is_some());
        // The archive keeps a peer's newest name only, the owner's too.
        assert_eq!(names_archived(&store), [(US, "Lew P.".to_string())]);
    }

    /// 1159: a home node that imported before names travelled holds them in
    /// its contacts file alone. Its start archives each one the archive has
    /// no petname entry for; a name a device set, even a cleared one, wins;
    /// a second start archives nothing.
    #[test]
    fn the_start_archives_the_contacts_names_once_and_a_name_said_wins() {
        let home = tempfile::tempdir().expect("home");
        std::fs::write(
            home.path().join(crate::names::CONTACTS_FILE),
            format!(
                "name {} Friend\nname {} Stranger\nblock {}\n",
                crate::address::to_hex(&FRIEND),
                crate::address::to_hex(&STRANGER),
                crate::address::to_hex(&[0x44; 16])
            ),
        )
        .expect("contacts");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let cleared = crate::home::wire::petname_entry(&STRANGER, None);
        store
            .archive_petname(STRANGER, &cleared, 1, u64::MAX)
            .expect("a device cleared it");
        assert_eq!(
            backfill_contacts(&mut store, home.path(), u64::MAX)
                .expect("first start")
                .0,
            1
        );
        assert_eq!(
            backfill_contacts(&mut store, home.path(), u64::MAX)
                .expect("second start")
                .0,
            0
        );
        assert_eq!(
            petnames_archived(&store),
            [(STRANGER, None), (FRIEND, Some("Friend".to_string()))]
        );
    }

    fn peer_states_archived(store: &Store) -> Vec<([u8; 16], crate::home::wire::PeerState)> {
        store
            .archive_since(0, usize::MAX)
            .expect("page")
            .entries
            .iter()
            .filter(|entry| entry.kind == crate::home::KIND_PEERSTATE)
            .map(|entry| {
                crate::home::wire::read_peer_state_entry(&entry.packed).expect("a peer state")
            })
            .collect()
    }

    /// 1162: a home node that imported before blocks travelled holds them
    /// in its contacts file alone. Its start archives each block as the
    /// peer state a device's block sends; a state a device set for that
    /// peer wins; a second start archives nothing.
    #[test]
    fn the_start_archives_the_contacts_blocks_once_and_a_state_said_wins() {
        let home = tempfile::tempdir().expect("home");
        let pinned = [0x55; 16];
        std::fs::write(
            home.path().join(crate::names::CONTACTS_FILE),
            format!(
                "block {}\nblock {}\n",
                crate::address::to_hex(&STRANGER),
                crate::address::to_hex(&pinned)
            ),
        )
        .expect("contacts");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let said = crate::home::wire::PeerState {
            pinned: true,
            ..crate::home::wire::PeerState::default()
        };
        let entry = crate::home::wire::peer_state_entry(&pinned, &said);
        store
            .archive_peer_state(pinned, &entry, 1, u64::MAX)
            .expect("a device pinned it");
        assert_eq!(
            backfill_contacts(&mut store, home.path(), u64::MAX).expect("first start"),
            (0, 1)
        );
        assert_eq!(
            backfill_contacts(&mut store, home.path(), u64::MAX).expect("second start"),
            (0, 0)
        );
        let blocked = crate::home::wire::PeerState {
            blocked: true,
            ..crate::home::wire::PeerState::default()
        };
        assert_eq!(
            peer_states_archived(&store),
            [(pinned, said), (STRANGER, blocked)]
        );
    }

    /// 1136: the home node hears the whole network. A stranger's announce
    /// appends nothing; a conversation peer's appends once per name; a
    /// stranger heard before a conversation starts has its name archived
    /// when it does.
    #[test]
    fn only_a_peer_with_a_relation_has_its_name_archived() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let mut relations = Relations::new([FRIEND]);
        for n in 0..50 {
            let name = format!("Stranger {n}");
            assert_eq!(
                heard_name(&mut store, &mut relations, STRANGER, &name, u64::MAX),
                None
            );
        }
        assert!(names_archived(&store).is_empty(), "no stranger's name");
        assert!(heard_name(&mut store, &mut relations, FRIEND, "Friend", u64::MAX).is_some());
        assert_eq!(
            heard_name(&mut store, &mut relations, FRIEND, "Friend", u64::MAX),
            None,
            "the same name again appends nothing"
        );
        assert_eq!(names_archived(&store), [(FRIEND, "Friend".to_string())]);
        // The stranger's first message: its newest name, at once, once.
        assert!(related(&mut store, &mut relations, STRANGER, u64::MAX).is_some());
        assert_eq!(
            related(&mut store, &mut relations, STRANGER, u64::MAX),
            None
        );
        assert_eq!(
            names_archived(&store),
            [
                (FRIEND, "Friend".to_string()),
                (STRANGER, "Stranger 49".to_string())
            ]
        );
    }

    /// A peer's names are capped to its newest: a rename replaces the entry
    /// before it, and the relations at start come from the archive.
    #[test]
    fn a_rename_keeps_only_the_newest_name_and_the_archive_relates_at_start() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let mut relations = Relations::new([FRIEND]);
        heard_name(&mut store, &mut relations, FRIEND, "Friend", u64::MAX).expect("first");
        heard_name(&mut store, &mut relations, FRIEND, "Renamed", u64::MAX).expect("rename");
        assert_eq!(names_archived(&store), [(FRIEND, "Renamed".to_string())]);

        let mut packed = vec![0u8; 120];
        packed[..16].copy_from_slice(&US);
        packed[16..32].copy_from_slice(&STRANGER);
        assert_eq!(source_of(&packed), Some(STRANGER));
        store
            .archive_append(crate::home::KIND_RECEIVED, &[7; 32], &packed, 1, u64::MAX)
            .expect("append")
            .expect("new");
        let again = relations_at_start(&store, home.path()).expect("relations");
        assert!(again.is_related(&STRANGER), "the sender of a message");
        assert!(!again.is_related(&FRIEND), "a name alone is no relation");
    }

    /// A name entry wakes nobody: the newest waking sequence passes it by,
    /// so a client told the message before it is owed nothing.
    #[test]
    fn a_name_entry_causes_no_wake() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let message = store
            .archive_append(crate::home::KIND_RECEIVED, &[7; 32], &[0; 40], 1, u64::MAX)
            .expect("append")
            .expect("new");
        let mut wakes: Wakes<u8> = Wakes::default();
        wakes.watch(1, [9; 16], message);
        let mut relations = Relations::new([FRIEND]);
        let name =
            heard_name(&mut store, &mut relations, FRIEND, "Friend", u64::MAX).expect("archived");
        assert!(name > message);
        wakes.appended(store.archive_newest_waking().expect("newest"));
        assert!(wakes.due(10_000).is_empty(), "no wake for a name");
        // Positive control: the newest of all entries would have woken it.
        wakes.appended(store.archive_newest().expect("newest"));
        assert_eq!(wakes.due(20_000).len(), 1);
    }

    /// 1307: a member's message to a hosted group is archived as the
    /// group's, proven, and fanned out through the outbox on the group's
    /// own engine; a copy's proof is booked in `group_fanout` and makes no
    /// entry the owner's devices see; the same message again is proven
    /// again and sent to nobody.
    #[test]
    fn a_groups_engine_archives_proves_and_fans_out() {
        use crate::outbox::fake::FakeOutbox;
        let home = tempfile::tempdir().expect("home");
        let person = || {
            let identity = leviculum_std::generate_identity();
            let address = crate::home::lxmf_address(&identity.public_key_bytes()).expect("address");
            (identity, address)
        };
        let (lew, anna, bob) = (person(), person(), person());
        let mut store = Store::open_in(home.path(), lew.1).expect("store");
        let hosted = group::create(
            home.path(),
            &mut store,
            "Familie",
            (lew.1, "Lew"),
            &[(anna.1, "Anna".into()), (bob.1, "Bob".into())],
            100,
        )
        .expect("create");
        // Anna joined before: her line goes with no list beside it (G2
        // tells the others of a member who joins by writing).
        store
            .group_flag(
                &hosted.group.address,
                &anna.1,
                crate::store::groups::MEMBER_JOINED,
            )
            .expect("joined");
        let original = leviculum_lxmf::Message::create(
            hosted.group.address,
            anna.1,
            &anna.0,
            1_800_000_000.0,
            Vec::new(),
            b"Bin gleich da".to_vec(),
            Vec::new(),
            leviculum_lxmf::DeliveryMethod::Direct,
        )
        .expect("message");
        let packed = original.pack();
        let arrived = OutboxEvent::Packed {
            message_id: original.message_id,
            packed: packed.clone(),
            arrived: crate::outbox::Arrived::Direct,
        };
        let mut engine = GroupEngine {
            hosted: hosted.clone(),
            engine: FakeOutbox::new(vec![arrived.clone()]),
            outgoing: Outgoing::new(Policy::default()),
            selected_pn: None,
            ended: false,
            census: None,
        };
        let mut sender = Sender {
            cap_bytes: u64::MAX,
            preferred_pn: None,
            collecting: false,
            told: std::collections::HashSet::new(),
            newest: 0,
            nodes: std::collections::HashMap::new(),
            proven: Vec::new(),
            lines: Default::default(),
        };
        pump_group(&mut store, &mut sender, &mut engine, None);
        let commands = engine.engine.commands.take();
        assert_eq!(
            commands[0],
            Command::Prove {
                message_id: original.message_id
            },
            "proven once archived"
        );
        let sent: Vec<Vec<u8>> = commands[1..]
            .iter()
            .map(|command| match command {
                Command::SendPacked {
                    packed,
                    via: Via::Link,
                } => packed.clone(),
                other => panic!("not a direct send: {other:?}"),
            })
            .collect();
        assert_eq!(sent.len(), 2, "Lew and Bob");
        let copies = store
            .group_copies(&hosted.group.address, &original.message_id)
            .expect("copies");
        assert_eq!(copies.len(), 2);
        let page = store.archive_since(0, usize::MAX).expect("archive");
        let kinds: Vec<u8> = page.entries.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [crate::home::KIND_GROUP]);
        assert_eq!(page.entries[0].packed, packed);
        // Bob's copy is proven.
        let bob_copy = copies.iter().find(|c| c.member == bob.1).expect("bob").copy;
        engine.engine.events.push_back(OutboxEvent::Left {
            message_id: bob_copy,
            last: Some(MessageState::Delivered),
            failure: None,
        });
        pump_group(&mut store, &mut sender, &mut engine, None);
        let proven = store.group_copy(&bob_copy).expect("row").expect("copy");
        assert_eq!(proven.state, crate::store::groups::COPY_PROVEN);
        let page = store.archive_since(0, usize::MAX).expect("archive");
        assert_eq!(page.entries.len(), 1, "no entry for the owner's devices");
        // The same message again: proven, nothing sent.
        engine.engine.events.push_back(arrived);
        pump_group(&mut store, &mut sender, &mut engine, None);
        assert_eq!(
            engine.engine.commands.take(),
            [Command::Prove {
                message_id: original.message_id
            }]
        );
    }

    /// 1328, Lew's group invite on miauhaus: a group copy whose direct leg
    /// failed goes to the propagation node the home's collections selected,
    /// and the group's engine is told that node first, since the home's
    /// engine selecting it selected nothing in the group's. Without the
    /// selection the group's router had no node and ended every copy's
    /// propagated leg unsent in one tick. Control: once the group's engine
    /// said it holds the node, the next copy is not selected again; the
    /// home's own engine, holding it, never is.
    #[test]
    fn a_group_copy_selects_the_homes_node_on_the_groups_engine_before_it_propagates() {
        use crate::outbox::fake::FakeOutbox;
        const PN: [u8; 16] = [0x5a; 16];
        let home = tempfile::tempdir().expect("home");
        let person = || {
            let identity = leviculum_std::generate_identity();
            let address = crate::home::lxmf_address(&identity.public_key_bytes()).expect("address");
            (identity, address)
        };
        let (lew, anna, bob) = (person(), person(), person());
        let mut store = Store::open_in(home.path(), lew.1).expect("store");
        let hosted = group::create(
            home.path(),
            &mut store,
            "Familie",
            (lew.1, "Lew"),
            &[(anna.1, "Anna".into()), (bob.1, "Bob".into())],
            100,
        )
        .expect("create");
        store
            .group_flag(
                &hosted.group.address,
                &anna.1,
                crate::store::groups::MEMBER_JOINED,
            )
            .expect("joined");
        let original = leviculum_lxmf::Message::create(
            hosted.group.address,
            anna.1,
            &anna.0,
            1_800_000_000.0,
            Vec::new(),
            b"Bin gleich da".to_vec(),
            Vec::new(),
            leviculum_lxmf::DeliveryMethod::Direct,
        )
        .expect("message");
        let mut engine = GroupEngine {
            hosted: hosted.clone(),
            engine: FakeOutbox::new(vec![OutboxEvent::Packed {
                message_id: original.message_id,
                packed: original.pack(),
                arrived: crate::outbox::Arrived::Direct,
            }]),
            outgoing: Outgoing::new(Policy::default()),
            selected_pn: None,
            ended: false,
            census: None,
        };
        let mut sender = Sender {
            cap_bytes: u64::MAX,
            preferred_pn: None,
            collecting: true,
            told: std::collections::HashSet::new(),
            newest: 0,
            nodes: std::collections::HashMap::new(),
            proven: Vec::new(),
            lines: Default::default(),
        };
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        let _ = engine.engine.commands.take();
        let copies = store
            .group_copies(&hosted.group.address, &original.message_id)
            .expect("copies");
        let bobs = copies.iter().find(|c| c.member == bob.1).expect("bob").copy;
        let lews = copies.iter().find(|c| c.member == lew.1).expect("lew").copy;

        // Bob's direct leg fails: the group's engine selects the home's
        // node, then takes the copy propagated.
        engine.engine.events.push_back(OutboxEvent::Left {
            message_id: bobs,
            last: Some(MessageState::Failed),
            failure: None,
        });
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        let commands = engine.engine.commands.take();
        assert_eq!(
            commands[0],
            Command::SelectPn {
                preferred: Some(PN),
                mailbox: None
            },
            "{commands:?}"
        );
        assert!(
            matches!(
                &commands[1],
                Command::SendPacked {
                    via: Via::Propagated,
                    ..
                }
            ),
            "{commands:?}"
        );
        assert_eq!(commands.len(), 2, "{commands:?}");

        // Control: the group's engine said it holds the node; Lew's copy
        // fails direct and goes propagated without a second selection.
        engine.engine.events.push_back(OutboxEvent::PnSelected {
            destination: PN,
            stamp_cost: Some(13),
        });
        engine.engine.events.push_back(OutboxEvent::Left {
            message_id: lews,
            last: Some(MessageState::Failed),
            failure: None,
        });
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        let commands = engine.engine.commands.take();
        assert!(
            matches!(
                commands.as_slice(),
                [Command::SendPacked {
                    via: Via::Propagated,
                    ..
                }]
            ),
            "{commands:?}"
        );
    }

    /// 1330, miauhaus 12:15:05-12:19:24: Bob's copy was handed on to the
    /// node, and every unanswered direct leg after it drew a path response
    /// to the router's own path request, which the group's pump took for
    /// Bob and tried direct again, every 100 s. Handed on, a path response
    /// sends nothing; control: Bob's own announce tries direct.
    #[test]
    fn a_path_response_after_a_hand_on_sends_nothing_on_the_groups_engine() {
        use crate::outbox::fake::FakeOutbox;
        const PN: [u8; 16] = [0x5a; 16];
        let home = tempfile::tempdir().expect("home");
        let person = || {
            let identity = leviculum_std::generate_identity();
            let address = crate::home::lxmf_address(&identity.public_key_bytes()).expect("address");
            (identity, address)
        };
        let (lew, anna, bob) = (person(), person(), person());
        let mut store = Store::open_in(home.path(), lew.1).expect("store");
        let hosted = group::create(
            home.path(),
            &mut store,
            "Familie",
            (lew.1, "Lew"),
            &[(anna.1, "Anna".into()), (bob.1, "Bob".into())],
            100,
        )
        .expect("create");
        store
            .group_flag(
                &hosted.group.address,
                &anna.1,
                crate::store::groups::MEMBER_JOINED,
            )
            .expect("joined");
        let original = leviculum_lxmf::Message::create(
            hosted.group.address,
            anna.1,
            &anna.0,
            1_800_000_000.0,
            Vec::new(),
            b"Bin gleich da".to_vec(),
            Vec::new(),
            leviculum_lxmf::DeliveryMethod::Direct,
        )
        .expect("message");
        let mut engine = GroupEngine {
            hosted: hosted.clone(),
            engine: FakeOutbox::new(vec![OutboxEvent::Packed {
                message_id: original.message_id,
                packed: original.pack(),
                arrived: crate::outbox::Arrived::Direct,
            }]),
            outgoing: Outgoing::new(Policy::default()),
            selected_pn: Some(PN),
            ended: false,
            census: None,
        };
        let mut sender = Sender {
            cap_bytes: u64::MAX,
            preferred_pn: None,
            collecting: true,
            told: std::collections::HashSet::new(),
            newest: 0,
            nodes: std::collections::HashMap::new(),
            proven: Vec::new(),
            lines: Default::default(),
        };
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        let _ = engine.engine.commands.take();
        let copies = store
            .group_copies(&hosted.group.address, &original.message_id)
            .expect("copies");
        let bobs = copies.iter().find(|c| c.member == bob.1).expect("bob").copy;
        engine.engine.events.push_back(OutboxEvent::Left {
            message_id: bobs,
            last: Some(MessageState::Failed),
            failure: None,
        });
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        engine.engine.events.push_back(OutboxEvent::Left {
            message_id: bobs,
            last: Some(MessageState::Sent),
            failure: None,
        });
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        let commands = engine.engine.commands.take();
        assert!(
            matches!(
                commands.as_slice(),
                [Command::SendPacked {
                    via: Via::Propagated,
                    ..
                }]
            ),
            "handed on: {commands:?}"
        );

        engine.engine.events.push_back(OutboxEvent::Announced {
            destination: bob.1,
            name: None,
            path_response: true,
        });
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        let commands = engine.engine.commands.take();
        assert!(commands.is_empty(), "a path response: {commands:?}");

        engine.engine.events.push_back(OutboxEvent::Announced {
            destination: bob.1,
            name: None,
            path_response: false,
        });
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        let commands = engine.engine.commands.take();
        assert!(
            matches!(
                commands.as_slice(),
                [Command::SendPacked { via: Via::Link, .. }]
            ),
            "Bob's announce: {commands:?}"
        );
    }

    /// 1406, the field mailbox: a copy's propagated leg to a member whose
    /// key the engine does not hold is refused at once (`KeyMissing`), and
    /// the message is on no leg: nothing more is sent until the member is
    /// heard. Its path response, which brings the key, offers the node the
    /// copy once; a second one sends nothing (1330).
    #[test]
    fn a_copy_without_the_members_key_is_offered_again_when_its_key_comes() {
        use crate::outbox::fake::FakeOutbox;
        const PN: [u8; 16] = [0x5a; 16];
        let home = tempfile::tempdir().expect("home");
        let person = || {
            let identity = leviculum_std::generate_identity();
            let address = crate::home::lxmf_address(&identity.public_key_bytes()).expect("address");
            (identity, address)
        };
        let (lew, anna, bob) = (person(), person(), person());
        let mut store = Store::open_in(home.path(), lew.1).expect("store");
        let hosted = group::create(
            home.path(),
            &mut store,
            "Familie",
            (lew.1, "Lew"),
            &[(anna.1, "Anna".into()), (bob.1, "Bob".into())],
            100,
        )
        .expect("create");
        store
            .group_flag(
                &hosted.group.address,
                &anna.1,
                crate::store::groups::MEMBER_JOINED,
            )
            .expect("joined");
        let original = leviculum_lxmf::Message::create(
            hosted.group.address,
            anna.1,
            &anna.0,
            1_800_000_000.0,
            Vec::new(),
            b"Bin gleich da".to_vec(),
            Vec::new(),
            leviculum_lxmf::DeliveryMethod::Direct,
        )
        .expect("message");
        let mut engine = GroupEngine {
            hosted: hosted.clone(),
            engine: FakeOutbox::new(vec![OutboxEvent::Packed {
                message_id: original.message_id,
                packed: original.pack(),
                arrived: crate::outbox::Arrived::Direct,
            }]),
            outgoing: Outgoing::new(Policy::default()),
            selected_pn: Some(PN),
            ended: false,
            census: None,
        };
        let mut sender = Sender {
            cap_bytes: u64::MAX,
            preferred_pn: None,
            collecting: true,
            told: std::collections::HashSet::new(),
            newest: 0,
            nodes: std::collections::HashMap::new(),
            proven: Vec::new(),
            lines: Default::default(),
        };
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        let _ = engine.engine.commands.take();
        let bobs = store
            .group_copy_for(&hosted.group.address, &original.message_id, &bob.1)
            .expect("row")
            .expect("copy")
            .copy;
        engine.engine.events.push_back(OutboxEvent::Left {
            message_id: bobs,
            last: Some(MessageState::Failed),
            failure: None,
        });
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        let propagated = |commands: &[Command]| {
            matches!(
                commands,
                [Command::SendPacked {
                    via: Via::Propagated,
                    ..
                }]
            )
        };
        let commands = engine.engine.commands.take();
        assert!(propagated(&commands), "{commands:?}");
        engine.engine.events.push_back(OutboxEvent::KeyMissing {
            message_id: bobs,
            destination: bob.1,
        });
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        assert_eq!(engine.outgoing.in_flight(&bobs), None, "on no leg");
        assert!(engine.engine.commands.take().is_empty());
        let acts = engine.outgoing.tick(unix_now());
        assert!(acts.is_empty(), "nothing until Bob is heard: {acts:?}");

        let path_response = OutboxEvent::Announced {
            destination: bob.1,
            name: None,
            path_response: true,
        };
        engine.engine.events.push_back(path_response.clone());
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        let commands = engine.engine.commands.take();
        assert!(propagated(&commands), "the key came: {commands:?}");
        engine.engine.events.push_back(OutboxEvent::Left {
            message_id: bobs,
            last: Some(MessageState::Sent),
            failure: None,
        });
        engine.engine.events.push_back(path_response);
        pump_group(&mut store, &mut sender, &mut engine, Some(PN));
        assert!(
            engine.engine.commands.take().is_empty(),
            "handed on, a path response sends nothing"
        );
        let row = store.group_copy(&bobs).expect("row").expect("copy");
        assert!(row.propagated, "the node took it");
    }

    /// 1307 G2, item 1: the daemon dies between the fan-out and the
    /// proofs; started again on the same store, it hands the group's
    /// engine the very copies it had sent (rebuilt from the archive) and
    /// books their proofs. Controls: a copy proven before the restart is
    /// not sent again; a start that skips `resume_group` sends nothing, so
    /// the resend is the resume's.
    #[test]
    fn a_restart_between_fan_out_and_proof_sends_the_same_copies_again() {
        use crate::outbox::fake::FakeOutbox;
        let home = tempfile::tempdir().expect("home");
        let person = || {
            let identity = leviculum_std::generate_identity();
            let address = crate::home::lxmf_address(&identity.public_key_bytes()).expect("address");
            (identity, address)
        };
        let (lew, anna, bob, cara) = (person(), person(), person(), person());
        let mut store = Store::open_in(home.path(), lew.1).expect("store");
        let hosted = group::create(
            home.path(),
            &mut store,
            "Familie",
            (lew.1, "Lew"),
            &[
                (anna.1, "Anna".into()),
                (bob.1, "Bob".into()),
                (cara.1, "Cara".into()),
            ],
            100,
        )
        .expect("create");
        // Anna joined before: her line is a line, with no list beside it.
        store
            .group_flag(
                &hosted.group.address,
                &anna.1,
                crate::store::groups::MEMBER_JOINED,
            )
            .expect("joined");
        let original = leviculum_lxmf::Message::create(
            hosted.group.address,
            anna.1,
            &anna.0,
            1_800_000_000.0,
            Vec::new(),
            b"Bin gleich da".to_vec(),
            Vec::new(),
            leviculum_lxmf::DeliveryMethod::Direct,
        )
        .expect("message");
        let sender = || Sender {
            cap_bytes: u64::MAX,
            preferred_pn: None,
            collecting: false,
            told: std::collections::HashSet::new(),
            newest: 0,
            nodes: std::collections::HashMap::new(),
            proven: Vec::new(),
            lines: Default::default(),
        };
        let sends = |commands: Vec<Command>| -> Vec<Vec<u8>> {
            let mut out: Vec<Vec<u8>> = commands
                .into_iter()
                .filter_map(|command| match command {
                    Command::SendPacked { packed, .. } => Some(packed),
                    _ => None,
                })
                .collect();
            out.sort();
            out
        };
        let mut engine = GroupEngine {
            hosted: hosted.clone(),
            engine: FakeOutbox::new(vec![OutboxEvent::Packed {
                message_id: original.message_id,
                packed: original.pack(),
                arrived: crate::outbox::Arrived::Direct,
            }]),
            outgoing: Outgoing::new(Policy::default()),
            selected_pn: None,
            ended: false,
            census: None,
        };
        let mut first_sender = sender();
        pump_group(&mut store, &mut first_sender, &mut engine, None);
        let before = sends(engine.engine.commands.take());
        assert_eq!(before.len(), 3, "Lew, Bob and Cara");
        // Cara's copy is proven before the daemon dies.
        let caras = store
            .group_copy_for(&hosted.group.address, &original.message_id, &cara.1)
            .expect("row")
            .expect("copy")
            .copy;
        engine.engine.events.push_back(OutboxEvent::Left {
            message_id: caras,
            last: Some(MessageState::Delivered),
            failure: None,
        });
        pump_group(&mut store, &mut first_sender, &mut engine, None);
        // Killed: the engine, its outbox and the store go.
        drop(engine);
        drop(store);

        let mut store = Store::open_in(home.path(), lew.1).expect("store again");
        let hosted = group::hosted(home.path(), &store)
            .expect("hosted")
            .pop()
            .expect("the group");
        let start = |store: &mut Store, resume: bool| {
            let mut engine = GroupEngine {
                hosted: hosted.clone(),
                engine: FakeOutbox::new(Vec::new()),
                outgoing: Outgoing::new(Policy::default()),
                selected_pn: None,
                ended: false,
                census: None,
            };
            if resume {
                resume_group(store, &mut engine, unix_now());
            }
            let acts = engine.outgoing.tick(unix_now());
            sender().carry_out(
                &mut engine.engine,
                store,
                &mut engine.outgoing,
                acts,
                None,
                None,
            );
            engine
        };
        let skipped = start(&mut store, false);
        assert!(
            sends(skipped.engine.commands.take()).is_empty(),
            "control: no resume, no send"
        );
        let mut engine = start(&mut store, true);
        let after = sends(engine.engine.commands.take());
        let expected: Vec<Vec<u8>> = {
            let caras_packed = before
                .iter()
                .find(|p| {
                    leviculum_lxmf::Message::unpack(
                        p,
                        None,
                        None,
                        leviculum_lxmf::DeliveryMethod::Direct,
                    )
                    .is_ok_and(|m| m.message_id == caras)
                })
                .expect("cara's")
                .clone();
            before
                .iter()
                .filter(|p| **p != caras_packed)
                .cloned()
                .collect()
        };
        assert_eq!(
            after, expected,
            "Lew's and Bob's copies, byte for byte; not Cara's"
        );
        // Their proofs come now and are booked.
        for member in [lew.1, bob.1] {
            let copy = store
                .group_copy_for(&hosted.group.address, &original.message_id, &member)
                .expect("row")
                .expect("copy")
                .copy;
            engine.engine.events.push_back(OutboxEvent::Left {
                message_id: copy,
                last: Some(MessageState::Delivered),
                failure: None,
            });
        }
        pump_group(&mut store, &mut sender(), &mut engine, None);
        let states: Vec<i64> = store
            .group_copies(&hosted.group.address, &original.message_id)
            .expect("copies")
            .iter()
            .map(|c| c.state)
            .collect();
        assert_eq!(states, [crate::store::groups::COPY_PROVEN; 3]);
        assert!(store
            .group_copies_open(&hosted.group.address)
            .expect("open")
            .is_empty());
    }

    /// 1406, the field mailbox at the 13:56 restart: a copy booked under
    /// one text rule could not be made again byte for byte under the next
    /// (1388 changed a photo's text), and the restart ended it, open and
    /// unproven. Every later change of the wire text would end whatever is
    /// open. Bob's copy here stands for one minted under a rule this
    /// program no longer has: its row names that copy's id. Started again,
    /// the home makes Bob's copy under the rule it has, books the row under
    /// the new id, sends it and books its proof. Control: Lew's copy, made
    /// under the same rule, goes again byte for byte.
    #[test]
    fn a_copy_minted_under_an_older_text_rule_survives_a_restart() {
        use crate::outbox::fake::FakeOutbox;
        let home = tempfile::tempdir().expect("home");
        let person = || {
            let identity = leviculum_std::generate_identity();
            let address = crate::home::lxmf_address(&identity.public_key_bytes()).expect("address");
            (identity, address)
        };
        let (lew, anna, bob) = (person(), person(), person());
        let mut store = Store::open_in(home.path(), lew.1).expect("store");
        let hosted = group::create(
            home.path(),
            &mut store,
            "Familie",
            (lew.1, "Lew"),
            &[(anna.1, "Anna".into()), (bob.1, "Bob".into())],
            100,
        )
        .expect("create");
        store
            .group_flag(
                &hosted.group.address,
                &anna.1,
                crate::store::groups::MEMBER_JOINED,
            )
            .expect("joined");
        let original = leviculum_lxmf::Message::create(
            hosted.group.address,
            anna.1,
            &anna.0,
            1_800_000_000.0,
            Vec::new(),
            b"Bin gleich da".to_vec(),
            Vec::new(),
            leviculum_lxmf::DeliveryMethod::Direct,
        )
        .expect("message");
        let sender = || Sender {
            cap_bytes: u64::MAX,
            preferred_pn: None,
            collecting: false,
            told: std::collections::HashSet::new(),
            newest: 0,
            nodes: std::collections::HashMap::new(),
            proven: Vec::new(),
            lines: Default::default(),
        };
        let unpack = |packed: &[u8]| {
            leviculum_lxmf::Message::unpack(
                packed,
                None,
                None,
                leviculum_lxmf::DeliveryMethod::Direct,
            )
            .expect("a copy")
        };
        let sends = |commands: Vec<Command>| -> Vec<Vec<u8>> {
            let mut out: Vec<Vec<u8>> = commands
                .into_iter()
                .filter_map(|command| match command {
                    Command::SendPacked { packed, .. } => Some(packed),
                    _ => None,
                })
                .collect();
            out.sort();
            out
        };
        let mut engine = GroupEngine {
            hosted: hosted.clone(),
            engine: FakeOutbox::new(vec![OutboxEvent::Packed {
                message_id: original.message_id,
                packed: original.pack(),
                arrived: crate::outbox::Arrived::Direct,
            }]),
            outgoing: Outgoing::new(Policy::default()),
            selected_pn: None,
            ended: false,
            census: None,
        };
        pump_group(&mut store, &mut sender(), &mut engine, None);
        let before = sends(engine.engine.commands.take());
        assert_eq!(before.len(), 2, "Lew and Bob");
        let bobs = store
            .group_copy_for(&hosted.group.address, &original.message_id, &bob.1)
            .expect("row")
            .expect("copy")
            .copy;
        // Bob's copy as an older rule made it: the same message in other
        // words, signed by the group, its id in the row.
        let now_rule = before
            .iter()
            .map(|packed| unpack(packed))
            .find(|copy| copy.message_id == bobs)
            .expect("Bob's");
        let old_rule = leviculum_lxmf::Message::create(
            now_rule.destination_hash,
            now_rule.source_hash,
            &hosted.identity,
            now_rule.timestamp,
            now_rule.title.clone(),
            b"Anna (a rule this program no longer has): Bin gleich da".to_vec(),
            now_rule.fields.clone(),
            leviculum_lxmf::DeliveryMethod::Direct,
        )
        .expect("the older copy");
        assert_ne!(old_rule.message_id, bobs);
        store
            .group_copy_renamed(&bobs, &old_rule.message_id)
            .expect("booked under the older rule");
        drop(engine);
        drop(store);

        let mut store = Store::open_in(home.path(), lew.1).expect("store again");
        let hosted = group::hosted(home.path(), &store)
            .expect("hosted")
            .pop()
            .expect("the group");
        let mut engine = GroupEngine {
            hosted: hosted.clone(),
            engine: FakeOutbox::new(Vec::new()),
            outgoing: Outgoing::new(Policy::default()),
            selected_pn: None,
            ended: false,
            census: None,
        };
        assert_eq!(resume_group(&mut store, &mut engine, unix_now()), 2);
        let acts = engine.outgoing.tick(unix_now());
        sender().carry_out(
            &mut engine.engine,
            &mut store,
            &mut engine.outgoing,
            acts,
            None,
            None,
        );
        assert_eq!(
            sends(engine.engine.commands.take()),
            before,
            "both copies, under the rule this program has"
        );
        let row = store
            .group_copy_for(&hosted.group.address, &original.message_id, &bob.1)
            .expect("row")
            .expect("copy");
        assert_eq!(row.copy, bobs, "the row names the copy sent");
        assert_eq!(row.state, 0, "open");
        engine.engine.events.push_back(OutboxEvent::Left {
            message_id: bobs,
            last: Some(MessageState::Delivered),
            failure: None,
        });
        pump_group(&mut store, &mut sender(), &mut engine, None);
        let row = store.group_copy(&bobs).expect("row").expect("copy");
        assert_eq!(row.state, crate::store::groups::COPY_PROVEN);
    }

    /// 1307 pass four, the 1330 side finding: a group's copy a propagation
    /// node took is kept so, and after a restart the copy is tried direct
    /// again but never uploaded a second time; the node answered `dup=true`
    /// to every such upload before. Control: a copy no node took is handed
    /// on once its direct leg fails after the restart.
    #[test]
    fn a_restart_after_a_hand_on_uploads_nothing_again() {
        use crate::outbox::fake::FakeOutbox;
        const PN: [u8; 16] = [0x5a; 16];
        let home = tempfile::tempdir().expect("home");
        let person = || {
            let identity = leviculum_std::generate_identity();
            let address = crate::home::lxmf_address(&identity.public_key_bytes()).expect("address");
            (identity, address)
        };
        let (lew, anna, bob) = (person(), person(), person());
        let mut store = Store::open_in(home.path(), lew.1).expect("store");
        let hosted = group::create(
            home.path(),
            &mut store,
            "Familie",
            (lew.1, "Lew"),
            &[(anna.1, "Anna".into()), (bob.1, "Bob".into())],
            100,
        )
        .expect("create");
        store
            .group_flag(
                &hosted.group.address,
                &anna.1,
                crate::store::groups::MEMBER_JOINED,
            )
            .expect("joined");
        let original = leviculum_lxmf::Message::create(
            hosted.group.address,
            anna.1,
            &anna.0,
            1_800_000_000.0,
            Vec::new(),
            b"Bin gleich da".to_vec(),
            Vec::new(),
            leviculum_lxmf::DeliveryMethod::Direct,
        )
        .expect("message");
        let sender = || Sender {
            cap_bytes: u64::MAX,
            preferred_pn: None,
            collecting: true,
            told: std::collections::HashSet::new(),
            newest: 0,
            nodes: std::collections::HashMap::new(),
            proven: Vec::new(),
            lines: Default::default(),
        };
        let mut engine = GroupEngine {
            hosted: hosted.clone(),
            engine: FakeOutbox::new(vec![OutboxEvent::Packed {
                message_id: original.message_id,
                packed: original.pack(),
                arrived: crate::outbox::Arrived::Direct,
            }]),
            outgoing: Outgoing::new(Policy::default()),
            selected_pn: Some(PN),
            ended: false,
            census: None,
        };
        let mut first = sender();
        pump_group(&mut store, &mut first, &mut engine, Some(PN));
        let copies = store
            .group_copies(&hosted.group.address, &original.message_id)
            .expect("copies");
        let copy_of = |member: [u8; 16]| {
            copies
                .iter()
                .find(|c| c.member == member)
                .expect("copy")
                .copy
        };
        let (lews, bobs) = (copy_of(lew.1), copy_of(bob.1));
        // Bob's copy: direct fails, the node takes it.
        for last in [MessageState::Failed, MessageState::Sent] {
            engine.engine.events.push_back(OutboxEvent::Left {
                message_id: bobs,
                last: Some(last),
                failure: None,
            });
            pump_group(&mut store, &mut first, &mut engine, Some(PN));
        }
        assert!(
            store
                .group_copy(&bobs)
                .expect("row")
                .expect("bob")
                .propagated,
            "kept"
        );
        assert!(
            !store
                .group_copy(&lews)
                .expect("row")
                .expect("lew")
                .propagated
        );
        drop(engine);
        drop(store);

        let mut store = Store::open_in(home.path(), lew.1).expect("store again");
        let hosted = group::hosted(home.path(), &store)
            .expect("hosted")
            .pop()
            .expect("the group");
        let mut engine = GroupEngine {
            hosted,
            engine: FakeOutbox::new(Vec::new()),
            outgoing: Outgoing::new(Policy::default()),
            selected_pn: Some(PN),
            ended: false,
            census: None,
        };
        assert_eq!(resume_group(&mut store, &mut engine, unix_now()), 2);
        let mut again = sender();
        let acts = engine.outgoing.tick(unix_now());
        again.carry_out(
            &mut engine.engine,
            &mut store,
            &mut engine.outgoing,
            acts,
            Some(PN),
            Some(PN),
        );
        let _ = engine.engine.commands.take();
        for id in [lews, bobs] {
            engine.engine.events.push_back(OutboxEvent::Left {
                message_id: id,
                last: Some(MessageState::Failed),
                failure: None,
            });
        }
        pump_group(&mut store, &mut again, &mut engine, Some(PN));
        let uploads: Vec<[u8; 32]> = engine
            .engine
            .commands
            .take()
            .into_iter()
            .filter_map(|command| match command {
                Command::SendPacked {
                    packed,
                    via: Via::Propagated,
                } => leviculum_lxmf::Message::unpack(
                    &packed,
                    None,
                    None,
                    leviculum_lxmf::DeliveryMethod::Direct,
                )
                .ok()
                .map(|m| m.message_id),
                _ => None,
            })
            .collect();
        assert_eq!(uploads, [lews], "Lew's handed on, Bob's not again");
    }
}
