//! The rendezvous (`docs/src/concepts/loquax-reachability.md`, way B): a
//! public Reticulum transport node that both the phone and its mailbox call
//! out to, so neither has to be reachable from outside. The phone dials it
//! on its access point port, the mailbox on its ordinary port as a peer, and
//! the node relays the home link between them, end to end encrypted.
//!
//! - **The list**, ordered: [`DEFAULT`] when nothing names one, the join
//!   code's when the phone joined with one (`super::code`), the config's
//!   `rendezvous` key for a power user.
//! - **One at a time** (the reviewer's decision on 1195's open point, 2026-10-31):
//!   one TCP connection is what a held push channel costs, a second would
//!   double it for a faster failover nobody asked for.
//! - **Failover and return** ([`Rotation`]): an entry that does not come up
//!   within [`Timing::dial_grace_ms`], or goes down and does not come back
//!   within it, moves both ends to the next after a doubling wait; while a later
//!   entry carries the traffic the first is tried again on its own backoff,
//!   and the traffic goes back to it once it answers.
//! - **A path request on every up** (rule 2): each time the entry comes up,
//!   the home destination's path is asked for at once, so the client never
//!   waits for an announce, which a phone behind an access point port does
//!   not hear anyway (`reference/Reticulum/RNS/Transport.py:1193-1196`).
//!
//! - **A network change** ([`Network`], 1298): the platform says the
//!   phone's network went, came or changed ([`NetworkKind`]); after a short
//!   settle ([`Settle`]) every rotation re-evaluates once ([`evaluate`]): a
//!   new network drops the connection the old one carried and dials the
//!   first entry at once, no network drops it and dials nothing until one
//!   comes, the same network back with the connection down cuts the
//!   backoff short, and the same network with it up asks the paths again.
//!   The node's own up handling re-announces each destination once on the
//!   new connection (`handle_interface_up`), and the phone side asks the
//!   home path on that up as on any other (rule 2).
//!
//! The rotation itself is plain logic on a millisecond clock and has no I/O;
//! [`run`] drives it over the node.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub use super::NetworkKind;

/// The host of our own rendezvous, `leviculum.network`.
pub const HOST: &str = "leviculum.network";
/// Its access point port, the phones' (the reviewer's decision on 1195,
/// 2026-10-31): a TCP server in `mode = access_point`, so a phone on it
/// hears no announce of the mesh and the node searches the paths it asks
/// for (`reference/Reticulum/RNS/Interfaces/Interface.py:54`,
/// `reference/Reticulum/RNS/Transport.py:2918`). The one place to change it.
pub const ACCESS_POINT_PORT: u16 = 4343;
/// Its ordinary port, the mesh's and the mailboxes': 4242, as every
/// Reticulum TCP server's default.
pub const PEER_PORT: u16 = 4242;
/// The shipped list: our node's access point port, then its ordinary port
/// as the last fallback, so an app that meets a server from before the
/// access point port still connects.
pub const DEFAULT: &[(&str, u16)] = &[(HOST, ACCESS_POINT_PORT), (HOST, PEER_PORT)];

/// The shipped list as `host:port` texts.
pub fn default_list() -> Vec<String> {
    DEFAULT
        .iter()
        .map(|(host, port)| alloc::format!("{host}:{port}"))
        .collect()
}

/// The port a mailbox dials for an entry the phones dial at `port`: a
/// phone's port is an access point port, a peer takes the server's ordinary
/// one beside it. Our node's access point port and a mailbox's own phone
/// port (`super::phone_server::PORT`, 4243) map to [`PEER_PORT`]; any other
/// port is a server with one port for both, dialled as it is.
pub fn peer_port(port: u16) -> u16 {
    match port {
        ACCESS_POINT_PORT | super::phone_server::PORT => PEER_PORT,
        other => other,
    }
}

/// `entries` as a mailbox dials them: each on its [`peer_port`], the
/// duplicates that makes (`host:4343` and `host:4242`) once, in order.
pub fn peer_list(entries: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in entries {
        let peer = match entry.trim().rsplit_once(':') {
            Some((host, port)) => match port.parse::<u16>() {
                Ok(port) => alloc::format!("{host}:{}", peer_port(port)),
                Err(_) => entry.trim().to_string(),
            },
            None => entry.trim().to_string(),
        };
        if !out.contains(&peer) {
            out.push(peer);
        }
    }
    out
}

/// The list with `first` in front and its other copy gone: a mailbox on a
/// public server names its own phone port first (rule 7).
pub fn with_first(first: &str, rest: &[String]) -> Vec<String> {
    let first = first.trim().to_string();
    let mut out = alloc::vec![first.clone()];
    out.extend(
        rest.iter()
            .map(|s| s.trim().to_string())
            .filter(|s| *s != first),
    );
    out
}

/// How the phone reaches its mailbox, as the config decides it (the
/// config surface of 1196). Defaults decide: no key set is the shipped
/// rendezvous list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Dial these in order, one at a time, with failover.
    List(Vec<String>),
    /// A fixed address (way D, "Advanced"): this one and nothing else.
    Fixed(String),
    /// No rendezvous at all ("mesh only"): the device reaches the mailbox
    /// through whatever else it has.
    MeshOnly,
}

/// What the config's `rendezvous`, `rendezvous_dial` and `home_address`
/// keys say, with `carried` the list a join code brought: `home_address`
/// wins, then `rendezvous_dial = no`, then the config's list, then the
/// carried one, then [`DEFAULT`].
pub fn reach(config: &crate::config::Config, carried: &[String]) -> Reach {
    if let Some(fixed) = &config.home_address {
        return Reach::Fixed(fixed.clone());
    }
    if config.rendezvous_dial == Some(false) {
        return Reach::MeshOnly;
    }
    match (&config.rendezvous, carried) {
        (Some(list), _) if !list.is_empty() => Reach::List(list.clone()),
        (_, carried) if !carried.is_empty() => Reach::List(carried.to_vec()),
        _ => Reach::List(default_list()),
    }
}

/// Whether this host keeps the shipped defaults out: the host marker or
/// `LEVICULUM_NO_AUTO` of lnsd's automatic layer
/// (`docs/src/concepts/zero-config.md`, "The marker"), read by the same
/// rule. A rig or CI host, which the marker is installed on, dials no
/// public server it was not told to; a list the config names is dialled
/// there too.
pub fn defaults_held_back() -> bool {
    use leviculum_std::auto_layer::{decide, AutoMedia, EnvSwitch, ENV_NO_AUTO, HOST_MARKER};
    use leviculum_std::config::ConfigSource;
    let env = std::env::var(ENV_NO_AUTO).ok();
    let marker = std::path::Path::new(HOST_MARKER).exists();
    !decide(
        ConfigSource::Default,
        AutoMedia::SHIPPED,
        EnvSwitch::parse(env.as_deref()),
        marker,
    )
    .media
    .any()
}

/// The rendezvous a mailbox dials as a peer, on their ordinary ports, and
/// where the list came from: `config` for the `rendezvous` key, `default`
/// for the shipped list, `off` for `rendezvous_dial = off` (mesh only), and
/// `held` for the shipped list on a host that holds defaults back
/// ([`defaults_held_back`]).
pub fn mailbox_list(config: &crate::config::Config) -> (Vec<String>, &'static str) {
    if config.rendezvous_dial == Some(false) {
        return (Vec::new(), "off");
    }
    match &config.rendezvous {
        Some(list) if !list.is_empty() => (peer_list(list), "config"),
        _ if defaults_held_back() => (Vec::new(), "held"),
        _ => (peer_list(&default_list()), "default"),
    }
}

/// How the rotation waits, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// How long a dial may take to come up, and a dropped entry to come
    /// back by itself (its interface reconnects on its own), before the
    /// next entry is tried.
    pub dial_grace_ms: u64,
    /// The first wait before the next entry, doubled on each failure in a
    /// row up to [`Timing::backoff_max_ms`], back to the first once one
    /// came up.
    pub backoff_first_ms: u64,
    /// The longest wait before the next entry.
    pub backoff_max_ms: u64,
    /// The first wait before the first entry is tried again while a later
    /// one carries the traffic, doubled on each try that finds it down.
    pub return_first_ms: u64,
    /// The longest wait between two tries of the first entry.
    pub return_max_ms: u64,
    /// How often an entry that is up is looked at besides the events that
    /// say something changed: the link keepalive's ceiling, so an idle
    /// phone wakes for it no more often than for its link keepalive (1176).
    pub up_look_ms: u64,
    /// How often a dial in progress is looked at.
    pub dial_look_ms: u64,
    /// How long the network changes must be quiet before a rotation
    /// evaluates them ([`Settle`]).
    pub network_settle_ms: u64,
    /// How long after the first change of a burst it is evaluated at the
    /// latest.
    pub network_settle_max_ms: u64,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            dial_grace_ms: 15_000,
            backoff_first_ms: 2_000,
            backoff_max_ms: 300_000,
            return_first_ms: 60_000,
            return_max_ms: 30 * 60_000,
            up_look_ms: leviculum_core::constants::LINK_KEEPALIVE_SECS * 1000,
            dial_look_ms: 500,
            network_settle_ms: 1_500,
            network_settle_max_ms: 6_000,
        }
    }
}

/// What one network change asks of a rotation, by [`evaluate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evaluation {
    /// No network: drop the connection, which no longer leads anywhere,
    /// and dial nothing until one comes; a dial into no network only grows
    /// the backoff that the coming network would then wait out.
    Pause,
    /// Drop the connection and dial the first entry now, the backoff
    /// started over: a new network (its socket is bound to an address the
    /// phone no longer has and dies only when the keepalive gives up), or
    /// the same network back with the connection down.
    Redial,
    /// The same network, the connection up: keep it, and ask the paths
    /// this side needs once, which also shows soon whether it still
    /// carries anything.
    Ask,
}

/// What a rotation does about a network of `kind`, `changed` when it is
/// another network than the one before, with its connection `up` or not.
pub fn evaluate(kind: NetworkKind, changed: bool, up: bool) -> Evaluation {
    match (kind, changed, up) {
        (NetworkKind::None, _, _) => Evaluation::Pause,
        (_, true, _) | (_, false, false) => Evaluation::Redial,
        (_, false, true) => Evaluation::Ask,
    }
}

/// The changes the platform says in a burst, folded into one evaluation:
/// a phone that leaves a WLAN says lost, then the mobile network
/// available, then its capabilities, within a second. The evaluation runs
/// once the changes went quiet for [`Timing::network_settle_ms`], and at
/// the latest [`Timing::network_settle_max_ms`] after the first, so a
/// network that flaps on and on is still evaluated. `changed` is kept if
/// any change of the burst said it; the kind is the last one's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settle {
    first_ms: Option<u64>,
    last_ms: u64,
    kind: NetworkKind,
    changed: bool,
}

impl Settle {
    /// A change said at `now_ms`.
    pub fn note(&mut self, now_ms: u64, kind: NetworkKind, changed: bool) {
        if self.first_ms.is_none() {
            self.first_ms = Some(now_ms);
            self.changed = false;
        }
        self.last_ms = now_ms;
        self.kind = kind;
        self.changed |= changed;
    }

    /// When the burst is due for its evaluation, if one is pending.
    pub fn due_at(&self, timing: &Timing) -> Option<u64> {
        self.first_ms.map(|first| {
            self.last_ms
                .saturating_add(timing.network_settle_ms)
                .min(first.saturating_add(timing.network_settle_max_ms))
        })
    }

    /// The burst's kind and `changed`, once it is due at `now_ms`; the
    /// settle is empty again then.
    pub fn take(&mut self, now_ms: u64, timing: &Timing) -> Option<(NetworkKind, bool)> {
        match self.due_at(timing) {
            Some(at) if now_ms >= at => {
                self.first_ms = None;
                Some((self.kind, self.changed))
            }
            _ => None,
        }
    }
}

/// The ordered list's state: which entry carries the traffic, the backoff
/// to the next, and when the first is tried again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rotation {
    len: usize,
    active: usize,
    timing: Timing,
    backoff_ms: u64,
    return_wait_ms: u64,
    return_at_ms: Option<u64>,
}

impl Rotation {
    /// A rotation over `len` entries, the first active. `len` 0 is a list
    /// of one, so the indices stay in range; [`run`] never builds one.
    pub fn new(len: usize, timing: Timing) -> Self {
        Self {
            len: len.max(1),
            active: 0,
            timing,
            backoff_ms: timing.backoff_first_ms,
            return_wait_ms: timing.return_first_ms,
            return_at_ms: None,
        }
    }

    /// The entry that carries the traffic, or is being dialled.
    pub fn active(&self) -> usize {
        self.active
    }

    /// The active entry came up at `now_ms`: the backoff starts over, and a
    /// later entry schedules the first's next try.
    pub fn up(&mut self, now_ms: u64) {
        self.backoff_ms = self.timing.backoff_first_ms;
        self.return_at_ms = match self.active {
            0 => None,
            _ => Some(now_ms.saturating_add(self.return_wait_ms)),
        };
    }

    /// The active entry did not come up, or went down and stayed down: move
    /// to the next, and how long to wait before dialling it.
    pub fn failed(&mut self) -> u64 {
        let wait = self.backoff_ms;
        self.backoff_ms = self
            .backoff_ms
            .saturating_mul(2)
            .min(self.timing.backoff_max_ms);
        self.active = (self.active + 1) % self.len;
        if self.active == 0 {
            self.return_at_ms = None;
            self.return_wait_ms = self.timing.return_first_ms;
        }
        wait
    }

    /// Whether the first entry is due for a try at `now_ms`.
    pub fn return_due(&self, now_ms: u64) -> bool {
        self.active != 0 && self.return_at_ms.is_some_and(|at| now_ms >= at)
    }

    /// When the first entry's next try is due, if one is.
    pub fn return_at(&self) -> Option<u64> {
        self.return_at_ms.filter(|_| self.active != 0)
    }

    /// The try of the first entry at `now_ms` found it down: the next try
    /// after twice the wait.
    pub fn return_refused(&mut self, now_ms: u64) {
        self.return_wait_ms = self
            .return_wait_ms
            .saturating_mul(2)
            .min(self.timing.return_max_ms);
        self.return_at_ms = Some(now_ms.saturating_add(self.return_wait_ms));
    }

    /// A network change asked for a fresh dial ([`Evaluation::Redial`],
    /// [`Evaluation::Pause`]): the first entry again, with the first
    /// backoff and the first return wait, as at the start.
    pub fn restart(&mut self) {
        *self = Self::new(self.len, self.timing);
    }

    /// The first entry answered: the traffic goes back to it.
    pub fn returned(&mut self) {
        self.active = 0;
        self.return_at_ms = None;
        self.return_wait_ms = self.timing.return_first_ms;
        self.backoff_ms = self.timing.backoff_first_ms;
    }
}

pub use driver::{run, Dial, Network, Rendezvous, Side, State};

mod driver {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use leviculum_core::transport::InterfaceId;
    use leviculum_core::DestinationHash;
    use leviculum_std::driver::TapEvent;
    use leviculum_std::ReticulumNode;

    use super::{evaluate, Evaluation, NetworkKind, Rotation, Settle, Timing};

    /// Which end dials: the name its interface gets, and whether it asks
    /// for a path when the entry comes up.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Side {
        /// The phone, on the entries' access point ports, asking for the
        /// home destination's path on every up.
        Phone(DestinationHash),
        /// The mailbox, on the entries' ordinary ports, as a peer: it asks
        /// for nothing and announces nothing of its own. The node announces
        /// every destination it announced before on each interface that
        /// registers or reconnects (`handle_interface_up`, Block D), so the
        /// entry it moved to hears the delivery and the home destination
        /// once each. A third announce here was one too many: a stock Python
        /// entry holds the third announce of a new interface's burst, and a
        /// sender beside it learnt a mailbox whose delivery announce came
        /// third only half a minute later (1274,
        /// `reference/Reticulum/RNS/Transport.py:1705`).
        Peer,
    }

    /// The platform's newest word about the network, and how many of all
    /// its words said "another network". A watch keeps only the newest
    /// value, so a burst said before a rotation looks (lost, available,
    /// capabilities, inside a millisecond) reaches it as its last word
    /// alone; the count is what tells it that an earlier one of the burst
    /// changed the network (1334).
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    struct Note {
        kind: NetworkKind,
        changes: u64,
    }

    /// A rotation's ear on [`Network`]: what it heard last of the count.
    struct Heard {
        notes: tokio::sync::watch::Receiver<Note>,
        changes: u64,
    }

    impl Heard {
        /// The newest kind, and whether any word since the last take said
        /// "another network".
        fn take(&mut self) -> (NetworkKind, bool) {
            let note = *self.notes.borrow_and_update();
            let changed = note.changes != self.changes;
            self.changes = note.changes;
            (note.kind, changed)
        }
    }

    /// The platform's word on the phone's network (1298), handed to every
    /// rotation of the process: the phone's leaf and its own mailbox hear
    /// the same change. It also counts the rotations that re-dial for a
    /// change and are not up again yet, which is what the app's line says
    /// ("Reconnecting…" while there is one), and the re-dials begun, so a
    /// re-dial that is up again before the line was looked at still reads
    /// as one; each start and end of one wakes whoever waits in
    /// [`Network::turned`] (1334).
    #[derive(Debug, Clone)]
    pub struct Network {
        notes: Arc<tokio::sync::watch::Sender<Note>>,
        owed: Arc<AtomicUsize>,
        redials: Arc<AtomicUsize>,
        turned: Arc<tokio::sync::Notify>,
    }

    impl Default for Network {
        fn default() -> Self {
            Self {
                notes: Arc::new(tokio::sync::watch::Sender::new(Note::default())),
                owed: Arc::new(AtomicUsize::new(0)),
                redials: Arc::new(AtomicUsize::new(0)),
                turned: Arc::new(tokio::sync::Notify::new()),
            }
        }
    }

    impl Network {
        /// A network that nobody has said anything about yet.
        pub fn new() -> Self {
            Self::default()
        }

        /// The phone's network is `kind` now, `changed` when it is another
        /// network than before (another WLAN, WLAN to mobile data, back
        /// from none). Every rotation evaluates it once the burst settled.
        pub fn say(&self, kind: NetworkKind, changed: bool) {
            self.notes.send_modify(|note| {
                note.kind = kind;
                if changed {
                    note.changes = note.changes.wrapping_add(1);
                }
            });
        }

        /// Whether a rotation re-dials for a change and is not up yet.
        pub fn reconnecting(&self) -> bool {
            self.owed.load(Ordering::Relaxed) > 0
        }

        /// How many re-dials for a change have begun, wrapping: a count
        /// that moved since the last look is a re-dial even when it is up
        /// again already (a loopback entry is up within a millisecond).
        pub fn redials(&self) -> usize {
            self.redials.load(Ordering::Relaxed)
        }

        /// Until a re-dial for a change begins or ends. One that happened
        /// while nobody waited is not lost: the next wait returns at once.
        pub async fn turned(&self) {
            self.turned.notified().await;
        }

        /// A rotation's share of the count, none owed yet. Not the notes:
        /// the rotation holding a sender would keep its own channel open.
        fn owed(&self) -> Owed {
            Owed {
                count: Arc::clone(&self.owed),
                redials: Arc::clone(&self.redials),
                turned: Arc::clone(&self.turned),
                owed: false,
            }
        }

        fn subscribe(&self) -> Heard {
            let mut notes = self.notes.subscribe();
            // What was said before the rotation started is no change to it.
            let changes = notes.borrow_and_update().changes;
            Heard { notes, changes }
        }
    }

    /// One rotation's share of [`Network::reconnecting`], given back when
    /// it is up again or its task ends.
    struct Owed {
        count: Arc<AtomicUsize>,
        redials: Arc<AtomicUsize>,
        turned: Arc<tokio::sync::Notify>,
        owed: bool,
    }

    impl Owed {
        /// `true` is said once per re-dial for a change, and each one
        /// counts, also one that comes while the last is not up yet (no
        /// network, then WLAN back); the share is taken only once.
        fn set(&mut self, owed: bool) {
            match (self.owed, owed) {
                (_, true) => {
                    self.redials.fetch_add(1, Ordering::Relaxed);
                    if !self.owed {
                        self.count.fetch_add(1, Ordering::Relaxed);
                    }
                    self.turned.notify_one();
                }
                (true, false) => {
                    self.count.fetch_sub(1, Ordering::Relaxed);
                    self.turned.notify_one();
                }
                _ => {}
            }
            self.owed = owed;
        }
    }

    impl Drop for Owed {
        fn drop(&mut self) {
            self.set(false);
        }
    }

    /// What [`run`] needs: the node, the entries as `host:port`, already
    /// on the ports this side dials, the side, the TCP keepalive, the
    /// timing, and the network word it listens to.
    pub struct Dial {
        pub node: Arc<ReticulumNode>,
        pub entries: Vec<(String, u16)>,
        pub side: Side,
        pub tcp: crate::config::TcpLiveness,
        pub timing: Timing,
        pub network: Network,
    }

    /// Where the rotation stands: the entry it dials or holds, as
    /// `host:port`, and whether its connection is up.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct State {
        pub entry: String,
        pub up: bool,
    }

    /// The rotation's task: dropping it stops the dialling, and the entry's
    /// interface goes with the node.
    pub struct Rendezvous {
        task: Option<tokio::task::JoinHandle<()>>,
        state: Arc<std::sync::Mutex<State>>,
    }

    impl Rendezvous {
        /// Start dialling on the current runtime.
        pub fn start(dial: Dial) -> Self {
            let state = Arc::new(std::sync::Mutex::new(State::default()));
            Self {
                task: Some(tokio::spawn(run(dial, Arc::clone(&state)))),
                state,
            }
        }

        /// Where the rotation stands now.
        pub fn state(&self) -> State {
            self.state
                .lock()
                .map(|state| state.clone())
                .unwrap_or_default()
        }

        /// Stop dialling and wait until the task let go of the node, so a
        /// stop of the node that follows finds it unshared.
        pub async fn stop(mut self) {
            if let Some(task) = self.task.take() {
                task.abort();
                let _ = task.await;
            }
        }
    }

    impl Drop for Rendezvous {
        fn drop(&mut self) {
            if let Some(task) = &self.task {
                task.abort();
            }
        }
    }

    /// The one line the rotation says per change, catalogued
    /// (`HOME_RENDEZVOUS`, `leviculum-std/src/event_log.rs`).
    fn said(host: &str, port: u16, state: &str, index: usize) {
        tracing::info!(
            event = "HOME_RENDEZVOUS",
            host = %host,
            port = port,
            state = state,
            entry = index,
        );
    }

    fn online(node: &ReticulumNode, ids: &[InterfaceId]) -> bool {
        node.interface_stats()
            .iter()
            .any(|i| ids.contains(&i.interface_id) && i.online)
    }

    fn spawn(node: &ReticulumNode, dial: &Dial, index: usize) -> Vec<InterfaceId> {
        let (host, port) = &dial.entries[index];
        let name = format!("Rendezvous {host}:{port}");
        let section = crate::engine::tcp_section(
            leviculum_std::config::InterfaceConfig {
                name,
                interface_type: "TCPClientInterface".to_string(),
                target_host: Some(host.clone()),
                target_port: Some(*port),
                ..Default::default()
            },
            dial.tcp,
        );
        match node.spawn_interface(section) {
            Ok(ids) => ids,
            Err(error) => {
                tracing::warn!("HOME rendezvous {host}:{port}: no interface: {error}");
                Vec::new()
            }
        }
    }

    fn drop_all(node: &ReticulumNode, ids: &mut Vec<InterfaceId>) {
        for id in ids.drain(..) {
            let _ = node.remove_interface(id);
        }
    }

    /// Whether `host:port` takes a TCP connection within five seconds: the
    /// first entry's try while a later one carries the traffic. The
    /// connection is closed at once; a server takes it as a client that
    /// left.
    async fn answers(host: &str, port: u16) -> bool {
        matches!(
            tokio::time::timeout(
                Duration::from_secs(5),
                tokio::net::TcpStream::connect((host, port))
            )
            .await,
            Ok(Ok(_))
        )
    }

    /// The phone's path request on an up and on [`Evaluation::Ask`]
    /// (rule 2): whether a path is held or not, one learned through an
    /// entry that is gone leads nowhere.
    async fn ask_home(node: &ReticulumNode, home: &DestinationHash, via: &str) {
        tracing::info!(
            event = "HOME_PATH_REQUEST",
            dst = %crate::address::to_hex(home.as_bytes()),
            via = %via,
        );
        if let Err(error) = node.request_path(home).await {
            tracing::warn!("HOME path request: {error}");
        }
    }

    /// Dial the entries one at a time until the task is dropped, saying
    /// where it stands in `state`.
    pub async fn run(dial: Dial, state: Arc<std::sync::Mutex<State>>) {
        if dial.entries.is_empty() {
            return;
        }
        let tell = |index: usize, up: bool| {
            if let Ok(mut state) = state.lock() {
                let (host, port) = &dial.entries[index];
                // As the list writes it: an IPv6 host in brackets.
                let entry = match host.contains(':') {
                    true => format!("[{host}]:{port}"),
                    false => format!("{host}:{port}"),
                };
                *state = State { entry, up };
            }
        };
        let origin = tokio::time::Instant::now();
        let now_ms = || u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut tap = dial.node.subscribe_events();
        let mut notes = dial.network.subscribe();
        let mut notes_open = true;
        let mut settle = Settle::default();
        let mut owed = dial.network.owed();
        let mut rotation = Rotation::new(dial.entries.len(), dial.timing);
        let mut ids = spawn(&dial.node, &dial, rotation.active());
        tell(rotation.active(), false);
        let mut up = false;
        // Down since, while a dial is in progress (`None` while up, while
        // waiting out a backoff, and while there is no network).
        let mut down_since: Option<u64> = Some(now_ms());
        // When the next entry is dialled, after a failure's backoff.
        let mut dial_at: Option<u64> = None;
        loop {
            let now = now_ms();
            let mut look = match up {
                true => dial.timing.up_look_ms,
                false => dial.timing.dial_look_ms,
            };
            for at in [rotation.return_at(), settle.due_at(&dial.timing), dial_at]
                .into_iter()
                .flatten()
            {
                look = look.min(at.saturating_sub(now).max(1));
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(look)) => {}
                event = tap.recv() => match event {
                    // Something that may mean the connection died: look now.
                    Some(TapEvent::Event(
                        leviculum_core::node::NodeEvent::InterfaceDown(_)
                        | leviculum_core::node::NodeEvent::LinkClosed { .. }
                        | leviculum_core::node::NodeEvent::RequestTimedOut { .. },
                    ) | TapEvent::Lagged(_)) => {}
                    Some(_) => continue,
                    None => return,
                },
                said = notes.notes.changed(), if notes_open => match said {
                    Ok(()) => {
                        let (kind, changed) = notes.take();
                        settle.note(now_ms(), kind, changed);
                        continue;
                    }
                    // Every handle is gone: nobody says a change again.
                    Err(_) => {
                        notes_open = false;
                        continue;
                    }
                },
            }
            let now = now_ms();
            if let Some((kind, changed)) = settle.take(now, &dial.timing) {
                let action = evaluate(kind, changed, up);
                let index = rotation.active();
                tracing::info!(
                    event = "HOME_NETWORK",
                    kind = kind.word(),
                    changed = changed,
                    action = match action {
                        Evaluation::Pause => "pause",
                        Evaluation::Redial => "redial",
                        Evaluation::Ask => "ask",
                    },
                    entry = index,
                );
                match action {
                    Evaluation::Pause | Evaluation::Redial => {
                        if up {
                            let (host, port) = &dial.entries[index];
                            said(host, *port, "down", index);
                        }
                        owed.set(true);
                        drop_all(&dial.node, &mut ids);
                        rotation.restart();
                        up = false;
                        dial_at = None;
                        tell(0, false);
                        if action == Evaluation::Redial {
                            ids = spawn(&dial.node, &dial, 0);
                            down_since = Some(now);
                        } else {
                            down_since = None;
                        }
                    }
                    Evaluation::Ask => {
                        if let Side::Phone(home) = dial.side {
                            let (host, port) = &dial.entries[index];
                            ask_home(&dial.node, &home, &format!("{host}:{port}")).await;
                        }
                    }
                }
                continue;
            }
            if dial_at.is_some_and(|at| now >= at) {
                dial_at = None;
                ids = spawn(&dial.node, &dial, rotation.active());
                down_since = Some(now);
            }
            let index = rotation.active();
            let (host, port) = dial.entries[index].clone();
            let is_online = !ids.is_empty() && online(&dial.node, &ids);
            match (up, is_online) {
                (false, true) => {
                    up = true;
                    down_since = None;
                    owed.set(false);
                    rotation.up(now);
                    tell(index, true);
                    said(&host, port, "up", index);
                    if let Side::Phone(home) = dial.side {
                        ask_home(&dial.node, &home, &format!("{host}:{port}")).await;
                    }
                }
                (true, false) => {
                    up = false;
                    down_since = Some(now);
                    tell(index, false);
                    said(&host, port, "down", index);
                }
                _ => {}
            }
            if !up
                && down_since
                    .is_some_and(|since| now.saturating_sub(since) >= dial.timing.dial_grace_ms)
            {
                drop_all(&dial.node, &mut ids);
                let wait = rotation.failed();
                tracing::info!(
                    "HOME rendezvous {host}:{port} did not come up; the next, {}:{}, in {} s",
                    dial.entries[rotation.active()].0,
                    dial.entries[rotation.active()].1,
                    wait / 1000
                );
                tell(rotation.active(), false);
                // Waited out in the loop, so a network change cuts it short.
                down_since = None;
                dial_at = Some(now.saturating_add(wait));
                continue;
            }
            if up && rotation.return_due(now_ms()) {
                let (first_host, first_port) = dial.entries[0].clone();
                if answers(&first_host, first_port).await {
                    tracing::info!("HOME rendezvous {first_host}:{first_port} answers again");
                    drop_all(&dial.node, &mut ids);
                    said(&host, port, "down", index);
                    rotation.returned();
                    tell(0, false);
                    ids = spawn(&dial.node, &dial, 0);
                    up = false;
                    down_since = Some(now_ms());
                } else {
                    rotation.return_refused(now_ms());
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The platform says lost, available and its capabilities inside a
        /// millisecond; the rotation's task, busy or not yet scheduled,
        /// looks only after the third (1334). The burst said "another
        /// network" twice: the rotation must hear that, not just the last
        /// word's "the same one".
        #[test]
        fn a_burst_said_before_the_rotation_looks_keeps_its_change() {
            let network = Network::new();
            let mut notes = network.subscribe();
            network.say(NetworkKind::None, true);
            network.say(NetworkKind::Mobile, true);
            network.say(NetworkKind::Mobile, false);
            assert_eq!(notes.take(), (NetworkKind::Mobile, true));
            // Heard once: the same network said again is no change.
            network.say(NetworkKind::Mobile, false);
            assert_eq!(notes.take(), (NetworkKind::Mobile, false));
        }

        /// A re-dial that begins and ends before anyone looks still counts
        /// and still wakes the waiter (1334): the session's pump, asleep
        /// for the keepalive's ceiling, otherwise saw "not reconnecting"
        /// before and after and never said the line.
        #[tokio::test]
        async fn a_redial_up_again_before_the_look_still_counts_and_wakes() {
            let network = Network::new();
            let before = network.redials();
            let mut owed = network.owed();
            owed.set(true);
            assert!(network.reconnecting());
            owed.set(false);
            assert!(!network.reconnecting());
            assert_ne!(network.redials(), before);
            tokio::time::timeout(Duration::from_secs(1), network.turned())
                .await
                .expect("the re-dial woke the waiter");
            // Up again without a re-dial: no count, and dropped not owed,
            // no share given back twice.
            let counted = network.redials();
            owed.set(false);
            drop(owed);
            assert_eq!(network.redials(), counted);
            assert!(!network.reconnecting());
        }

        /// A change said before the rotation subscribed is not one to it.
        #[test]
        fn a_change_said_before_the_rotation_started_is_none_to_it() {
            let network = Network::new();
            network.say(NetworkKind::Wifi, true);
            let mut notes = network.subscribe();
            assert!(!notes.notes.has_changed().unwrap_or(true));
            network.say(NetworkKind::Wifi, false);
            assert_eq!(notes.take(), (NetworkKind::Wifi, false));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing() -> Timing {
        Timing {
            dial_grace_ms: 1_000,
            backoff_first_ms: 100,
            backoff_max_ms: 800,
            return_first_ms: 10_000,
            return_max_ms: 40_000,
            up_look_ms: 1_000,
            dial_look_ms: 100,
            network_settle_ms: 500,
            network_settle_max_ms: 2_000,
        }
    }

    #[test]
    fn a_failure_moves_to_the_next_with_a_growing_backoff_and_an_up_resets_it() {
        let mut rotation = Rotation::new(3, timing());
        assert_eq!(rotation.active(), 0);
        assert_eq!(rotation.failed(), 100);
        assert_eq!(rotation.active(), 1);
        assert_eq!(rotation.failed(), 200);
        assert_eq!(rotation.active(), 2);
        assert_eq!(rotation.failed(), 400);
        assert_eq!(rotation.active(), 0, "the list wraps");
        assert_eq!(rotation.failed(), 800);
        assert_eq!(rotation.failed(), 800, "capped");
        rotation.up(0);
        assert_eq!(rotation.failed(), 100, "an up starts the backoff over");
    }

    #[test]
    fn a_later_entry_tries_the_first_again_and_returns_when_it_answers() {
        let mut rotation = Rotation::new(2, timing());
        rotation.up(0);
        assert_eq!(
            rotation.return_at(),
            None,
            "the first entry has nothing to return to"
        );
        rotation.failed();
        rotation.up(5_000);
        assert_eq!(rotation.return_at(), Some(15_000));
        assert!(!rotation.return_due(14_999));
        assert!(rotation.return_due(15_000));
        rotation.return_refused(15_000);
        assert_eq!(rotation.return_at(), Some(35_000), "the wait doubles");
        rotation.return_refused(35_000);
        assert_eq!(rotation.return_at(), Some(75_000), "and is capped");
        rotation.returned();
        assert_eq!(rotation.active(), 0);
        assert_eq!(rotation.return_at(), None);
        // A later failure over to the second starts the return wait over.
        rotation.failed();
        rotation.up(100_000);
        assert_eq!(rotation.return_at(), Some(110_000));
    }

    #[test]
    fn a_network_change_is_evaluated_by_kind_change_and_connection() {
        use Evaluation::*;
        use NetworkKind::*;
        // No network pauses whatever was before.
        for (changed, up) in [(true, true), (true, false), (false, true), (false, false)] {
            assert_eq!(evaluate(None, changed, up), Pause);
        }
        // Another network re-dials, up or not: the old socket leads nowhere.
        for kind in [Wifi, Mobile, Other] {
            assert_eq!(evaluate(kind, true, true), Redial);
            assert_eq!(evaluate(kind, true, false), Redial);
            // The same network back (after Doze): a down connection is
            // dialled at once, an up one is kept and asked.
            assert_eq!(evaluate(kind, false, false), Redial);
            assert_eq!(evaluate(kind, false, true), Ask);
        }
        assert_eq!(NetworkKind::from_word("mobile"), Mobile);
        assert_eq!(NetworkKind::from_word("ethernet"), Other);
        for kind in [None, Wifi, Mobile, Other] {
            assert_eq!(NetworkKind::from_word(kind.word()), kind);
        }
    }

    #[test]
    fn a_burst_of_changes_is_one_evaluation_and_a_flapping_one_still_comes() {
        let timing = timing();
        let mut settle = Settle::default();
        assert_eq!(settle.due_at(&timing), Option::None);
        // Leaving a WLAN: lost, mobile available, its capabilities.
        settle.note(1_000, NetworkKind::None, true);
        settle.note(1_200, NetworkKind::Mobile, true);
        settle.note(1_300, NetworkKind::Mobile, false);
        assert_eq!(settle.due_at(&timing), Some(1_800));
        assert_eq!(settle.take(1_799, &timing), Option::None);
        assert_eq!(
            settle.take(1_800, &timing),
            Some((NetworkKind::Mobile, true)),
            "the last kind, and changed if any said it"
        );
        assert_eq!(settle.take(10_000, &timing), Option::None, "once");
        // A network that flaps every 400 ms is evaluated after the cap.
        let mut at = 20_000;
        while at < 23_000 {
            settle.note(at, NetworkKind::Wifi, false);
            at += 400;
        }
        assert_eq!(settle.due_at(&timing), Some(22_000));
        assert_eq!(
            settle.take(22_000, &timing),
            Some((NetworkKind::Wifi, false))
        );
    }

    #[test]
    fn a_restart_dials_the_first_entry_with_the_first_backoff() {
        let mut rotation = Rotation::new(3, timing());
        rotation.failed();
        rotation.failed();
        rotation.up(5_000);
        assert_eq!(rotation.active(), 2);
        assert!(rotation.return_at().is_some());
        rotation.restart();
        assert_eq!(rotation.active(), 0);
        assert_eq!(rotation.return_at(), Option::None);
        assert_eq!(rotation.failed(), 100, "the backoff starts over");
    }

    #[test]
    fn the_shipped_list_is_our_access_point_port_then_the_ordinary_one() {
        assert_eq!(
            default_list(),
            ["leviculum.network:4343", "leviculum.network:4242"]
        );
        assert_eq!(peer_list(&default_list()), ["leviculum.network:4242"]);
        assert_eq!(
            peer_list(&[
                "home.example:4243".to_string(),
                "rns.example:5000".to_string(),
                "[fd00::1]:4343".to_string(),
            ]),
            ["home.example:4242", "rns.example:5000", "[fd00::1]:4242"]
        );
    }

    #[test]
    fn a_mailboxs_own_port_goes_first_and_only_once() {
        let rest = default_list();
        assert_eq!(
            with_first("home.example:4243", &rest),
            [
                "home.example:4243",
                "leviculum.network:4343",
                "leviculum.network:4242"
            ]
        );
        assert_eq!(with_first("leviculum.network:4343", &rest), rest);
    }

    #[test]
    fn the_config_decides_and_no_key_is_the_shipped_list() {
        use crate::config::Config;
        let none = Config::default();
        assert_eq!(reach(&none, &[]), Reach::List(default_list()));
        let carried = ["home.example:4243".to_string()];
        assert_eq!(reach(&none, &carried), Reach::List(carried.to_vec()));
        let listed = Config {
            rendezvous: Some(alloc::vec!["a.example:1".to_string()]),
            ..Config::default()
        };
        assert_eq!(
            reach(&listed, &carried),
            Reach::List(alloc::vec!["a.example:1".to_string()])
        );
        let mesh_only = Config {
            rendezvous_dial: Some(false),
            ..listed.clone()
        };
        assert_eq!(reach(&mesh_only, &carried), Reach::MeshOnly);
        let fixed = Config {
            home_address: Some("10.0.0.2:4242".to_string()),
            ..mesh_only
        };
        assert_eq!(
            reach(&fixed, &carried),
            Reach::Fixed("10.0.0.2:4242".to_string())
        );
    }
}
