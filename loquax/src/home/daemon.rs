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
use leviculum_lxmf::router::MessageState;
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
use crate::home::outgoing::{Act, Change, Leg, Outcome, Outgoing, Policy};
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

/// How often the engine's events are read.
const PUMP: Duration = Duration::from_millis(20);
/// How often the outbox looks for a due retry or a passed horizon. Both are
/// counted in seconds.
const OUTBOX_TICK: Duration = Duration::from_secs(1);
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
}

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
    let mut relations = relations_at_start(&store, &home)?;
    let signer = identity.clone();
    let mut outgoing = Outgoing::new(policy);
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
    tracing::warn!(
        "HOME up address={} home={} archive_cap={}B outbox={}",
        crate::address::to_hex(&address),
        crate::address::to_hex(&home_destination),
        archive_cap_bytes,
        outgoing.len()
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
    };
    let mut sender = Sender {
        cap_bytes: archive_cap_bytes,
        preferred_pn,
        collecting: sync_bound_ms.is_some(),
        told: std::collections::HashSet::new(),
        newest: 0,
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
    wakes.appended(store.archive_newest_waking().map_err(|e| e.to_string())?);
    let mut outside_version = store.data_version().map_err(|e| e.to_string())?;

    let mut fanout = Fanout::default();
    let mut schedule = sync_bound_ms.map(|bound| Schedule::new(Some(bound)));
    let mut fetch: Option<tokio::task::JoinHandle<Option<usize>>> = None;
    // The node the engine's router has selected, as its PnSelected said:
    // not `--pn` itself, which the router learns only from a SelectPn, and
    // with collections off nothing else sends one.
    let mut selected_pn = None;
    let origin = tokio::time::Instant::now();
    let now_ms = || u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut pump = tokio::time::interval(PUMP);
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

    let outcome = loop {
        tokio::select! {
            Some(request) = requests.recv() => {
                let (link, remote) = (request.link_id, request.remote);
                serving.handover =
                    handover(&home, [&ratchet_file, &known_file], &own_name, &request.path);
                let (accepted, watching, gone, (made, added)) =
                    answer(&node, &mut store, &serving, request).await;
                if let Some((address, told)) = added {
                    // Members added (1307 G2): the new membership goes out
                    // on the group's own engine.
                    if let Some(engine) = groups.iter_mut().find(|g| g.hosted.group.address == address) {
                        let now = unix_now();
                        for out in told {
                            engine.outgoing.add(out.id, out.destination, out.packed, now, false);
                        }
                        let acts = engine.outgoing.tick(now);
                        sender.carry_out(&mut engine.engine, &mut store, &mut engine.outgoing, acts, selected_pn);
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
                            sender.carry_out(&mut engine.engine, &mut store, &mut engine.outgoing, acts, selected_pn);
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
                    sender.carry_out(&mut attached.outbox, &mut store, &mut outgoing, acts, selected_pn);
                }
            }
            _ = outside.tick() => {
                if let Some(newest) = written_outside(&mut store, &mut outside_version) {
                    wakes.appended(newest);
                }
            }
            _ = outbox_tick.tick() => {
                let acts = outgoing.tick(unix_now());
                sender.carry_out(&mut attached.outbox, &mut store, &mut outgoing, acts, selected_pn);
                for hosted in &mut groups {
                    digests(&mut store, hosted, unix_now());
                    let acts = hosted.outgoing.tick(unix_now());
                    sender.carry_out(&mut hosted.engine, &mut store, &mut hosted.outgoing, acts, selected_pn);
                }
            }
            _ = tokio::time::sleep_until(next_group_announce) => {
                next_group_announce = tokio::time::Instant::now() + GROUP_ANNOUNCE_EVERY;
                for hosted in &groups {
                    // A group whose engine went away is said by its pump.
                    let _ = hosted.engine.submit(Command::Announce);
                }
            }
            _ = pump.tick() => {
                let events = match fanout.pump(&mut attached.outbox) {
                    Ok(events) => events,
                    Err(gone) => break Err(gone.to_string()),
                };
                for event in events {
                    match event {
                        OutboxEvent::Ready { .. } => {
                            if let Some(schedule) = &mut schedule {
                                schedule.on(SyncEvent::Start, now_ms());
                            }
                        }
                        OutboxEvent::Packed { message_id, packed } => {
                            let Ok(new) = archive(&mut store, &message_id, &packed, archive_cap_bytes)
                            else {
                                // Not archived, so not proven: the sender
                                // tries again, or propagates.
                                continue;
                            };
                            // Archived, now or before: the sender may be
                            // told so (1239).
                            // An engine that went away is said by the next pump.
                            let _ = attached.outbox.submit(Command::Prove { message_id });
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
                        OutboxEvent::PnUnavailable { detail } => {
                            tracing::info!("HOME no propagation node: {detail}");
                        }
                        OutboxEvent::Announced { destination, name } => {
                            if let Some(name) = name.filter(|_| destination != address) {
                                heard_name(
                                    &mut store,
                                    &mut relations,
                                    destination,
                                    &name,
                                    archive_cap_bytes,
                                );
                            }
                            let acts = outgoing.announced(destination, unix_now());
                            sender.carry_out(&mut attached.outbox, &mut store, &mut outgoing, acts, selected_pn);
                        }
                        OutboxEvent::Left { message_id, last } => {
                            let outcome = match last {
                                Some(MessageState::Delivered) => Outcome::Delivered,
                                Some(MessageState::Sent | MessageState::AwaitingCollection) => {
                                    Outcome::Sent
                                }
                                _ => Outcome::Failed,
                            };
                            sender.left(&mut attached.outbox, &mut store, &mut outgoing, &message_id, outcome, selected_pn);
                        }
                        OutboxEvent::State { message_id, .. } => {
                            // A sign of the leg: the stall bound counts from now.
                            outgoing.heard(&message_id, unix_now());
                        }
                        OutboxEvent::Unsent { message_id, detail } => {
                            tracing::info!(
                                "HOME outbox unsent message={} reason={detail}",
                                crate::address::to_hex(&message_id)
                            );
                            sender.left(&mut attached.outbox, &mut store, &mut outgoing, &message_id, Outcome::Failed, selected_pn);
                        }
                        OutboxEvent::NodeAnnounced { destination }
                            if Some(destination) == selected_pn =>
                        {
                            if let Some(schedule) = &mut schedule {
                                schedule.on(SyncEvent::Announce, now_ms());
                            }
                        }
                        OutboxEvent::Broken { detail } => {
                            tracing::warn!("HOME engine broken: {detail}");
                        }
                        _ => {}
                    }
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
                }
                wakes.appended(sender.newest);
                for due in wakes.due(now_ms()) {
                    send_wake(&node, &mut store, &mut wakes, due).await;
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
            _ = signalled(terminate.as_mut()) => break Ok(()),
            _ = stopped(stop.as_mut()) => break Ok(()),
        }
    };
    if let Some(task) = fetch {
        task.abort();
    }
    drop(node);
    attached.stop().await;
    for hosted in groups {
        hosted.engine.stop().await;
    }
    outcome
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
            Ok(None) => return,
            Err(gone) => {
                tracing::warn!("GROUP {} engine: {gone}", group.hosted.group.name);
                return;
            }
        };
        match event {
            OutboxEvent::Packed { message_id, packed } => {
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
                );
            }
            OutboxEvent::Announced { destination, .. } => {
                let acts = group.outgoing.announced(destination, unix_now());
                sender.carry_out(
                    &mut group.engine,
                    store,
                    &mut group.outgoing,
                    acts,
                    selected_pn,
                );
            }
            OutboxEvent::Left { message_id, last } => {
                let outcome = match last {
                    Some(MessageState::Delivered) => Outcome::Delivered,
                    Some(MessageState::Sent | MessageState::AwaitingCollection) => Outcome::Sent,
                    _ => Outcome::Failed,
                };
                sender.left(
                    &mut group.engine,
                    store,
                    &mut group.outgoing,
                    &message_id,
                    outcome,
                    selected_pn,
                );
            }
            OutboxEvent::State { message_id, .. } => {
                group.outgoing.heard(&message_id, unix_now());
            }
            OutboxEvent::Unsent { message_id, .. } => {
                sender.left(
                    &mut group.engine,
                    store,
                    &mut group.outgoing,
                    &message_id,
                    Outcome::Failed,
                    selected_pn,
                );
            }
            _ => {}
        }
    }
}

/// A group's copies a restart cut off (1307 G2, study 2.4): every copy
/// sent alone with no proof yet is made again from the archived original,
/// byte for byte, and handed to the group's outbox with its acceptance
/// time, so the outbox's retry bound counts from then; a copy that cannot
/// be made again (its original evicted, its member gone) is ended. Every
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
                group.outgoing.add(
                    out.id,
                    out.destination,
                    out.packed,
                    row.sent_at.min(now),
                    false,
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
    let sent = node
        .link_handle(&due.link)
        .try_send(&super::wire::wake(due.newest))
        .await;
    match sent {
        Ok(()) => tracing::info!("HOME wake device={device} newest={}", due.newest),
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
    ),
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
    let sent = match node
        .send_response(&request.link_id, &request.request_id, bytes)
        .await
    {
        Err(leviculum_std::Error::Request(RequestError::PayloadTooLarge)) => {
            node.send_response_resource(&request.link_id, &request.request_id, bytes)
                .await
        }
        other => other,
    };
    if let Err(error) = sent {
        // The outbox has the message whether the answer arrived or not: a
        // client that retries with the same id is answered from it.
        tracing::warn!("HOME answer to {}: {error}", request.path);
        return (answer.accepted, None, false, (answer.group, answer.added));
    }
    (
        answer.accepted,
        answer.watching,
        answer.gone,
        (answer.group, answer.added),
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
}

impl Sender {
    /// The router let go of `id`: tell the outbox, and carry out what it
    /// says.
    fn left(
        &mut self,
        engine: &mut impl Outbox,
        store: &mut Store,
        outgoing: &mut Outgoing,
        id: &[u8; 32],
        outcome: Outcome,
        selected_pn: Option<[u8; 16]>,
    ) {
        if let Some(leg) = outgoing.in_flight(id) {
            tracing::info!(
                "HOME outbox {} {} message={}",
                leg_word(leg),
                match outcome {
                    Outcome::Delivered => "delivered",
                    Outcome::Sent => "handed-on",
                    Outcome::Failed => "failed",
                },
                crate::address::to_hex(id)
            );
        }
        let acts = outgoing.left(id, outcome, unix_now());
        self.carry_out(engine, store, outgoing, acts, selected_pn);
    }

    /// Hand each message to the router on its leg, and record each state
    /// change. A leg that cannot start is ended as failed at once, so the
    /// outbox moves on.
    fn carry_out(
        &mut self,
        engine: &mut impl Outbox,
        store: &mut Store,
        outgoing: &mut Outgoing,
        acts: Vec<Act>,
        selected_pn: Option<[u8; 16]>,
    ) {
        let mut acts = std::collections::VecDeque::from(acts);
        while let Some(act) = acts.pop_front() {
            match act {
                Act::Record { id, change } => self.record(store, &id, change),
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
                    let started = match leg {
                        Leg::Direct => engine
                            .submit(Command::SendPacked {
                                packed,
                                via: Via::Link,
                            })
                            .is_ok(),
                        Leg::Propagated => match selected_pn.or(self.preferred_pn) {
                            Some(node) => {
                                // The node the collections use, selected again
                                // if none was this run: one command queue, so
                                // the selection is in before the send.
                                let selected = selected_pn.is_some()
                                    || engine
                                        .submit(Command::SelectPn {
                                            preferred: Some(node),
                                            mailbox: None,
                                        })
                                        .is_ok();
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
                        tracing::info!(
                            "HOME outbox {} message={}",
                            leg_word(leg),
                            crate::address::to_hex(&id)
                        );
                    } else {
                        acts.extend(outgoing.left(&id, Outcome::Failed, unix_now()));
                    }
                }
            }
        }
    }

    fn record(&mut self, store: &mut Store, id: &[u8; 32], change: Change) {
        if change != Change::Propagated {
            self.told.remove(id);
        }
        // A group's copy is the group's bookkeeping, not an entry the
        // owner's devices see.
        match group::book(store, id, change, unix_now()) {
            Ok(true) => {
                tracing::info!(
                    "GROUP copy {} message={}",
                    change.word(),
                    crate::address::to_hex(id)
                );
                return;
            }
            Ok(false) => {}
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
        };
        let mut engine = GroupEngine {
            hosted: hosted.clone(),
            engine: FakeOutbox::new(vec![arrived.clone()]),
            outgoing: Outgoing::new(Policy::default()),
        };
        let mut sender = Sender {
            cap_bytes: u64::MAX,
            preferred_pn: None,
            collecting: false,
            told: std::collections::HashSet::new(),
            newest: 0,
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
            }]),
            outgoing: Outgoing::new(Policy::default()),
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
            };
            if resume {
                resume_group(store, &mut engine, unix_now());
            }
            let acts = engine.outgoing.tick(unix_now());
            sender().carry_out(&mut engine.engine, store, &mut engine.outgoing, acts, None);
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
}
