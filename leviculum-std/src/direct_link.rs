//! The driver's half of the direct-link upgrade (+ciris, leviculum#70).
//!
//! The node decides (see `leviculum_core::node` and
//! [`leviculum_core::direct_link`]); this module owns the sockets: it probes
//! the facilitator, punches, hands a punched socket to the event loop as a
//! [`crate::interfaces::direct_udp`] interface, and runs this node's own
//! facilitator when one is configured.
//!
//! Config, in the `[reticulum]` section (key names shared with rns-rs):
//!
//! ```text
//! probe_port = 4343                   # run a facilitator on this UDP port
//! probe_addr = rns.example.org:4343   # the facilitator this node proposes with
//! probe_protocol = rnsp               # or `stun`, to use a public STUN server
//! direct_connect_policy = reject      # | accept_all | identified_only | ask_host
//! ```

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use leviculum_core::direct_link::wire::{self, PunchKind, SessionId};
use leviculum_core::direct_link::ProbeProtocol;
use leviculum_core::node::{DirectLinkJob, DirectLinkPolicy};
use leviculum_core::transport::InterfaceId;
use rand_core::RngCore;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::error::Error;
use crate::interfaces::direct_udp::{spawn_direct_udp_interface, DirectUdpTiming};
use crate::interfaces::InterfaceHandle;

/// Direct-link settings, from config or the builder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DirectLinkSettings {
    /// How this node answers a peer's upgrade request.
    pub policy: DirectLinkPolicy,
    /// The facilitator this node probes when it proposes, as `host:port`.
    /// Resolved at each proposal, so a DNS change is picked up.
    pub facilitator: Option<String>,
    /// How to probe `facilitator`.
    pub protocol: ProbeProtocol,
    /// Run a facilitator on this UDP port (all IPv4 addresses).
    pub facilitator_port: Option<u16>,
    /// +ciris (review item 6): a second observer (`host:port`), probed from
    /// the same session socket to classify this node's NAT. Unset: the node
    /// advertises neither predicted ports nor "b". No default: the node never
    /// asks a party it was not told to.
    pub facilitator2: Option<String>,
    /// How to probe `facilitator2`.
    pub protocol2: ProbeProtocol,
}

impl DirectLinkSettings {
    /// Read the `[reticulum]` keys. Unknown policy or protocol words are a
    /// config error rather than a silent default: getting either wrong
    /// changes who learns this node's public address.
    pub fn from_config(config: &crate::config::ReticulumConfig) -> Result<Self, Error> {
        if let Some(bad) = &config.direct_link.invalid_probe_port {
            return Err(Error::Config(format!(
                "probe_port must be a UDP port number, not {bad:?}"
            )));
        }
        let policy = match config
            .direct_link
            .direct_connect_policy
            .as_deref()
            .map(str::trim)
        {
            None | Some("") | Some("reject") => DirectLinkPolicy::Reject,
            Some("accept_all") => DirectLinkPolicy::AcceptAll,
            Some("identified_only") => DirectLinkPolicy::IdentifiedOnly,
            Some("ask_host") => DirectLinkPolicy::AskHost,
            Some(other) => {
                return Err(Error::Config(format!(
                    "direct_connect_policy must be reject, accept_all, identified_only or \
                     ask_host, not {other:?}"
                )))
            }
        };
        let protocol = match config.direct_link.probe_protocol.as_deref().map(str::trim) {
            None | Some("") | Some("rnsp") => ProbeProtocol::Rnsp,
            Some("stun") => ProbeProtocol::Stun,
            Some(other) => {
                return Err(Error::Config(format!(
                    "probe_protocol must be rnsp or stun, not {other:?}"
                )))
            }
        };
        Ok(DirectLinkSettings {
            policy,
            facilitator: config
                .direct_link
                .probe_addr
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            protocol,
            facilitator_port: config.direct_link.probe_port,
            facilitator2: config
                .direct_link
                .probe_addr2
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            protocol2: match config.direct_link.probe_protocol2.as_deref().map(str::trim) {
                None | Some("") | Some("rnsp") => ProbeProtocol::Rnsp,
                Some("stun") => ProbeProtocol::Stun,
                Some(other) => {
                    return Err(Error::Config(format!(
                        "probe_protocol2 must be rnsp or stun, not {other:?}"
                    )))
                }
            },
        })
    }

    /// Whether the node can ever hold a session: it proposes (has a
    /// facilitator) or accepts (policy other than reject). When false the
    /// event loop never looks for direct-link work.
    pub fn active(&self) -> bool {
        self.facilitator.is_some() || self.policy != DirectLinkPolicy::Reject
    }
}

/// The `[reticulum]` keys of the direct-link upgrade, as read. Flattened into
/// [`crate::config::ReticulumConfig`], so in TOML they sit beside the other
/// `[reticulum]` keys; defined here so the fork adds as few lines as it can to
/// upstream's config file. Validated by [`DirectLinkSettings::from_config`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DirectLinkKeys {
    /// Run a direct-link facilitator on this UDP port, answering RNSP and
    /// STUN probes. `None` runs none.
    #[serde(default)]
    pub probe_port: Option<u16>,
    /// The facilitator (`host:port`) this node probes when it proposes a
    /// direct link. `None` means it cannot propose.
    #[serde(default)]
    pub probe_addr: Option<String>,
    /// How to probe: `rnsp` (default) or `stun`.
    #[serde(default)]
    pub probe_protocol: Option<String>,
    /// Which peers' direct-link requests to accept: `reject` (default),
    /// `accept_all` or `identified_only`.
    #[serde(default)]
    pub direct_connect_policy: Option<String>,
    /// +ciris: a second observer, probed from the same session socket to
    /// classify this node's NAT (see `DirectLinkSettings::facilitator2`).
    #[serde(default)]
    pub probe_addr2: Option<String>,
    /// How to probe `probe_addr2`: `rnsp` (default) or `stun`.
    #[serde(default)]
    pub probe_protocol2: Option<String>,
    /// An INI `probe_port` that did not parse, kept so validation refuses it
    /// (TOML rejects a bad value while parsing).
    #[serde(skip)]
    pub invalid_probe_port: Option<String>,
}

/// Read a `[reticulum]` INI key of the direct-link upgrade. Reached from the
/// catch-all arm of `ini_config::apply_reticulum_key`, so the fork adds no
/// line inside that match.
/// Leviculum-only keys, named as rns-rs names them; a stock rnsd config has
/// none of them and runs with the feature off. Values are validated when the
/// builder reads them, so a typo stops the node instead of silently changing
/// who learns its public address. Anything else stays tolerated and ignored.
pub(crate) fn apply_ini_key(config: &mut crate::config::ReticulumConfig, key: &str, value: &str) {
    match key {
        "probe_port" => match value.trim().parse() {
            Ok(port) => config.direct_link.probe_port = Some(port),
            // Kept for from_config to refuse: a typo here would otherwise be
            // a node that starts fine and never answers a probe.
            Err(_) => config.direct_link.invalid_probe_port = Some(value.trim().to_string()),
        },
        "probe_addr" => config.direct_link.probe_addr = Some(value.trim().to_string()),
        "probe_protocol" => {
            config.direct_link.probe_protocol = Some(value.trim().to_ascii_lowercase())
        }
        "direct_connect_policy" => {
            config.direct_link.direct_connect_policy = Some(value.trim().to_ascii_lowercase())
        }
        "probe_addr2" => config.direct_link.probe_addr2 = Some(value.trim().to_string()),
        "probe_protocol2" => {
            config.direct_link.probe_protocol2 = Some(value.trim().to_ascii_lowercase())
        }
        _ => {}
    }
}

/// Resolve a `host:port` facilitator to the address put in the REQUEST.
/// The peer probes that address too, so it has to be an IP, not a name.
pub(crate) async fn resolve_facilitator(addr: &str) -> io::Result<SocketAddr> {
    // IPv4 first: it is what NATs punch most reliably and what most
    // facilitators serve. An IPv6-only facilitator is used as such.
    let resolved: Vec<SocketAddr> = tokio::net::lookup_host(addr).await?.collect();
    resolved
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| resolved.first())
        .copied()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{addr} has no address")))
}

/// [`resolve_facilitator`], blocking: for the builder, before any runtime
/// work. `None` when it does not resolve (the next proposal tries again).
pub(crate) fn resolve_facilitator_sync(addr: &str) -> Option<SocketAddr> {
    use std::net::ToSocketAddrs;
    let resolved: Vec<SocketAddr> = addr.to_socket_addrs().ok()?.collect();
    resolved
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| resolved.first())
        .copied()
}

// Probe

/// Probe attempts and the wait for each answer.
const PROBE_ATTEMPTS: u32 = 3;
const PROBE_WAIT: Duration = Duration::from_secs(2);

/// One probe request and what identifies its answer.
enum ProbeAttempt {
    Rnsp([u8; 16]),
    Stun([u8; 12]),
}

impl ProbeAttempt {
    fn new(protocol: ProbeProtocol, rng: &mut impl RngCore) -> Self {
        match protocol {
            ProbeProtocol::Rnsp => {
                let mut nonce = [0u8; 16];
                rng.fill_bytes(&mut nonce);
                ProbeAttempt::Rnsp(nonce)
            }
            ProbeProtocol::Stun => {
                let mut txn = [0u8; 12];
                rng.fill_bytes(&mut txn);
                ProbeAttempt::Stun(txn)
            }
        }
    }

    fn request(&self) -> Vec<u8> {
        match self {
            ProbeAttempt::Rnsp(nonce) => wire::rnsp_request(nonce).to_vec(),
            ProbeAttempt::Stun(txn) => wire::stun_binding_request(txn).to_vec(),
        }
    }

    fn answer(&self, data: &[u8]) -> Option<SocketAddr> {
        match self {
            ProbeAttempt::Rnsp(nonce) => wire::parse_rnsp_response(data, nonce),
            ProbeAttempt::Stun(txn) => wire::parse_stun_binding_response(data, txn),
        }
    }
}

/// Learn `socket`'s reflexive address from `server`.
pub(crate) async fn probe(
    socket: &UdpSocket,
    server: SocketAddr,
    protocol: ProbeProtocol,
) -> io::Result<SocketAddr> {
    probe_with(socket, server, protocol, PROBE_ATTEMPTS, PROBE_WAIT).await
}

/// [`probe`], with the attempts and the wait for each given.
pub(crate) async fn probe_with(
    socket: &UdpSocket,
    server: SocketAddr,
    protocol: ProbeProtocol,
    attempts: u32,
    wait: Duration,
) -> io::Result<SocketAddr> {
    let mut rng = rand_core::OsRng;
    let mut buf = [0u8; 512];
    for _ in 0..attempts {
        // A fresh nonce per attempt, so a late answer to an earlier attempt
        // is not mistaken for this one (either is equally true, but matching
        // exactly keeps the rule simple).
        let attempt = ProbeAttempt::new(protocol, &mut rng);
        let request = attempt.request();
        socket.send_to(&request, on_socket(socket, server)).await?;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            match tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
                Err(_) => break,
                Ok(Err(e)) => {
                    tracing::debug!("direct link: probe recv error: {e}");
                    break;
                }
                Ok(Ok((len, src))) => {
                    if canonical(src) != canonical(server) {
                        continue;
                    }
                    if let Some(public) = attempt.answer(&buf[..len]) {
                        return Ok(public);
                    }
                }
            }
        }
    }
    // No address in the error: it reaches the node's INFO log (review 9).
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "the facilitator did not answer",
    ))
}

// Punch

/// When a new direct interface's announces are repeated, after the one on
/// registration. The peer finishes punching within about a round trip of
/// this node, and this interface acks any late punch, so the first repeat
/// normally lands; the second covers a slow peer.
const REANNOUNCE_AFTER: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(4)];

/// Interval between punch frames, and the whole window. Same as rns-rs, so
/// the two ends of a mixed pair give up at about the same time.
const PUNCH_INTERVAL: Duration = Duration::from_millis(100);
const PUNCH_WINDOW: Duration = Duration::from_secs(10);

/// What a successful punch hands on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Punched {
    /// Where the peer was actually seen, which a port-preserving NAT makes
    /// the address punched toward.
    pub peer: SocketAddr,
    /// Ordinary datagrams the peer sent while this end was still punching.
    /// The peer can finish first, move its link onto the new path and send
    /// at once; dropping those here would lose one-shot packets (a close,
    /// an identify) that nothing repeats (Codex review on #78). The
    /// interface delivers them before anything it reads itself.
    pub early: Vec<Vec<u8>>,
}

/// How many early datagrams a punch keeps, and how many bytes in all; a
/// peer that sends more before this end finishes is not waiting for
/// anything. Bounded by bytes, not only count, so a peer cannot pin
/// megabytes per session (Codex review on #78).
const EARLY_MAX: usize = 64;
const EARLY_MAX_BYTES: usize = 64 * 1024;
/// The largest early datagram kept. Once the peer's link is on the direct
/// path its packets are sized for `DIRECT_LINK_MTU`; anything far larger
/// is not one of them.
const EARLY_DATAGRAM_MAX: usize = 2_048;

/// Peer-reflexive addresses one punch adds to its targets, at most: where
/// the peer's punches were seen coming from, when that is none of the
/// addresses it advertised.
const PEER_REFLEXIVE_MAX: usize = 4;

/// Punch toward every address in `targets` at once until one of them is
/// proven both ways: an ack arrives FROM an address this end punched, so
/// its punch reached the peer there and the peer's answer came back over
/// the same pair (+ciris; ICE's symmetric check, RFC 8445 7.2.5.2.1), and a
/// punch from the peer has arrived from that same address and been acked,
/// so the peer can complete too. That address is the path. An ack from
/// anywhere else proves nothing about the pair this end would send on, and
/// accepting it let the two ends settle on different paths.
///
/// Every punch heard is acked to where it came from, and that address is
/// punched too from then on (a peer-reflexive candidate): behind a NAT that
/// did not keep the advertised port, it is the address the peer is really
/// reachable at. `targets` is the peer's public address, then (+ciris) its
/// host candidates; the socket may be dual-stack, so addresses are compared
/// in canonical form.
///
/// The runtime calls [`punch_with`]; this is its plain form, for the tests.
#[cfg(test)]
pub(crate) async fn punch(
    socket: &UdpSocket,
    targets: &[SocketAddr],
    session: &SessionId,
    token: &[u8; 32],
) -> Option<Punched> {
    punch_with(socket, false, targets, &[], session, token, false)
        .await
        .map(|(p, _)| p)
}

/// +ciris — [`punch`], with the two halves of the birthday approach for a
/// peer pair where one NAT maps per destination at random. Both start only
/// after [`ESCALATE_AFTER`] (or [`BIRTHDAY_AFTER`]) with nothing heard, so a
/// pair that reaches each other at once never sprays or opens a socket more:
/// - `birthday` (this node's NAT is the random one): up to
///   [`wire::MAX_BIRTHDAY_SOCKETS`] more sockets, each sending one punch a
///   [`BIRTHDAY_REFRESH`] toward the peer's advertised address so its own
///   mapping stays open. The first one the peer reaches becomes the punch
///   socket for the rest of the window and is returned, so the interface
///   runs on it; every other one is closed.
/// - `spray` (the peer's NAT is the random one): also punch random ports on
///   the peer's public IP, [`SPRAY_PER_TICK`] a tick, until anything is heard
///   from it or [`wire::MAX_SPRAY_PORTS`] are used.
pub(crate) async fn punch_with(
    socket: &UdpSocket,
    birthday: bool,
    targets: &[SocketAddr],
    predicted: &[SocketAddr],
    session: &SessionId,
    token: &[u8; 32],
    spray: bool,
) -> Option<(Punched, Option<Arc<UdpSocket>>)> {
    let spray: Option<SprayPorts> = spray.then(|| {
        let mut rng = rand_core::OsRng;
        Box::new(move || spray_port(&mut rng)) as SprayPorts
    });
    let extras = birthday.then_some(Extras::Open);
    punch_with_ports(
        socket,
        extras,
        targets,
        predicted,
        session,
        token,
        spray,
        knock_budget(),
    )
    .await
}

/// +ciris (review item 2): how often one node may spray one target IP. The
/// IP is the peer's own report ("a"), so without this a peer, or many cheap
/// links, could aim a node's spray at a victim.
pub(crate) const SPRAY_IP_COOLDOWN: Duration = Duration::from_secs(300);
/// +ciris (review item 2): knocks (spray and predicted ports) per second
/// the whole process may send, all sessions together.
pub(crate) const KNOCK_GLOBAL_PPS: u32 = 400;

/// +ciris (review item 2): the process-wide budget for knocks: one spray per
/// target IP per [`SPRAY_IP_COOLDOWN`], and at most [`KNOCK_GLOBAL_PPS`]
/// knocks a second (a token bucket of that depth).
pub(crate) struct KnockBudget {
    inner: std::sync::Mutex<KnockBudgetInner>,
}

struct KnockBudgetInner {
    sprayed: HashMap<std::net::IpAddr, std::time::Instant>,
    tokens: f64,
    refilled: Option<std::time::Instant>,
}

impl KnockBudget {
    pub(crate) fn new() -> Self {
        KnockBudget {
            inner: std::sync::Mutex::new(KnockBudgetInner {
                sprayed: HashMap::new(),
                tokens: f64::from(KNOCK_GLOBAL_PPS),
                refilled: None,
            }),
        }
    }

    /// May a spray start toward `ip` now? Records it if so.
    pub(crate) fn spray_permit(&self, ip: std::net::IpAddr, now: std::time::Instant) -> bool {
        let mut b = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        b.sprayed
            .retain(|_, at| now.saturating_duration_since(*at) < SPRAY_IP_COOLDOWN);
        if b.sprayed.contains_key(&ip) {
            return false;
        }
        b.sprayed.insert(ip, now);
        true
    }

    /// Take one knock from the global bucket.
    pub(crate) fn take(&self, now: std::time::Instant) -> bool {
        let mut b = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let cap = f64::from(KNOCK_GLOBAL_PPS);
        if let Some(at) = b.refilled {
            let gained = now.saturating_duration_since(at).as_secs_f64() * cap;
            b.tokens = (b.tokens + gained).min(cap);
        }
        b.refilled = Some(now);
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// The process's knock budget.
pub(crate) fn knock_budget() -> &'static KnockBudget {
    static BUDGET: std::sync::OnceLock<KnockBudget> = std::sync::OnceLock::new();
    BUDGET.get_or_init(KnockBudget::new)
}

/// +ciris (review item 3): birthday sockets the whole process may hold at
/// once, all sessions together: one session's full set.
pub(crate) fn birthday_permits() -> &'static Arc<tokio::sync::Semaphore> {
    static PERMITS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    PERMITS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(wire::MAX_BIRTHDAY_SOCKETS)))
}

/// Where a spray draws its ports: random in production, fixed in tests.
pub(crate) type SprayPorts = Box<dyn FnMut() -> u16 + Send>;

/// Where a punch from behind a random NAT gets its birthday sockets.
pub(crate) enum Extras {
    /// Bind [`wire::MAX_BIRTHDAY_SOCKETS`] when they are needed.
    Open,
    /// These, already bound (tests).
    #[cfg(test)]
    Given(Vec<Arc<UdpSocket>>),
}

/// [`punch_with`], with the spray's port source and the birthday sockets
/// given.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn punch_with_ports(
    socket: &UdpSocket,
    extras: Option<Extras>,
    targets: &[SocketAddr],
    predicted: &[SocketAddr],
    session: &SessionId,
    token: &[u8; 32],
    spray: Option<SprayPorts>,
    budget: &KnockBudget,
) -> Option<(Punched, Option<Arc<UdpSocket>>)> {
    let mut diag = PunchDiag::new(targets, predicted, *session);
    let now = tokio::time::Instant::now();
    let deadline = now + PUNCH_WINDOW;
    let escalate_at = now + ESCALATE_AFTER;
    let result = match extras {
        None => punch_inner(
            socket,
            targets,
            predicted,
            session,
            token,
            &mut diag,
            deadline,
            escalate_at,
            spray,
            None,
            budget,
        )
        .await
        .map(|p| (p, None)),
        Some(extras) => {
            punch_birthday(
                socket,
                extras,
                targets,
                predicted,
                session,
                token,
                &mut diag,
                deadline,
                escalate_at,
                spray,
                budget,
            )
            .await
        }
    };
    diag.finish(if result.is_some() {
        "punched"
    } else {
        "window_expired"
    });
    result
}

/// Random ports knocked on the peer's IP per punch tick (see [`punch_with`]):
/// [`wire::MAX_SPRAY_PORTS`] over about 4.4 s, so the knocks still meet the
/// peer's birthday sockets when the two ends start up to RTT/2 (at most
/// RESPONDER_START_DELAY_MAX) apart (review item 7).
pub(crate) const SPRAY_PER_TICK: usize = 22;

/// How long a punch hears nothing before it punches the peer's predicted
/// ports and sprays: a pair that reaches each other does so well inside it
/// (7 to 567 ms in every clean live sample), so it never does either.
pub(crate) const ESCALATE_AFTER: Duration = Duration::from_millis(1_500);

/// How long a punch from behind a random NAT hears nothing before it opens
/// its birthday sockets: before the peer starts spraying, so each mapping is
/// open when the spray arrives.
pub(crate) const BIRTHDAY_AFTER: Duration = Duration::from_millis(1_000);

/// How often each birthday socket repeats its punch toward the peer. With
/// the knock fix the peer's knock only opens its filter, and the next of
/// these punches is what gets through, so this is the added latency.
const BIRTHDAY_REFRESH: Duration = Duration::from_millis(500);

/// How a birthday punch ended, before the diag is free to be written.
enum BirthdayEnd {
    Primary(Option<Punched>),
    Hit {
        index: usize,
        src: SocketAddr,
        seq: Option<u32>,
    },
}

/// Punch from `socket`; past [`BIRTHDAY_AFTER`] with nothing heard, hold
/// birthday sockets open too and switch to the first one the peer reaches.
#[allow(clippy::too_many_arguments)]
async fn punch_birthday(
    socket: &UdpSocket,
    extras: Extras,
    targets: &[SocketAddr],
    predicted: &[SocketAddr],
    session: &SessionId,
    token: &[u8; 32],
    diag: &mut PunchDiag,
    deadline: tokio::time::Instant,
    escalate_at: tokio::time::Instant,
    spray: Option<SprayPorts>,
    budget: &KnockBudget,
) -> Option<(Punched, Option<Arc<UdpSocket>>)> {
    let peer = canonical(*targets.first()?);
    let mut opened: Vec<Arc<UdpSocket>> = Vec::new();
    // One per opened socket (none for given ones): released as each closes.
    let mut permits: Vec<Option<tokio::sync::OwnedSemaphorePermit>> = Vec::new();
    // Written the moment the sockets are bound, while `diag` is lent to the
    // primary punch, so a punch the peer's REJECT aborts still logs them.
    let open_count = Arc::clone(&diag.birthday_open);
    // Set by the session socket's punch the moment it hears the peer, before
    // its ack leaves; read here on every birthday hit.
    let primary_heard = Arc::clone(&diag.primary_heard);
    let mut ignored = 0usize;
    let end = {
        let primary = punch_inner(
            socket,
            targets,
            predicted,
            session,
            token,
            diag,
            deadline,
            escalate_at,
            spray,
            None,
            budget,
        );
        tokio::pin!(primary);
        let early = tokio::select! {
            r = &mut primary => Some(r),
            _ = tokio::time::sleep(BIRTHDAY_AFTER) => None,
        };
        match early {
            Some(r) => BirthdayEnd::Primary(r),
            // Heard already at the gate: nothing to open (review item 6).
            None if primary_heard.load(std::sync::atomic::Ordering::SeqCst) => {
                BirthdayEnd::Primary((&mut primary).await)
            }
            None => {
                (opened, permits) = match extras {
                    Extras::Open => birthday_sockets(peer, birthday_permits()).await,
                    #[cfg(test)]
                    Extras::Given(given) => {
                        let n = given.len();
                        (given, (0..n).map(|_| None).collect())
                    }
                };
                opened.truncate(wire::MAX_BIRTHDAY_SOCKETS);
                open_count.store(opened.len(), std::sync::atomic::Ordering::Relaxed);
                let (hit_tx, mut hit_rx) = mpsc::channel::<(usize, SocketAddr, Option<u32>)>(1);
                let mut tasks = Vec::with_capacity(opened.len());
                for (index, extra) in opened.iter().enumerate() {
                    tasks.push(birthday_task(
                        index,
                        Arc::clone(extra),
                        peer,
                        hit_tx.clone(),
                        *session,
                        *token,
                    ));
                }
                drop(hit_tx);
                let end = loop {
                    tokio::select! {
                        r = &mut primary => break BirthdayEnd::Primary(r),
                        hit = hit_rx.recv() => match hit {
                            // The session socket has heard the peer, so the
                            // peer may settle on it: switching now would put
                            // the two ends on different socket pairs (found
                            // by the TLA+ model, docs/direct_link). Stay.
                            Some(_) if primary_heard.load(std::sync::atomic::Ordering::SeqCst) => {
                                ignored += 1;
                            }
                            Some((index, src, seq)) => break BirthdayEnd::Hit { index, src, seq },
                            // Every birthday task reported and was passed
                            // over: the session socket finishes it.
                            None => break BirthdayEnd::Primary((&mut primary).await),
                        },
                    }
                };
                // Every birthday task stops here, whichever way it went.
                drop(tasks);
                end
            }
        }
    };
    diag.birthday_ignored = ignored;
    let (index, src, seq) = match end {
        BirthdayEnd::Primary(r) => return r.map(|p| (p, None)),
        BirthdayEnd::Hit { index, src, seq } => (index, src, seq),
    };
    // The peer reached the mapping of birthday socket `index`: close every
    // other one, ack the punch that got through, and punch on from there.
    let winner = Arc::clone(&opened[index]);
    drop(opened);
    // The winner's permit goes too: from here it is the path's socket.
    drop(permits);
    diag.birthday_switched = true;
    diag.birthday_winner = Some(index);
    if let Some(seq) = seq {
        let ack = wire::punch_frame(PunchKind::Ack, session, token, seq);
        let _ = winner.send_to(&ack, on_socket(&winner, src)).await;
    }
    // A punch that got through is already heard: the peer may finish on our
    // ack and then only ack, never punch again, so it must not be needed
    // twice.
    let seed = seq.is_some().then_some(src);
    let now = tokio::time::Instant::now();
    punch_inner(
        &winner,
        targets,
        &[],
        session,
        token,
        diag,
        deadline,
        now,
        None,
        seed,
        budget,
    )
    .await
    .map(|p| (p, Some(winner)))
}

/// One birthday socket: a punch toward `peer` every [`BIRTHDAY_REFRESH`],
/// and the first valid frame back reported on `hit`.
fn birthday_task(
    index: usize,
    extra: Arc<UdpSocket>,
    peer: SocketAddr,
    hit: mpsc::Sender<(usize, SocketAddr, Option<u32>)>,
    session: SessionId,
    token: [u8; 32],
) -> AbortOnDrop {
    AbortOnDrop(
        tokio::spawn(async move {
            let frame = wire::punch_frame(PunchKind::Punch, &session, &token, 0);
            let mut refresh = tokio::time::interval(BIRTHDAY_REFRESH);
            let mut buf = [0u8; 512];
            loop {
                tokio::select! {
                    _ = refresh.tick() => {
                        let _ = extra.send_to(&frame, on_socket(&extra, peer)).await;
                    }
                    r = extra.recv_from(&mut buf) => {
                        let Ok((len, src)) = r else { continue };
                        let seq = match wire::parse_punch_frame(&buf[..len], &session, &token) {
                            Some((PunchKind::Punch, seq)) => Some(seq),
                            Some((PunchKind::Ack, _)) => None,
                            None => continue,
                        };
                        let _ = hit.send((index, canonical(src), seq)).await;
                        return;
                    }
                }
            }
        })
        .abort_handle(),
    )
}

#[allow(clippy::too_many_arguments)]
async fn punch_inner(
    socket: &UdpSocket,
    targets: &[SocketAddr],
    predicted: &[SocketAddr],
    session: &SessionId,
    token: &[u8; 32],
    diag: &mut PunchDiag,
    deadline: tokio::time::Instant,
    escalate_at: tokio::time::Instant,
    spray: Option<SprayPorts>,
    seed_heard: Option<SocketAddr>,
    budget: &KnockBudget,
) -> Option<Punched> {
    let mut targets: Vec<SocketAddr> = alloc_targets(
        canonical(*targets.first()?),
        &targets[1..]
            .iter()
            .map(|a| canonical(*a))
            .collect::<Vec<_>>(),
    );
    // +ciris: the peer's predicted ports (on its public IP), held back until
    // nothing has been heard for ESCALATE_AFTER; then KNOCKED, with the
    // spray (review item 1): a knock opens this node's filter toward a port
    // that may be anyone's on the peer's NAT, and carries no token. Only the
    // advertised address, host candidates and sources that proved the token
    // get tokened punches.
    let public = targets[0];
    let mut held: Vec<SocketAddr> = predicted
        .iter()
        .map(|a| canonical(*a))
        .filter(|a| !targets.contains(a))
        .collect();
    let mut pending_spray = spray;
    let mut spray: Option<SprayPorts> = None;
    let mut escalated = false;
    let mut knocking: Vec<SocketAddr> = Vec::new();
    let mut rng = rand_core::OsRng;
    let advertised = targets.len() + held.len();
    // Addresses a punch actually left for; only an ack from one counts.
    let mut punched: Vec<SocketAddr> = Vec::new();
    // Addresses a valid punch came from (each acked at once), and those an
    // ack came from; the path is the first address in both, and punched.
    let mut heard: Vec<SocketAddr> = Vec::new();
    let mut acked: Vec<SocketAddr> = Vec::new();
    // +ciris: a punch heard (and acked) before this loop began, on this
    // socket: a birthday socket the peer reached. Punched too, as any
    // peer-reflexive source is.
    if let Some(src) = seed_heard.map(canonical) {
        if !targets.contains(&src) && targets.len() < advertised + PEER_REFLEXIVE_MAX {
            targets.push(src);
            diag.peer_reflexive += 1;
        }
        heard.push(src);
        diag.heard(src);
    }
    let mut early: Vec<Vec<u8>> = Vec::new();
    let mut early_bytes = 0usize;
    let mut seq: u32 = 0;
    let mut tick = tokio::time::interval(PUNCH_INTERVAL);
    let mut buf = vec![0u8; 65_535];
    // +ciris: ports knocked on the peer's IP (its NAT is random). Never
    // "ours" for an ack: a knock proves nothing (review item 1).
    let mut sprayed: std::collections::HashSet<SocketAddr> = std::collections::HashSet::new();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return None,
            _ = tick.tick() => {
                if !escalated && tokio::time::Instant::now() >= escalate_at {
                    escalated = true;
                    if heard.is_empty() {
                        diag.escalated = true;
                        knocking.append(&mut held);
                        // One spray per target IP per SPRAY_IP_COOLDOWN, in
                        // the whole process (review item 2).
                        spray = pending_spray.take().filter(|_| {
                            let ok = budget.spray_permit(public.ip(), std::time::Instant::now());
                            if !ok {
                                diag.spray_refused = true;
                            }
                            ok
                        });
                    } else {
                        // Heard already: the peer's mapping did not move.
                        diag.predicted_dropped += held.len();
                        held.clear();
                        pending_spray = None;
                    }
                }
                let frame = wire::punch_frame(PunchKind::Punch, session, token, seq);
                seq = seq.wrapping_add(1) % wire::KEEPALIVE_SEQ;
                for target in &targets {
                    let sent = socket.send_to(&frame, on_socket(socket, *target)).await.is_ok();
                    if !sent {
                        diag.send_errors += 1;
                    }
                    if sent && !punched.contains(target) {
                        punched.push(*target);
                    }
                }
                diag.punched = punched.len();
                // Knocks stop once anything is heard: the peer is reached.
                if !heard.is_empty() {
                    diag.predicted_dropped += knocking.len();
                    knocking.clear();
                }
                for addr in &knocking {
                    if !budget.take(std::time::Instant::now()) {
                        diag.knocks_refused += 1;
                        break;
                    }
                    let mut pad = [0u8; 12];
                    rng.fill_bytes(&mut pad);
                    if socket.send_to(&wire::knock_frame(&pad), on_socket(socket, *addr)).await.is_err() {
                        diag.send_errors += 1;
                    }
                    diag.knocks += 1;
                }
                if spray.is_some() && (!heard.is_empty() || sprayed.len() >= wire::MAX_SPRAY_PORTS) {
                    spray = None;
                }
                if let Some(next_port) = spray.as_mut() {
                    let peer_ip = targets[0].ip();
                    let mut sent_now = 0;
                    // Bounded draws too: a source that repeats cannot spin.
                    let mut draws = 0;
                    while sent_now < SPRAY_PER_TICK
                        && sprayed.len() < wire::MAX_SPRAY_PORTS
                        && draws < 4 * SPRAY_PER_TICK
                    {
                        draws += 1;
                        let port = next_port();
                        let addr = SocketAddr::new(peer_ip, port);
                        if targets.contains(&addr) || knocking.contains(&addr) || !sprayed.insert(addr) {
                            continue;
                        }
                        if !budget.take(std::time::Instant::now()) {
                            diag.knocks_refused += 1;
                            break;
                        }
                        sent_now += 1;
                        let mut pad = [0u8; 12];
                        rng.fill_bytes(&mut pad);
                        if socket.send_to(&wire::knock_frame(&pad), on_socket(socket, addr)).await.is_err() {
                            diag.send_errors += 1;
                        }
                        diag.knocks += 1;
                    }
                    diag.sprayed = sprayed.len();
                }
            }
            result = socket.recv_from(&mut buf) => {
                let Ok((len, src)) = result else {
                    diag.recv_errors += 1;
                    continue
                };
                let src = canonical(src);
                let frame = &buf[..len];
                diag.rx_frames += 1;
                // +ciris (leviculum#104): the peer may already be probing the
                // path's MTU; answer, and never keep a probe as data.
                if wire::is_mtu_probe(frame) {
                    answer_mtu_probe(socket, frame, src, session, token).await;
                    continue;
                }
                // +ciris: a peer's knock only opened its own filter; it is no
                // punch and never data.
                if wire::is_knock(frame) {
                    diag.rx_knocks += 1;
                    continue;
                }
                match wire::parse_punch_frame(frame, session, token) {
                    Some((PunchKind::Punch, their_seq)) => {
                        // Before the ack leaves: from here the peer can
                        // settle on this socket (see punch_birthday).
                        diag.primary_heard.store(true, std::sync::atomic::Ordering::SeqCst);
                        let ack = wire::punch_frame(PunchKind::Ack, session, token, their_seq);
                        let _ = socket.send_to(&ack, on_socket(socket, src)).await;
                        // A source that sent a valid punch proved the token:
                        // it is punched (tokened) from now on.
                        if !targets.contains(&src) && targets.len() < advertised + PEER_REFLEXIVE_MAX {
                            tracing::debug!("direct link: peer-reflexive candidate");
                            targets.push(src);
                            diag.peer_reflexive += 1;
                        }
                        if !heard.contains(&src) && heard.len() < targets.len() {
                            heard.push(src);
                        }
                        diag.heard(src);
                    }
                    Some((PunchKind::Ack, _)) => {
                        // Only an address this end sent a TOKENED punch to:
                        // a knocked or sprayed one proves nothing.
                        let ours = punched.contains(&src);
                        if ours {
                            diag.primary_heard.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        if ours && !acked.contains(&src) {
                            acked.push(src);
                        }
                        diag.acked(src, ours);
                    }
                    None => {
                        diag.unparsed(wire::punch_frame_kind(frame).is_some());
                        if targets.contains(&src)
                            && wire::punch_frame_kind(frame).is_none()
                            && !frame.is_empty()
                            && frame.len() <= EARLY_DATAGRAM_MAX
                            && early.len() < EARLY_MAX
                            && early_bytes + frame.len() <= EARLY_MAX_BYTES
                        {
                            early_bytes += frame.len();
                            early.push(frame.to_vec());
                        }
                    }
                }
                if let Some(path) = acked.iter().find(|a| heard.contains(a)) {
                    return Some(Punched { peer: *path, early });
                }
            }
        }
    }
}

/// +ciris: up to [`wire::MAX_BIRTHDAY_SOCKETS`] fresh sockets for a punch
/// toward `peer` from behind a random NAT, each a mapping of its own, and
/// each holding one of `permits` (review item 3: the process-wide count).
/// Fewer when the permits or the host run out; none is no birthday phase.
async fn birthday_sockets(
    peer: SocketAddr,
    permits: &Arc<tokio::sync::Semaphore>,
) -> (
    Vec<Arc<UdpSocket>>,
    Vec<Option<tokio::sync::OwnedSemaphorePermit>>,
) {
    let bind = if canonical(peer).is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0u16; 8], 0))
    };
    let mut out = Vec::with_capacity(wire::MAX_BIRTHDAY_SOCKETS);
    let mut held = Vec::with_capacity(wire::MAX_BIRTHDAY_SOCKETS);
    for _ in 0..wire::MAX_BIRTHDAY_SOCKETS {
        let Ok(permit) = Arc::clone(permits).try_acquire_owned() else {
            break;
        };
        match UdpSocket::bind(bind).await {
            Ok(s) => {
                out.push(Arc::new(s));
                held.push(Some(permit));
            }
            Err(_) => break,
        }
    }
    (out, held)
}

/// A random port to spray, above the well-known range.
fn spray_port(rng: &mut impl RngCore) -> u16 {
    1024 + (rng.next_u32() % (65_536 - 1024)) as u16
}

// Port prediction (+ciris)

/// The largest per-allocation step taken as a steady one. Past it the NAT
/// allocates randomly, or the measurement saw too many other hosts'
/// allocations in between, and a prediction would only spray.
const MAX_PREDICT_STEP: i32 = 64;

/// How far apart the NAT put three consecutive mappings: the session
/// socket's (`base`), then two fresh sockets', each toward the facilitator.
/// Differences only, never a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AllocationSteps {
    base: u16,
    last: u16,
    pub(crate) step1: i32,
    pub(crate) step2: i32,
}

/// Probe `server` from two fresh sockets after the session socket's probe
/// mapped to `base_port`: the steps between the three mappings. `None` when
/// either probe went unanswered.
async fn allocation_steps(
    server: SocketAddr,
    protocol: ProbeProtocol,
    base_port: u16,
) -> Option<AllocationSteps> {
    let bind = if server.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0u16; 8], 0))
    };
    let (s1, s2) = (
        UdpSocket::bind(bind).await.ok()?,
        UdpSocket::bind(bind).await.ok()?,
    );
    // +ciris (review item 8): both at once, inside ALLOCATION_DEADLINE, so
    // the measurement cannot eat DISCOVER_TIMEOUT; no answer, no prediction.
    let both = async {
        tokio::join!(
            probe_with(&s1, server, protocol, 2, ALLOCATION_DEADLINE / 2),
            probe_with(&s2, server, protocol, 2, ALLOCATION_DEADLINE / 2),
        )
    };
    let (a, b) = tokio::time::timeout(ALLOCATION_DEADLINE, both).await.ok()?;
    Some(steps_from(base_port, a.ok()?.port(), b.ok()?.port()))
}

/// The two probes run at once, so which mapping came first is not known:
/// order them by distance from `base` (a steady allocator gives base+s and
/// base+2s in either order).
pub(crate) fn steps_from(base: u16, a: u16, b: u16) -> AllocationSteps {
    let d = |p: u16| i32::from(p) - i32::from(base);
    let (near, far) = if d(a).abs() <= d(b).abs() {
        (a, b)
    } else {
        (b, a)
    };
    AllocationSteps {
        base,
        last: far,
        step1: d(near),
        step2: i32::from(far) - i32::from(near),
    }
}

/// +ciris (review item 8): the whole allocation measurement's budget.
const ALLOCATION_DEADLINE: Duration = Duration::from_millis(1_000);

/// +ciris (review item 6): this NAT's mapping, from the SESSION socket's
/// own mappings toward two observers. A NAT that gives each socket a random
/// port but keeps one mapping per socket (an endpoint-independent CGNAT)
/// differs only between sockets, so only the same socket toward two
/// destinations can tell it from one that maps per destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NatClass {
    /// The first observer saw the socket's own address.
    NoNat,
    /// Both observers saw the same mapping.
    EndpointIndependent,
    /// The two observers saw different mappings.
    EndpointDependent,
    /// Not decidable: no second observer, or it did not answer.
    Unknown,
}

/// Classify from what the two observers saw (`first`, `second`) and this
/// socket's host candidates.
pub(crate) fn classify_nat(
    first: Option<SocketAddr>,
    second: Option<SocketAddr>,
    local: &[SocketAddr],
) -> NatClass {
    let Some(first) = first.map(canonical) else {
        return NatClass::Unknown;
    };
    if local.iter().any(|l| canonical(*l) == first) {
        return NatClass::NoNat;
    }
    match second.map(canonical) {
        None => NatClass::Unknown,
        Some(second) if second == first => NatClass::EndpointIndependent,
        Some(_) => NatClass::EndpointDependent,
    }
}

/// What to advertise: predicted ports ("q") only for an endpoint-dependent
/// NAT that allocates in steady steps, "b" only for one that allocates at
/// random; neither otherwise (review items 6 and the f46a9da nit).
pub(crate) fn advertise(class: NatClass, steps: Option<AllocationSteps>) -> (Vec<u16>, bool) {
    if class != NatClass::EndpointDependent {
        return (Vec::new(), false);
    }
    match steps {
        None => (Vec::new(), false),
        Some(s) => {
            let predicted = predict_ports(s);
            let random = predicted.is_empty() && s.step1 != 0 && s.step2 != 0;
            (predicted, random)
        }
    }
}

/// The ports a NAT that allocates in `steps` will likely give the session
/// socket's next mapping (toward the peer): the next
/// [`wire::MAX_PREDICTED_PORTS`] allocations past the last one seen, at the
/// smaller of the two steps. None when the steps disagree in sign, either is
/// zero (one mapping: nothing to predict) or past [`MAX_PREDICT_STEP`].
pub(crate) fn predict_ports(steps: AllocationSteps) -> Vec<u16> {
    let (a, b) = (steps.step1, steps.step2);
    if a == 0 || b == 0 || a.signum() != b.signum() {
        return Vec::new();
    }
    if a.abs() > MAX_PREDICT_STEP || b.abs() > MAX_PREDICT_STEP {
        return Vec::new();
    }
    let step = a.signum() * a.abs().min(b.abs());
    let mut out = Vec::new();
    for k in 1..=wire::MAX_PREDICTED_PORTS as i32 {
        let port = i32::from(steps.last) + step * k;
        match u16::try_from(port) {
            Ok(p) if p >= 1024 && p != steps.base && !out.contains(&p) => out.push(p),
            _ => {}
        }
    }
    out
}

// Punch diagnostics (+ciris)

/// The tracing target of the one-line punch and probe summaries, so a tool
/// can collect them without the address-bearing lines beside them.
pub const DIAG_TARGET: &str = "direct_link_diag";

/// +ciris — where a punch was heard from, relative to what the peer
/// advertised. Addresses never leave this function: only the class does,
/// which says what the peer's NAT did without saying where anyone is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeardRelation {
    /// Exactly the public address the peer advertised (its facilitator
    /// mapping): the peer's NAT used one mapping for both destinations.
    Same,
    /// The advertised public IP on another port: the peer's NAT made a new
    /// mapping for this destination (endpoint-dependent mapping, "symmetric",
    /// on the PEER's side).
    SameIpOtherPort,
    /// One of the peer's host candidates, exactly.
    HostCandidate,
    /// One of the ports the peer predicted its NAT would map toward this
    /// node, on its public IP: the prediction closed an endpoint-dependent
    /// mapping on the peer's side.
    Predicted,
    /// A host candidate's IP on another port.
    HostIpOtherPort,
    /// None of the addresses the peer advertised (another public IP: a
    /// multi-homed NAT or a CGNAT pool).
    OtherIp,
}

impl HeardRelation {
    /// The word logged.
    pub fn as_str(self) -> &'static str {
        match self {
            HeardRelation::Same => "same",
            HeardRelation::SameIpOtherPort => "same_ip_other_port",
            HeardRelation::HostCandidate => "host_candidate",
            HeardRelation::Predicted => "predicted",
            HeardRelation::HostIpOtherPort => "host_ip_other_port",
            HeardRelation::OtherIp => "other_ip",
        }
    }
}

/// +ciris — classify `src` against the peer's advertised `public` address
/// and its other targets (`host`): its host candidates and, on its public IP,
/// the ports it predicted. All in canonical form.
pub fn heard_relation(src: SocketAddr, public: SocketAddr, host: &[SocketAddr]) -> HeardRelation {
    let (src, public) = (canonical(src), canonical(public));
    if src == public {
        HeardRelation::Same
    } else if host.iter().any(|h| canonical(*h) == src) {
        if src.ip() == public.ip() {
            HeardRelation::Predicted
        } else {
            HeardRelation::HostCandidate
        }
    } else if src.ip() == public.ip() {
        HeardRelation::SameIpOtherPort
    } else if host.iter().any(|h| canonical(*h).ip() == src.ip()) {
        HeardRelation::HostIpOtherPort
    } else {
        HeardRelation::OtherIp
    }
}

/// +ciris — what one punch saw, in counts and classes only; logged once
/// when it ends.
#[derive(Debug)]
pub(crate) struct PunchDiag {
    session: SessionId,
    /// Set once the line is out, so the drop does not log it twice.
    logged: bool,
    start: tokio::time::Instant,
    started_unix_ms: u64,
    public: Option<SocketAddr>,
    host: Vec<SocketAddr>,
    host_v4: usize,
    host_v6: usize,
    predicted: usize,
    /// The peer's predicted ports, for the `predicted` class.
    pred: Vec<SocketAddr>,
    /// Predicted targets dropped because the peer was heard at its
    /// advertised address (its mapping did not move).
    pub(crate) predicted_dropped: usize,
    /// Random ports sprayed on the peer's IP.
    pub(crate) sprayed: usize,
    /// Extra sockets held open toward the peer (this NAT is random).
    /// Shared with the birthday block, which writes it while the diag is
    /// lent to the primary punch.
    pub(crate) birthday_open: Arc<std::sync::atomic::AtomicUsize>,
    /// The punching socket has heard the peer; shared with the birthday
    /// block, which must not switch away from it after that.
    pub(crate) primary_heard: Arc<std::sync::atomic::AtomicBool>,
    /// Birthday hits passed over because the session socket had heard.
    pub(crate) birthday_ignored: usize,
    /// Knocks sent (predicted ports and spray), knocks the budget refused,
    /// knocks received, and whether the per-IP spray budget refused.
    pub(crate) knocks: u32,
    pub(crate) knocks_refused: u32,
    pub(crate) rx_knocks: u32,
    pub(crate) spray_refused: bool,
    /// The peer reached one of them, which became the punch socket.
    pub(crate) birthday_switched: bool,
    /// Which one (its index among them, not a port).
    pub(crate) birthday_winner: Option<usize>,
    /// Nothing was heard for ESCALATE_AFTER: predicted ports and the spray
    /// started.
    pub(crate) escalated: bool,
    pub(crate) peer_reflexive: usize,
    pub(crate) punched: usize,
    pub(crate) send_errors: u32,
    pub(crate) recv_errors: u32,
    pub(crate) rx_frames: u32,
    rx_foreign_punch: u32,
    rx_other: u32,
    punches_heard: u32,
    acks_heard: u32,
    acks_unpunched: u32,
    heard: Vec<(SocketAddr, HeardRelation)>,
    acked: Vec<(SocketAddr, HeardRelation)>,
    first_heard_ms: Option<u64>,
    first_acked_ms: Option<u64>,
}

impl PunchDiag {
    pub(crate) fn new(
        targets: &[SocketAddr],
        predicted: &[SocketAddr],
        session: SessionId,
    ) -> Self {
        let public = targets.first().map(|a| canonical(*a));
        let pred: Vec<SocketAddr> = predicted.iter().map(|a| canonical(*a)).collect();
        let host: Vec<SocketAddr> = targets.iter().skip(1).map(|a| canonical(*a)).collect();
        PunchDiag {
            session,
            logged: false,
            start: tokio::time::Instant::now(),
            started_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            public,
            host_v4: host.iter().filter(|a| a.is_ipv4()).count(),
            host_v6: host.iter().filter(|a| a.is_ipv6()).count(),
            predicted: pred.len(),
            pred,
            predicted_dropped: 0,
            sprayed: 0,
            birthday_open: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            primary_heard: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            birthday_ignored: 0,
            knocks: 0,
            knocks_refused: 0,
            rx_knocks: 0,
            spray_refused: false,
            birthday_switched: false,
            birthday_winner: None,
            escalated: false,
            host,
            peer_reflexive: 0,
            punched: 0,
            send_errors: 0,
            recv_errors: 0,
            rx_frames: 0,
            rx_foreign_punch: 0,
            rx_other: 0,
            punches_heard: 0,
            acks_heard: 0,
            acks_unpunched: 0,
            heard: Vec::new(),
            acked: Vec::new(),
            first_heard_ms: None,
            first_acked_ms: None,
        }
    }

    fn relation(&self, src: SocketAddr) -> HeardRelation {
        // One of the ports the peer predicted (an explicit list, never
        // guessed from the IP).
        if self.pred.contains(&canonical(src)) {
            return HeardRelation::Predicted;
        }
        match self.public {
            Some(public) => heard_relation(src, public, &self.host),
            None => HeardRelation::OtherIp,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    pub(crate) fn heard(&mut self, src: SocketAddr) {
        self.punches_heard += 1;
        self.first_heard_ms
            .get_or_insert_with(|| self.start.elapsed().as_millis() as u64);
        if !self.heard.iter().any(|(a, _)| *a == src) && self.heard.len() < 16 {
            let r = self.relation(src);
            self.heard.push((src, r));
        }
    }

    pub(crate) fn acked(&mut self, src: SocketAddr, was_punched: bool) {
        self.acks_heard += 1;
        if !was_punched {
            self.acks_unpunched += 1;
            return;
        }
        self.first_acked_ms
            .get_or_insert_with(|| self.start.elapsed().as_millis() as u64);
        if !self.acked.iter().any(|(a, _)| *a == src) && self.acked.len() < 16 {
            let r = self.relation(src);
            self.acked.push((src, r));
        }
    }

    pub(crate) fn unparsed(&mut self, punch_shaped: bool) {
        if punch_shaped {
            self.rx_foreign_punch += 1;
        } else {
            self.rx_other += 1;
        }
    }

    fn classes(list: &[(SocketAddr, HeardRelation)]) -> String {
        let words: Vec<&str> = list.iter().map(|(_, r)| r.as_str()).collect();
        format!("[{}]", words.join(","))
    }

    /// Birthday sockets held open toward the peer (this NAT is random).
    pub(crate) fn birthday_sockets(&self) -> usize {
        self.birthday_open
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The datagrams a second those sockets send together toward the peer.
    pub(crate) fn birthday_pps(&self) -> u32 {
        (self.birthday_sockets() as u128 * 1000 / BIRTHDAY_REFRESH.as_millis().max(1)) as u32
    }

    /// Log the line with `outcome`, once.
    pub(crate) fn finish(&mut self, outcome: &'static str) {
        if !self.logged {
            self.logged = true;
            self.log(outcome);
        }
    }

    /// The one INFO line, with no address or port in it.
    fn log(&self, outcome: &'static str) {
        let heard_and_acked = self
            .heard
            .iter()
            .any(|(h, _)| self.acked.iter().any(|(a, _)| a == h));
        tracing::info!(
            target: DIAG_TARGET,
            event = "punch",
            session = %session_short(&self.session),
            outcome,
            elapsed_ms = self.elapsed_ms(),
            started_unix_ms = self.started_unix_ms,
            targets_facilitator_public = usize::from(self.public.is_some()),
            targets_host_v4 = self.host_v4,
            targets_host_v6 = self.host_v6,
            targets_predicted = self.predicted,
            predicted_dropped = self.predicted_dropped,
            sprayed = self.sprayed,
            birthday_sockets = self.birthday_sockets(),
            birthday_switched = self.birthday_switched,
            birthday_ignored = self.birthday_ignored,
            knocks = self.knocks,
            knocks_refused = self.knocks_refused,
            rx_knocks = self.rx_knocks,
            spray_refused = self.spray_refused,
            birthday_winner = self.birthday_winner.map_or(-1, |i| i as i64),
            birthday_pps = self.birthday_pps(),
            escalated = self.escalated,
            targets_peer_reflexive = self.peer_reflexive,
            public_family = match self.public { Some(a) if a.is_ipv6() => "v6", Some(_) => "v4", None => "none" },
            punched = self.punched,
            heard = self.heard.len(),
            acked = self.acked.len(),
            heard_classes = %Self::classes(&self.heard),
            acked_classes = %Self::classes(&self.acked),
            heard_and_acked,
            punches_heard = self.punches_heard,
            acks_heard = self.acks_heard,
            acks_from_unpunched = self.acks_unpunched,
            rx_frames = self.rx_frames,
            rx_foreign_punch = self.rx_foreign_punch,
            rx_other = self.rx_other,
            send_errors = self.send_errors,
            recv_errors = self.recv_errors,
            first_heard_ms = self.first_heard_ms.map(|v| v as i64).unwrap_or(-1),
            first_acked_ms = self.first_acked_ms.map(|v| v as i64).unwrap_or(-1),
            "direct link diag: punch summary"
        );
    }
}

impl Drop for PunchDiag {
    /// A punch the session aborted (the peer's REJECT, a release) logs what
    /// it saw up to then.
    fn drop(&mut self) {
        self.finish("aborted");
    }
}

/// The first four bytes of a session id, in hex: enough to pair two ends'
/// lines, nothing more.
fn session_short(session: &SessionId) -> String {
    session[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// +ciris — the one INFO line after a facilitator probe: whether this NAT
/// preserved the port, and whether there is a NAT at all. No address.
#[allow(clippy::too_many_arguments)]
fn log_probe_diag(
    session: &SessionId,
    socket: &UdpSocket,
    public: Option<SocketAddr>,
    local: &[SocketAddr],
    class: NatClass,
    steps: Option<AllocationSteps>,
    predicted: usize,
    birthday: bool,
    elapsed_ms: u64,
) {
    let local_port = socket.local_addr().map(|a| a.port()).ok();
    let (port_preserved, no_nat) = match public {
        Some(p) => {
            let p = canonical(p);
            (
                Some(local_port == Some(p.port())),
                Some(local.iter().any(|l| canonical(*l).ip() == p.ip())),
            )
        }
        None => (None, None),
    };
    let word = |b: Option<bool>| match b {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    };
    tracing::info!(
        target: DIAG_TARGET,
        event = "probe",
        session = %session_short(session),
        answered = public.is_some(),
        elapsed_ms,
        public_family = match public { Some(a) if canonical(a).is_ipv6() => "v6", Some(_) => "v4", None => "none" },
        port_preserved = word(port_preserved),
        no_nat = word(no_nat),
        host_v4 = local.iter().filter(|a| a.is_ipv4()).count(),
        host_v6 = local.iter().filter(|a| a.is_ipv6()).count(),
        mapping = match class {
            NatClass::NoNat => "no_nat",
            NatClass::EndpointIndependent => "endpoint_independent",
            NatClass::EndpointDependent => "endpoint_dependent",
            NatClass::Unknown => "unknown",
        },
        alloc_measured = steps.is_some(),
        alloc_step1 = steps.map_or(0, |s| s.step1),
        alloc_step2 = steps.map_or(0, |s| s.step2),
        predicted,
        birthday,
        "direct link diag: probe summary"
    );
}

/// +ciris (leviculum#104): how long a path-MTU probe sends, and how long it
/// keeps answering the peer's own probes. The peer starts probing within a
/// punch interval or two of this end (both finish punching on each other's
/// acks), so a window this much longer than the sending phase covers it.
const MTU_PROBE_SEND: Duration = Duration::from_millis(400);
const MTU_PROBE_WINDOW: Duration = Duration::from_millis(600);
const MTU_PROBE_INTERVAL: Duration = Duration::from_millis(100);

/// +ciris (leviculum#104): find the largest of [`wire::MTU_PROBE_STEPS`] the
/// punched path to `peer` carries, by sending padded probes of each size
/// (acked through the punch frames' sequence numbers) for
/// [`MTU_PROBE_SEND`]. `None` when none was acked: a path that drops
/// datagrams near 1400 (WireGuard, IPsec, some APNs, 464XLAT) while the
/// small punch frames pass, or a peer that predates probes. The interface
/// then signals no MTU and links born on it hold the base 500.
///
/// Answers the peer's probes and punches throughout, and keeps ordinary
/// datagrams the peer sends meanwhile in `early`, as [`punch`] does.
pub(crate) async fn probe_path_mtu(
    socket: &UdpSocket,
    peer: SocketAddr,
    session: &SessionId,
    token: &[u8; 32],
    early: &mut Vec<Vec<u8>>,
) -> Option<u32> {
    let start = tokio::time::Instant::now();
    let largest = wire::MTU_PROBE_STEPS[0];
    let mut acked: Option<u32> = None;
    let mut tick = tokio::time::interval(MTU_PROBE_INTERVAL);
    let mut buf = vec![0u8; 65_535];
    let send_to = on_socket(socket, peer);
    let mut early_bytes: usize = early.iter().map(Vec::len).sum();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(start + MTU_PROBE_WINDOW) => return acked,
            _ = tick.tick() => {
                if start.elapsed() < MTU_PROBE_SEND && acked != Some(largest) {
                    for size in wire::MTU_PROBE_STEPS {
                        if acked.is_some_and(|a| a >= size) {
                            continue;
                        }
                        let probe = wire::mtu_probe_frame(session, token, size);
                        let _ = socket.send_to(&probe, send_to).await;
                    }
                }
            }
            result = socket.recv_from(&mut buf) => {
                let Ok((len, src)) = result else { continue };
                let src = canonical(src);
                if src != peer {
                    continue;
                }
                let frame = &buf[..len];
                if wire::is_mtu_probe(frame) {
                    answer_mtu_probe(socket, frame, src, session, token).await;
                    continue;
                }
                match wire::parse_punch_frame(frame, session, token) {
                    Some((PunchKind::Ack, seq)) => {
                        if let Some(size) = wire::mtu_probe_acked_size(seq) {
                            if wire::MTU_PROBE_STEPS.contains(&size) {
                                acked = acked.max(Some(size));
                            }
                        }
                    }
                    Some((PunchKind::Punch, seq)) => {
                        let ack = wire::punch_frame(PunchKind::Ack, session, token, seq);
                        let _ = socket.send_to(&ack, on_socket(socket, src)).await;
                    }
                    None => {
                        if wire::punch_frame_kind(frame).is_none()
                            && !frame.is_empty()
                            && frame.len() <= EARLY_DATAGRAM_MAX
                            && early.len() < EARLY_MAX
                            && early_bytes + frame.len() <= EARLY_MAX_BYTES
                        {
                            early_bytes += frame.len();
                            early.push(frame.to_vec());
                        }
                    }
                }
            }
        }
    }
}

/// +ciris (leviculum#104): ack the peer's path-MTU probe if it is this
/// session's and arrived whole.
pub(crate) async fn answer_mtu_probe(
    socket: &UdpSocket,
    frame: &[u8],
    src: SocketAddr,
    session: &SessionId,
    token: &[u8; 32],
) {
    if let Some(seq) = wire::parse_mtu_probe(frame, session, token) {
        let ack = wire::punch_frame(PunchKind::Ack, session, token, seq);
        let _ = socket.send_to(&ack, on_socket(socket, src)).await;
    }
}

/// +ciris: `addr` with an IPv4-mapped IPv6 address turned back into IPv4,
/// which is how a dual-stack socket reports an IPv4 peer.
pub(crate) fn canonical(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => SocketAddr::new(v4.into(), v6.port()),
            None => addr,
        },
        SocketAddr::V4(_) => addr,
    }
}

/// +ciris: `addr` in the form `socket` can send to: an IPv4 address goes
/// IPv4-mapped on an IPv6 (dual-stack) socket.
pub(crate) fn on_socket(socket: &UdpSocket, addr: SocketAddr) -> SocketAddr {
    let socket_v6 = socket.local_addr().is_ok_and(|a| a.is_ipv6());
    match addr {
        SocketAddr::V4(v4) if socket_v6 => {
            SocketAddr::new(v4.ip().to_ipv6_mapped().into(), v4.port())
        }
        _ => addr,
    }
}

/// +ciris: the session socket. Dual-stack where the host allows it, so one
/// socket (one NAT mapping, one interface) can punch the peer's IPv4 public
/// address and its global IPv6 address at once; plain IPv4 otherwise.
fn bind_session_socket(server: SocketAddr) -> io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let dual = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP)).and_then(|s| {
        s.set_only_v6(false)?;
        s.bind(&SocketAddr::from(([0u16; 8], 0)).into())?;
        Ok(s)
    });
    let socket: std::net::UdpSocket = match dual {
        Ok(s) => s.into(),
        Err(e) => {
            tracing::debug!("direct link: no dual-stack socket ({e}), IPv4 only");
            let bind = if server.is_ipv4() {
                SocketAddr::from(([0, 0, 0, 0], 0))
            } else {
                SocketAddr::from(([0u16; 8], 0))
            };
            std::net::UdpSocket::bind(bind)?
        }
    };
    socket.set_nonblocking(true)?;
    Ok(socket)
}

/// The punch targets: `peer` first, then each alternate not already there.
fn alloc_targets(peer: SocketAddr, alternates: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut targets = vec![peer];
    for a in alternates {
        if !targets.contains(a) {
            targets.push(*a);
        }
    }
    targets
}

/// +ciris — this socket's host candidates, on its port: the local IPv4
/// address the OS would send from toward `server`, and, on a dual-stack
/// socket, the global IPv6 address it would send from toward the IPv6
/// internet. Each is learned by connecting a throwaway UDP socket, which
/// sends nothing. Unspecified, loopback and non-global IPv6 addresses are
/// left out: no peer on another host can reach them.
fn host_candidates(socket: &UdpSocket, server: SocketAddr) -> Vec<SocketAddr> {
    let Ok(local) = socket.local_addr() else {
        return Vec::new();
    };
    let route = |bind: SocketAddr, toward: SocketAddr| -> Option<std::net::IpAddr> {
        let probe = std::net::UdpSocket::bind(bind).ok()?;
        probe.connect(toward).ok()?;
        Some(probe.local_addr().ok()?.ip())
    };
    let mut out = Vec::new();
    let v4_bind = SocketAddr::from(([0, 0, 0, 0], 0));
    if let Some(ip) = route(v4_bind, canonical(server)).filter(|ip| ip.is_ipv4()) {
        if !ip.is_unspecified() && !ip.is_loopback() {
            out.push(SocketAddr::new(ip, local.port()));
        }
    }
    if local.is_ipv6() {
        // A public resolver's address; only the route to it is looked up.
        let v6_internet: SocketAddr = "[2001:4860:4860::8888]:53".parse().expect("literal");
        let v6_bind = SocketAddr::from(([0u16; 8], 0));
        if let Some(std::net::IpAddr::V6(ip)) = route(v6_bind, v6_internet) {
            if leviculum_core::direct_link::session::is_global_v6(&ip) {
                out.push(SocketAddr::new(ip.into(), local.port()));
            }
        }
    }
    out
}

// Facilitator

/// Answer RNSP and STUN Binding requests on `port`, telling each prober the
/// address its datagram arrived from. Stateless; runs until aborted.
pub(crate) async fn spawn_facilitator(port: u16) -> io::Result<AbortHandle> {
    let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?;
    tracing::info!(
        port = socket.local_addr()?.port(),
        "direct link: facilitator listening"
    );
    let task = tokio::spawn(async move {
        let mut buf = [0u8; 576];
        loop {
            let (len, src) = match socket.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!("direct link: facilitator recv error: {e}");
                    continue;
                }
            };
            let request = &buf[..len];
            let answer = if let Some(nonce) = wire::parse_rnsp_request(request) {
                wire::rnsp_response(&nonce, &src)
            } else if let Some(txn) = wire::parse_stun_binding_request(request) {
                wire::stun_binding_response(&txn, &src)
            } else {
                continue;
            };
            let _ = socket.send_to(&answer, src).await;
        }
    });
    Ok(task.abort_handle())
}

/// Aborts a task when dropped: ties the facilitator's lifetime to the event
/// loop's.
pub(crate) struct AbortOnDrop(pub(crate) AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// Runtime

/// What a probe or punch task reports back to the event loop.
#[derive(Debug)]
pub(crate) enum Outcome {
    Probed {
        session: SessionId,
        public: Option<SocketAddr>,
        /// +ciris: the probe socket's host candidates.
        local: Vec<SocketAddr>,
        /// +ciris: the ports this node predicts its NAT will map toward the
        /// peer (empty: one mapping, or no steady step).
        predicted: Vec<u16>,
        /// +ciris: this node's NAT allocates at random (birthday approach).
        birthday: bool,
    },
    Punched {
        session: SessionId,
        peer: Option<SocketAddr>,
        /// Datagrams the peer sent before this end finished punching.
        early: Vec<Vec<u8>>,
        /// +ciris (leviculum#104): the largest probe step the path carried,
        /// which the interface signals; `None` signals no MTU (base 500).
        mtu: Option<u32>,
        /// +ciris: the birthday socket the peer reached, when the path runs
        /// on it rather than on the session's probe socket.
        socket: Option<Arc<UdpSocket>>,
    },
}

/// The sockets and tasks behind the node's direct-link sessions. Owned by
/// the event loop.
pub(crate) struct DirectLinkRuntime {
    outcomes: mpsc::Sender<Outcome>,
    /// Each session's socket: bound by the probe, reused by the punch, and
    /// finally owned by the interface.
    sockets: HashMap<SessionId, Arc<UdpSocket>>,
    tokens: HashMap<SessionId, [u8; 32]>,
    tasks: HashMap<SessionId, AbortHandle>,
    /// Direct interfaces sent for registration, not yet registered.
    registering: HashMap<InterfaceId, SessionId>,
    /// Live direct interfaces' I/O tasks, by interface index.
    interfaces: HashMap<usize, AbortHandle>,
    /// Repeats of a new direct interface's announces, due at the instant
    /// given. The first announce goes out on registration, possibly while
    /// the peer is still punching, and its punch loop discards anything
    /// that is not a punch frame (Codex review on #74).
    reannounce: Vec<(std::time::Instant, usize)>,
    timing: DirectUdpTiming,
    /// +ciris: sessions whose peer acks sequenced keepalives (both ends run
    /// the two-sided commit), so their interface uses the fast liveness.
    fast_liveness: HashMap<SessionId, bool>,
    /// +ciris: sessions whose own NAT allocates at random, from the probe:
    /// their punch holds birthday sockets open.
    birthday: HashMap<SessionId, bool>,
    /// +ciris (review item 6): the second observer the session socket also
    /// probes, to tell an endpoint-dependent NAT from one that keeps a
    /// mapping per socket. `None`: no "q", no "b".
    observer2: Option<(String, ProbeProtocol)>,
}

impl DirectLinkRuntime {
    pub(crate) fn new(outcomes: mpsc::Sender<Outcome>) -> Self {
        DirectLinkRuntime {
            outcomes,
            sockets: HashMap::new(),
            tokens: HashMap::new(),
            tasks: HashMap::new(),
            registering: HashMap::new(),
            interfaces: HashMap::new(),
            reannounce: Vec::new(),
            timing: DirectUdpTiming::default(),
            fast_liveness: HashMap::new(),
            birthday: HashMap::new(),
            observer2: None,
        }
    }

    /// +ciris (review item 6): the second observer, from the settings.
    pub(crate) fn set_second_observer(&mut self, observer: Option<(String, ProbeProtocol)>) {
        self.observer2 = observer;
    }

    /// Start (or stop) whatever a node job asks for.
    pub(crate) fn run(&mut self, job: DirectLinkJob) {
        match job {
            DirectLinkJob::Probe {
                session,
                server,
                protocol,
            } => self.start_probe(session, server, protocol),
            DirectLinkJob::Punch {
                session,
                peer,
                alternates,
                predicted,
                token,
                start_delay_ms,
                fast_liveness,
                spray,
            } => {
                self.fast_liveness.insert(session, fast_liveness);
                self.start_punch(
                    session,
                    peer,
                    alternates,
                    predicted,
                    token,
                    Duration::from_millis(start_delay_ms),
                    spray,
                )
            }
            DirectLinkJob::Release { session } => self.release(&session),
            DirectLinkJob::CloseInterface { interface_index } => {
                if let Some(task) = self.interfaces.remove(&interface_index) {
                    tracing::info!("direct link: closing direct interface {interface_index}");
                    task.abort();
                }
            }
        }
    }

    fn start_probe(&mut self, session: SessionId, server: SocketAddr, protocol: ProbeProtocol) {
        let outcomes = self.outcomes.clone();
        let socket = match bind_session_socket(server).and_then(UdpSocket::from_std) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                tracing::warn!("direct link: cannot bind a probe socket: {e}");
                let _ = outcomes.try_send(Outcome::Probed {
                    session,
                    public: None,
                    local: Vec::new(),
                    predicted: Vec::new(),
                    birthday: false,
                });
                return;
            }
        };
        self.sockets.insert(session, Arc::clone(&socket));
        let observer2 = self.observer2.clone();
        let task = tokio::spawn(async move {
            let probe_start = tokio::time::Instant::now();
            let public = match probe(&socket, server, protocol).await {
                Ok(public) => {
                    tracing::info!("direct link: the facilitator answered the probe");
                    Some(public)
                }
                Err(e) => {
                    tracing::info!("direct link: probe failed: {e}");
                    None
                }
            };
            let local = host_candidates(&socket, server);
            // +ciris (review item 6): the same session socket toward a second
            // observer, when one is configured; never a third party chosen
            // here (no STUN default).
            let second = match (&observer2, public) {
                (Some((addr2, protocol2)), Some(_)) => match resolve_facilitator(addr2).await {
                    Ok(server2) if canonical(server2) != canonical(server) => {
                        probe_with(&socket, server2, *protocol2, 2, ALLOCATION_DEADLINE / 2)
                            .await
                            .ok()
                    }
                    _ => None,
                },
                _ => None,
            };
            let class = classify_nat(public, second, &local);
            let steps = match (class, public) {
                (NatClass::EndpointDependent, Some(p)) => {
                    allocation_steps(server, protocol, p.port()).await
                }
                _ => None,
            };
            let (predicted, birthday) = advertise(class, steps);
            log_probe_diag(
                &session,
                &socket,
                public,
                &local,
                class,
                steps,
                predicted.len(),
                birthday,
                probe_start.elapsed().as_millis() as u64,
            );
            let _ = outcomes
                .send(Outcome::Probed {
                    session,
                    public,
                    local,
                    predicted,
                    birthday,
                })
                .await;
        });
        self.replace_task(session, task.abort_handle());
    }

    #[allow(clippy::too_many_arguments)]
    fn start_punch(
        &mut self,
        session: SessionId,
        peer: SocketAddr,
        alternates: Vec<SocketAddr>,
        predicted: Vec<SocketAddr>,
        token: [u8; 32],
        start_delay: Duration,
        spray: bool,
    ) {
        let outcomes = self.outcomes.clone();
        let birthday = self.birthday.remove(&session).unwrap_or(false);
        let Some(socket) = self.sockets.get(&session).cloned() else {
            // No probe socket (it failed to bind, or was released): there is
            // no NAT mapping to punch from.
            let _ = outcomes.try_send(Outcome::Punched {
                session,
                peer: None,
                early: Vec::new(),
                mtu: None,
                socket: None,
            });
            return;
        };
        self.tokens.insert(session, token);
        let task = tokio::spawn(async move {
            // The responder waits out the READY's trip to the initiator, so
            // both ends start together.
            if !start_delay.is_zero() {
                tokio::time::sleep(start_delay).await;
            }
            let targets = alloc_targets(peer, &alternates);
            // Counts only: no address in a log line (review item 9).
            tracing::info!(
                targets = targets.len(),
                spray,
                birthday,
                "direct link: punching"
            );
            let seen = punch_with(
                &socket, birthday, &targets, &predicted, &session, &token, spray,
            )
            .await;
            match &seen {
                Some((_, winner)) => tracing::info!(
                    on_birthday_socket = winner.is_some(),
                    "direct link: punched through"
                ),
                None => tracing::info!("direct link: punch failed"),
            }
            let (peer, early, mtu, winner) = match seen {
                Some((mut p, winner)) => {
                    let on = winner.as_deref().unwrap_or(&socket);
                    let mtu = probe_path_mtu(on, p.peer, &session, &token, &mut p.early).await;
                    tracing::info!("direct link: the path carries {mtu:?}");
                    (Some(p.peer), p.early, mtu, winner)
                }
                None => (None, Vec::new(), None, None),
            };
            let _ = outcomes
                .send(Outcome::Punched {
                    session,
                    peer,
                    early,
                    mtu,
                    socket: winner,
                })
                .await;
        });
        self.replace_task(session, task.abort_handle());
    }

    /// +ciris: the probe found this session's NAT to be a random allocator
    /// (`birthday`); its punch holds birthday sockets open.
    pub(crate) fn note_birthday(&mut self, session: SessionId, birthday: bool) {
        // Only for a session still holding its socket: one released while
        // its probe ran would never be removed again (review item 9).
        if birthday && self.sockets.contains_key(&session) {
            self.birthday.insert(session, true);
        }
    }

    fn replace_task(&mut self, session: SessionId, task: AbortHandle) {
        if let Some(old) = self.tasks.insert(session, task) {
            old.abort();
        }
    }

    fn release(&mut self, session: &SessionId) {
        if let Some(task) = self.tasks.remove(session) {
            task.abort();
        }
        self.sockets.remove(session);
        self.tokens.remove(session);
        self.fast_liveness.remove(session);
        self.birthday.remove(session);
    }

    /// The punch for `session` succeeded toward `peer`: build its interface
    /// under `id`. The caller sends the handle for registration and, once it
    /// is registered, reports the index to the node via
    /// [`Self::registered`].
    pub(crate) fn interface_for(
        &mut self,
        session: SessionId,
        peer: SocketAddr,
        early: Vec<Vec<u8>>,
        hw_mtu: Option<u32>,
        id: InterfaceId,
        winner: Option<Arc<UdpSocket>>,
    ) -> Option<InterfaceHandle> {
        self.tasks.remove(&session);
        let probe_socket = self.sockets.remove(&session)?;
        // +ciris: a birthday socket the peer reached carries the path.
        let socket = winner.unwrap_or(probe_socket);
        let token = self.tokens.remove(&session)?;
        let timing = if self.fast_liveness.remove(&session).unwrap_or(false) {
            DirectUdpTiming::fast()
        } else {
            self.timing
        };
        let (handle, task) =
            spawn_direct_udp_interface(id, socket, peer, session, token, early, timing, hw_mtu);
        self.registering.insert(id, session);
        self.interfaces.insert(id.0, task);
        Some(handle)
    }

    /// A handle just got registered; if it was a direct interface, the
    /// session it belongs to.
    pub(crate) fn registered(&mut self, id: InterfaceId) -> Option<SessionId> {
        let session = self.registering.remove(&id)?;
        let now = std::time::Instant::now();
        for delay in REANNOUNCE_AFTER {
            self.reannounce.push((now + delay, id.0));
        }
        Some(session)
    }

    /// Direct interfaces whose announces are due again: still live, and
    /// past their repeat time.
    pub(crate) fn take_due_reannounces(&mut self, now: std::time::Instant) -> Vec<usize> {
        let mut due = Vec::new();
        self.reannounce.retain(|(at, index)| {
            if *at <= now {
                due.push(*index);
                false
            } else {
                true
            }
        });
        due.retain(|index| self.interfaces.contains_key(index));
        due
    }

    /// An interface disconnected; forget its task if it was a direct one.
    pub(crate) fn interface_gone(&mut self, id: InterfaceId) {
        self.interfaces.remove(&id.0);
        self.registering.remove(&id);
        self.reannounce.retain(|(_, index)| *index != id.0);
    }
}

impl Drop for DirectLinkRuntime {
    fn drop(&mut self) {
        for task in self.tasks.values().chain(self.interfaces.values()) {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A knock budget of a test's own: the process one is shared by every
    /// test running at once, and its per-IP cooldown would refuse each
    /// loopback spray after the first.
    fn test_budget() -> &'static KnockBudget {
        Box::leak(Box::new(KnockBudget::new()))
    }

    async fn loopback() -> UdpSocket {
        UdpSocket::bind("127.0.0.1:0").await.unwrap()
    }

    #[tokio::test]
    async fn a_probe_learns_its_address_from_our_facilitator_both_protocols() {
        let facilitator = loopback().await;
        let port = facilitator.local_addr().unwrap().port();
        drop(facilitator);
        let task = spawn_facilitator(port).await.unwrap();
        let server: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        for protocol in [ProbeProtocol::Rnsp, ProbeProtocol::Stun] {
            let socket = loopback().await;
            let public = probe(&socket, server, protocol).await.unwrap();
            assert_eq!(public, socket.local_addr().unwrap(), "{protocol:?}");
        }
        task.abort();
    }

    #[test]
    fn heard_relation_classifies_each_case() {
        let public: SocketAddr = "203.0.113.7:40000".parse().unwrap();
        let host_v4: SocketAddr = "192.168.1.20:40000".parse().unwrap();
        let host_v6: SocketAddr = "[2001:db8::20]:40000".parse().unwrap();
        let host = [host_v4, host_v6];
        let rel = |s: &str| heard_relation(s.parse().unwrap(), public, &host);
        assert_eq!(rel("203.0.113.7:40000"), HeardRelation::Same);
        assert_eq!(rel("203.0.113.7:51234"), HeardRelation::SameIpOtherPort);
        assert_eq!(rel("192.168.1.20:40000"), HeardRelation::HostCandidate);
        assert_eq!(rel("[2001:db8::20]:40000"), HeardRelation::HostCandidate);
        assert_eq!(rel("[2001:db8::20]:40001"), HeardRelation::HostIpOtherPort);
        assert_eq!(rel("198.51.100.9:40000"), HeardRelation::OtherIp);
        // A dual-stack socket reports an IPv4 peer IPv4-mapped.
        assert_eq!(rel("[::ffff:203.0.113.7]:40000"), HeardRelation::Same);
        assert_eq!(
            rel("[::ffff:203.0.113.7]:40002"),
            HeardRelation::SameIpOtherPort
        );
        assert_eq!(
            heard_relation("[::ffff:203.0.113.7]:40000".parse().unwrap(), public, &[]),
            HeardRelation::Same
        );
        assert_eq!(
            HeardRelation::SameIpOtherPort.as_str(),
            "same_ip_other_port"
        );
    }

    #[tokio::test]
    async fn the_punch_diag_counts_heard_and_acked_by_class() {
        let public: SocketAddr = "203.0.113.7:40000".parse().unwrap();
        let host: SocketAddr = "[2001:db8::20]:40000".parse().unwrap();
        let predicted: SocketAddr = "203.0.113.7:40017".parse().unwrap();
        let mut diag = PunchDiag::new(&[public, host], &[predicted], [0u8; 16]);
        assert_eq!((diag.host_v4, diag.host_v6, diag.predicted), (0, 1, 1));
        diag.heard(predicted);
        assert_eq!(diag.heard[0].1, HeardRelation::Predicted);
        diag.heard.clear();
        diag.punches_heard = 0;
        let remapped: SocketAddr = "203.0.113.7:50000".parse().unwrap();
        diag.heard(remapped);
        diag.heard(remapped);
        diag.acked(public, true);
        diag.acked(remapped, false);
        assert_eq!(diag.punches_heard, 2);
        assert_eq!(diag.heard, vec![(remapped, HeardRelation::SameIpOtherPort)]);
        assert_eq!(diag.acked, vec![(public, HeardRelation::Same)]);
        assert_eq!(diag.acks_unpunched, 1);
        assert_eq!(PunchDiag::classes(&diag.heard), "[same_ip_other_port]");
        assert!(diag.first_heard_ms.is_some() && diag.first_acked_ms.is_some());
        diag.finish("punched");
        assert!(diag.logged, "dropping it now logs nothing more");
    }

    fn steps(base: u16, p1: u16, p2: u16) -> AllocationSteps {
        AllocationSteps {
            base,
            last: p2,
            step1: i32::from(p1) - i32::from(base),
            step2: i32::from(p2) - i32::from(p1),
        }
    }

    #[test]
    fn a_steady_step_predicts_the_next_allocations() {
        let ports = predict_ports(steps(40000, 40001, 40002));
        assert_eq!(ports.len(), wire::MAX_PREDICTED_PORTS);
        assert_eq!(ports[0], 40003);
        assert_eq!(
            *ports.last().unwrap(),
            40002 + wire::MAX_PREDICTED_PORTS as u16
        );
        // Other hosts allocating between the probes: the smaller step, so
        // the range still covers the next allocations.
        assert_eq!(
            predict_ports(steps(40000, 40002, 40005))[..2],
            [40007, 40009]
        );
        // Downward allocation.
        assert_eq!(predict_ports(steps(40010, 40008, 40006))[0], 40004);
    }

    #[test]
    fn no_prediction_without_a_steady_step() {
        // One mapping for every socket's port (or a measurement that saw
        // the same port twice): nothing to predict.
        assert!(predict_ports(steps(40000, 40000, 40001)).is_empty());
        // Random allocation.
        assert!(predict_ports(steps(40000, 51234, 33310)).is_empty());
        assert!(predict_ports(steps(40000, 40100, 40200)).is_empty());
        // Steps of opposite sign.
        assert!(predict_ports(steps(40000, 40003, 40001)).is_empty());
        // Never below 1024, never past 65535.
        assert!(predict_ports(steps(65530, 65532, 65534))
            .iter()
            .all(|p| *p > 65534));
        assert!(predict_ports(steps(1030, 1028, 1026))
            .iter()
            .all(|p| *p >= 1024));
    }

    /// The hard side of a pair, seen through the easy side's filter: its
    /// tokened punches from its real mapping only get through once the easy
    /// side has sent there (a knock or a punch opened the easy side's
    /// address-and-port filter). Until then it is silent; after, it punches
    /// back every tick and acks, as the hard side's session socket does.
    async fn filtered_peer(socket: UdpSocket, session: SessionId, token: [u8; 32]) {
        let mut buf = [0u8; 256];
        let until = tokio::time::Instant::now() + PUNCH_WINDOW + Duration::from_secs(1);
        let mut toward: Option<SocketAddr> = None;
        let mut tick = tokio::time::interval(PUNCH_INTERVAL);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(until) => return,
                _ = tick.tick() => if let Some(to) = toward {
                    let frame = wire::punch_frame(PunchKind::Punch, &session, &token, 1);
                    let _ = socket.send_to(&frame, to).await;
                },
                r = socket.recv_from(&mut buf) => {
                    let Ok((len, src)) = r else { continue };
                    let frame = &buf[..len];
                    if wire::is_knock(frame) {
                        toward.get_or_insert(src);
                    } else if let Some((PunchKind::Punch, seq)) =
                        wire::parse_punch_frame(frame, &session, &token)
                    {
                        toward.get_or_insert(src);
                        let ack = wire::punch_frame(PunchKind::Ack, &session, &token, seq);
                        let _ = socket.send_to(&ack, src).await;
                    }
                }
            }
        }
    }

    /// +ciris: the peer's advertised address is not where its NAT maps
    /// toward this end (endpoint-dependent mapping), and its filter drops
    /// everything until this end has sent to the real one. Only a predicted
    /// port gets there.
    #[tokio::test]
    async fn a_predicted_port_punches_a_peer_its_advertised_address_cannot() {
        let a = loopback().await;
        let b = loopback().await;
        let b_addr = b.local_addr().unwrap();
        // The peer's facilitator mapping: same IP, another port.
        let advertised = loopback().await.local_addr().unwrap();
        let (session, token) = ([7u8; 16], [8u8; 32]);
        let peer = tokio::spawn(filtered_peer(b, session, token));
        let result = punch_with_ports(
            &a,
            None,
            std::slice::from_ref(&advertised),
            std::slice::from_ref(&b_addr),
            &session,
            &token,
            None,
            test_budget(),
        )
        .await
        .map(|(p, _)| p);
        peer.abort();
        assert_eq!(result.map(|p| p.peer), Some(b_addr));
        assert_eq!(
            heard_relation(b_addr, advertised, &[b_addr]),
            HeardRelation::Predicted
        );
    }

    /// Without the prediction the same peer is never reached: the deadlock
    /// the runners showed (heard 0, rx 0).
    #[tokio::test]
    async fn without_the_prediction_the_filtered_peer_is_never_heard() {
        let a = loopback().await;
        let b = loopback().await;
        let advertised = loopback().await.local_addr().unwrap();
        let (session, token) = ([9u8; 16], [10u8; 32]);
        let peer = tokio::spawn(filtered_peer(b, session, token));
        let result = punch(&a, std::slice::from_ref(&advertised), &session, &token).await;
        peer.abort();
        assert_eq!(result, None);
    }

    /// A spray source that yields `ports` in turn, forever.
    fn cycle(ports: Vec<u16>) -> Option<SprayPorts> {
        let mut i = 0usize;
        Some(Box::new(move || {
            let p = ports[i % ports.len()];
            i += 1;
            p
        }))
    }

    /// +ciris: the peer's NAT is random, so its real mapping is no address
    /// it advertised or predicted, and its filter drops everything until it
    /// is punched there. The spray finds it.
    #[tokio::test]
    async fn the_spray_reaches_a_filtered_peer_on_an_unadvertised_port() {
        let a = loopback().await;
        let b = loopback().await;
        let b_addr = b.local_addr().unwrap();
        let advertised = loopback().await.local_addr().unwrap();
        let (session, token) = ([13u8; 16], [14u8; 32]);
        let peer = tokio::spawn(filtered_peer(b, session, token));
        // Misses first, then the real mapping.
        let mut ports: Vec<u16> = (0..40u16).map(|i| 50_000 + i).collect();
        ports.retain(|p| *p != b_addr.port());
        ports.push(b_addr.port());
        let result = punch_with_ports(
            &a,
            None,
            std::slice::from_ref(&advertised),
            &[],
            &session,
            &token,
            cycle(ports),
            test_budget(),
        )
        .await;
        peer.abort();
        assert_eq!(
            result.map(|(p, w)| (p.peer, w.is_none())),
            Some((b_addr, true))
        );
    }

    /// +ciris: a pair that reaches each other at once never escalates: a
    /// bystander that both the spray and a predicted port point at hears
    /// nothing, though both were armed.
    #[tokio::test]
    async fn an_easy_pair_never_sprays_or_punches_predicted_ports() {
        let a = loopback().await;
        let b = loopback().await;
        let bystander = loopback().await;
        let (a_addr, b_addr, by) = (
            a.local_addr().unwrap(),
            b.local_addr().unwrap(),
            bystander.local_addr().unwrap(),
        );
        let (session, token) = ([17u8; 16], [18u8; 32]);
        let (ra, rb) = tokio::join!(
            punch_with_ports(
                &a,
                None,
                std::slice::from_ref(&b_addr),
                std::slice::from_ref(&by),
                &session,
                &token,
                cycle(vec![by.port()]),
                test_budget()
            ),
            punch(&b, std::slice::from_ref(&a_addr), &session, &token)
        );
        assert_eq!(
            (ra.map(|(p, _)| p.peer), rb.map(|p| p.peer)),
            (Some(b_addr), Some(a_addr))
        );
        // Past the escalation point, in case anything was still due.
        let mut buf = [0u8; 256];
        let heard = tokio::time::timeout(
            ESCALATE_AFTER + Duration::from_millis(300),
            bystander.recv_from(&mut buf),
        )
        .await;
        assert!(heard.is_err(), "the bystander was punched");
    }

    /// +ciris: a hard side whose punch the peer's REJECT aborts (the session
    /// drops it mid-window) still knows how many birthday sockets it held,
    /// which is what its "aborted" line logs: the count is written the moment
    /// they are bound, not after the birthday block ends.
    #[tokio::test]
    async fn an_aborted_birthday_punch_still_logs_its_sockets() {
        let b = loopback().await;
        let nobody = loopback().await.local_addr().unwrap();
        let mut extras = Vec::new();
        for _ in 0..5 {
            extras.push(Arc::new(loopback().await));
        }
        let (session, token) = ([19u8; 16], [20u8; 32]);
        let mut diag = PunchDiag::new(std::slice::from_ref(&nobody), &[], session);
        let now = tokio::time::Instant::now();
        let punching = punch_birthday(
            &b,
            Extras::Given(extras),
            std::slice::from_ref(&nobody),
            &[],
            &session,
            &token,
            &mut diag,
            now + PUNCH_WINDOW,
            now + ESCALATE_AFTER,
            None,
            test_budget(),
        );
        // Past BIRTHDAY_AFTER, then dropped as the session does on REJECT.
        let r = tokio::time::timeout(BIRTHDAY_AFTER + Duration::from_millis(500), punching).await;
        assert!(r.is_err(), "still punching when dropped");
        assert_eq!(diag.birthday_sockets(), 5);
        // Five sockets, one punch each every BIRTHDAY_REFRESH (500 ms).
        assert_eq!(diag.birthday_pps(), 10);
        assert!(!diag.logged, "the drop is what logs it, as \"aborted\"");
    }

    /// +ciris (found by docs/direct_link/DirectLinkPunch.tla): once the
    /// session socket has heard the peer, a later birthday hit must not move
    /// the punch, or the two ends settle on different socket pairs. Here the
    /// peer reaches the session socket and, 20 ms later, a birthday socket.
    #[tokio::test]
    async fn a_birthday_hit_after_the_session_socket_heard_does_not_switch() {
        let a = Arc::new(loopback().await);
        let a_addr = a.local_addr().unwrap();
        let b = loopback().await;
        let b_addr = b.local_addr().unwrap();
        let extra = Arc::new(loopback().await);
        let extra_addr = extra.local_addr().unwrap();
        let (session, token) = ([21u8; 16], [22u8; 32]);
        let peer = {
            let a = Arc::clone(&a);
            tokio::spawn(async move {
                // Silent past BIRTHDAY_AFTER, so B opens its birthday socket.
                tokio::time::sleep(BIRTHDAY_AFTER + Duration::from_millis(150)).await;
                let punch = wire::punch_frame(PunchKind::Punch, &session, &token, 1);
                let _ = a.send_to(&punch, b_addr).await;
                tokio::time::sleep(Duration::from_millis(20)).await;
                let _ = a.send_to(&punch, extra_addr).await;
                // Nothing acked yet: the hit lands while the session socket
                // has heard but not been acked. Answer only from here on, as
                // a peer a round trip away would.
                tokio::time::sleep(Duration::from_millis(100)).await;
                let mut buf = [0u8; 256];
                loop {
                    let Ok((len, src)) = a.recv_from(&mut buf).await else {
                        continue;
                    };
                    if let Some((PunchKind::Punch, seq)) =
                        wire::parse_punch_frame(&buf[..len], &session, &token)
                    {
                        let ack = wire::punch_frame(PunchKind::Ack, &session, &token, seq);
                        let _ = a.send_to(&ack, src).await;
                        let _ = a.send_to(&punch, src).await;
                    }
                }
            })
        };
        let r = punch_with_ports(
            &b,
            Some(Extras::Given(vec![extra])),
            std::slice::from_ref(&a_addr),
            &[],
            &session,
            &token,
            None,
            test_budget(),
        )
        .await;
        peer.abort();
        let (p, winner) = r.expect("B punches through");
        assert_eq!(p.peer, a_addr);
        assert!(
            winner.is_none(),
            "B moved off the socket the peer reached first"
        );
    }

    /// +ciris (review item 1): the spray reaches ports that may be anyone's.
    /// A neighbour on a sprayed port gets only knocks (no token, no session),
    /// and even one that somehow had the token cannot make this end commit
    /// to it with an ack: a sprayed port is never "ours".
    #[tokio::test]
    async fn a_neighbour_on_a_sprayed_port_learns_nothing_and_cannot_hijack() {
        let a = loopback().await;
        let neighbour = Arc::new(loopback().await);
        let n_addr = neighbour.local_addr().unwrap();
        let advertised = loopback().await.local_addr().unwrap();
        let (session, token) = ([23u8; 16], [24u8; 32]);
        let seen = Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
        let task = {
            let (neighbour, seen) = (Arc::clone(&neighbour), Arc::clone(&seen));
            tokio::spawn(async move {
                let mut buf = [0u8; 256];
                loop {
                    let Ok((len, src)) = neighbour.recv_from(&mut buf).await else {
                        continue;
                    };
                    seen.lock().unwrap().push(buf[..len].to_vec());
                    // A hostile neighbour, given the token anyway, acks.
                    let ack = wire::punch_frame(PunchKind::Ack, &session, &token, 0);
                    let _ = neighbour.send_to(&ack, src).await;
                }
            })
        };
        let result = punch_with_ports(
            &a,
            None,
            std::slice::from_ref(&advertised),
            &[],
            &session,
            &token,
            cycle(vec![n_addr.port()]),
            test_budget(),
        )
        .await;
        task.abort();
        assert!(result.is_none(), "committed to a neighbour");
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty(), "the spray reached the neighbour");
        assert!(seen.iter().all(|f| wire::is_knock(f)), "only knocks");
        assert!(seen
            .iter()
            .all(|f| !f.windows(4).any(|w| w == &token[..4] || w == &session[..4])));
    }

    /// +ciris (review item 2): one spray per target IP per cooldown, and a
    /// global ceiling on knocks a second.
    #[test]
    fn the_knock_budget_limits_sprays_per_ip_and_knocks_per_second() {
        let budget = KnockBudget::new();
        let t0 = std::time::Instant::now();
        let ip: std::net::IpAddr = "198.51.100.7".parse().unwrap();
        let other: std::net::IpAddr = "198.51.100.8".parse().unwrap();
        assert!(budget.spray_permit(ip, t0));
        assert!(
            !budget.spray_permit(ip, t0 + Duration::from_secs(60)),
            "within the cooldown"
        );
        assert!(budget.spray_permit(other, t0), "another IP");
        assert!(budget.spray_permit(ip, t0 + SPRAY_IP_COOLDOWN), "after it");
        let taken = (0..2 * KNOCK_GLOBAL_PPS)
            .filter(|_| budget.take(t0))
            .count();
        assert_eq!(taken as u32, KNOCK_GLOBAL_PPS, "one bucket's worth at once");
        assert!(
            budget.take(t0 + Duration::from_millis(10)),
            "refills with time"
        );
    }

    /// +ciris (review item 3): birthday sockets are counted process-wide; with
    /// no permit left there is no birthday phase, not more sockets.
    #[tokio::test]
    async fn no_birthday_sockets_without_a_permit() {
        let sem = Arc::new(tokio::sync::Semaphore::new(3));
        let peer: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let (some, permits) = birthday_sockets(peer, &sem).await;
        assert_eq!((some.len(), permits.len()), (3, 3), "as many as permits");
        let (none, _) = birthday_sockets(peer, &sem).await;
        assert!(none.is_empty(), "the permits are held");
        drop((some, permits));
        assert_eq!(sem.available_permits(), 3, "released with the sockets");
    }

    /// +ciris (review item 4): no birthday socket at all (none bound, no
    /// permit) leaves the session socket punching, not a dead punch.
    #[tokio::test]
    async fn a_birthday_phase_with_no_socket_still_punches_on() {
        let a = Arc::new(loopback().await);
        let a_addr = a.local_addr().unwrap();
        let b = loopback().await;
        let b_addr = b.local_addr().unwrap();
        let (session, token) = ([25u8; 16], [26u8; 32]);
        let peer = {
            let a = Arc::clone(&a);
            tokio::spawn(async move {
                // Silent past BIRTHDAY_AFTER, then an ordinary peer.
                tokio::time::sleep(BIRTHDAY_AFTER + Duration::from_millis(300)).await;
                let punch = wire::punch_frame(PunchKind::Punch, &session, &token, 1);
                let _ = a.send_to(&punch, b_addr).await;
                let mut buf = [0u8; 256];
                loop {
                    let Ok((len, src)) = a.recv_from(&mut buf).await else {
                        continue;
                    };
                    if let Some((PunchKind::Punch, seq)) =
                        wire::parse_punch_frame(&buf[..len], &session, &token)
                    {
                        let ack = wire::punch_frame(PunchKind::Ack, &session, &token, seq);
                        let _ = a.send_to(&ack, src).await;
                    }
                }
            })
        };
        let r = punch_with_ports(
            &b,
            Some(Extras::Given(Vec::new())),
            std::slice::from_ref(&a_addr),
            &[],
            &session,
            &token,
            None,
            test_budget(),
        )
        .await;
        peer.abort();
        assert_eq!(r.map(|(p, w)| (p.peer, w.is_none())), Some((a_addr, true)));
    }

    /// +ciris (review item 6): heard already at the birthday gate, the punch
    /// opens no birthday socket.
    #[tokio::test]
    async fn no_birthday_sockets_once_the_session_socket_has_heard() {
        let a = Arc::new(loopback().await);
        let a_addr = a.local_addr().unwrap();
        let b = loopback().await;
        let b_addr = b.local_addr().unwrap();
        let mut extras = Vec::new();
        for _ in 0..3 {
            extras.push(Arc::new(loopback().await));
        }
        let (session, token) = ([27u8; 16], [28u8; 32]);
        // The peer punches at once but never acks: heard, not done.
        let punch = wire::punch_frame(PunchKind::Punch, &session, &token, 1);
        let _ = a.send_to(&punch, b_addr).await;
        let mut diag = PunchDiag::new(std::slice::from_ref(&a_addr), &[], session);
        let now = tokio::time::Instant::now();
        let punching = punch_birthday(
            &b,
            Extras::Given(extras),
            std::slice::from_ref(&a_addr),
            &[],
            &session,
            &token,
            &mut diag,
            now + PUNCH_WINDOW,
            now + ESCALATE_AFTER,
            None,
            test_budget(),
        );
        let _ = tokio::time::timeout(BIRTHDAY_AFTER + Duration::from_millis(400), punching).await;
        assert_eq!(diag.birthday_sockets(), 0);
    }

    /// +ciris (review item 6): only the SAME socket toward two observers
    /// says whether a NAT maps per destination, and only that decides "q"
    /// or "b".
    #[test]
    fn the_nat_class_needs_two_observers_and_decides_what_is_advertised() {
        let m = |p: u16| SocketAddr::new("203.0.113.9".parse().unwrap(), p);
        let host: SocketAddr = "192.168.1.4:40000".parse().unwrap();
        assert_eq!(
            classify_nat(Some(m(40000)), None, &[host]),
            NatClass::Unknown
        );
        assert_eq!(classify_nat(None, Some(m(1)), &[host]), NatClass::Unknown);
        assert_eq!(
            classify_nat(Some(m(40000)), Some(m(40000)), &[host]),
            NatClass::EndpointIndependent
        );
        assert_eq!(
            classify_nat(Some(m(40000)), Some(m(40001)), &[host]),
            NatClass::EndpointDependent
        );
        let public: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        assert_eq!(classify_nat(Some(public), None, &[public]), NatClass::NoNat);
        let steady = steps_from(40000, 40001, 40002);
        let random = steps_from(40000, 51234, 33310);
        // An EIM NAT that randomises per socket: random steps, but no "b".
        assert_eq!(
            advertise(NatClass::EndpointIndependent, Some(random)),
            (vec![], false)
        );
        assert_eq!(advertise(NatClass::Unknown, Some(random)), (vec![], false));
        assert!(advertise(NatClass::EndpointDependent, Some(random)).1);
        let (q, b) = advertise(NatClass::EndpointDependent, Some(steady));
        assert!(!q.is_empty() && !b);
        assert_eq!(
            advertise(NatClass::EndpointDependent, None),
            (vec![], false)
        );
    }

    /// The two allocation probes run at once: order them by distance from
    /// the base, so a steady allocator reads steady either way round.
    #[test]
    fn parallel_allocation_steps_read_the_same_either_way_round() {
        assert_eq!(
            steps_from(40000, 40002, 40001),
            steps_from(40000, 40001, 40002)
        );
        let s = steps_from(40000, 40002, 40001);
        assert_eq!((s.step1, s.step2, s.last), (1, 1, 40002));
    }

    /// +ciris (review item 8): a silent facilitator ends the allocation
    /// measurement within its deadline, with no prediction.
    #[tokio::test]
    async fn the_allocation_measurement_keeps_its_deadline() {
        let silent = loopback().await;
        let started = tokio::time::Instant::now();
        let steps =
            allocation_steps(silent.local_addr().unwrap(), ProbeProtocol::Rnsp, 40000).await;
        assert!(steps.is_none());
        assert!(started.elapsed() < ALLOCATION_DEADLINE + Duration::from_millis(300));
    }

    /// +ciris: behind a random NAT, the punch runs on whichever birthday
    /// socket the peer reaches; both ends settle on that one pair, and the
    /// socket comes back for the interface.
    #[tokio::test]
    async fn a_birthday_socket_the_peer_reaches_carries_the_path() {
        let a = loopback().await;
        let a_addr = a.local_addr().unwrap();
        // B's probe socket cannot reach A at all (IPv6-only, A is IPv4): only
        // a birthday socket can.
        let Ok(b) = UdpSocket::bind("[::1]:0").await else {
            return;
        };
        let mut extras = Vec::new();
        for _ in 0..8 {
            extras.push(Arc::new(loopback().await));
        }
        let extra_addrs: Vec<SocketAddr> = extras.iter().map(|e| e.local_addr().unwrap()).collect();
        let b_advertised = b.local_addr().unwrap();
        let (session, token) = ([15u8; 16], [16u8; 32]);
        let spray_ports: Vec<u16> = extra_addrs.iter().map(|a| a.port()).collect();
        // A knows B only by its advertised port, on the IPv4 side.
        let to_b = [SocketAddr::new(a_addr.ip(), b_advertised.port())];
        // A acks late punches once it is through, as its direct interface
        // does.
        let a_side = async {
            let r = punch_with_ports(
                &a,
                None,
                &to_b,
                &[],
                &session,
                &token,
                cycle(spray_ports),
                test_budget(),
            )
            .await;
            let mut buf = [0u8; 256];
            let until = tokio::time::Instant::now() + Duration::from_secs(3);
            while let Ok(Ok((len, src))) =
                tokio::time::timeout_at(until, a.recv_from(&mut buf)).await
            {
                if let Some((PunchKind::Punch, seq)) =
                    wire::parse_punch_frame(&buf[..len], &session, &token)
                {
                    let ack = wire::punch_frame(PunchKind::Ack, &session, &token, seq);
                    let _ = a.send_to(&ack, src).await;
                }
            }
            r
        };
        let (ra, rb) = tokio::join!(
            a_side,
            punch_with_ports(
                &b,
                Some(Extras::Given(extras)),
                std::slice::from_ref(&a_addr),
                &[],
                &session,
                &token,
                None,
                test_budget()
            )
        );
        let (pa, _) = ra.expect("A punches through");
        let (pb, winner) = rb.expect("B punches through");
        let winner = winner.expect("on a birthday socket");
        assert_eq!(pb.peer, a_addr);
        assert_eq!(pa.peer, winner.local_addr().unwrap());
        assert!(extra_addrs.contains(&pa.peer));
    }

    /// Heard at its advertised address, the peer's mapping did not move: its
    /// predicted ports stop being punched, and the punch still completes.
    #[tokio::test]
    async fn predicted_targets_are_dropped_once_the_peer_is_heard_where_it_said() {
        let a = loopback().await;
        let b = loopback().await;
        let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        // B predicts two ports on its own IP that nobody holds.
        let spare1 = loopback().await.local_addr().unwrap();
        let spare2 = loopback().await.local_addr().unwrap();
        let (session, token) = ([11u8; 16], [12u8; 32]);
        let spares = [spare1, spare2];
        let (ra, rb) = tokio::join!(
            async {
                punch_with_ports(
                    &a,
                    None,
                    std::slice::from_ref(&b_addr),
                    &spares,
                    &session,
                    &token,
                    None,
                    test_budget(),
                )
                .await
                .map(|(p, _)| p)
            },
            punch(&b, std::slice::from_ref(&a_addr), &session, &token)
        );
        assert_eq!(
            (ra.map(|p| p.peer), rb.map(|p| p.peer)),
            (Some(b_addr), Some(a_addr))
        );
    }

    #[tokio::test]
    async fn a_probe_with_no_answer_fails() {
        let silent = loopback().await;
        let socket = loopback().await;
        let started = tokio::time::Instant::now();
        let err = probe(&socket, silent.local_addr().unwrap(), ProbeProtocol::Rnsp)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= PROBE_WAIT * PROBE_ATTEMPTS);
    }

    #[tokio::test]
    async fn two_sockets_punch_through_to_each_other() {
        let a = loopback().await;
        let b = loopback().await;
        let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        let session = [3u8; 16];
        let token = [4u8; 32];
        let (ra, rb) = tokio::join!(
            punch(&a, std::slice::from_ref(&b_addr), &session, &token),
            punch(&b, std::slice::from_ref(&a_addr), &session, &token)
        );
        assert_eq!(
            (ra.map(|p| p.peer), rb.map(|p| p.peer)),
            (Some(b_addr), Some(a_addr))
        );
    }

    /// +ciris (leviculum#104): a peer that answers probes as a direct
    /// interface does, behind a path that drops every datagram over
    /// `path_mtu` bytes on the way to it.
    async fn probe_through(path_mtu: usize) -> Option<u32> {
        let a = loopback().await;
        let b = std::sync::Arc::new(loopback().await);
        let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        let session = [5u8; 16];
        let token = [6u8; 32];
        let peer = std::sync::Arc::clone(&b);
        let answering = tokio::spawn(async move {
            let mut buf = vec![0u8; 65_535];
            loop {
                let Ok((len, src)) = peer.recv_from(&mut buf).await else {
                    return;
                };
                if len > path_mtu {
                    continue; // the path drops it
                }
                answer_mtu_probe(&peer, &buf[..len], src, &session, &token).await;
            }
        });
        let mut early = Vec::new();
        let found = probe_path_mtu(&a, b_addr, &session, &token, &mut early).await;
        answering.abort();
        let _ = a_addr;
        found
    }

    /// +ciris (leviculum#104): the interface signals the largest probe step
    /// the path carries, never more: a path that drops datagrams over N
    /// bytes gets the largest step at or under N, and one that carries
    /// neither step gets no MTU (base 500).
    #[tokio::test]
    async fn the_path_mtu_is_the_largest_step_acked() {
        assert_eq!(probe_through(65_535).await, Some(1400));
        assert_eq!(probe_through(1400).await, Some(1400));
        assert_eq!(probe_through(1399).await, Some(1280));
        assert_eq!(probe_through(1300).await, Some(1280));
        assert_eq!(probe_through(1279).await, None);
        assert_eq!(probe_through(100).await, None);
    }

    /// +ciris: two peers behind one NAT that does not hairpin. Each side's
    /// punch to the other's PUBLIC address goes nowhere (here: a closed
    /// port), and the path comes up over the host candidate punched beside
    /// it.
    #[tokio::test]
    async fn behind_one_nat_the_punch_goes_through_the_host_candidate() {
        let a = loopback().await;
        let b = loopback().await;
        let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        // Addresses nothing answers on: the shared public IP the router will
        // not route back inside.
        let dead = |s: UdpSocket| s.local_addr().unwrap();
        let (a_public, b_public) = (dead(loopback().await), dead(loopback().await));
        let session = [3u8; 16];
        let token = [4u8; 32];
        let (to_b, to_a) = ([b_public, b_addr], [a_public, a_addr]);
        let (ra, rb) = tokio::join!(
            punch(&a, &to_b, &session, &token),
            punch(&b, &to_a, &session, &token)
        );
        assert_eq!(
            (ra.map(|p| p.peer), rb.map(|p| p.peer)),
            (Some(b_addr), Some(a_addr)),
            "each side's path is the other's host candidate"
        );
    }

    #[tokio::test]
    async fn datagrams_the_peer_sends_during_the_punch_are_kept() {
        let a = loopback().await;
        let b = loopback().await;
        let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        let session = [3u8; 16];
        let token = [4u8; 32];
        // A has finished and moved its link already: an ordinary datagram
        // reaches B before B's punch completes.
        a.send_to(b"a reticulum packet", b_addr).await.unwrap();
        let (ra, rb) = tokio::join!(
            punch(&a, std::slice::from_ref(&b_addr), &session, &token),
            punch(&b, std::slice::from_ref(&a_addr), &session, &token)
        );
        assert!(ra.is_some());
        let rb = rb.expect("B punches through");
        assert_eq!(rb.early, vec![b"a reticulum packet".to_vec()]);
    }

    #[tokio::test]
    async fn early_datagrams_are_bounded_by_size() {
        let a = loopback().await;
        let b = loopback().await;
        let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        let session = [3u8; 16];
        let token = [4u8; 32];
        // An oversized datagram is not kept; a normal one after it is.
        a.send_to(&vec![1u8; EARLY_DATAGRAM_MAX + 1], b_addr)
            .await
            .unwrap();
        a.send_to(b"small", b_addr).await.unwrap();
        let (_, rb) = tokio::join!(
            punch(&a, std::slice::from_ref(&b_addr), &session, &token),
            punch(&b, std::slice::from_ref(&a_addr), &session, &token)
        );
        assert_eq!(rb.expect("punched").early, vec![b"small".to_vec()]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_punch_with_the_wrong_token_never_completes() {
        let a = loopback().await;
        let b = loopback().await;
        let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        let session = [3u8; 16];
        let (ra, rb) = tokio::join!(
            punch(&a, std::slice::from_ref(&b_addr), &session, &[4u8; 32]),
            punch(&b, std::slice::from_ref(&a_addr), &session, &[5u8; 32])
        );
        assert!(ra.is_none() && rb.is_none());
    }

    /// +ciris: an ack only counts from an address this end punched. A
    /// valid ack from anywhere else (here a third socket that knows the
    /// token, standing in for an ack that came back over another route)
    /// never completes the punch: it proves nothing about the pair this end
    /// would send on.
    #[tokio::test]
    async fn an_ack_from_an_address_never_punched_does_not_complete() {
        let a = loopback().await;
        let elsewhere = loopback().await;
        let dead = loopback().await.local_addr().unwrap();
        let a_addr = a.local_addr().unwrap();
        let (session, token) = ([3u8; 16], [4u8; 32]);
        let spam = async {
            for seq in 0..40 {
                let ack = wire::punch_frame(PunchKind::Ack, &session, &token, seq);
                let _ = elsewhere.send_to(&ack, a_addr).await;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let (result, ()) = tokio::join!(
            tokio::time::timeout(
                Duration::from_millis(1_000),
                punch(&a, std::slice::from_ref(&dead), &session, &token)
            ),
            spam
        );
        assert!(result.is_err(), "still punching: {result:?}");
    }

    /// +ciris: a peer seen at an address it never advertised (a NAT that
    /// did not keep the port) is punched there too, and the path comes up
    /// on that address for both ends.
    #[tokio::test]
    async fn a_peer_heard_from_an_unadvertised_address_is_punched_there() {
        let a = loopback().await;
        let b = loopback().await;
        let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        // A was told a wrong address for B; B knows A's.
        let wrong = loopback().await.local_addr().unwrap();
        let (session, token) = ([3u8; 16], [4u8; 32]);
        // B finishes first (A acks its punches at once) and then acks late
        // punches, as its direct interface does.
        let b_side = async {
            let result = punch(&b, std::slice::from_ref(&a_addr), &session, &token).await;
            let mut buf = [0u8; 256];
            let until = tokio::time::Instant::now() + Duration::from_secs(3);
            while let Ok(Ok((len, src))) =
                tokio::time::timeout_at(until, b.recv_from(&mut buf)).await
            {
                if let Some((PunchKind::Punch, seq)) =
                    wire::parse_punch_frame(&buf[..len], &session, &token)
                {
                    let ack = wire::punch_frame(PunchKind::Ack, &session, &token, seq);
                    let _ = b.send_to(&ack, src).await;
                }
            }
            result
        };
        let (ra, rb) = tokio::join!(
            punch(&a, std::slice::from_ref(&wrong), &session, &token),
            b_side
        );
        assert_eq!(
            (ra.map(|p| p.peer), rb.map(|p| p.peer)),
            (Some(b_addr), Some(a_addr))
        );
    }

    /// +ciris: a dual-stack session socket punches an IPv4-only peer, and
    /// reports the path as plain IPv4.
    #[tokio::test]
    async fn a_dual_stack_socket_punches_an_ipv4_peer() {
        let Ok(std_socket) = bind_session_socket("127.0.0.1:1".parse().unwrap()) else {
            return;
        };
        let a = UdpSocket::from_std(std_socket).unwrap();
        let a_port = a.local_addr().unwrap().port();
        let a_v4: SocketAddr = format!("127.0.0.1:{a_port}").parse().unwrap();
        let b = loopback().await;
        let b_addr = b.local_addr().unwrap();
        let (session, token) = ([3u8; 16], [4u8; 32]);
        let (ra, rb) = tokio::join!(
            punch(&a, std::slice::from_ref(&b_addr), &session, &token),
            punch(&b, std::slice::from_ref(&a_v4), &session, &token)
        );
        assert_eq!(
            (ra.map(|p| p.peer), rb.map(|p| p.peer)),
            (Some(b_addr), Some(a_v4))
        );
    }

    #[tokio::test]
    async fn a_new_direct_interface_announces_again_while_it_lives() {
        let (tx, _rx) = mpsc::channel(4);
        let mut rt = DirectLinkRuntime::new(tx);
        let live = InterfaceId(41);
        let gone = InterfaceId(42);
        for id in [live, gone] {
            rt.registering.insert(id, [id.0 as u8; 16]);
            rt.interfaces
                .insert(id.0, tokio::spawn(async {}).abort_handle());
            assert!(rt.registered(id).is_some());
        }
        rt.interface_gone(gone);
        let now = std::time::Instant::now();
        assert!(rt.take_due_reannounces(now).is_empty(), "not yet due");
        assert_eq!(
            rt.take_due_reannounces(now + Duration::from_millis(1_500)),
            vec![live.0],
            "the first repeat, for the live interface only"
        );
        assert_eq!(
            rt.take_due_reannounces(now + Duration::from_secs(5)),
            vec![live.0]
        );
        assert!(rt
            .take_due_reannounces(now + Duration::from_secs(60))
            .is_empty());
    }

    fn reticulum(keys: &[(&str, &str)]) -> crate::config::ReticulumConfig {
        let mut ini = String::from("[reticulum]\n");
        for (k, v) in keys {
            ini.push_str(&format!("  {k} = {v}\n"));
        }
        crate::ini_config::parse_ini(&ini).unwrap().reticulum
    }

    #[test]
    fn settings_default_to_off_and_parse_every_key() {
        let off = DirectLinkSettings::from_config(&reticulum(&[])).unwrap();
        assert_eq!(off, DirectLinkSettings::default());
        assert!(!off.active());

        let on = DirectLinkSettings::from_config(&reticulum(&[
            ("probe_port", "4343"),
            ("probe_addr", "rns.example.org:4343"),
            ("probe_protocol", "stun"),
            ("direct_connect_policy", "identified_only"),
            ("probe_addr2", "rns2.example.org:4343"),
            ("probe_protocol2", "rnsp"),
        ]))
        .unwrap();
        assert_eq!(
            on,
            DirectLinkSettings {
                policy: DirectLinkPolicy::IdentifiedOnly,
                facilitator: Some("rns.example.org:4343".into()),
                protocol: ProbeProtocol::Stun,
                facilitator_port: Some(4343),
                facilitator2: Some("rns2.example.org:4343".into()),
                protocol2: ProbeProtocol::Rnsp,
            }
        );
        assert!(on.active());
    }

    #[test]
    fn a_malformed_probe_port_is_a_config_error() {
        for bad in ["434x", "70000"] {
            let err =
                DirectLinkSettings::from_config(&reticulum(&[("probe_port", bad)])).unwrap_err();
            assert!(matches!(err, Error::Config(_)), "{bad}: {err:?}");
        }
    }

    #[test]
    fn a_misspelt_policy_is_a_config_error() {
        let err =
            DirectLinkSettings::from_config(&reticulum(&[("direct_connect_policy", "accept-all")]))
                .unwrap_err();
        assert!(matches!(err, Error::Config(_)));
        assert!(DirectLinkSettings::from_config(&reticulum(&[("probe_protocol", "ice")])).is_err());
    }
}
