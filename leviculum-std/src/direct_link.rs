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
use leviculum_core::direct_link::{ProbeProtocol, Role};
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
///
/// `#[non_exhaustive]` (+ciris): the fork keeps adding settings. Outside
/// this crate start from [`Default`] and set the fields.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
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
    /// the same session socket to classify this node's NAT. Unset, no NAT is
    /// classified: the node never advertises predicted ports ("q"), and
    /// advertises "b" only on its own evidence (its allocation probes look
    /// random, the fallback). No default: the node never asks a party it was
    /// not told to.
    pub facilitator2: Option<String>,
    /// How to probe `facilitator2`.
    pub protocol2: ProbeProtocol,
    /// +ciris (review item 6): a node running a facilitator also answers
    /// probes on the next UDP port (`facilitator_port + 1`): the second
    /// observer probers derive from its dial. UDP and probes only. Off by
    /// default; a facilitator meant to be both observers (the canonical)
    /// turns it on (`probe_second_port = yes`).
    pub facilitator_second_port: bool,
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
            facilitator_second_port: matches!(
                config
                    .direct_link
                    .probe_second_port
                    .as_deref()
                    .map(str::trim),
                Some("yes" | "true" | "on" | "1")
            ),
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
    /// +ciris: `yes` runs the facilitator's second probe port (udp) too
    /// (see `DirectLinkSettings::facilitator_second_port`).
    #[serde(default)]
    pub probe_second_port: Option<String>,
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
        "probe_second_port" => {
            config.direct_link.probe_second_port = Some(value.trim().to_ascii_lowercase())
        }
        _ => {}
    }
}

/// Resolve a `host:port` facilitator to the address put in the REQUEST.
/// The peer probes that address too, so it has to be an IP, not a name.
pub(crate) async fn resolve_facilitator(addr: &str) -> io::Result<SocketAddr> {
    // IPv4 first: it is what NATs punch most reliably and what most
    // facilitators serve. An IPv6-only facilitator is used as such. Bounded
    // (re-review items 8, A): a resolver that hangs must not hold a session.
    let lookup = tokio::net::lookup_host(addr);
    let resolved: Vec<SocketAddr> = tokio::time::timeout(RESOLVE_TIMEOUT, lookup)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the facilitator did not resolve"))??
        .collect();
    // No address or name in the error: it can reach the INFO log (review 9).
    resolved
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| resolved.first())
        .copied()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the facilitator has no address"))
}

/// +ciris (re-review items 8, A): how long one facilitator lookup may take.
pub(crate) const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);
/// +ciris (re-review items 8, A): how often a resolved facilitator (and the
/// second observer) is looked up again, in the background.
pub(crate) const RESOLVE_REFRESH: Duration = Duration::from_secs(300);
/// +ciris (re-review A): how soon an unresolved one is tried again.
pub(crate) const RESOLVE_RETRY: Duration = Duration::from_secs(30);

// Probe

/// Probe attempts and the wait for each answer.
const PROBE_ATTEMPTS: u32 = 3;
const PROBE_WAIT: Duration = Duration::from_secs(2);
/// +ciris (fourth review F): past this into discovery the second observer
/// and the allocation probes are skipped, so the whole discovery (at most
/// about 3 s + 0.5 s + 1 s) stays well inside the peer's 10 s wait.
pub(crate) const DISCOVERY_EXTRAS_UNTIL: Duration = Duration::from_secs(3);

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
#[cfg(test)]
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
    probe_with_stats(socket, server, protocol, attempts, wait)
        .await
        .map(|(public, _)| public)
}

/// +ciris (diagnostics): how a probe that got its answer went: the attempt
/// that was answered (1-based) and the round trip of that attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProbeStats {
    pub(crate) attempts: u32,
    pub(crate) rtt_ms: u64,
}

/// [`probe_with`], with [`ProbeStats`].
pub(crate) async fn probe_with_stats(
    socket: &UdpSocket,
    server: SocketAddr,
    protocol: ProbeProtocol,
    attempts: u32,
    wait: Duration,
) -> io::Result<(SocketAddr, ProbeStats)> {
    let mut rng = rand_core::OsRng;
    let mut buf = [0u8; 512];
    for attempt_no in 1..=attempts {
        // A fresh nonce per attempt, so a late answer to an earlier attempt
        // is not mistaken for this one (either is equally true, but matching
        // exactly keeps the rule simple).
        let attempt = ProbeAttempt::new(protocol, &mut rng);
        let request = attempt.request();
        socket.send_to(&request, on_socket(socket, server)).await?;
        let sent_at = tokio::time::Instant::now();
        let deadline = sent_at + wait;
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
                        let stats = ProbeStats {
                            attempts: attempt_no,
                            rtt_ms: sent_at.elapsed().as_millis() as u64,
                        };
                        return Ok((public, stats));
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
const PUNCH_WINDOW: Duration =
    Duration::from_millis(leviculum_core::direct_link::session::PUNCH_WINDOW_MS);
/// +ciris: the window of an ESCALATED punch (either side advertised "b" or
/// "q", under the two-sided commit): long enough for the whole spray. Both
/// ends derive it from the same exchanged keys. Plain punches keep
/// [`PUNCH_WINDOW`], so rns-rs timing is unchanged.
pub(crate) const ESCALATED_PUNCH_WINDOW: Duration =
    Duration::from_millis(leviculum_core::direct_link::session::ESCALATED_PUNCH_WINDOW_MS);

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
/// +ciris (pair agreement): each punch's sequence number names the target
/// it went to ([`wire::punch_seq`]). Under nomination the ack must echo the
/// index of the address it came from, so the check is really symmetric; the
/// initiator then nominates the pair it settled on, and the responder takes
/// exactly that pair instead of its own first one (RFC 8445 8.1.1, the
/// controlling agent's USE-CANDIDATE). Without nomination (rns-rs, an older
/// peer) each end settles on its own first proven address, as before.
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
    punch_with(socket, targets, &[], session, token, PunchPlan::plain())
        .await
        .map(|(p, _)| p)
}

/// +ciris — [`punch`], with the two halves of the birthday approach for a
/// peer pair where one NAT maps per destination at random. Both start only
/// after [`ESCALATE_AFTER`] (or [`BIRTHDAY_AFTER`]) with nothing heard, so a
/// pair that reaches each other at once never sprays or opens a socket more:
/// - `birthday` (this node's NAT is the random one): up to
///   [`wire::MAX_BIRTHDAY_SOCKETS`] more sockets, opened in tranches of
///   [`BIRTHDAY_TRANCHE`] every [`BIRTHDAY_TRANCHE_EVERY`] while nothing is
///   heard, each sending one punch a [`BIRTHDAY_REFRESH`] toward the peer's
///   advertised address so its own mapping stays open. The first one the
///   peer reaches becomes the punch socket for the rest of the window and is
///   returned, so the interface runs on it; every other one is closed.
/// - `spray` (the peer's NAT is the random one): also knock random ports on
///   the peer's public IP, [`SPRAY_PER_TICK`] a tick, until anything is
///   heard from it or [`wire::MAX_SPRAY_PORTS`] are used.
pub(crate) async fn punch_with(
    socket: &UdpSocket,
    targets: &[SocketAddr],
    predicted: &[SocketAddr],
    session: &SessionId,
    token: &[u8; 32],
    plan: PunchPlan,
) -> Option<(Punched, Option<Arc<UdpSocket>>)> {
    let esc = Escalation::new(
        knock_budget(),
        plan.pair,
        plan.identity,
        plan.escalate,
        plan.birthday_after,
    );
    let spray: Option<SprayPorts> = (plan.spray && plan.escalate).then(|| {
        let mut rng = rand_core::OsRng;
        Box::new(move || spray_port(&mut rng)) as SprayPorts
    });
    let predicted: &[SocketAddr] = if plan.escalate { predicted } else { &[] };
    let extras = (plan.birthday && plan.escalate).then_some(Extras::Open);
    punch_with_ports(
        socket,
        extras,
        targets,
        predicted,
        session,
        token,
        spray,
        &esc,
        PunchShape {
            window: plan.window,
            nomination: plan.nomination,
        },
    )
    .await
}

/// +ciris: what a punch may do beyond punching its targets.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PunchPlan {
    /// This node's NAT is random (two-observer or own-evidence): hold
    /// birthday sockets open once silent past `birthday_after`.
    pub(crate) birthday: bool,
    /// The peer hinted its NAT is random ("b"): knock random ports on its
    /// IP once silent past ESCALATE_AFTER.
    pub(crate) spray: bool,
    /// The policy gate: only a node answering identified peers escalates.
    pub(crate) escalate: bool,
    /// The budget's pair key (the link).
    pub(crate) pair: [u8; 16],
    /// The peer's identity, when known: the pair allowance is kept for it
    /// too (fourth review B).
    pub(crate) identity: Option<[u8; 16]>,
    /// When the birthday phase may open: BIRTHDAY_AFTER on two-observer
    /// evidence, ESCALATE_AFTER on own evidence alone (Eric's fallback).
    pub(crate) birthday_after: Duration,
    /// [`PUNCH_WINDOW`], or [`ESCALATED_PUNCH_WINDOW`] when both ends
    /// agreed the punch is escalated.
    pub(crate) window: Duration,
    /// Pair agreement (see [`punch`]): `Initiator` nominates, `Responder`
    /// settles on the nomination, `None` settles on its own.
    pub(crate) nomination: Option<Role>,
}

impl PunchPlan {
    /// Punch the targets, nothing more.
    #[cfg(test)]
    pub(crate) fn plain() -> Self {
        PunchPlan {
            birthday: false,
            spray: false,
            escalate: false,
            pair: [0u8; 16],
            identity: None,
            birthday_after: BIRTHDAY_AFTER,
            window: PUNCH_WINDOW,
            nomination: None,
        }
    }
}

/// +ciris: how long a punch runs and how it settles.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PunchShape {
    pub(crate) window: Duration,
    pub(crate) nomination: Option<Role>,
}

impl PunchShape {
    /// The plain window, each end settling on its own.
    #[cfg(test)]
    pub(crate) fn plain() -> Self {
        PunchShape {
            window: PUNCH_WINDOW,
            nomination: None,
        }
    }
}

/// +ciris (review item 2): every escalated send (a knock to a predicted or
/// random port, a birthday socket's punch) passes here: the policy gate, the
/// pair's allowance (decided once per punch, at its first escalated send),
/// then the target's volume and the global ceiling, per datagram.
#[derive(Clone)]
pub(crate) struct Escalation {
    budget: &'static KnockBudget,
    pair: [u8; 16],
    identity: Option<[u8; 16]>,
    allowed: bool,
    pub(crate) birthday_after: Duration,
    /// `None` until the first escalated send; then whether the pair's
    /// allowance granted this punch.
    granted: Arc<std::sync::Mutex<Option<bool>>>,
    /// Birthday sockets this punch holds open right now: its rate need is
    /// budgeted by these, not by the cap (Eric's condition on the ramp).
    pub(crate) open: Arc<std::sync::atomic::AtomicUsize>,
}

impl Escalation {
    pub(crate) fn new(
        budget: &'static KnockBudget,
        pair: [u8; 16],
        identity: Option<[u8; 16]>,
        allowed: bool,
        birthday_after: Duration,
    ) -> Self {
        Escalation {
            budget,
            pair,
            identity,
            allowed,
            birthday_after,
            granted: Arc::new(std::sync::Mutex::new(None)),
            open: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Is this punch granted its pair's allowance? Decided once, at the
    /// first ask (a send, or the birthday phase before it binds anything:
    /// third review N1b).
    pub(crate) fn grant(&self) -> bool {
        if !self.allowed {
            return false;
        }
        let mut g = self.granted.lock().unwrap_or_else(|e| e.into_inner());
        match *g {
            Some(ok) => ok,
            None => {
                let ok = self
                    .budget
                    .escalate(self.pair, self.identity, std::time::Instant::now());
                *g = Some(ok);
                ok
            }
        }
    }

    /// The datagrams a second this punch needs now: a spray's rate, and
    /// one refresh per open birthday socket per [`BIRTHDAY_REFRESH`].
    fn need_pps(&self) -> u32 {
        let open = self.open.load(std::sync::atomic::Ordering::Relaxed) as u32;
        SPRAY_PPS + open * BIRTHDAY_REFRESHES_PER_SEC
    }

    /// May one escalated datagram go to `ip` now?
    pub(crate) fn send(&self, ip: std::net::IpAddr) -> bool {
        self.grant()
            && self
                .budget
                .take(self.pair, ip, std::time::Instant::now(), self.need_pps())
    }

    /// Whether the pair's allowance refused this punch.
    #[cfg(test)]
    pub(crate) fn refused(&self) -> bool {
        *self.granted.lock().unwrap_or_else(|e| e.into_inner()) == Some(false)
    }

    pub(crate) fn allowed(&self) -> bool {
        self.allowed
    }
}

/// +ciris (review item 2): escalations one link, and one peer identity, may
/// start per [`BUDGET_WINDOW`], whatever target they spray: a retry after a
/// birthday miss escalates again within these.
pub(crate) const PAIR_ESCALATIONS: usize = 3;
/// +ciris (review item 2): the budget's window.
pub(crate) const BUDGET_WINDOW: Duration = Duration::from_secs(300);
/// Random ports knocked a second (the spray's pacing).
pub(crate) const SPRAY_PPS: u32 = SPRAY_PER_TICK as u32 * 10;
/// One birthday socket's punches a second.
pub(crate) const BIRTHDAY_REFRESHES_PER_SEC: u32 = 2;
/// Birthday punches one session sends at most: every socket, every
/// BIRTHDAY_REFRESH, for a whole escalated window.
pub(crate) const BIRTHDAY_SENDS_MAX: u32 = wire::MAX_BIRTHDAY_SOCKETS as u32
    * BIRTHDAY_REFRESHES_PER_SEC
    * (leviculum_core::direct_link::session::ESCALATED_PUNCH_WINDOW_MS / 1_000) as u32;
/// Knocks to predicted ports one session sends at most: each, every tick,
/// for a whole escalated window.
pub(crate) const PREDICTED_SENDS_MAX: u32 = wire::MAX_PREDICTED_PORTS as u32
    * (leviculum_core::direct_link::session::ESCALATED_PUNCH_WINDOW_MS / 100) as u32;
/// Escalated datagrams one escalation sends at most: the spray, the
/// predicted knocks and the birthday punches.
pub(crate) const ESCALATION_SENDS: u32 =
    wire::MAX_SPRAY_PORTS as u32 + PREDICTED_SENDS_MAX + BIRTHDAY_SENDS_MAX;
/// +ciris (third review, B): escalated datagrams one PAIR may send one target
/// per window: its own three escalations.
pub(crate) const PAIR_TARGET_VOLUME: u32 = PAIR_ESCALATIONS as u32 * ESCALATION_SENDS;
/// +ciris (review item 2, third review B): escalated datagrams one target (an
/// IPv4 address, an IPv6 /64) may receive per window from this process, all
/// pairs together: a ceiling over the pairs' shares, so no one pair can
/// exhaust a shared (CGNAT) address for the others.
pub(crate) const TARGET_VOLUME: u32 = 2 * PAIR_TARGET_VOLUME;
/// One escalation at its peak: the spray and every birthday socket open
/// (both ends random), 320 + 512 a second.
pub(crate) const PAIR_FULL_PPS: u32 =
    SPRAY_PPS + wire::MAX_BIRTHDAY_SOCKETS as u32 * BIRTHDAY_REFRESHES_PER_SEC;
/// +ciris (review item 2): escalated datagrams per second the whole process
/// may send: two concurrent escalations at their peak (2 x 832).
pub(crate) const ESCALATED_GLOBAL_PPS: u32 = 2 * PAIR_FULL_PPS;
/// How recently a pair must have sent to count as active for the fair share.
const ACTIVE_PAIR: Duration = Duration::from_secs(2);

/// A budget key: an IPv4 address, or an IPv6 /64 (one subscriber's prefix).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    V4([u8; 4]),
    V6([u8; 8]),
}

impl Target {
    pub(crate) fn of(ip: std::net::IpAddr) -> Self {
        match ip {
            std::net::IpAddr::V4(v4) => Target::V4(v4.octets()),
            std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Target::V4(v4.octets()),
                None => {
                    let mut p = [0u8; 8];
                    p.copy_from_slice(&v6.octets()[..8]);
                    Target::V6(p)
                }
            },
        }
    }
}

/// +ciris (fourth review B): whose allowance an escalation spends: the
/// link's, and the peer identity's when it identified itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AllowanceKey {
    Link([u8; 16]),
    Identity([u8; 16]),
}

/// +ciris (third review, item 2): a count over a sliding [`BUDGET_WINDOW`],
/// two buckets: the previous window's count weighted by how much of it is
/// still inside, plus the current one's (no 2x burst at a boundary).
#[derive(Debug, Clone, Copy)]
struct Sliding {
    start: std::time::Instant,
    cur: u32,
    prev: u32,
}

impl Sliding {
    fn new(now: std::time::Instant) -> Self {
        Sliding {
            start: now,
            cur: 0,
            prev: 0,
        }
    }

    fn roll(&mut self, now: std::time::Instant) {
        let gone = now.saturating_duration_since(self.start);
        if gone >= 2 * BUDGET_WINDOW {
            *self = Sliding::new(now);
        } else if gone >= BUDGET_WINDOW {
            self.prev = self.cur;
            self.cur = 0;
            self.start += BUDGET_WINDOW;
        }
    }

    fn estimate(&mut self, now: std::time::Instant) -> f64 {
        self.roll(now);
        let into = now.saturating_duration_since(self.start).as_secs_f64();
        let left = (1.0 - into / BUDGET_WINDOW.as_secs_f64()).max(0.0);
        f64::from(self.prev) * left + f64::from(self.cur)
    }

    fn idle(&self, now: std::time::Instant) -> bool {
        now.saturating_duration_since(self.start) >= 2 * BUDGET_WINDOW
    }
}

/// A token bucket: `rate` a second, `rate` deep.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: std::time::Instant,
}

impl Bucket {
    fn take(&mut self, rate: f64, now: std::time::Instant) -> bool {
        let gained = now.saturating_duration_since(self.at).as_secs_f64() * rate;
        self.tokens = (self.tokens + gained).min(rate);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// +ciris: the process-wide budget for escalated sends (review item 2,
/// third review B, N3).
pub(crate) struct KnockBudget {
    inner: std::sync::Mutex<KnockBudgetInner>,
}

struct KnockBudgetInner {
    /// Escalations started per link and per peer identity, with when.
    allowances: HashMap<AllowanceKey, Vec<std::time::Instant>>,
    /// Datagrams per target, all pairs (the ceiling).
    volume: HashMap<Target, Sliding>,
    /// Datagrams per (pair, target): each pair's share of a target.
    pair_volume: HashMap<([u8; 16], Target), Sliding>,
    /// Each pair's rate bucket: a fair share of the global ceiling.
    pair_rate: HashMap<[u8; 16], Bucket>,
    global: Bucket,
}

impl KnockBudget {
    pub(crate) fn new() -> Self {
        KnockBudget {
            inner: std::sync::Mutex::new(KnockBudgetInner {
                allowances: HashMap::new(),
                volume: HashMap::new(),
                pair_volume: HashMap::new(),
                pair_rate: HashMap::new(),
                global: Bucket {
                    tokens: f64::from(ESCALATED_GLOBAL_PPS),
                    at: std::time::Instant::now(),
                },
            }),
        }
    }

    /// May the pair (the link `pair`, and the peer `identity` when known)
    /// start an escalation now? Both allowances must have room; both are
    /// spent if so.
    pub(crate) fn escalate(
        &self,
        pair: [u8; 16],
        identity: Option<[u8; 16]>,
        now: std::time::Instant,
    ) -> bool {
        let mut b = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        b.allowances.retain(|_, starts| {
            starts.retain(|at| now.saturating_duration_since(*at) < BUDGET_WINDOW);
            !starts.is_empty()
        });
        let keys: Vec<AllowanceKey> = core::iter::once(AllowanceKey::Link(pair))
            .chain(identity.map(AllowanceKey::Identity))
            .collect();
        let full = keys.iter().any(|k| {
            b.allowances
                .get(k)
                .is_some_and(|starts| starts.len() >= PAIR_ESCALATIONS)
        });
        if full {
            return false;
        }
        for k in keys {
            b.allowances.entry(k).or_default().push(now);
        }
        true
    }

    /// One escalated datagram from `pair` toward `ip`: the pair's share of
    /// the target, the target's ceiling, the pair's fair share of the global
    /// rate (never below `need_pps`, what the pair has open, up to the whole
    /// ceiling), then the global ceiling.
    pub(crate) fn take(
        &self,
        pair: [u8; 16],
        ip: std::net::IpAddr,
        now: std::time::Instant,
        need_pps: u32,
    ) -> bool {
        let mut b = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Prune what nothing refers to any more (third review, item 2).
        b.volume.retain(|_, w| !w.idle(now));
        b.pair_volume.retain(|_, w| !w.idle(now));
        b.pair_rate
            .retain(|_, k| now.saturating_duration_since(k.at) < BUDGET_WINDOW);
        let key = Target::of(ip);
        let pk = (pair, key);
        if b.pair_volume
            .entry(pk)
            .or_insert_with(|| Sliding::new(now))
            .estimate(now)
            >= f64::from(PAIR_TARGET_VOLUME)
        {
            return false;
        }
        if b.volume
            .entry(key)
            .or_insert_with(|| Sliding::new(now))
            .estimate(now)
            >= f64::from(TARGET_VOLUME)
        {
            return false;
        }
        let active = b
            .pair_rate
            .iter()
            .filter(|(p, k)| **p != pair && now.saturating_duration_since(k.at) < ACTIVE_PAIR)
            .count()
            + 1;
        let share = (f64::from(ESCALATED_GLOBAL_PPS) / active as f64)
            .max(f64::from(need_pps))
            .min(f64::from(ESCALATED_GLOBAL_PPS));
        // A new pair's bucket starts full at its share.
        let mine = b.pair_rate.entry(pair).or_insert(Bucket {
            tokens: share,
            at: now,
        });
        if !mine.take(share, now) {
            return false;
        }
        if !b.global.take(f64::from(ESCALATED_GLOBAL_PPS), now) {
            return false;
        }
        if let Some(w) = b.pair_volume.get_mut(&pk) {
            w.cur += 1;
        }
        if let Some(w) = b.volume.get_mut(&key) {
            w.cur += 1;
        }
        true
    }
}

/// The process's budget for escalated sends.
pub(crate) fn knock_budget() -> &'static KnockBudget {
    static BUDGET: std::sync::OnceLock<KnockBudget> = std::sync::OnceLock::new();
    BUDGET.get_or_init(KnockBudget::new)
}

/// +ciris: birthday sockets the whole process may hold at once, all
/// sessions together: two sessions' full sets. A third concurrent session
/// gets what is left, and punches on its session socket whatever it gets.
pub(crate) const PROCESS_BIRTHDAY_SOCKETS: usize = 2 * wire::MAX_BIRTHDAY_SOCKETS;

/// +ciris (review item 3): the process-wide count of birthday sockets.
pub(crate) fn birthday_permits() -> &'static Arc<tokio::sync::Semaphore> {
    static PERMITS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    PERMITS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(PROCESS_BIRTHDAY_SOCKETS)))
}

/// Where a spray draws its ports: random in production, fixed in tests.
pub(crate) type SprayPorts = Box<dyn FnMut() -> u16 + Send>;

/// Where a punch from behind a random NAT gets its birthday sockets.
pub(crate) enum Extras {
    /// Bind them, tranche by tranche, when they are needed.
    Open,
    /// The same, against a semaphore of the test's own.
    #[cfg(test)]
    OpenFrom(Arc<tokio::sync::Semaphore>),
    /// These, already bound, all in the first tranche (tests).
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
    esc: &Escalation,
    shape: PunchShape,
) -> Option<(Punched, Option<Arc<UdpSocket>>)> {
    let mut diag = PunchDiag::new(targets, predicted, *session);
    diag.escalation = Some(Arc::clone(&esc.granted));
    diag.escalation_allowed = esc.allowed();
    diag.window_ms = shape.window.as_millis() as u64;
    diag.nomination = shape.nomination;
    let now = tokio::time::Instant::now();
    // +ciris (pair agreement): a responder waiting on a nomination waits
    // NOMINATION_GRACE past its window, so an initiator that proves a pair
    // in its last moments still gets its nomination through (it nominates
    // for as long, see punch_inner).
    let grace = if shape.nomination == Some(Role::Responder) {
        NOMINATION_GRACE
    } else {
        Duration::ZERO
    };
    let deadline = now + shape.window + grace;
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
            esc,
            shape.nomination,
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
                esc,
                shape.nomination,
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
/// 320 a second, so [`wire::MAX_SPRAY_PORTS`] (1,750) take about 5.5 s and,
/// started at [`ESCALATE_AFTER`], end by about 7 s, inside
/// [`ESCALATED_PUNCH_WINDOW`] with room for a start skew of up to
/// RESPONDER_START_DELAY_MAX and the birthday side's next refresh.
pub(crate) const SPRAY_PER_TICK: usize = 32;

/// +ciris (pair agreement): how long past its window a responder waits for a
/// nomination, and an initiator that settled keeps nominating. Inside both
/// sessions' punch waits (core `Session`), which add it where nomination is
/// in force.
pub(crate) const NOMINATION_GRACE: Duration =
    Duration::from_millis(leviculum_core::direct_link::session::NOMINATION_GRACE_MS);

/// How long a punch hears nothing before it punches the peer's predicted
/// ports and sprays: a pair that reaches each other does so well inside it
/// (7 to 567 ms in every clean live sample), so it never does either.
pub(crate) const ESCALATE_AFTER: Duration = Duration::from_millis(1_500);

/// How long a punch from behind a random NAT hears nothing before it opens
/// its birthday sockets: before the peer starts spraying, so each mapping is
/// open when the spray arrives.
pub(crate) const BIRTHDAY_AFTER: Duration = Duration::from_millis(1_000);

/// +ciris: birthday sockets opened at a time. The first tranche opens at
/// the birthday gate, the next every [`BIRTHDAY_TRANCHE_EVERY`] while
/// nothing is heard, up to [`wire::MAX_BIRTHDAY_SOCKETS`] (four tranches):
/// a pair that meets early never holds the full set.
pub(crate) const BIRTHDAY_TRANCHE: usize = 64;
pub(crate) const BIRTHDAY_TRANCHE_EVERY: Duration = Duration::from_millis(500);

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

/// +ciris: the birthday sockets of one punch, as they are opened.
struct BirthdaySet {
    opened: Vec<Arc<UdpSocket>>,
    permits: Vec<Option<tokio::sync::OwnedSemaphorePermit>>,
    tasks: Vec<AbortOnDrop>,
    /// Tranches opened so far.
    tranches: usize,
    /// A bind or send failed: the host (or a carrier NAT's per-subscriber
    /// port quota) has no more; nothing more is opened.
    quota: Arc<std::sync::atomic::AtomicBool>,
    /// The set is all there will be (tests hand one in whole).
    #[cfg(test)]
    closed: bool,
}

impl BirthdaySet {
    fn new() -> Self {
        BirthdaySet {
            opened: Vec::new(),
            permits: Vec::new(),
            tasks: Vec::new(),
            tranches: 0,
            quota: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(test)]
            closed: false,
        }
    }

    fn full(&self) -> bool {
        #[cfg(test)]
        if self.closed {
            return true;
        }
        self.opened.len() >= wire::MAX_BIRTHDAY_SOCKETS
            || self.quota.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Start a task for each socket from `from` on.
    #[allow(clippy::too_many_arguments)]
    fn spawn_from(
        &mut self,
        from: usize,
        peer: SocketAddr,
        hit_tx: &mpsc::Sender<(usize, SocketAddr, Option<u32>)>,
        session: &SessionId,
        token: &[u8; 32],
        esc: &Escalation,
        ack_all: bool,
    ) {
        for index in from..self.opened.len() {
            self.tasks.push(birthday_task(
                index,
                Arc::clone(&self.opened[index]),
                peer,
                hit_tx.clone(),
                *session,
                *token,
                esc.clone(),
                Arc::clone(&self.quota),
                ack_all,
            ));
        }
        esc.open
            .store(self.opened.len(), std::sync::atomic::Ordering::Relaxed);
    }
}

/// Punch from `socket`; past [`BIRTHDAY_AFTER`] with nothing heard, hold
/// birthday sockets open too, tranche by tranche, and switch to the first
/// one the peer reaches.
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
    esc: &Escalation,
    nomination: Option<Role>,
) -> Option<(Punched, Option<Arc<UdpSocket>>)> {
    let peer = canonical(*targets.first()?);
    let mut set = BirthdaySet::new();
    // Written as the sockets are bound, while `diag` is lent to the primary
    // punch, so a punch the peer's REJECT aborts still logs them.
    let open_count = Arc::clone(&diag.birthday_open);
    let tranche_count = Arc::clone(&diag.birthday_tranches);
    // The set's quota flag IS the diag's, so a punch the session aborts
    // still logs that the ramp hit it.
    set.quota = Arc::clone(&diag.birthday_quota);
    // Set by the session socket's punch the moment it hears the peer, before
    // its ack leaves; read here on every birthday hit.
    let primary_heard = Arc::clone(&diag.primary_heard);
    // Set once one of the session socket's punches is acked: its packets
    // reach the peer. Until then this NAT's mapping is what is in the way,
    // heard or not, and the birthday sockets are what helps (strategy
    // model: "heard, never acked" used to suppress them).
    let primary_acked = Arc::clone(&diag.primary_acked);
    let acked =
        |a: &Arc<std::sync::atomic::AtomicBool>| a.load(std::sync::atomic::Ordering::SeqCst);
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
            esc,
            nomination,
        );
        tokio::pin!(primary);
        // BIRTHDAY_AFTER on two-observer evidence; ESCALATE_AFTER when the
        // only evidence is our own (silence and random steps).
        let early = tokio::select! {
            r = &mut primary => Some(r),
            _ = tokio::time::sleep(esc.birthday_after) => None,
        };
        match early {
            Some(r) => BirthdayEnd::Primary(r),
            // Acked already at the gate (the session socket's mapping
            // works), not allowed to escalate (the trust gate), or the
            // pair's allowance refused before anything is bound (third
            // review N1b): nothing to open.
            None if acked(&primary_acked) || !esc.grant() => {
                BirthdayEnd::Primary((&mut primary).await)
            }
            None => {
                let permits = match &extras {
                    #[cfg(test)]
                    Extras::OpenFrom(sem) => Arc::clone(sem),
                    _ => Arc::clone(birthday_permits()),
                };
                let mut given: Option<Vec<Arc<UdpSocket>>> = match extras {
                    Extras::Open => None,
                    #[cfg(test)]
                    Extras::OpenFrom(_) => None,
                    #[cfg(test)]
                    Extras::Given(given) => Some(given),
                };
                let (hit_tx, mut hit_rx) = mpsc::channel::<(usize, SocketAddr, Option<u32>)>(1);
                let mut next_tranche = tokio::time::interval(BIRTHDAY_TRANCHE_EVERY);
                next_tranche.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let end = loop {
                    tokio::select! {
                        r = &mut primary => break BirthdayEnd::Primary(r),
                        // The first tick fires at once: the first tranche.
                        _ = next_tranche.tick(), if !set.full() && !acked(&primary_acked) =>
                        {
                            // The guard was read when this select began; a
                            // send may have failed (or the peer been heard)
                            // while it waited for the tick: read it again.
                            if set.full() || acked(&primary_acked) {
                                continue;
                            }
                            let from = set.opened.len();
                            match given.take() {
                                Some(g) => {
                                    let n = g.len().min(wire::MAX_BIRTHDAY_SOCKETS);
                                    set.opened.extend(g.into_iter().take(n));
                                    set.permits.extend((0..n).map(|_| None));
                                    // Given sockets are the whole set.
                                    #[cfg(test)]
                                    {
                                        set.closed = true;
                                    }
                                }
                                None => open_tranche(&mut set, peer, &permits).await,
                            }
                            if set.opened.len() > from {
                                set.tranches += 1;
                            }
                            set.spawn_from(
                                from,
                                peer,
                                &hit_tx,
                                session,
                                token,
                                esc,
                                nomination == Some(Role::Responder),
                            );
                            open_count.store(set.opened.len(), std::sync::atomic::Ordering::Relaxed);
                            tranche_count.store(set.tranches, std::sync::atomic::Ordering::Relaxed);
                            if set.opened.is_empty() {
                                // Nothing could be opened: the session
                                // socket punches on alone.
                                break BirthdayEnd::Primary((&mut primary).await);
                            }
                        }
                        hit = hit_rx.recv() => match hit {
                            // Whether this hit may take the punch off the
                            // session socket (docs/direct_link/nomination.md):
                            // - no nomination: only while the session socket
                            //   has heard nothing, or the peer may settle on
                            //   it and the ends split (the TLA+ model);
                            // - the initiator: while nothing it sent from the
                            //   session socket is acked; it nominates, so the
                            //   peer follows wherever it goes;
                            // - the responder: only a nomination, which its
                            //   birthday tasks alone report (they ack every
                            //   other punch and keep listening).
                            Some(_) if !may_switch(nomination, acked(&primary_heard), acked(&primary_acked)) => {
                                ignored += 1;
                            }
                            Some((index, src, seq)) => break BirthdayEnd::Hit { index, src, seq },
                            // Unreachable while `hit_tx` lives here.
                            None => break BirthdayEnd::Primary((&mut primary).await),
                        },
                    }
                };
                // Every birthday task stops here, whichever way it went.
                set.tasks.clear();
                drop(hit_tx);
                end
            }
        }
    };
    diag.birthday_ignored = ignored;
    let (index, src, seq) = match end {
        BirthdayEnd::Primary(r) => {
            esc.open.store(0, std::sync::atomic::Ordering::Relaxed);
            return r.map(|p| (p, None));
        }
        BirthdayEnd::Hit { index, src, seq } => (index, src, seq),
    };
    // The peer reached the mapping of birthday socket `index`: close every
    // other one at once, ack the punch that got through, and punch on from
    // there.
    let winner = Arc::clone(&set.opened[index]);
    drop(set);
    esc.open.store(0, std::sync::atomic::Ordering::Relaxed);
    diag.birthday_switched = true;
    diag.birthday_winner = Some(index);
    if let Some(seq) = seq {
        let ack = wire::punch_frame(PunchKind::Ack, session, token, seq);
        let _ = winner.send_to(&ack, on_socket(&winner, src)).await;
    }
    // A punch that got through is already heard: the peer may finish on our
    // ack and then only ack, never punch again, so it must not be needed
    // twice.
    let seed = seq.map(|seq| (src, wire::punch_seq_nominates(seq)));
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
        esc,
        nomination,
    )
    .await
    .map(|p| (p, Some(winner)))
}

/// +ciris (docs/direct_link/nomination.md): may a birthday hit take the
/// punch off the session socket? Without nomination only while that socket
/// has heard nothing; the initiator while nothing it sent from it is acked;
/// the responder on a nomination only (its tasks report nothing else).
pub(crate) fn may_switch(nomination: Option<Role>, heard: bool, acked: bool) -> bool {
    match nomination {
        None => !heard,
        Some(Role::Initiator) => !acked,
        Some(Role::Responder) => true,
    }
}

/// One birthday socket: a punch toward `peer` every [`BIRTHDAY_REFRESH`],
/// and the first valid frame back reported on `hit`. A send that fails sets
/// `quota`: no more sockets are opened. With `ack_all` (the responder under
/// nomination) every valid punch is acked where it came from and only a
/// nomination is reported: the initiator proves this socket's pair itself
/// and names it, and the responder settles wherever the name arrives.
#[allow(clippy::too_many_arguments)]
fn birthday_task(
    index: usize,
    extra: Arc<UdpSocket>,
    peer: SocketAddr,
    hit: mpsc::Sender<(usize, SocketAddr, Option<u32>)>,
    session: SessionId,
    token: [u8; 32],
    esc: Escalation,
    quota: Arc<std::sync::atomic::AtomicBool>,
    ack_all: bool,
) -> AbortOnDrop {
    AbortOnDrop(
        tokio::spawn(async move {
            // Target 0, the peer's advertised address, which is where it
            // punches from too.
            let frame = wire::punch_frame(
                PunchKind::Punch,
                &session,
                &token,
                wire::punch_seq(0, 0, false),
            );
            let mut refresh = tokio::time::interval(BIRTHDAY_REFRESH);
            let mut buf = [0u8; 512];
            loop {
                tokio::select! {
                    _ = refresh.tick() => {
                        // Each birthday punch is an escalated send toward the
                        // peer's IP: spent from the same budget (re-review 2).
                        if esc.send(peer.ip())
                            && extra.send_to(&frame, on_socket(&extra, peer)).await.is_err()
                        {
                            quota.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    r = extra.recv_from(&mut buf) => {
                        let Ok((len, src)) = r else { continue };
                        let seq = match wire::parse_punch_frame(&buf[..len], &session, &token) {
                            Some((PunchKind::Punch, seq)) => Some(seq),
                            Some((PunchKind::Ack, _)) if !ack_all => None,
                            _ => continue,
                        };
                        if ack_all {
                            let Some(seq) = seq else { continue };
                            let ack = wire::punch_frame(PunchKind::Ack, &session, &token, seq);
                            let _ = extra.send_to(&ack, on_socket(&extra, src)).await;
                            if !wire::punch_seq_nominates(seq) {
                                continue;
                            }
                        }
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
    seed_heard: Option<(SocketAddr, bool)>,
    esc: &Escalation,
    nomination: Option<Role>,
) -> Option<Punched> {
    let mut targets: Vec<SocketAddr> = alloc_targets(
        canonical(*targets.first()?),
        &targets[1..]
            .iter()
            .map(|a| canonical(*a))
            .collect::<Vec<_>>(),
    );
    // +ciris (pair agreement): every target's index travels in its punches'
    // sequence numbers, so no more than the field can name.
    targets.truncate(wire::PUNCH_TARGETS_MAX);
    // +ciris: the peer's predicted ports (on its public IP), held back until
    // nothing has been heard for ESCALATE_AFTER; then KNOCKED, with the
    // spray (review item 1): a knock opens this node's filter toward a port
    // that may be anyone's on the peer's NAT, and carries no token. Only the
    // advertised address, host candidates and sources that proved the token
    // get tokened punches.
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
    let reflexive_max = (advertised + PEER_REFLEXIVE_MAX).min(wire::PUNCH_TARGETS_MAX);
    // Addresses a punch actually left for; only an ack from one counts.
    let mut punched: Vec<SocketAddr> = Vec::new();
    // Addresses a valid punch came from (each acked at once), and those an
    // ack came from; the path is the first address in both, and punched.
    let mut heard: Vec<SocketAddr> = Vec::new();
    let mut acked: Vec<SocketAddr> = Vec::new();
    // +ciris (pair agreement): the initiator's settled pair, being
    // nominated until the responder acks a nomination on it.
    let mut nominating: Option<SocketAddr> = None;
    // The initiator nominates up to NOMINATION_GRACE past its window: the
    // responder waits as long.
    let mut deadline = deadline;
    // +ciris: a punch heard (and acked) before this loop began, on this
    // socket: a birthday socket the peer reached. Punched too, as any
    // peer-reflexive source is.
    if let Some((src, nominated)) = seed_heard.map(|(a, n)| (canonical(a), n)) {
        if nominated && nomination == Some(Role::Responder) {
            diag.heard(src);
            diag.nominated_ms = Some(diag.elapsed_ms());
            diag.path = Some(src);
            return Some(Punched {
                peer: src,
                early: Vec::new(),
            });
        }
        if !targets.contains(&src) && targets.len() < reflexive_max {
            targets.push(src);
            diag.peer_reflexive += 1;
        }
        heard.push(src);
        diag.heard(src);
    }
    let mut early: Vec<Vec<u8>> = Vec::new();
    let mut early_bytes = 0usize;
    let mut counter: u32 = 0;
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
                        // Every knock is spent from the budget as it goes
                        // (review B: per datagram, not per escalation).
                        spray = pending_spray.take();
                    } else {
                        // Heard already: the peer's mapping did not move.
                        diag.predicted_dropped += held.len();
                        held.clear();
                        pending_spray = None;
                    }
                }
                counter = counter.wrapping_add(1);
                if let Some(path) = nominating {
                    // +ciris (pair agreement): only the settled pair, each
                    // punch on it a nomination, until one is acked.
                    let index = targets.iter().position(|t| *t == path).unwrap_or(0);
                    let frame = wire::punch_frame(
                        PunchKind::Punch,
                        session,
                        token,
                        wire::punch_seq(counter, index, true),
                    );
                    if socket.send_to(&frame, on_socket(socket, path)).await.is_err() {
                        diag.send_errors += 1;
                    }
                    diag.nominations += 1;
                    continue;
                }
                for (index, target) in targets.iter().enumerate() {
                    let frame = wire::punch_frame(
                        PunchKind::Punch,
                        session,
                        token,
                        wire::punch_seq(counter, index, false),
                    );
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
                    if !esc.send(addr.ip()) {
                        diag.knocks_refused += 1;
                        break;
                    }
                    let mut pad = [0u8; wire::KNOCK_LEN];
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
                        if targets.contains(&addr) || knocking.contains(&addr) || sprayed.contains(&addr) {
                            continue;
                        }
                        // The budget first: a refused port is not "sprayed"
                        // (re-review item 7).
                        if !esc.send(addr.ip()) {
                            diag.knocks_refused += 1;
                            break;
                        }
                        sprayed.insert(addr);
                        sent_now += 1;
                        let mut pad = [0u8; wire::KNOCK_LEN];
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
                        diag.heard(src);
                        // +ciris (pair agreement): the initiator named this
                        // pair; it proved it both ways from its side, so the
                        // responder takes it as it arrived.
                        if nomination == Some(Role::Responder)
                            && wire::punch_seq_nominates(their_seq)
                        {
                            diag.nominated_ms = Some(diag.elapsed_ms());
                            diag.path = Some(src);
                            return Some(Punched { peer: src, early });
                        }
                        // A source that sent a valid punch proved the token:
                        // it is punched (tokened) from now on.
                        if !targets.contains(&src) && targets.len() < reflexive_max {
                            tracing::debug!("direct link: peer-reflexive candidate");
                            targets.push(src);
                            diag.peer_reflexive += 1;
                        }
                        if !heard.contains(&src) && heard.len() < targets.len() {
                            heard.push(src);
                        }
                    }
                    Some((PunchKind::Ack, seq)) => {
                        // Only an address this end sent a TOKENED punch to:
                        // a knocked or sprayed one proves nothing. Under
                        // nomination, only an ack for a punch sent to that
                        // very address (the symmetric check).
                        let ours = punched.contains(&src)
                            && (nomination.is_none()
                                || targets.get(wire::punch_seq_target(seq)) == Some(&src));
                        if ours {
                            diag.primary_heard.store(true, std::sync::atomic::Ordering::SeqCst);
                            diag.primary_acked.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        if nominating == Some(src) && wire::punch_seq_nominates(seq) {
                            diag.nominated_ms = Some(diag.elapsed_ms());
                            return Some(Punched { peer: src, early });
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
                if nominating.is_some() || nomination == Some(Role::Responder) {
                    continue;
                }
                if let Some(path) = acked.iter().find(|a| heard.contains(a)).copied() {
                    diag.path = Some(path);
                    if nomination == Some(Role::Initiator) {
                        // Nominate at once, then every tick until acked.
                        nominating = Some(path);
                        deadline = deadline.max(tokio::time::Instant::now() + NOMINATION_GRACE);
                        let index = targets.iter().position(|t| *t == path).unwrap_or(0);
                        let frame = wire::punch_frame(
                            PunchKind::Punch,
                            session,
                            token,
                            wire::punch_seq(counter, index, true),
                        );
                        let _ = socket.send_to(&frame, on_socket(socket, path)).await;
                        diag.nominations += 1;
                        continue;
                    }
                    return Some(Punched { peer: path, early });
                }
            }
        }
    }
}

/// +ciris: open the next tranche of birthday sockets toward `peer`: up to
/// [`BIRTHDAY_TRANCHE`] more, never past [`wire::MAX_BIRTHDAY_SOCKETS`],
/// each holding one of `permits` (review item 3: the process-wide count).
/// Fewer when the permits run out; a bind that fails sets the set's quota
/// flag (the host, or its carrier NAT's per-subscriber cap, has no more).
async fn open_tranche(
    set: &mut BirthdaySet,
    peer: SocketAddr,
    permits: &Arc<tokio::sync::Semaphore>,
) {
    let bind = if canonical(peer).is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0u16; 8], 0))
    };
    let room = wire::MAX_BIRTHDAY_SOCKETS.saturating_sub(set.opened.len());
    for _ in 0..BIRTHDAY_TRANCHE.min(room) {
        let Ok(permit) = Arc::clone(permits).try_acquire_owned() else {
            break;
        };
        match UdpSocket::bind(bind).await {
            Ok(s) => {
                set.opened.push(Arc::new(s));
                set.permits.push(Some(permit));
            }
            Err(_) => {
                set.quota.store(true, std::sync::atomic::Ordering::Relaxed);
                break;
            }
        }
    }
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
    /// +ciris (strategy review): every mapping sat at its socket's own local
    /// port plus one fixed offset (zero: port-preserving, or no NAT). Then
    /// the "steps" are the OS's random ephemeral ports, not the NAT's, and
    /// the NAT is never random.
    pub(crate) tracks_local: bool,
}

/// Probe `server` from two fresh sockets after the session socket's probe
/// mapped to `base_port`: the steps between the three mappings. `None` when
/// either probe went unanswered.
async fn allocation_steps(
    server: SocketAddr,
    protocol: ProbeProtocol,
    base_port: u16,
    base_local: Option<u16>,
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
    let (a, b) = (a.ok()?.port(), b.ok()?.port());
    let mut steps = steps_from(base_port, a, b);
    let locals = (
        base_local,
        s1.local_addr().ok().map(|l| l.port()),
        s2.local_addr().ok().map(|l| l.port()),
    );
    if let (Some(l0), Some(l1), Some(l2)) = locals {
        steps.tracks_local = tracks_local([(base_port, l0), (a, l1), (b, l2)]);
    }
    Some(steps)
}

/// +ciris (strategy review): whether each mapped port is its socket's local
/// port plus one shared offset (a port-preserving NAT, a port-shifting one,
/// or none): the NAT then allocates nothing of its own.
pub(crate) fn tracks_local(pairs: [(u16, u16); 3]) -> bool {
    let offset = |(mapped, local): (u16, u16)| i32::from(mapped) - i32::from(local);
    let first = offset(pairs[0]);
    pairs.iter().all(|p| offset(*p) == first)
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
        tracks_local: false,
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

/// +ciris (Eric's ruling on review item 6): on what evidence this node holds
/// its NAT to be a random endpoint-dependent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdmEvidence {
    /// The session socket's mapping moved between two observers. (Two ports
    /// on one IP detect a port-dependent mapping, not an address-only one.)
    TwoObserver,
    /// No observer could tell; our own allocation probes look random. The
    /// birthday phase then opens only once the punch has been silent past
    /// ESCALATE_AFTER, the rest of the evidence.
    FallbackSilenceRandom,
    /// Nothing says the NAT is random.
    None,
}

impl EdmEvidence {
    /// The word logged.
    pub fn as_str(self) -> &'static str {
        match self {
            EdmEvidence::TwoObserver => "two_observer",
            EdmEvidence::FallbackSilenceRandom => "fallback_silence_random",
            EdmEvidence::None => "none",
        }
    }
}

/// +ciris (re-review 6): a second observer is compared only within the
/// facilitator's address family, and only if it is another endpoint.
pub(crate) fn second_observer_usable(first: SocketAddr, second: SocketAddr) -> bool {
    let (a, b) = (canonical(first), canonical(second));
    a.is_ipv4() == b.is_ipv4() && a != b
}

/// What this node advertises and on what evidence: two-observer EDM as
/// [`advertise`]; with no observer able to tell, "b" alone when our own
/// steps look random (the fallback), never "q".
pub(crate) fn decide(
    class: NatClass,
    steps: Option<AllocationSteps>,
) -> (Vec<u16>, bool, EdmEvidence) {
    match class {
        NatClass::EndpointDependent => {
            let (q, b) = advertise(class, steps);
            let evidence = if b {
                EdmEvidence::TwoObserver
            } else {
                EdmEvidence::None
            };
            (q, b, evidence)
        }
        NatClass::Unknown => match steps {
            Some(s)
                if predict_ports(s).is_empty()
                    && s.step1 != 0
                    && s.step2 != 0
                    && !s.tracks_local =>
            {
                (Vec::new(), true, EdmEvidence::FallbackSilenceRandom)
            }
            _ => (Vec::new(), false, EdmEvidence::None),
        },
        _ => (Vec::new(), false, EdmEvidence::None),
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
            // A NAT that keeps each socket's port (or shifts it by one
            // offset) allocates nothing to predict and is never random.
            if s.tracks_local {
                return (Vec::new(), false);
            }
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
    /// The punching socket's own punch was acked (strategy model): what
    /// gates the birthday sockets.
    pub(crate) primary_acked: Arc<std::sync::atomic::AtomicBool>,
    /// Birthday hits passed over because the session socket had heard.
    pub(crate) birthday_ignored: usize,
    /// Knocks sent (predicted ports and spray), knocks the budget refused,
    /// knocks received, and whether the per-IP spray budget refused.
    pub(crate) knocks: u32,
    pub(crate) knocks_refused: u32,
    pub(crate) rx_knocks: u32,
    /// The punch's escalation decision, shared with its Escalation: `None`
    /// until the first escalated send, then whether the pair's allowance
    /// granted it.
    pub(crate) escalation: Option<Arc<std::sync::Mutex<Option<bool>>>>,
    /// The policy gate let this punch escalate.
    pub(crate) escalation_allowed: bool,
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
    /// +ciris: knocks sent when the peer was first heard or first acked:
    /// what the spray cost before it answered.
    knocks_at_first_answer: Option<u32>,
    /// +ciris: the punch window, in ms (plain or escalated).
    pub(crate) window_ms: u64,
    /// +ciris (pair agreement): nomination in force, and this side's part.
    pub(crate) nomination: Option<Role>,
    /// +ciris (pair agreement): nominations sent (initiator).
    pub(crate) nominations: u32,
    /// +ciris (pair agreement): when the nomination was acked (initiator)
    /// or arrived (responder).
    pub(crate) nominated_ms: Option<u64>,
    /// +ciris: the address settled on; only its class is logged.
    pub(crate) path: Option<SocketAddr>,
    /// +ciris: birthday tranches opened, shared with the birthday block.
    pub(crate) birthday_tranches: Arc<std::sync::atomic::AtomicUsize>,
    /// +ciris: a bind or send failed while opening birthday sockets.
    pub(crate) birthday_quota: Arc<std::sync::atomic::AtomicBool>,
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
            primary_acked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            birthday_ignored: 0,
            knocks: 0,
            knocks_refused: 0,
            rx_knocks: 0,
            escalation: None,
            escalation_allowed: false,
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
            knocks_at_first_answer: None,
            window_ms: PUNCH_WINDOW.as_millis() as u64,
            nomination: None,
            nominations: 0,
            nominated_ms: None,
            path: None,
            birthday_tranches: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            birthday_quota: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        self.knocks_at_first_answer.get_or_insert(self.knocks);
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
        self.knocks_at_first_answer.get_or_insert(self.knocks);
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
        let heard_not_acked = self
            .heard
            .iter()
            .filter(|(h, _)| !self.acked.iter().any(|(a, _)| a == h))
            .count();
        let acked_not_heard = self
            .acked
            .iter()
            .filter(|(a, _)| !self.heard.iter().any(|(h, _)| h == a))
            .count();
        let path_class = self.path.map_or("none", |p| self.relation(p).as_str());
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
            escalation_allowed = self.escalation_allowed,
            escalation_refused = self.escalation.as_ref().is_some_and(|g| {
                *g.lock().unwrap_or_else(|e| e.into_inner()) == Some(false)
            }),
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
            heard_not_acked,
            acked_not_heard,
            path_class,
            knocks_at_first_answer = self.knocks_at_first_answer.map_or(-1, i64::from),
            window_ms = self.window_ms,
            nomination = match self.nomination {
                Some(Role::Initiator) => "nominates",
                Some(Role::Responder) => "nominated",
                None => "off",
            },
            nominations = self.nominations,
            nominated_ms = self.nominated_ms.map_or(-1, |v| v as i64),
            birthday_tranches = self.birthday_tranches.load(std::sync::atomic::Ordering::Relaxed),
            birthday_quota = self.birthday_quota.load(std::sync::atomic::Ordering::Relaxed),
            spray_pps = SPRAY_PPS,
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
    evidence: EdmEvidence,
    steps: Option<AllocationSteps>,
    predicted: usize,
    birthday: bool,
    elapsed_ms: u64,
    stats: Option<ProbeStats>,
    extras_skipped: bool,
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
        edm_evidence = evidence.as_str(),
        alloc_measured = steps.is_some(),
        alloc_step1 = steps.map_or(0, |s| s.step1),
        alloc_step2 = steps.map_or(0, |s| s.step2),
        predicted,
        birthday,
        probe_attempts = stats.map_or(0, |s| s.attempts),
        probe_rtt_ms = stats.map_or(-1, |s| s.rtt_ms as i64),
        extras_skipped,
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
    let fragmenting = pmtu::dont_fragment(socket);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(start + MTU_PROBE_WINDOW) => {
                pmtu::restore(socket, fragmenting);
                return acked;
            }
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
                // +ciris (re-review D/E): a knock is never data.
                if wire::is_knock(frame) {
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
    // By role, never by number (the no-addresses rule covers ports).
    tracing::info!("direct link: facilitator listening (udp)");
    let task = tokio::spawn(async move {
        let mut buf = [0u8; 576];
        let mut limit = ReplyLimit::new();
        loop {
            let (len, src) = match socket.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!("direct link: facilitator recv error: {e}");
                    continue;
                }
            };
            // A stateless responder must not be a reflector (third review
            // N4): at most FACILITATOR_REPLIES_PER_SEC replies per source.
            if !limit.allow(src, std::time::Instant::now()) {
                continue;
            }
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

/// +ciris (third review N4, fourth review C): what a facilitator port
/// answers. One source (IP and port: one prober's socket) gets
/// [`FACILITATOR_REPLIES_PER_SEC`] a second; one IP, all its ports together,
/// [`FACILITATOR_IP_REPLIES_PER_SEC`], so a carrier-NAT address shared by
/// many subscribers is not held to one prober's rate; the whole port
/// [`FACILITATOR_GLOBAL_REPLIES_PER_SEC`]. A full table forgets its least
/// recently used entries instead of refusing newcomers. Honest probers send
/// a handful per proposal.
pub(crate) const FACILITATOR_REPLIES_PER_SEC: f64 = 10.0;
pub(crate) const FACILITATOR_IP_REPLIES_PER_SEC: f64 = 200.0;
pub(crate) const FACILITATOR_GLOBAL_REPLIES_PER_SEC: f64 = 5_000.0;
pub(crate) const FACILITATOR_SOURCES: usize = 4_096;

/// Reply buckets for a facilitator port.
pub(crate) struct ReplyLimit {
    sources: HashMap<SocketAddr, Bucket>,
    ips: HashMap<std::net::IpAddr, Bucket>,
    global: Bucket,
}

/// Insert-or-get a bucket in a table bounded at `max`: when full, the
/// least recently used eighth goes (amortised, so a flood of new sources
/// costs a sort per 512 of them, not a scan per datagram).
fn lru_bucket<K: std::hash::Hash + Eq + Copy>(
    table: &mut HashMap<K, Bucket>,
    key: K,
    max: usize,
    rate: f64,
    now: std::time::Instant,
) -> &mut Bucket {
    if table.len() >= max && !table.contains_key(&key) {
        let mut by_age: Vec<(std::time::Instant, K)> =
            table.iter().map(|(k, b)| (b.at, *k)).collect();
        by_age.sort_by_key(|(at, _)| *at);
        for (_, k) in by_age.into_iter().take((max / 8).max(1)) {
            table.remove(&k);
        }
    }
    table.entry(key).or_insert(Bucket {
        tokens: rate,
        at: now,
    })
}

impl ReplyLimit {
    pub(crate) fn new() -> Self {
        ReplyLimit {
            sources: HashMap::new(),
            ips: HashMap::new(),
            global: Bucket {
                tokens: FACILITATOR_GLOBAL_REPLIES_PER_SEC,
                at: std::time::Instant::now(),
            },
        }
    }

    /// May this source get a reply now? The source's bucket, its IP's,
    /// then the port's; a refusal at one spends nothing further on.
    pub(crate) fn allow(&mut self, src: SocketAddr, now: std::time::Instant) -> bool {
        let src = canonical(src);
        lru_bucket(
            &mut self.sources,
            src,
            FACILITATOR_SOURCES,
            FACILITATOR_REPLIES_PER_SEC,
            now,
        )
        .take(FACILITATOR_REPLIES_PER_SEC, now)
            && lru_bucket(
                &mut self.ips,
                src.ip(),
                FACILITATOR_SOURCES,
                FACILITATOR_IP_REPLIES_PER_SEC,
                now,
            )
            .take(FACILITATOR_IP_REPLIES_PER_SEC, now)
            && self.global.take(FACILITATOR_GLOBAL_REPLIES_PER_SEC, now)
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.sources.len()
    }
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
        /// +ciris: on what evidence (two observers, or own evidence alone).
        evidence: EdmEvidence,
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

/// +ciris: what a punch job says about escalation and settling, as the
/// runtime passes it on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PunchJobPlan {
    spray: bool,
    escalate: bool,
    link: [u8; 16],
    identity: Option<[u8; 16]>,
    escalated_window: bool,
    nomination: Option<Role>,
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
    birthday: HashMap<SessionId, EdmEvidence>,
    /// +ciris (review item 6, re-review 8): the second observer the session
    /// socket also probes, resolved in the background and cached. `None`
    /// until it resolves (or none configured): the fallback decides alone.
    observer2: Arc<std::sync::Mutex<Option<(SocketAddr, ProbeProtocol)>>>,
    observer2_resolver: Option<AbortOnDrop>,
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
            observer2: Arc::new(std::sync::Mutex::new(None)),
            observer2_resolver: None,
        }
    }

    /// +ciris (review item 6, re-review 8): the second observer, from the
    /// settings: resolved now and every RESOLVE_REFRESH (RESOLVE_RETRY while
    /// it does not resolve), each lookup bounded.
    pub(crate) fn set_second_observer(&mut self, observer: Option<(String, ProbeProtocol)>) {
        self.observer2_resolver = None;
        *self.observer2.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let Some((addr, protocol)) = observer else {
            return;
        };
        let cache = Arc::clone(&self.observer2);
        self.observer2_resolver = Some(AbortOnDrop(
            tokio::spawn(async move {
                loop {
                    let wait = match resolve_facilitator(&addr).await {
                        Ok(server2) => {
                            *cache.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some((server2, protocol));
                            RESOLVE_REFRESH
                        }
                        Err(_) => RESOLVE_RETRY,
                    };
                    tokio::time::sleep(wait).await;
                }
            })
            .abort_handle(),
        ));
    }

    fn observer2_cached(&self) -> Option<(SocketAddr, ProbeProtocol)> {
        *self.observer2.lock().unwrap_or_else(|e| e.into_inner())
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
                escalate,
                link,
                peer_identity,
                token,
                start_delay_ms,
                fast_liveness,
                spray,
                escalated_window,
                nomination,
                ..
            } => {
                self.fast_liveness.insert(session, fast_liveness);
                self.start_punch(
                    session,
                    peer,
                    alternates,
                    predicted,
                    token,
                    Duration::from_millis(start_delay_ms),
                    PunchJobPlan {
                        spray,
                        escalate,
                        link,
                        identity: peer_identity,
                        escalated_window,
                        nomination,
                    },
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
                    evidence: EdmEvidence::None,
                });
                return;
            }
        };
        self.sockets.insert(session, Arc::clone(&socket));
        // Resolved in the background and cached (re-review item 8): never
        // a lookup per session.
        let observer2 = self.observer2_cached();
        let task = tokio::spawn(async move {
            let probe_start = tokio::time::Instant::now();
            let (public, stats) =
                match probe_with_stats(&socket, server, protocol, PROBE_ATTEMPTS, PROBE_WAIT).await
                {
                    Ok((public, stats)) => {
                        tracing::info!("direct link: the facilitator answered the probe");
                        (Some(public), Some(stats))
                    }
                    Err(e) => {
                        tracing::info!("direct link: probe failed: {e}");
                        (None, None)
                    }
                };
            // +ciris (fourth review F): a responder's READY must beat the
            // initiator's READY_TIMEOUT (10 s). A main probe that already
            // took past DISCOVERY_EXTRAS_UNTIL (a lost attempt or two) skips
            // the second observer and the allocation probes: no class, so no
            // "q" and no two-observer "b", but the READY is in time.
            let late = probe_start.elapsed() > DISCOVERY_EXTRAS_UNTIL;
            let local = host_candidates(&socket, server);
            // +ciris (review item 6): the same session socket toward a second
            // observer (the facilitator's second probe port, or one the
            // operator configured); never a third party chosen here. Only
            // the same address family as the facilitator, and another
            // endpoint (re-review 6): across families nothing is compared.
            let second = match (observer2, public) {
                (Some((server2, protocol2)), Some(_))
                    if !late && second_observer_usable(server, server2) =>
                {
                    probe_with(&socket, server2, protocol2, 2, ALLOCATION_DEADLINE / 2)
                        .await
                        .ok()
                }
                _ => None,
            };
            let class = classify_nat(public, second, &local);
            // Our own allocation steps: on two-observer EDM, and as the
            // fallback's evidence when no observer could tell (a NAT that
            // did not keep the port and is not "no NAT").
            let local_port = socket.local_addr().map(|a| a.port()).ok();
            let measure = match (class, public) {
                (NatClass::EndpointDependent, Some(_)) => true,
                (NatClass::Unknown, Some(p)) => Some(p.port()) != local_port,
                _ => false,
            };
            let steps = match (measure && !late, public) {
                (true, Some(p)) => allocation_steps(server, protocol, p.port(), local_port).await,
                _ => None,
            };
            // The fallback ends once a usable second observer is resolved
            // (third review N1d): then only two-observer evidence counts.
            let observer_resolved =
                observer2.is_some_and(|(s2, _)| second_observer_usable(server, s2));
            let (predicted, birthday, evidence) = if observer_resolved && class == NatClass::Unknown
            {
                (Vec::new(), false, EdmEvidence::None)
            } else {
                decide(class, steps)
            };
            log_probe_diag(
                &session,
                &socket,
                public,
                &local,
                class,
                evidence,
                steps,
                predicted.len(),
                birthday,
                probe_start.elapsed().as_millis() as u64,
                stats,
                late,
            );
            let _ = outcomes
                .send(Outcome::Probed {
                    session,
                    public,
                    local,
                    predicted,
                    birthday,
                    evidence,
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
        job: PunchJobPlan,
    ) {
        let outcomes = self.outcomes.clone();
        let evidence = self.birthday.remove(&session).unwrap_or(EdmEvidence::None);
        let birthday = evidence != EdmEvidence::None;
        let spray = job.spray;
        let plan = PunchPlan {
            birthday,
            spray,
            escalate: job.escalate,
            pair: job.link,
            identity: job.identity,
            // Own evidence alone waits out the full silence gate (the same
            // socket caps on both evidence kinds: Eric's caps).
            birthday_after: if evidence == EdmEvidence::TwoObserver {
                BIRTHDAY_AFTER
            } else {
                ESCALATE_AFTER
            },
            window: if job.escalated_window {
                ESCALATED_PUNCH_WINDOW
            } else {
                PUNCH_WINDOW
            },
            nomination: job.nomination,
        };
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
            let seen = punch_with(&socket, &targets, &predicted, &session, &token, plan).await;
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
    pub(crate) fn note_birthday(&mut self, session: SessionId, evidence: EdmEvidence) {
        // Only for a session still holding its socket: one released while
        // its probe ran would never be removed again (review item 9).
        if evidence != EdmEvidence::None && self.sockets.contains_key(&session) {
            self.birthday.insert(session, evidence);
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

/// +ciris (leviculum#107): the Don't Fragment bit for the path-MTU probe.
/// Without it a path that fragments carries a 1400-byte probe in pieces, and
/// Linux, once an ICMP "fragmentation needed" has lowered its cached path
/// MTU, fragments the next probe itself: either way the probe measures 1400
/// on a path that does not carry it. `IP_PMTUDISC_PROBE` sets DF and sends at
/// the requested size whatever the cache says, so an oversized probe is
/// dropped on the way and never acked. The socket's own setting comes back
/// afterwards, so the interface's datagrams (no larger than what the probe
/// found) may still fragment if the path shrinks later. Linux and Android
/// only; elsewhere the probe runs as before.
#[cfg(any(target_os = "linux", target_os = "android"))]
mod pmtu {
    use std::os::fd::AsRawFd;

    /// The socket's settings before the probe, for each family it carries.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Previous {
        v4: Option<libc::c_int>,
        v6: Option<libc::c_int>,
    }

    fn get(fd: libc::c_int, level: libc::c_int, name: libc::c_int) -> Option<libc::c_int> {
        let mut value: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: `value` and `len` are valid for writes of the sizes given.
        let rc =
            unsafe { libc::getsockopt(fd, level, name, (&raw mut value).cast(), &raw mut len) };
        (rc == 0).then_some(value)
    }

    fn set(fd: libc::c_int, level: libc::c_int, name: libc::c_int, value: libc::c_int) -> bool {
        let len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: `value` is valid for reads of `len` bytes.
        let rc = unsafe { libc::setsockopt(fd, level, name, (&raw const value).cast(), len) };
        rc == 0
    }

    /// Set DF on `socket` (IPv4, and IPv6 on a dual-stack socket, whose
    /// IPv4-mapped traffic takes the IPv4 setting). A failure leaves that
    /// family as it was.
    pub(super) fn dont_fragment(socket: &tokio::net::UdpSocket) -> Previous {
        let fd = socket.as_raw_fd();
        let swap =
            |level, name, probe| get(fd, level, name).filter(|_| set(fd, level, name, probe));
        let v4 = swap(
            libc::IPPROTO_IP,
            libc::IP_MTU_DISCOVER,
            libc::IP_PMTUDISC_PROBE,
        );
        let v6 = if socket.local_addr().is_ok_and(|a| a.is_ipv6()) {
            swap(
                libc::IPPROTO_IPV6,
                libc::IPV6_MTU_DISCOVER,
                libc::IPV6_PMTUDISC_PROBE,
            )
        } else {
            None
        };
        Previous { v4, v6 }
    }

    /// Put back what [`dont_fragment`] changed.
    pub(super) fn restore(socket: &tokio::net::UdpSocket, previous: Previous) {
        let fd = socket.as_raw_fd();
        if let Some(v4) = previous.v4 {
            set(fd, libc::IPPROTO_IP, libc::IP_MTU_DISCOVER, v4);
        }
        if let Some(v6) = previous.v6 {
            set(fd, libc::IPPROTO_IPV6, libc::IPV6_MTU_DISCOVER, v6);
        }
    }

    /// The socket's current IPv4 and IPv6 settings, for tests.
    #[cfg(test)]
    pub(super) fn current(socket: &tokio::net::UdpSocket) -> (Option<i32>, Option<i32>) {
        let fd = socket.as_raw_fd();
        (
            get(fd, libc::IPPROTO_IP, libc::IP_MTU_DISCOVER),
            get(fd, libc::IPPROTO_IPV6, libc::IPV6_MTU_DISCOVER),
        )
    }
}

/// No DF control here: the probe runs as it did before leviculum#107.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
mod pmtu {
    pub(super) struct Previous;

    pub(super) fn dont_fragment(_: &tokio::net::UdpSocket) -> Previous {
        Previous
    }

    pub(super) fn restore(_: &tokio::net::UdpSocket, _: Previous) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A knock budget of a test's own: the process one is shared by every
    /// test running at once, and its per-IP cooldown would refuse each
    /// loopback spray after the first.
    fn test_budget() -> &'static Escalation {
        let budget: &'static KnockBudget = Box::leak(Box::new(KnockBudget::new()));
        Box::leak(Box::new(Escalation::new(
            budget,
            [9u8; 16],
            None,
            true,
            BIRTHDAY_AFTER,
        )))
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
            tracks_local: false,
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
            PunchShape::plain(),
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
            PunchShape::plain(),
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
                test_budget(),
                PunchShape::plain()
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
            None,
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
            PunchShape::plain(),
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
            PunchShape::plain(),
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

    /// +ciris (re-review 2, B; fourth review B, G): the escalation
    /// allowance is per link and per peer identity, 3 per window, whatever
    /// target; the volume per target (v4 address, v6 /64) is shared by every
    /// pair; the global ceiling covers every escalated send.
    #[test]
    fn the_budget_allows_three_escalations_per_pair_and_caps_each_target() {
        let budget = KnockBudget::new();
        let t0 = std::time::Instant::now();
        let ip: std::net::IpAddr = "198.51.100.7".parse().unwrap();
        let (pair, other_pair) = ([1u8; 16], [2u8; 16]);
        for _ in 0..PAIR_ESCALATIONS {
            assert!(budget.escalate(pair, None, t0));
        }
        assert!(
            !budget.escalate(pair, None, t0 + Duration::from_secs(60)),
            "3 per window, whatever the target"
        );
        assert!(
            budget.escalate(other_pair, None, t0),
            "another pair keeps its own"
        );
        assert!(
            budget.escalate(pair, None, t0 + BUDGET_WINDOW),
            "after the window"
        );
        // One pair gets its share of the target, never the whole of it; a
        // second pair the rest, up to the target's ceiling (third review B).
        // Spread over time so no rate binds.
        let send = |pair: [u8; 16], n: u32, from: u64| {
            (0..n)
                .filter(|i| {
                    budget.take(
                        pair,
                        ip,
                        t0 + Duration::from_millis(from + u64::from(*i)),
                        0,
                    )
                })
                .count() as u32
        };
        assert_eq!(
            send(pair, PAIR_TARGET_VOLUME + 10, 0),
            PAIR_TARGET_VOLUME,
            "one pair's share"
        );
        let off = 2 * u64::from(PAIR_TARGET_VOLUME + 10);
        assert_eq!(
            send(other_pair, PAIR_TARGET_VOLUME + 10, off),
            TARGET_VOLUME - PAIR_TARGET_VOLUME,
            "the target's ceiling over both"
        );
        assert_eq!(
            send([7u8; 16], 10, 2 * off),
            0,
            "a third pair finds the target full"
        );
        // IPv6 is one target per /64.
        let a: std::net::IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: std::net::IpAddr = "2001:db8:1:2::ffff".parse().unwrap();
        let c: std::net::IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(Target::of(a), Target::of(b));
        assert_ne!(Target::of(a), Target::of(c));
        let mapped: std::net::IpAddr = "::ffff:198.51.100.7".parse().unwrap();
        assert_eq!(Target::of(mapped), Target::of(ip));
    }

    /// +ciris (fourth review B): one identity cannot buy fresh allowances
    /// by opening fresh links; a link keeps its own when the peer shows a
    /// fresh identity each time.
    #[test]
    fn the_pair_allowance_is_kept_per_identity_as_well_as_per_link() {
        let budget = KnockBudget::new();
        let t0 = std::time::Instant::now();
        let who = Some([0xAAu8; 16]);
        for link in 0..PAIR_ESCALATIONS as u8 {
            assert!(budget.escalate([link; 16], who, t0), "link {link}");
        }
        assert!(
            !budget.escalate([0x77u8; 16], who, t0),
            "a fresh link, the same identity: spent"
        );
        let link = [0x55u8; 16];
        for i in 0..PAIR_ESCALATIONS as u8 {
            assert!(budget.escalate(link, Some([i; 16]), t0));
        }
        assert!(
            !budget.escalate(link, Some([0x99u8; 16]), t0),
            "the same link, a fresh identity: spent"
        );
        // A refusal spends nothing: the identity behind it still has room.
        assert!(budget.escalate([0x66u8; 16], Some([0x99u8; 16]), t0));
    }

    /// +ciris (third review N3): concurrent pairs share the global ceiling
    /// fairly: one busy pair cannot take another's floor.
    #[test]
    fn concurrent_pairs_each_keep_their_fair_share() {
        let budget = KnockBudget::new();
        let t0 = std::time::Instant::now();
        let ip = |i: u8| std::net::IpAddr::from([198, 51, 100, i]);
        let (greedy, honest) = ([10u8; 16], [11u8; 16]);
        // Both active at t0; the greedy one tries to send a whole ceiling.
        assert!(budget.take(honest, ip(2), t0, 0));
        let greedy_got = (0..ESCALATED_GLOBAL_PPS)
            .filter(|_| budget.take(greedy, ip(1), t0, 0))
            .count() as u32;
        assert!(
            greedy_got < ESCALATED_GLOBAL_PPS,
            "the greedy pair is held to its share"
        );
        let honest_got = (0..PAIR_FULL_PPS / 2)
            .filter(|_| budget.take(honest, ip(2), t0 + Duration::from_millis(300), 0))
            .count() as u32;
        assert!(honest_got > 0, "the honest pair still sends");
    }

    /// +ciris (third review, item 2): the window slides: a burst at the end
    /// of one window still counts at the start of the next.
    #[test]
    fn the_volume_window_slides() {
        let t0 = std::time::Instant::now();
        let mut w = Sliding::new(t0);
        w.cur = 1_000;
        let just_after = t0 + BUDGET_WINDOW + Duration::from_secs(1);
        let est = w.estimate(just_after);
        assert!(est > 990.0, "the previous window still counts: {est}");
        let est = w.estimate(t0 + 2 * BUDGET_WINDOW - Duration::from_secs(1));
        assert!(est < 10.0, "and fades as it slides out: {est}");
    }

    /// +ciris (fourth review G): the caps as Eric chose them, and the
    /// budget sized to fit them: three escalations per pair per window, and
    /// two concurrent escalations at their peak (320 knocks and 256 sockets'
    /// refreshes a second each), refilling with time.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn the_budget_fits_three_escalations_and_two_concurrent_at_their_peak() {
        assert_eq!(wire::MAX_BIRTHDAY_SOCKETS, 256);
        assert_eq!(wire::MAX_SPRAY_PORTS, 1_750);
        assert_eq!(SPRAY_PPS, 320);
        assert_eq!(PAIR_FULL_PPS, 832);
        assert_eq!(ESCALATED_GLOBAL_PPS, 1_664);
        // The spray, started at ESCALATE_AFTER, ends inside the escalated
        // window with room to spare.
        let spray_ends = ESCALATE_AFTER
            + Duration::from_millis(wire::MAX_SPRAY_PORTS as u64 * 1_000 / u64::from(SPRAY_PPS));
        assert!(spray_ends <= Duration::from_millis(7_100), "{spray_ends:?}");
        assert!(spray_ends + Duration::from_secs(5) <= ESCALATED_PUNCH_WINDOW);
        assert!(PAIR_TARGET_VOLUME >= PAIR_ESCALATIONS as u32 * ESCALATION_SENDS);
        assert!(TARGET_VOLUME >= 2 * PAIR_TARGET_VOLUME);
        let budget = KnockBudget::new();
        let t0 = std::time::Instant::now();
        let ip = |i: u8| std::net::IpAddr::from([198, 51, 100, i]);
        // Two pairs, each at its peak need, over one second in 1 ms steps.
        let (p1, p2) = ([12u8; 16], [13u8; 16]);
        let mut got = (0u32, 0u32);
        for ms in 0..1_000u64 {
            let at = t0 + Duration::from_millis(ms);
            for _ in 0..PAIR_FULL_PPS.div_ceil(1_000) {
                got.0 += u32::from(budget.take(p1, ip(1), at, PAIR_FULL_PPS));
                got.1 += u32::from(budget.take(p2, ip(2), at, PAIR_FULL_PPS));
            }
        }
        assert!(
            got.0 >= PAIR_FULL_PPS * 95 / 100 && got.1 >= PAIR_FULL_PPS * 95 / 100,
            "both peaks fit: {got:?}"
        );
        // And the ceiling holds: one second's worth at once, then refills.
        let budget = KnockBudget::new();
        let ips: Vec<std::net::IpAddr> = (1..=200u8).map(ip).collect();
        let taken = (0..2 * ESCALATED_GLOBAL_PPS as usize)
            .filter(|i| budget.take([8u8; 16], ips[i % ips.len()], t0, ESCALATED_GLOBAL_PPS))
            .count();
        assert_eq!(
            taken as u32, ESCALATED_GLOBAL_PPS,
            "one second's worth at once"
        );
        assert!(
            budget.take([8u8; 16], ips[0], t0 + Duration::from_millis(10), 0),
            "refills"
        );
    }

    /// +ciris (Eric's condition 2): a pair's rate share is budgeted by the
    /// sockets it actually holds open, not by the 256 cap.
    #[test]
    fn the_rate_need_follows_the_sockets_actually_open() {
        let budget: &'static KnockBudget = Box::leak(Box::new(KnockBudget::new()));
        let esc = Escalation::new(budget, [14u8; 16], None, true, BIRTHDAY_AFTER);
        assert_eq!(esc.need_pps(), SPRAY_PPS);
        esc.open.store(64, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(esc.need_pps(), SPRAY_PPS + 128);
        esc.open.store(
            wire::MAX_BIRTHDAY_SOCKETS,
            std::sync::atomic::Ordering::Relaxed,
        );
        assert_eq!(esc.need_pps(), PAIR_FULL_PPS);
    }

    /// +ciris (re-review 2): no escalated send at all under a policy that
    /// may not escalate, and a pair whose allowance is spent sends nothing
    /// more; the decision is taken once per punch, at its first send.
    #[test]
    fn an_escalation_is_gated_by_policy_and_by_the_pair_allowance() {
        let ip: std::net::IpAddr = "198.51.100.9".parse().unwrap();
        let budget: &'static KnockBudget = Box::leak(Box::new(KnockBudget::new()));
        let gated = Escalation::new(budget, [3u8; 16], None, false, BIRTHDAY_AFTER);
        assert!(!gated.send(ip), "accept_all never escalates");
        for _ in 0..PAIR_ESCALATIONS {
            let esc = Escalation::new(budget, [4u8; 16], None, true, BIRTHDAY_AFTER);
            assert!(
                esc.send(ip) && esc.send(ip),
                "a granted punch keeps sending"
            );
            assert!(!esc.refused());
        }
        let spent = Escalation::new(budget, [4u8; 16], None, true, BIRTHDAY_AFTER);
        assert!(!spent.send(ip));
        assert!(spent.refused());
    }

    /// +ciris (review item 3, fourth review G): birthday sockets are
    /// counted process-wide (two sessions' full sets); a session takes at
    /// most a tranche at a time and the per-session cap in all; a third
    /// session gets what is left, and with no permit left opens nothing.
    #[allow(clippy::assertions_on_constants)]
    #[tokio::test]
    async fn birthday_sockets_open_by_tranche_within_the_permits() {
        assert_eq!(PROCESS_BIRTHDAY_SOCKETS, 512);
        let sem = Arc::new(tokio::sync::Semaphore::new(PROCESS_BIRTHDAY_SOCKETS));
        let peer: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let mut one = BirthdaySet::new();
        let mut two = BirthdaySet::new();
        for (round, want) in [(1, 64), (2, 128), (3, 192), (4, 256), (5, 256)] {
            open_tranche(&mut one, peer, &sem).await;
            open_tranche(&mut two, peer, &sem).await;
            assert_eq!(one.opened.len(), want, "tranche {round}");
            assert_eq!(two.opened.len(), want, "tranche {round}");
        }
        assert!(one.full() && two.full(), "the per-session cap");
        let mut third = BirthdaySet::new();
        open_tranche(&mut third, peer, &sem).await;
        assert!(third.opened.is_empty(), "nothing left for a third");
        assert!(!third.quota.load(std::sync::atomic::Ordering::Relaxed));
        drop(one);
        open_tranche(&mut third, peer, &sem).await;
        assert_eq!(third.opened.len(), BIRTHDAY_TRANCHE, "what was freed");
        drop((two, third));
        assert_eq!(
            sem.available_permits(),
            PROCESS_BIRTHDAY_SOCKETS,
            "released with the sockets"
        );
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
            PunchShape::plain(),
        )
        .await;
        peer.abort();
        assert_eq!(r.map(|(p, w)| (p.peer, w.is_none())), Some((a_addr, true)));
    }

    /// +ciris (strategy model): the birthday sockets are gated on ACKS, not
    /// on hearing. A random side that hears the peer but whose own punches
    /// are never acked (its mapping toward the peer is not what the peer
    /// punches) opens them; one whose punch is acked at the gate does not.
    #[tokio::test]
    async fn birthday_sockets_open_unless_the_session_socket_is_acked() {
        for acks in [false, true] {
            let a = Arc::new(loopback().await);
            let a_addr = a.local_addr().unwrap();
            let b = loopback().await;
            let b_addr = b.local_addr().unwrap();
            let mut extras = Vec::new();
            for _ in 0..3 {
                extras.push(Arc::new(loopback().await));
            }
            let (session, token) = ([27u8; 16], [28u8; 32]);
            // The peer punches at once; it acks B's punches only when `acks`.
            let peer = {
                let a = Arc::clone(&a);
                tokio::spawn(async move {
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
                            if acks && src == b_addr {
                                let ack = wire::punch_frame(PunchKind::Ack, &session, &token, seq);
                                let _ = a.send_to(&ack, src).await;
                            }
                        }
                    }
                })
            };
            let mut diag = PunchDiag::new(std::slice::from_ref(&a_addr), &[], session);
            let open = Arc::clone(&diag.birthday_open);
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
                None,
            );
            let _ =
                tokio::time::timeout(BIRTHDAY_AFTER + Duration::from_millis(400), punching).await;
            peer.abort();
            let opened = open.load(std::sync::atomic::Ordering::Relaxed);
            assert_eq!(opened, if acks { 0 } else { 3 }, "acks={acks}");
        }
    }

    /// +ciris (docs/direct_link/nomination.md): who may leave the session
    /// socket for a birthday socket the peer reached.
    #[test]
    fn the_birthday_switch_follows_the_nomination_rule() {
        // Without nomination: only while the session socket heard nothing.
        assert!(may_switch(None, false, false));
        assert!(!may_switch(None, true, false));
        // The initiator: while nothing it sent is acked, heard or not.
        assert!(may_switch(Some(Role::Initiator), true, false));
        assert!(!may_switch(Some(Role::Initiator), true, true));
        // The responder: on a nomination, whatever its session socket saw.
        assert!(may_switch(Some(Role::Responder), true, true));
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

    /// +ciris (Eric's ruling, item 6): with no observer able to tell, our
    /// own random allocation steps are the evidence: "b", and the birthday
    /// phase gated on the full silence gate; steady steps advertise nothing
    /// (never "q" without two observers).
    #[test]
    fn the_own_evidence_fallback_needs_random_steps() {
        let random = steps_from(40000, 51234, 33310);
        let steady = steps_from(40000, 40001, 40002);
        assert_eq!(
            decide(NatClass::Unknown, Some(random)),
            (vec![], true, EdmEvidence::FallbackSilenceRandom)
        );
        assert_eq!(
            decide(NatClass::Unknown, Some(steady)),
            (vec![], false, EdmEvidence::None)
        );
        assert_eq!(
            decide(NatClass::Unknown, None),
            (vec![], false, EdmEvidence::None)
        );
        assert_eq!(
            decide(NatClass::EndpointDependent, Some(random)),
            (vec![], true, EdmEvidence::TwoObserver)
        );
        let (q, b, e) = decide(NatClass::EndpointDependent, Some(steady));
        assert!(!q.is_empty() && !b && e == EdmEvidence::None);
        assert_eq!(
            decide(NatClass::EndpointIndependent, Some(random)),
            (vec![], false, EdmEvidence::None)
        );
    }

    /// +ciris (re-review 6): a second observer of the other address family,
    /// or the facilitator itself, is never compared.
    #[test]
    fn a_second_observer_counts_only_in_the_same_family() {
        let v4: SocketAddr = "203.0.113.7:4242".parse().unwrap();
        let v4b: SocketAddr = "203.0.113.7:4243".parse().unwrap();
        let v6: SocketAddr = "[2001:db8::7]:4243".parse().unwrap();
        let mapped: SocketAddr = "[::ffff:203.0.113.7]:4243".parse().unwrap();
        assert!(second_observer_usable(v4, v4b));
        assert!(second_observer_usable(v4, mapped));
        assert!(!second_observer_usable(v4, v6));
        assert!(!second_observer_usable(v4, v4));
    }

    /// +ciris (the server's condition on dial + 1): the prober sends the
    /// facilitator's second probe port (udp) probes and nothing else.
    #[tokio::test]
    async fn the_second_probe_port_gets_probes_only() {
        let first = loopback().await;
        let first_addr = first.local_addr().unwrap();
        let second = Arc::new(loopback().await);
        let second_addr = second.local_addr().unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
        // The facilitator answers on the first port; the second records.
        let answer = tokio::spawn(async move {
            let mut buf = [0u8; 576];
            loop {
                let Ok((len, src)) = first.recv_from(&mut buf).await else {
                    continue;
                };
                if let Some(nonce) = wire::parse_rnsp_request(&buf[..len]) {
                    let _ = first.send_to(&wire::rnsp_response(&nonce, &src), src).await;
                }
            }
        });
        let record = {
            let (second, seen) = (Arc::clone(&second), Arc::clone(&seen));
            tokio::spawn(async move {
                let mut buf = [0u8; 576];
                loop {
                    let Ok((len, src)) = second.recv_from(&mut buf).await else {
                        continue;
                    };
                    seen.lock().unwrap().push(buf[..len].to_vec());
                    if let Some(nonce) = wire::parse_rnsp_request(&buf[..len]) {
                        let _ = second
                            .send_to(&wire::rnsp_response(&nonce, &src), src)
                            .await;
                    }
                }
            })
        };
        let (tx, mut rx) = mpsc::channel(4);
        let mut rt = DirectLinkRuntime::new(tx);
        *rt.observer2.lock().unwrap() = Some((second_addr, ProbeProtocol::Rnsp));
        rt.run(DirectLinkJob::Probe {
            session: [31u8; 16],
            server: first_addr,
            protocol: ProbeProtocol::Rnsp,
        });
        let outcome = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        answer.abort();
        record.abort();
        assert!(matches!(
            outcome,
            Ok(Some(Outcome::Probed {
                public: Some(_),
                ..
            }))
        ));
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty(), "the second port was probed");
        assert!(
            seen.iter().all(|f| wire::parse_rnsp_request(f).is_some()),
            "only RNSP probes"
        );
    }

    /// +ciris (re-review 7): a knock the budget refuses is not counted as
    /// sprayed (it never left).
    #[tokio::test]
    async fn a_refused_knock_is_not_counted_as_sprayed() {
        let a = loopback().await;
        let advertised = loopback().await.local_addr().unwrap();
        let (session, token) = ([33u8; 16], [34u8; 32]);
        let budget: &'static KnockBudget = Box::leak(Box::new(KnockBudget::new()));
        for _ in 0..PAIR_ESCALATIONS {
            assert!(budget.escalate([5u8; 16], None, std::time::Instant::now()));
        }
        let esc = Escalation::new(budget, [5u8; 16], None, true, BIRTHDAY_AFTER);
        let mut diag = PunchDiag::new(std::slice::from_ref(&advertised), &[], session);
        let now = tokio::time::Instant::now();
        let r = punch_inner(
            &a,
            std::slice::from_ref(&advertised),
            &[],
            &session,
            &token,
            &mut diag,
            now + Duration::from_millis(600),
            now,
            cycle(vec![50_001, 50_002, 50_003]),
            None,
            &esc,
            None,
        )
        .await;
        assert!(r.is_none());
        assert!(esc.refused());
        assert_eq!(diag.sprayed, 0);
        assert!(diag.knocks_refused > 0);
        assert_eq!(diag.knocks, 0);
    }

    /// +ciris (third review N4): a facilitator answers one source at most
    /// FACILITATOR_REPLIES_PER_SEC a second; others are not held back by it.
    #[test]
    fn a_facilitator_limits_replies_per_source() {
        let mut limit = ReplyLimit::new();
        let t0 = std::time::Instant::now();
        let a: SocketAddr = "198.51.100.1:40000".parse().unwrap();
        let b: SocketAddr = "198.51.100.2:40000".parse().unwrap();
        let got = (0..100).filter(|_| limit.allow(a, t0)).count();
        assert_eq!(got as f64, FACILITATOR_REPLIES_PER_SEC);
        assert!(limit.allow(b, t0), "another source is unaffected");
        assert!(limit.allow(a, t0 + Duration::from_secs(1)), "it refills");
    }

    /// +ciris (fourth review C): a carrier-NAT address is many subscribers:
    /// each of its ports (one prober's socket) gets its own rate, under a
    /// larger cap for the IP; a full table forgets its least recently used
    /// sources instead of refusing a newcomer; a global cap bounds the port.
    #[test]
    fn a_facilitator_serves_a_shared_address_and_never_locks_out_a_newcomer() {
        let mut limit = ReplyLimit::new();
        let t0 = std::time::Instant::now();
        let cgnat = |port: u16| SocketAddr::new("198.51.100.3".parse().unwrap(), port);
        // Twenty subscribers behind one address, each probing a handful.
        let served = (0..20u16)
            .flat_map(|i| (0..5).map(move |_| cgnat(20_000 + i)))
            .filter(|src| limit.allow(*src, t0))
            .count();
        assert_eq!(served, 100, "one address, many probers");
        // But not without bound: the address's own cap.
        let flood = (0..1_000u16)
            .filter(|i| limit.allow(cgnat(30_000 + i), t0))
            .count();
        assert!(flood < 1_000 && flood as f64 <= FACILITATOR_IP_REPLIES_PER_SEC);
        // A full table (sources spread so no rate binds) takes a newcomer.
        let mut limit = ReplyLimit::new();
        for i in 0..FACILITATOR_SOURCES as u32 {
            let src = SocketAddr::new(std::net::IpAddr::from(i.to_be_bytes()), 1);
            assert!(limit.allow(src, t0 + Duration::from_millis(u64::from(i))));
        }
        let late = t0 + Duration::from_secs(10);
        assert!(
            limit.allow("203.0.113.200:9".parse().unwrap(), late),
            "a newcomer"
        );
        assert!(limit.tracked() <= FACILITATOR_SOURCES);
        // The global cap.
        let mut limit = ReplyLimit::new();
        let total = (0..(2.0 * FACILITATOR_GLOBAL_REPLIES_PER_SEC) as u32)
            .filter(|i| {
                let src = SocketAddr::new(std::net::IpAddr::from(i.to_be_bytes()), 2);
                limit.allow(src, t0)
            })
            .count();
        assert_eq!(total as f64, FACILITATOR_GLOBAL_REPLIES_PER_SEC);
    }

    /// +ciris (strategy review): the measuring sockets get OS-random local
    /// ports, so behind a port-preserving NAT (or none) their mappings look
    /// like random allocation. Mappings that track the local ports are never
    /// "random" and predict nothing, on either evidence kind.
    #[test]
    fn steps_that_track_the_local_ports_are_never_random() {
        // Port-preserving, OS-random locals.
        assert!(tracks_local([
            (40_000, 40_000),
            (51_234, 51_234),
            (33_017, 33_017)
        ]));
        // A port-shifting NAT: one offset.
        assert!(tracks_local([
            (40_100, 40_000),
            (51_334, 51_234),
            (33_117, 33_017)
        ]));
        // A NAT of its own allocation.
        assert!(!tracks_local([
            (61_002, 40_000),
            (17_455, 51_234),
            (29_871, 33_017)
        ]));
        // A genuinely random NAT (mappings that do not track the locals)
        // still reads random, on both evidence kinds.
        let mut s = steps_from(40_000, 51_234, 33_017);
        assert!(!s.tracks_local);
        assert!(
            decide(NatClass::Unknown, Some(s)).1,
            "random-looking steps alone"
        );
        assert!(
            advertise(NatClass::EndpointDependent, Some(s)).1,
            "two-observer random"
        );
        s.tracks_local = true;
        assert_eq!(
            decide(NatClass::Unknown, Some(s)),
            (Vec::new(), false, EdmEvidence::None)
        );
        assert_eq!(
            advertise(NatClass::EndpointDependent, Some(s)),
            (Vec::new(), false)
        );
    }

    /// +ciris (strategy review): on loopback (no NAT, OS-random ports) the
    /// live measurement says the mappings track the local ports.
    #[tokio::test]
    async fn the_allocation_measurement_sees_no_nat_on_loopback() {
        let facilitator = loopback().await;
        let port = facilitator.local_addr().unwrap().port();
        drop(facilitator);
        let task = spawn_facilitator(port).await.unwrap();
        let server: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let socket = loopback().await;
        let local = socket.local_addr().unwrap().port();
        let public = probe(&socket, server, ProbeProtocol::Rnsp).await.unwrap();
        let steps = allocation_steps(server, ProbeProtocol::Rnsp, public.port(), Some(local))
            .await
            .expect("measured");
        task.abort();
        assert!(steps.tracks_local, "{steps:?}");
        assert!(!decide(NatClass::Unknown, Some(steps)).1);
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
        let steps = allocation_steps(
            silent.local_addr().unwrap(),
            ProbeProtocol::Rnsp,
            40000,
            None,
        )
        .await;
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
                PunchShape::plain(),
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
                test_budget(),
                PunchShape::plain()
            )
        );
        let (pa, _) = ra.expect("A punches through");
        let (pb, winner) = rb.expect("B punches through");
        let winner = winner.expect("on a birthday socket");
        assert_eq!(pb.peer, a_addr);
        assert_eq!(pa.peer, winner.local_addr().unwrap());
        assert!(extra_addrs.contains(&pa.peer));
    }

    /// +ciris (fourth review G): toward a silent peer the birthday sockets
    /// open in tranches of 64, one every 500 ms from the birthday gate, up
    /// to 256, and the diag line knows how far they got.
    #[tokio::test]
    async fn birthday_sockets_ramp_by_tranche_while_nothing_is_heard() {
        let b = loopback().await;
        let nobody = loopback().await.local_addr().unwrap();
        let sem = Arc::new(tokio::sync::Semaphore::new(PROCESS_BIRTHDAY_SOCKETS));
        let (session, token) = ([41u8; 16], [42u8; 32]);
        let mut diag = PunchDiag::new(std::slice::from_ref(&nobody), &[], session);
        let open = Arc::clone(&diag.birthday_open);
        let tranches = Arc::clone(&diag.birthday_tranches);
        let now = tokio::time::Instant::now();
        let punching = punch_birthday(
            &b,
            Extras::OpenFrom(Arc::clone(&sem)),
            std::slice::from_ref(&nobody),
            &[],
            &session,
            &token,
            &mut diag,
            now + ESCALATED_PUNCH_WINDOW,
            now + ESCALATE_AFTER,
            None,
            test_budget(),
            None,
        );
        let sample = async {
            let mut seen = Vec::new();
            for at in [250u64, 750, 1_250, 1_750, 2_750] {
                tokio::time::sleep_until(now + BIRTHDAY_AFTER + Duration::from_millis(at)).await;
                seen.push(open.load(std::sync::atomic::Ordering::Relaxed));
            }
            seen
        };
        let (_, seen) = tokio::join!(
            tokio::time::timeout(BIRTHDAY_AFTER + Duration::from_millis(3_000), punching),
            sample
        );
        assert_eq!(seen, [64, 128, 192, 256, 256]);
        assert_eq!(tranches.load(std::sync::atomic::Ordering::Relaxed), 4);
        assert_eq!(
            sem.available_permits(),
            PROCESS_BIRTHDAY_SOCKETS,
            "every socket closed with the punch"
        );
    }

    /// +ciris (fourth review G): a send that fails (a carrier NAT's port
    /// quota, a host out of sockets) stops the ramp where it is.
    #[tokio::test]
    async fn a_failing_send_stops_the_ramp() {
        let b = loopback().await;
        // Nothing can be sent to port 0.
        let nowhere: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let sem = Arc::new(tokio::sync::Semaphore::new(PROCESS_BIRTHDAY_SOCKETS));
        let (session, token) = ([43u8; 16], [44u8; 32]);
        let mut diag = PunchDiag::new(std::slice::from_ref(&nowhere), &[], session);
        let open = Arc::clone(&diag.birthday_open);
        let quota = Arc::clone(&diag.birthday_quota);
        let now = tokio::time::Instant::now();
        let punching = punch_birthday(
            &b,
            Extras::OpenFrom(Arc::clone(&sem)),
            std::slice::from_ref(&nowhere),
            &[],
            &session,
            &token,
            &mut diag,
            now + ESCALATED_PUNCH_WINDOW,
            now + ESCALATE_AFTER,
            None,
            test_budget(),
            None,
        );
        let _ = tokio::time::timeout(BIRTHDAY_AFTER + Duration::from_millis(1_800), punching).await;
        assert_eq!(
            open.load(std::sync::atomic::Ordering::Relaxed),
            BIRTHDAY_TRANCHE
        );
        assert!(quota.load(std::sync::atomic::Ordering::Relaxed));
    }

    /// +ciris: a relay standing in for one path between A and B: A reaches
    /// B at `to_b` and is seen there from `to_a`, and the reverse. It drops
    /// the punch frames of one kind in one direction, so a test decides
    /// which pairs each side can prove.
    struct PathRelay {
        to_b: SocketAddr,
        to_a: SocketAddr,
        task: tokio::task::JoinHandle<()>,
    }

    impl PathRelay {
        async fn new(
            bind: &str,
            a: SocketAddr,
            b: SocketAddr,
            drop_a_to_b: Option<PunchKind>,
            drop_b_to_a: Option<PunchKind>,
        ) -> Option<Self> {
            let p = UdpSocket::bind(bind).await.ok()?;
            let q = UdpSocket::bind(bind).await.ok()?;
            let (to_b, to_a) = (p.local_addr().ok()?, q.local_addr().ok()?);
            let task = tokio::spawn(async move {
                let (mut bp, mut bq) = ([0u8; 2048], [0u8; 2048]);
                loop {
                    tokio::select! {
                        Ok((len, _)) = p.recv_from(&mut bp) => {
                            let f = &bp[..len];
                            if drop_a_to_b.is_none() || wire::punch_frame_kind(f) != drop_a_to_b {
                                let _ = q.send_to(f, b).await;
                            }
                        }
                        Ok((len, _)) = q.recv_from(&mut bq) => {
                            let f = &bq[..len];
                            if drop_b_to_a.is_none() || wire::punch_frame_kind(f) != drop_b_to_a {
                                let _ = p.send_to(f, a).await;
                            }
                        }
                    }
                }
            });
            Some(PathRelay { to_b, to_a, task })
        }
    }

    impl Drop for PathRelay {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// Punch A (the initiator) and B (the responder) toward each other over
    /// two relays, `first` and `second`, each side naming its targets in
    /// its own order; `nominate` puts nomination in force.
    async fn punch_over(
        a: &UdpSocket,
        b: &UdpSocket,
        a_targets: &[SocketAddr],
        b_targets: &[SocketAddr],
        nominate: bool,
        session: SessionId,
    ) -> (Option<SocketAddr>, Option<SocketAddr>) {
        let token = [0x3Cu8; 32];
        let shape = |role| PunchShape {
            window: Duration::from_secs(4),
            nomination: nominate.then_some(role),
        };
        let (ra, rb) = tokio::join!(
            punch_with_ports(
                a,
                None,
                a_targets,
                &[],
                &session,
                &token,
                None,
                test_budget(),
                shape(Role::Initiator)
            ),
            async {
                let r = punch_with_ports(
                    b,
                    None,
                    b_targets,
                    &[],
                    &session,
                    &token,
                    None,
                    test_budget(),
                    shape(Role::Responder),
                )
                .await;
                // B's direct interface (here: its MTU probe phase) acks the
                // initiator's late punches, nominations among them.
                if let Some((p, _)) = &r {
                    let mut early = Vec::new();
                    let _ = probe_path_mtu(b, p.peer, &session, &token, &mut early).await;
                }
                r
            }
        );
        (ra.map(|(p, _)| p.peer), rb.map(|(p, _)| p.peer))
    }

    /// +ciris (pair agreement; the TLA+ model's `samenat-hairpin` trace,
    /// property SamePath): two nodes behind one hairpinning NAT. A proves
    /// B's host candidate first (the LAN path: its acks to B are lost),
    /// B proves A's hairpinned public mapping first (that path's acks to A
    /// are lost). Each settling on its own first proven pair, they commit to
    /// different pairs and each drops the other's frames. With nomination B
    /// takes A's pair: both ends use one 5-tuple.
    #[tokio::test]
    async fn samenat_hairpin_both_ends_settle_on_the_nominated_pair() {
        let (a, b) = (loopback().await, loopback().await);
        let (a_real, b_real) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        for nominate in [false, true] {
            let lan = PathRelay::new("127.0.0.1:0", a_real, b_real, Some(PunchKind::Ack), None)
                .await
                .unwrap();
            let hairpin = PathRelay::new("127.0.0.1:0", a_real, b_real, None, Some(PunchKind::Ack))
                .await
                .unwrap();
            // Each side's public candidate first, then the host candidate.
            let a_targets = [hairpin.to_b, lan.to_b];
            let b_targets = [hairpin.to_a, lan.to_a];
            let session = [0x50 + u8::from(nominate); 16];
            let (pa, pb) = punch_over(&a, &b, &a_targets, &b_targets, nominate, session).await;
            if nominate {
                assert_eq!(pa, Some(lan.to_b), "A on the LAN pair");
                assert_eq!(pb, Some(lan.to_a), "and B on the same pair");
            } else {
                // The bug, without nomination: two pairs.
                assert_eq!((pa, pb), (Some(lan.to_b), Some(hairpin.to_a)));
            }
        }
    }

    /// +ciris (pair agreement): a dual-stack pair where each side hears a
    /// different family first (A proves IPv4 only, B IPv6 only). With
    /// nomination both use A's IPv4 pair.
    #[tokio::test]
    async fn a_dual_stack_pair_hearing_different_families_first_agrees() {
        let dual = || async {
            let s = bind_session_socket("127.0.0.1:1".parse().unwrap()).ok()?;
            let s = UdpSocket::from_std(s).ok()?;
            s.local_addr().ok()?.is_ipv6().then_some(s)
        };
        let (Some(a), Some(b)) = (dual().await, dual().await) else {
            return; // no dual-stack sockets on this host
        };
        let (pa, pb) = (
            a.local_addr().unwrap().port(),
            b.local_addr().unwrap().port(),
        );
        let v4 = |p: u16| SocketAddr::from(([127, 0, 0, 1], p));
        let v6 = |p: u16| SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], p));
        let Some(r6) = PathRelay::new("[::1]:0", v6(pa), v6(pb), None, Some(PunchKind::Ack)).await
        else {
            return; // no IPv6 loopback
        };
        let r4 = PathRelay::new("127.0.0.1:0", v4(pa), v4(pb), Some(PunchKind::Ack), None)
            .await
            .unwrap();
        // A lists IPv4 first, B IPv6 first.
        let a_targets = [r4.to_b, r6.to_b];
        let b_targets = [r6.to_a, r4.to_a];
        let (ga, gb) = punch_over(&a, &b, &a_targets, &b_targets, false, [0x60; 16]).await;
        assert_eq!(
            (ga, gb),
            (Some(r4.to_b), Some(r6.to_a)),
            "the bug: two families"
        );
        let (ga, gb) = punch_over(&a, &b, &a_targets, &b_targets, true, [0x61; 16]).await;
        assert_eq!((ga, gb), (Some(r4.to_b), Some(r4.to_a)), "one pair, A's");
    }

    /// +ciris (docs/direct_link/nomination.md, one rule): the hard side as
    /// the responder under nomination. Its birthday sockets ack every punch
    /// and keep listening; the initiator proves one of their pairs, names
    /// it, and the responder settles on exactly the birthday socket the
    /// nomination arrived on, which carries the path.
    #[tokio::test]
    async fn a_nominated_birthday_socket_carries_the_path() {
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
        let (session, token) = ([45u8; 16], [46u8; 32]);
        let spray_ports: Vec<u16> = extra_addrs.iter().map(|a| a.port()).collect();
        let to_b = [SocketAddr::new(a_addr.ip(), b.local_addr().unwrap().port())];
        let shape = |role| PunchShape {
            window: PUNCH_WINDOW,
            nomination: Some(role),
        };
        let (ra, rb) = tokio::join!(
            punch_with_ports(
                &a,
                None,
                &to_b,
                &[],
                &session,
                &token,
                cycle(spray_ports),
                test_budget(),
                shape(Role::Initiator)
            ),
            punch_with_ports(
                &b,
                Some(Extras::Given(extras)),
                std::slice::from_ref(&a_addr),
                &[],
                &session,
                &token,
                None,
                test_budget(),
                shape(Role::Responder)
            )
        );
        let (pa, _) = ra.expect("A punches through");
        let (pb, winner) = rb.expect("B punches through");
        let winner = winner.expect("on a birthday socket");
        assert_eq!(pb.peer, a_addr);
        assert_eq!(
            pa.peer,
            winner.local_addr().unwrap(),
            "A nominated B's winner"
        );
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
                    PunchShape::plain(),
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
        // +ciris (leviculum#107): an IPv6 path of exactly 1280 carries 1232.
        assert_eq!(probe_through(1279).await, Some(1232));
        assert_eq!(probe_through(1232).await, Some(1232));
        assert_eq!(probe_through(1231).await, None);
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
                facilitator_second_port: false,
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

    /// +ciris (leviculum#107): the probe sends with DF set whatever the
    /// cached path MTU says, and gives the socket its own setting back.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[tokio::test]
    async fn the_probe_sets_dont_fragment_and_puts_it_back() {
        let dual = bind_session_socket("127.0.0.1:9".parse().unwrap())
            .and_then(UdpSocket::from_std)
            .unwrap();
        let before = pmtu::current(&dual);
        let previous = pmtu::dont_fragment(&dual);
        assert_eq!(pmtu::current(&dual).0, Some(libc::IP_PMTUDISC_PROBE));
        if dual.local_addr().unwrap().is_ipv6() {
            assert_eq!(pmtu::current(&dual).1, Some(libc::IPV6_PMTUDISC_PROBE));
        }
        pmtu::restore(&dual, previous);
        assert_eq!(pmtu::current(&dual), before);
        // And around a whole probe, answered or not.
        assert_eq!(probe_through(1300).await, Some(1280));
        let mut early = Vec::new();
        let nobody = loopback().await.local_addr().unwrap();
        assert_eq!(
            probe_path_mtu(&dual, nobody, &[7; 16], &[8; 32], &mut early).await,
            None
        );
        assert_eq!(pmtu::current(&dual), before);
    }
}
