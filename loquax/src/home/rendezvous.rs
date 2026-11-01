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
//! The rotation itself is plain logic on a millisecond clock and has no I/O;
//! [`run`] drives it over the node.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

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

    /// The first entry answered: the traffic goes back to it.
    pub fn returned(&mut self) {
        self.active = 0;
        self.return_at_ms = None;
        self.return_wait_ms = self.timing.return_first_ms;
        self.backoff_ms = self.timing.backoff_first_ms;
    }
}

pub use driver::{run, Dial, Rendezvous, Side, State};

mod driver {
    use std::sync::Arc;
    use std::time::Duration;

    use leviculum_core::transport::InterfaceId;
    use leviculum_core::DestinationHash;
    use leviculum_std::driver::TapEvent;
    use leviculum_std::ReticulumNode;

    use super::{Rotation, Timing};

    /// Which end dials: the name its interface gets, and whether it asks
    /// for a path when the entry comes up.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Side {
        /// The phone, on the entries' access point ports, asking for the
        /// home destination's path on every up.
        Phone(DestinationHash),
        /// The mailbox, on the entries' ordinary ports, as a peer: it asks
        /// for nothing, its destinations are its own. On every up it
        /// announces the home destination it carries, so the entry it moved
        /// to holds the path before the first phone asks; `None` for a node
        /// with none.
        Peer(Option<DestinationHash>),
    }

    /// What [`run`] needs: the node, the entries as `host:port`, already
    /// on the ports this side dials, the side, the TCP keepalive and the
    /// timing.
    pub struct Dial {
        pub node: Arc<ReticulumNode>,
        pub entries: Vec<(String, u16)>,
        pub side: Side,
        pub tcp: crate::config::TcpLiveness,
        pub timing: Timing,
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
        let mut rotation = Rotation::new(dial.entries.len(), dial.timing);
        let mut ids = spawn(&dial.node, &dial, rotation.active());
        tell(rotation.active(), false);
        // Up since, or down since (`None` until the first up of this dial).
        let mut up = false;
        let mut down_since: Option<u64> = Some(now_ms());
        loop {
            let look = match up {
                true => dial.timing.up_look_ms,
                false => dial.timing.dial_look_ms,
            };
            let look = match rotation.return_at() {
                Some(at) => look.min(at.saturating_sub(now_ms()).max(1)),
                None => look,
            };
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
            }
            let index = rotation.active();
            let (host, port) = dial.entries[index].clone();
            let now = now_ms();
            let is_online = online(&dial.node, &ids);
            match (up, is_online) {
                (false, true) => {
                    up = true;
                    down_since = None;
                    rotation.up(now);
                    tell(index, true);
                    said(&host, port, "up", index);
                    if let Side::Peer(Some(home)) = dial.side {
                        if let Err(error) = dial.node.announce_destination(&home, None).await {
                            tracing::warn!("HOME announce on the rendezvous: {error}");
                        }
                    }
                    if let Side::Phone(home) = dial.side {
                        // Rule 2: the path, asked for at once on every up,
                        // whether a path is held or not; one learned
                        // through an entry that is gone leads nowhere.
                        tracing::info!(
                            event = "HOME_PATH_REQUEST",
                            dst = %crate::address::to_hex(home.as_bytes()),
                            via = %format!("{host}:{port}"),
                        );
                        if let Err(error) = dial.node.request_path(&home).await {
                            tracing::warn!("HOME path request: {error}");
                        }
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
                for id in ids.drain(..) {
                    let _ = dial.node.remove_interface(id);
                }
                let wait = rotation.failed();
                tracing::info!(
                    "HOME rendezvous {host}:{port} did not come up; the next, {}:{}, in {} s",
                    dial.entries[rotation.active()].0,
                    dial.entries[rotation.active()].1,
                    wait / 1000
                );
                tell(rotation.active(), false);
                tokio::time::sleep(Duration::from_millis(wait)).await;
                ids = spawn(&dial.node, &dial, rotation.active());
                down_since = Some(now_ms());
                continue;
            }
            if up && rotation.return_due(now_ms()) {
                let (first_host, first_port) = dial.entries[0].clone();
                if answers(&first_host, first_port).await {
                    tracing::info!("HOME rendezvous {first_host}:{first_port} answers again");
                    for id in ids.drain(..) {
                        let _ = dial.node.remove_interface(id);
                    }
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
