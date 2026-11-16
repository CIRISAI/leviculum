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
//! nat_probe_addrs = rns2.example.org:4343, stun:stun.example.net:3478
//! answer_probes = yes                 # answer direct-link peers' NAT probes
//! ```
//!
//! +ciris (leviculum#100): a node tells its NAT type
//! ([`leviculum_core::direct_link::NatMapping`]) by probing two servers at
//! distinct addresses from one socket; see [`nat`]. The servers are its
//! facilitators, its live direct-link peers (every node answers their
//! probes unless `answer_probes = no`), and the optional `nat_probe_addrs`
//! (comma-separated `host:port`, `stun:` for a STUN server).

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use leviculum_core::direct_link::wire::{self, PunchKind, SessionId};
use leviculum_core::direct_link::ProbeProtocol;
use leviculum_core::node::{DirectLinkJob, DirectLinkPolicy};
use leviculum_core::transport::InterfaceId;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::error::Error;
use crate::interfaces::direct_udp::{spawn_direct_udp_interface, DirectUdpTiming};
use crate::interfaces::InterfaceHandle;
use crate::sync_ext::MutexRecover;

// +ciris (leviculum#100): the NAT-type probe.
pub(crate) mod nat;
pub use nat::{parse_nat_probe_servers, NatProbeServer};
pub(crate) use nat::{DirectLinkExtras, DirectLinkExtrasConfig};

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
    /// +ciris (leviculum#100): more probe servers, comma-separated
    /// `host:port`, each optionally `stun:`-prefixed; see
    /// [`parse_nat_probe_servers`].
    #[serde(default)]
    pub nat_probe_addrs: Option<String>,
    /// +ciris (leviculum#100): answer direct-link peers' NAT probes
    /// (default yes).
    #[serde(default)]
    pub answer_probes: Option<bool>,
    /// An INI `answer_probes` that is no boolean, kept for the builder to
    /// refuse.
    #[serde(skip)]
    pub invalid_answer_probes: Option<String>,
    /// Which peers' direct-link requests to accept: `reject` (default),
    /// `accept_all` or `identified_only`.
    #[serde(default)]
    pub direct_connect_policy: Option<String>,
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
        "nat_probe_addrs" => config.direct_link.nat_probe_addrs = Some(value.trim().to_string()),
        "answer_probes" => match nat::parse_bool(value) {
            Some(on) => config.direct_link.answer_probes = Some(on),
            None => config.direct_link.invalid_answer_probes = Some(value.trim().to_string()),
        },
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

// Probe

/// Probe attempts and the wait for each answer.
const PROBE_ATTEMPTS: u32 = 3;
const PROBE_WAIT: Duration = Duration::from_secs(2);

/// Learn `socket`'s reflexive address from `server`.
#[cfg(test)]
pub(crate) async fn probe(
    socket: &UdpSocket,
    server: SocketAddr,
    protocol: ProbeProtocol,
) -> io::Result<SocketAddr> {
    nat::probe_many(socket, &[(server, nat::Ask::Server(protocol))], true)
        .await
        .first()
        .and_then(|p| p.mapped)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("facilitator {server} did not answer"),
            )
        })
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
#[cfg(test)]
pub(crate) async fn punch(
    socket: &UdpSocket,
    targets: &[SocketAddr],
    session: &SessionId,
    token: &[u8; 32],
) -> Option<Punched> {
    punch_logged(socket, targets, session, token, &Label::default()).await
}

/// [`punch`], naming the session's link and peer hash in its log line for
/// the first punch heard (+ciris, leviculum#100).
pub(crate) async fn punch_logged(
    socket: &UdpSocket,
    targets: &[SocketAddr],
    session: &SessionId,
    token: &[u8; 32],
    label: &Label,
) -> Option<Punched> {
    let started = tokio::time::Instant::now();
    let deadline = tokio::time::Instant::now() + PUNCH_WINDOW;
    let mut targets: Vec<SocketAddr> = alloc_targets(
        canonical(*targets.first()?),
        &targets[1..]
            .iter()
            .map(|a| canonical(*a))
            .collect::<Vec<_>>(),
    );
    let advertised = targets.len();
    // Addresses a punch actually left for; only an ack from one counts.
    let mut punched: Vec<SocketAddr> = Vec::new();
    // Addresses a valid punch came from (each acked at once), and those an
    // ack came from; the path is the first address in both, and punched.
    let mut heard: Vec<SocketAddr> = Vec::new();
    let mut acked: Vec<SocketAddr> = Vec::new();
    let mut early: Vec<Vec<u8>> = Vec::new();
    let mut early_bytes = 0usize;
    let mut seq: u32 = 0;
    let mut tick = tokio::time::interval(PUNCH_INTERVAL);
    let mut buf = vec![0u8; 65_535];
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return None,
            _ = tick.tick() => {
                let frame = wire::punch_frame(PunchKind::Punch, session, token, seq);
                seq = seq.wrapping_add(1) % wire::KEEPALIVE_SEQ;
                for target in &targets {
                    if socket.send_to(&frame, on_socket(socket, *target)).await.is_ok()
                        && !punched.contains(target)
                    {
                        punched.push(*target);
                    }
                }
            }
            result = socket.recv_from(&mut buf) => {
                let Ok((len, src)) = result else { continue };
                let src = canonical(src);
                let frame = &buf[..len];
                // +ciris (leviculum#104): the peer may already be probing the
                // path's MTU; answer, and never keep a probe as data.
                if wire::is_mtu_probe(frame) {
                    answer_mtu_probe(socket, frame, src, session, token).await;
                    continue;
                }
                match wire::parse_punch_frame(frame, session, token) {
                    Some((PunchKind::Punch, their_seq)) => {
                        let ack = wire::punch_frame(PunchKind::Ack, session, token, their_seq);
                        let _ = socket.send_to(&ack, on_socket(socket, src)).await;
                        if !targets.contains(&src) && targets.len() < advertised + PEER_REFLEXIVE_MAX {
                            tracing::debug!("direct link: peer-reflexive candidate {src}");
                            targets.push(src);
                        }
                        if heard.is_empty() {
                            tracing::info!(
                                link = %label.link,
                                session = %hex_short(session),
                                peer = %label.peer,
                                after_ms = started.elapsed().as_millis() as u64,
                                "direct link: first punch heard"
                            );
                            tracing::debug!("direct link: the first punch came from {src}");
                        }
                        if !heard.contains(&src) && heard.len() < targets.len() {
                            heard.push(src);
                        }
                    }
                    Some((PunchKind::Ack, _)) => {
                        if punched.contains(&src) && !acked.contains(&src) {
                            acked.push(src);
                        }
                    }
                    None => {
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

/// +ciris (leviculum#100): an id in hex, as the node's log lines print link
/// and session ids.
fn hex_short(id: &[u8]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
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

/// +ciris (leviculum#100): the driver's jobs, each with the link its
/// session upgrades, under one hold of the core lock.
pub(crate) fn take_jobs(
    core: &mut crate::driver::StdNodeCore,
) -> Vec<(DirectLinkJob, Option<Label>)> {
    core.take_direct_link_jobs()
        .into_iter()
        .map(|job| {
            let label = match &job {
                DirectLinkJob::Probe { session, .. } | DirectLinkJob::Punch { session, .. } => {
                    core.direct_link_session_link(session).map(|link| Label {
                        link: hex_short(link.as_bytes()),
                        peer: core
                            .direct_link_peer_hash(&link)
                            .map_or_else(|| "-".to_string(), |h| hex_short(&h)),
                    })
                }
                _ => None,
            };
            (job, label)
        })
        .collect()
}

/// +ciris (leviculum#100): what the driver's log lines name a session by:
/// its link and its peer's hash, in hex, never an address.
#[derive(Debug, Clone)]
pub(crate) struct Label {
    link: String,
    peer: String,
}

impl Default for Label {
    fn default() -> Self {
        Label {
            link: "-".into(),
            peer: "-".into(),
        }
    }
}

/// +ciris (leviculum#100): a probe finished ([`Outcome::Probed`]): record
/// its NAT type, then hand the node the reflexive address. One output for
/// both, the NAT report's events first. A re-probe ([`Outcome::NatProbed`])
/// records only the type.
pub(crate) fn probed(
    core: &mut crate::driver::StdNodeCore,
    outcome: Outcome,
) -> leviculum_core::transport::TickOutput {
    let (session, public, local, nat) = match outcome {
        Outcome::Probed {
            session,
            public,
            local,
            nat,
        } => (session, public, local, nat),
        // A probe outside any session: only the NAT type to record.
        Outcome::NatProbed { report } => return core.record_nat_report(None, *report),
        Outcome::Punched { .. } => return leviculum_core::transport::TickOutput::empty(),
    };
    let first = nat.map(|report| core.record_nat_report(Some(&session), *report));
    let mut output = core.direct_link_probed_with_local(&session, public, local);
    if let Some(mut first) = first {
        first.actions.append(&mut output.actions);
        first.events.append(&mut output.events);
        first.next_deadline_ms = output.next_deadline_ms;
        output = first;
    }
    output
}

// Facilitator

/// +ciris (leviculum#100): a facilitator's answers per source address at
/// once, and the wait that earns one more. A prober sends three per probe.
const FACILITATOR_BURST: u32 = 10;
const FACILITATOR_REFILL: Duration = Duration::from_millis(500);

/// Answer RNSP and STUN Binding requests on `port`, telling each prober the
/// address its datagram arrived from. Runs until aborted. +ciris
/// (leviculum#100): at most [`FACILITATOR_BURST`] answers at once per
/// source address and one per [`FACILITATOR_REFILL`] after, so it cannot be
/// made to flood a forged source; and a padded request (as a peer probe
/// sends) gets an answer no larger than itself. The bare 21-byte request
/// every prober has always sent is answered as before, with its 28 or 40
/// bytes.
pub(crate) async fn spawn_facilitator(port: u16) -> io::Result<AbortHandle> {
    let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?;
    tracing::info!(
        "direct link: facilitator listening on {}",
        socket.local_addr()?
    );
    let task = tokio::spawn(async move {
        let mut buf = [0u8; 576];
        let mut limit = nat::PerSource::new(FACILITATOR_BURST, FACILITATOR_REFILL);
        loop {
            let (len, src) = match socket.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!("direct link: facilitator recv error: {e}");
                    continue;
                }
            };
            let request = &buf[..len];
            if !limit.take(src.ip(), std::time::Instant::now()) {
                continue;
            }
            let answer = if let Some(answer) =
                leviculum_core::direct_link::nat::answer_peer_probe(request, &src)
            {
                answer
            } else if let Some(nonce) = wire::parse_rnsp_request(request) {
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
        /// +ciris (leviculum#100): the NAT type the probe found, when it
        /// ran (not when the socket failed to bind).
        nat: Option<Box<leviculum_core::direct_link::NatReport>>,
    },
    /// +ciris (leviculum#100): a NAT re-probe outside any session.
    NatProbed {
        report: Box<leviculum_core::direct_link::NatReport>,
    },
    Punched {
        session: SessionId,
        peer: Option<SocketAddr>,
        /// Datagrams the peer sent before this end finished punching.
        early: Vec<Vec<u8>>,
        /// +ciris (leviculum#104): the largest probe step the path carried,
        /// which the interface signals; `None` signals no MTU (base 500).
        mtu: Option<u32>,
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
    /// +ciris (leviculum#99): the timing of an interface whose peer acks
    /// sequenced keepalives.
    fast_timing: DirectUdpTiming,
    /// +ciris: sessions whose peer acks sequenced keepalives (both ends run
    /// the two-sided commit), so their interface uses the fast liveness.
    fast_liveness: HashMap<SessionId, bool>,
    /// +ciris (leviculum#100): servers probed beside the facilitator, for
    /// the NAT type: this node's own facilitator first, then
    /// `nat_probe_addrs`.
    nat_servers: Vec<NatProbeServer>,
    /// +ciris (leviculum#100): the link and peer each session upgrades, for
    /// the log lines.
    labels: HashMap<SessionId, Label>,
    /// +ciris (leviculum#100): each live direct interface's peer address,
    /// probed for the NAT type; mirrored in `extras.peers` for
    /// `ReticulumNode::probe_nat`.
    peers: HashMap<usize, SocketAddr>,
    /// +ciris (leviculum#100): the host's extras, and this node's settings,
    /// for re-probes.
    extras: DirectLinkExtras,
    settings: DirectLinkSettings,
    /// +ciris (leviculum#100): when the NAT is probed again.
    reprobe: nat::Reprobe,
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
            fast_timing: DirectUdpTiming::fast(),
            fast_liveness: HashMap::new(),
            nat_servers: Vec::new(),
            labels: HashMap::new(),
            peers: HashMap::new(),
            extras: DirectLinkExtras::default(),
            settings: DirectLinkSettings::default(),
            reprobe: nat::Reprobe::new(std::time::Instant::now()),
        }
    }

    /// +ciris (leviculum#99, #100): take the host's keepalive, and the
    /// servers probed beside each session's facilitator for the NAT type.
    pub(crate) fn configure(&mut self, settings: &DirectLinkSettings, extras: DirectLinkExtras) {
        if let Some((keepalive, inactivity)) = extras.keepalive {
            self.set_fast_timing(keepalive, inactivity);
        }
        self.timing.answer_probes = extras.answer_probes;
        self.fast_timing.answer_probes = extras.answer_probes;
        self.nat_servers = nat::nat_servers(settings, extras.nat_probe_servers.clone());
        self.settings = settings.clone();
        self.extras = extras;
    }

    /// +ciris (leviculum#100): the live direct-link peers' addresses.
    fn peer_addrs(&self) -> Vec<SocketAddr> {
        let mut peers: Vec<SocketAddr> = self.peers.values().copied().collect();
        peers.sort();
        peers
    }

    /// +ciris (leviculum#100): the peer set changed; share it, and see
    /// whether a new address makes a re-probe due.
    fn peers_changed(&mut self, added: Option<SocketAddr>) {
        let peers = self.peer_addrs();
        *self.extras.peers.lock_recover() = peers;
        if let Some(peer) = added {
            self.reprobe
                .peer_seen(canonical(peer).ip(), std::time::Instant::now());
        }
    }

    /// +ciris (leviculum#100): start a NAT re-probe if one is due: this
    /// node's facilitator, the host's servers and the live peers, from a
    /// fresh socket, recorded as a probe outside any session.
    fn start_due_reprobe(&mut self, now: std::time::Instant) {
        if !self.reprobe.take_due(now) {
            return;
        }
        let peers = self.peer_addrs();
        self.reprobe
            .probed(peers.iter().map(|p| canonical(*p).ip()));
        let (settings, extras) = (self.settings.clone(), self.extras.clone());
        let outcomes = self.outcomes.clone();
        tokio::spawn(async move {
            match nat::probe_nat_now(&settings, &extras).await {
                Ok(report) => {
                    let _ = outcomes
                        .send(Outcome::NatProbed {
                            report: Box::new(report),
                        })
                        .await;
                }
                Err(e) => tracing::debug!("direct link: NAT re-probe not run: {e}"),
            }
        });
    }

    /// +ciris (leviculum#100): [`Self::run`], for a job of the session that
    /// upgrades `link` (named in the log lines).
    pub(crate) fn run_on(&mut self, job: DirectLinkJob, label: Option<Label>) {
        if let (
            DirectLinkJob::Probe { session, .. } | DirectLinkJob::Punch { session, .. },
            Some(label),
        ) = (&job, label)
        {
            self.labels.insert(*session, label);
        }
        self.run(job);
    }

    /// The link and peer a session upgrades, in hex, or `-`.
    fn label(&self, session: &SessionId) -> Label {
        self.labels.get(session).cloned().unwrap_or_default()
    }

    /// +ciris (leviculum#99): the keepalive interval and silence bound of a
    /// direct interface whose peer acks sequenced keepalives.
    pub(crate) fn set_fast_timing(&mut self, keepalive: Duration, inactivity: Duration) {
        self.fast_timing = DirectUdpTiming::fast_with(keepalive, inactivity);
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
                token,
                start_delay_ms,
                fast_liveness,
            } => {
                self.fast_liveness.insert(session, fast_liveness);
                self.start_punch(
                    session,
                    peer,
                    alternates,
                    token,
                    Duration::from_millis(start_delay_ms),
                )
            }
            DirectLinkJob::Release { session } => self.release(&session),
            DirectLinkJob::CloseInterface { interface_index } => {
                if let Some(task) = self.interfaces.remove(&interface_index) {
                    tracing::info!("direct link: closing direct interface {interface_index}");
                    task.abort();
                }
                if self.peers.remove(&interface_index).is_some() {
                    self.peers_changed(None);
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
                    nat: None,
                });
                return;
            }
        };
        self.sockets.insert(session, Arc::clone(&socket));
        let nat_servers = self.nat_servers.clone();
        // +ciris (leviculum#100): the live direct-link peers too; they count
        // as probed now, so no re-probe repeats them.
        let peers = self.peer_addrs();
        self.reprobe
            .probed(peers.iter().map(|p| canonical(*p).ip()));
        let label = self.label(&session);
        let task = tokio::spawn(async move {
            // +ciris (leviculum#100): the facilitator, the NAT probe servers
            // and the peers at once, off this one socket.
            let report =
                nat::probe_nat_type(&socket, Some((server, protocol)), &nat_servers, &peers).await;
            let public = report.probes.first().and_then(|p| p.mapped);
            let tag = hex_short(&session);
            tracing::info!(
                link = %label.link,
                session = %tag,
                peer = %label.peer,
                answered = public.is_some(),
                "direct link: facilitator probed"
            );
            match public {
                Some(public) => tracing::debug!(
                    session = %tag,
                    "direct link: reflexive address {public} (via {server})"
                ),
                None => {
                    tracing::debug!(session = %tag, "direct link: facilitator {server} did not answer")
                }
            }
            let local = report.host_candidates.clone();
            let _ = outcomes
                .send(Outcome::Probed {
                    session,
                    public,
                    local,
                    nat: Some(Box::new(report)),
                })
                .await;
        });
        self.replace_task(session, task.abort_handle());
    }

    fn start_punch(
        &mut self,
        session: SessionId,
        peer: SocketAddr,
        alternates: Vec<SocketAddr>,
        token: [u8; 32],
        start_delay: Duration,
    ) {
        let outcomes = self.outcomes.clone();
        let Some(socket) = self.sockets.get(&session).cloned() else {
            // No probe socket (it failed to bind, or was released): there is
            // no NAT mapping to punch from.
            let _ = outcomes.try_send(Outcome::Punched {
                session,
                peer: None,
                early: Vec::new(),
                mtu: None,
            });
            return;
        };
        self.tokens.insert(session, token);
        let label = self.label(&session);
        let task = tokio::spawn(async move {
            // The responder waits out the READY's trip to the initiator, so
            // both ends start together.
            if !start_delay.is_zero() {
                tokio::time::sleep(start_delay).await;
            }
            let targets = alloc_targets(peer, &alternates);
            let tag = hex_short(&session);
            // +ciris (leviculum#100): info lines name the link and the peer's
            // hash; the addresses are at debug.
            tracing::info!(
                link = %label.link,
                session = %tag,
                peer = %label.peer,
                targets = targets.len(),
                "direct link: punching"
            );
            tracing::debug!(session = %tag, "direct link: punching toward {peer}, also {alternates:?}");
            let seen = punch_logged(&socket, &targets, &session, &token, &label).await;
            tracing::info!(
                link = %label.link,
                session = %tag,
                peer = %label.peer,
                through = seen.is_some(),
                "direct link: punch ended"
            );
            if let Some(p) = &seen {
                tracing::debug!(session = %tag, "direct link: punch through to {}", p.peer);
            }
            let (peer, early, mtu) = match seen {
                Some(mut p) => {
                    let mtu = probe_path_mtu(&socket, p.peer, &session, &token, &mut p.early).await;
                    tracing::info!(
                        link = %label.link,
                        session = %tag,
                        peer = %label.peer,
                        "direct link: the path carries {mtu:?}"
                    );
                    (Some(p.peer), p.early, mtu)
                }
                None => (None, Vec::new(), None),
            };
            let _ = outcomes
                .send(Outcome::Punched {
                    session,
                    peer,
                    early,
                    mtu,
                })
                .await;
        });
        self.replace_task(session, task.abort_handle());
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
        self.labels.remove(session);
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
    ) -> Option<InterfaceHandle> {
        self.tasks.remove(&session);
        self.labels.remove(&session);
        let socket = self.sockets.remove(&session)?;
        let token = self.tokens.remove(&session)?;
        let timing = if self.fast_liveness.remove(&session).unwrap_or(false) {
            self.fast_timing
        } else {
            self.timing
        };
        let (handle, task) =
            spawn_direct_udp_interface(id, socket, peer, session, token, early, timing, hw_mtu);
        self.registering.insert(id, session);
        self.interfaces.insert(id.0, task);
        // +ciris (leviculum#100): a new peer for the NAT probe.
        self.peers.insert(id.0, canonical(peer));
        self.peers_changed(Some(peer));
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
    ///
    /// +ciris (leviculum#100): the event loop's periodic pass also starts a
    /// NAT re-probe that has come due here.
    pub(crate) fn take_due_reannounces(&mut self, now: std::time::Instant) -> Vec<usize> {
        self.start_due_reprobe(now);
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
        if self.peers.remove(&id.0).is_some() {
            self.peers_changed(None);
        }
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

    async fn loopback() -> UdpSocket {
        UdpSocket::bind("127.0.0.1:0").await.unwrap()
    }

    /// leviculum#99: a host-set keepalive is the timing a fast-liveness
    /// interface gets; the default stays 5 s and 15 s.
    #[test]
    fn a_host_set_keepalive_is_the_fast_timing() {
        let (tx, _rx) = mpsc::channel(1);
        let mut runtime = DirectLinkRuntime::new(tx);
        assert_eq!(runtime.fast_timing.keepalive, Duration::from_secs(5));
        assert_eq!(runtime.fast_timing.inactivity, Duration::from_secs(15));
        runtime.set_fast_timing(Duration::from_secs(25), Duration::from_secs(80));
        assert_eq!(runtime.fast_timing.keepalive, Duration::from_secs(25));
        assert_eq!(runtime.fast_timing.inactivity, Duration::from_secs(80));
        assert!(runtime.fast_timing.sequenced);
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

    /// +ciris (leviculum#100): a direct interface answers its peer's NAT
    /// probe, from the peer's IP on any port, with no more bytes than the
    /// request; it answers nobody else, never a bare request, at most its
    /// burst at once, and nothing when probe answering is off. Needs a
    /// second loopback address for the stranger (Linux has 127/8).
    /// Send a (padded or bare) RNSP request from `socket` to `to`; what the
    /// answer says, if one comes.
    async fn ask_nat(
        socket: &UdpSocket,
        to: SocketAddr,
        padded: bool,
        n: u8,
    ) -> Option<SocketAddr> {
        use leviculum_core::direct_link::nat::{rnsp_peer_request, RNSP_PEER_REQUEST_LEN};
        let request = if padded {
            rnsp_peer_request(&[n; 16]).to_vec()
        } else {
            wire::rnsp_request(&[n; 16]).to_vec()
        };
        socket.send_to(&request, to).await.unwrap();
        let mut buf = [0u8; 128];
        match tokio::time::timeout(Duration::from_millis(200), socket.recv_from(&mut buf)).await {
            Ok(Ok((len, _))) => {
                assert!(len <= RNSP_PEER_REQUEST_LEN, "never amplifies");
                wire::parse_rnsp_response(&buf[..len], &[n; 16])
            }
            _ => None,
        }
    }

    #[tokio::test]
    async fn a_direct_interface_answers_only_its_peers_padded_probes() {
        use crate::interfaces::direct_udp::{DirectUdpTiming, PROBE_ANSWER_BURST};
        let Ok(stranger) = UdpSocket::bind("127.0.0.2:0").await else {
            return;
        };
        for answer_probes in [true, false] {
            let iface = Arc::new(loopback().await);
            let iface_addr = iface.local_addr().unwrap();
            let punched = loopback().await; // the peer's punched socket
            let fresh = loopback().await; // the peer probing from a new one
            let timing = DirectUdpTiming {
                answer_probes,
                ..DirectUdpTiming::default()
            };
            let (_handle, task) = spawn_direct_udp_interface(
                InterfaceId(9),
                iface,
                punched.local_addr().unwrap(),
                [1; 16],
                [2; 32],
                Vec::new(),
                timing,
                None,
            );
            let seen = ask_nat(&fresh, iface_addr, true, 1).await;
            if answer_probes {
                assert_eq!(
                    seen,
                    Some(fresh.local_addr().unwrap()),
                    "the peer's IP, a new port"
                );
            } else {
                assert_eq!(seen, None, "answering is off");
            }
            assert_eq!(
                ask_nat(&fresh, iface_addr, false, 2).await,
                None,
                "a bare request gets nothing"
            );
            assert_eq!(
                ask_nat(&stranger, iface_addr, true, 3).await,
                None,
                "another address gets nothing"
            );
            if answer_probes {
                let mut answered = 1;
                for n in 10..30 {
                    answered += u32::from(ask_nat(&fresh, iface_addr, true, n).await.is_some());
                }
                // About 3 s of asking: the burst, and a refill or two.
                assert!(
                    (PROBE_ANSWER_BURST..=PROBE_ANSWER_BURST + 2).contains(&answered),
                    "the burst, then the rate limit: {answered}"
                );
            }
            task.abort();
        }
    }

    /// +ciris (leviculum#100): with no facilitator and no server, two live
    /// direct-link peers at distinct addresses classify the NAT, each
    /// answering from its direct socket; one peer alone leaves it unknown.
    #[tokio::test]
    async fn two_direct_link_peers_classify_without_a_server() {
        use crate::interfaces::direct_udp::DirectUdpTiming;
        use leviculum_core::direct_link::{NatMapping, NatUnknown};
        let prober_ip = "127.0.0.1";
        let mut peers = Vec::new();
        let mut tasks = Vec::new();
        for ip in ["127.0.0.2", "127.0.0.3"] {
            let Ok(socket) = UdpSocket::bind(format!("{ip}:0")).await else {
                return; // no extra loopback addresses here
            };
            let socket = Arc::new(socket);
            peers.push(socket.local_addr().unwrap());
            // Each peer's punched path ends at some socket of the prober's IP.
            let punched: SocketAddr = format!("{prober_ip}:9").parse().unwrap();
            let (_h, task) = spawn_direct_udp_interface(
                InterfaceId(peers.len()),
                socket,
                punched,
                [1; 16],
                [2; 32],
                Vec::new(),
                DirectUdpTiming::default(),
                None,
            );
            tasks.push((_h, task));
        }
        let socket = UdpSocket::bind(format!("{prober_ip}:0")).await.unwrap();
        let both = nat::probe_nat_type(&socket, None, &[], &peers).await;
        assert_eq!(
            both.mapping,
            NatMapping::EndpointIndependent,
            "{:?}",
            both.probes
        );
        let one = nat::probe_nat_type(&socket, None, &[], &peers[..1]).await;
        assert_eq!(one.mapping, NatMapping::Unknown(NatUnknown::OneServer));
        for (_, t) in tasks {
            t.abort();
        }
    }

    /// +ciris (leviculum#100): the facilitator rate-limits each source, and
    /// answers a padded request with no more bytes than it.
    #[tokio::test]
    async fn the_facilitator_limits_each_source_and_never_amplifies_a_padded_probe() {
        use leviculum_core::direct_link::nat::rnsp_peer_request;
        let port = loopback().await.local_addr().unwrap().port();
        let task = spawn_facilitator(port).await.unwrap();
        let server: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let socket = loopback().await;
        let mut buf = [0u8; 128];
        let request = rnsp_peer_request(&[4; 16]);
        socket.send_to(&request, server).await.unwrap();
        let (len, _) = socket.recv_from(&mut buf).await.unwrap();
        assert!(len <= request.len());
        let mut answered = 1;
        for n in 0..30u8 {
            socket
                .send_to(&wire::rnsp_request(&[n; 16]), server)
                .await
                .unwrap();
            if let Ok(Ok(_)) =
                tokio::time::timeout(Duration::from_millis(50), socket.recv_from(&mut buf)).await
            {
                answered += 1;
            }
        }
        assert!(answered <= FACILITATOR_BURST + 2, "{answered}");
        task.abort();
    }

    #[test]
    fn answer_probes_is_read_and_a_bad_value_refused() {
        let off = reticulum(&[("answer_probes", "no")]);
        assert_eq!(off.direct_link.answer_probes, Some(false));
        let extras = DirectLinkExtrasConfig::default().resolve(&off).unwrap();
        assert!(!extras.answer_probes);
        let default = DirectLinkExtrasConfig::default()
            .resolve(&reticulum(&[]))
            .unwrap();
        assert!(default.answer_probes, "on by default");
        let host = DirectLinkExtrasConfig {
            answer_probes: Some(true),
            ..Default::default()
        };
        assert!(
            host.resolve(&off).unwrap().answer_probes,
            "the builder wins"
        );
        assert!(DirectLinkExtrasConfig::default()
            .resolve(&reticulum(&[("answer_probes", "perhaps")]))
            .is_err());
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
        ]))
        .unwrap();
        assert_eq!(
            on,
            DirectLinkSettings {
                policy: DirectLinkPolicy::IdentifiedOnly,
                facilitator: Some("rns.example.org:4343".into()),
                protocol: ProbeProtocol::Stun,
                facilitator_port: Some(4343),
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
