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
//!   name rides along with the next pull (1136).

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
use crate::engine::{attach_as, attach_mesh, AttachConfig, HomeRequest, MeshConfig, Role};
use crate::fanout::Fanout;
use crate::fetch::{run_fetch, FetchOptions};
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
    } = options;
    let _lock = super::lock::DeliveryLock::acquire(&home, super::lock::HOME_NODE)?;
    let address = crate::engine::delivery_address(&identity)?;
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
    let (requests_tx, mut requests) = unbounded_channel::<HomeRequest>();
    let role = Role::Home {
        requests: requests_tx,
    };
    let (instance, attached, mailbox) = match upstream {
        DaemonUpstream::Shared {
            instance,
            config_dir,
            mailbox,
        } => {
            let attached = attach_as(
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
            (instance, attached, mailbox)
        }
        DaemonUpstream::Mesh {
            servers,
            screens,
            tcp,
        } => {
            let attached = attach_mesh(
                MeshConfig {
                    servers,
                    screens,
                    storage_dir,
                    identity,
                    display_name,
                    notices: Terminal::new(home.clone()),
                    tcp,
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
    let serving = super::server::Home {
        identity: &signer,
        address,
        token_ttl_secs,
        cap_bytes: archive_cap_bytes,
    };
    let mut sender = Sender {
        cap_bytes: archive_cap_bytes,
        preferred_pn,
        collecting: sync_bound_ms.is_some(),
        told: std::collections::HashSet::new(),
        newest: 0,
    };
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
                let (accepted, watching) = answer(&node, &mut store, &serving, request).await;
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
                if let Some(task) = fetch.take_if(|task| task.is_finished()) {
                    let event = match task.await {
                        Ok(Some(new)) => SyncEvent::Done { new },
                        _ => SyncEvent::Failed,
                    };
                    if let Some(schedule) = &mut schedule {
                        schedule.on(event, now_ms());
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
    outcome
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
) -> (Vec<crate::store::Outgoing>, Option<u64>) {
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
        return (answer.accepted, None);
    }
    (answer.accepted, answer.watching)
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
        let acts = outgoing.left(id, outcome);
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
                        acts.extend(outgoing.left(&id, Outcome::Failed));
                    }
                }
            }
        }
    }

    fn record(&mut self, store: &mut Store, id: &[u8; 32], change: Change) {
        if change != Change::Propagated {
            self.told.remove(id);
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
}
