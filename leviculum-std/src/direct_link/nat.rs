//! The NAT-type probe (+ciris, leviculum#100): the driver's half of
//! [`leviculum_core::direct_link::nat`].
//!
//! A node classifies its NAT by probing servers at two distinct IP
//! addresses from one socket and comparing what each saw. "The second probe
//! is any node we peer with": the servers are
//!
//! - the facilitator a session probes anyway, and this node's own
//!   facilitator when the session's is another;
//! - every live direct-link peer, at the address its punched socket has.
//!   Every leviculum node answers a NAT probe on its direct sockets, by
//!   default (`answer_probes`), and only from that peer's IP address, so a
//!   node can only probe the peers it already holds a direct path with.
//!   Such a probe comes from a fresh socket, and the peer's own NAT must let
//!   it in: a peer with a public address or an endpoint-independent filter
//!   answers, one behind an address-and-port-dependent filter (Linux
//!   conntrack, most home routers) cannot, and its silence just leaves the
//!   verdict to the others;
//! - the host's `nat_probe_addrs`, optional.
//!
//! The verdict stays `Unknown` until two distinct addresses have answered.
//! A new direct-link peer at an address not probed before starts a
//! re-probe, spaced by a doubling backoff so churn cannot make a storm.
//!
//! The wire is RNSP, unchanged. A facilitator answers the bare 21-byte
//! request, as every version always has. A peer's direct socket answers
//! only a request padded to the largest answer's size, so its answer is
//! never larger than the request; an old peer ignores it.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leviculum_core::direct_link::wire;
use leviculum_core::direct_link::{NatProbe, NatReport, ProbeProtocol};
use rand_core::RngCore;
use tokio::net::UdpSocket;

use super::{
    bind_session_socket, canonical, host_candidates, on_socket, resolve_facilitator,
    DirectLinkSettings, PROBE_ATTEMPTS, PROBE_WAIT,
};
use crate::error::Error;
use crate::sync_ext::MutexRecover;

/// One more server to probe for the NAT type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatProbeServer {
    /// `host:port`, resolved at each probe.
    pub addr: String,
    /// How to probe it.
    pub protocol: ProbeProtocol,
}

/// Read a `nat_probe_addrs` value: comma-separated `host:port` entries,
/// each probed with RNSP, or with STUN when prefixed `stun:` (`rnsp:` is
/// accepted too). An entry with no port is a config error.
pub fn parse_nat_probe_servers(value: &str) -> Result<Vec<NatProbeServer>, Error> {
    let mut out = Vec::new();
    for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (protocol, addr) = if let Some(rest) = entry.strip_prefix("stun:") {
            (ProbeProtocol::Stun, rest)
        } else if let Some(rest) = entry.strip_prefix("rnsp:") {
            (ProbeProtocol::Rnsp, rest)
        } else {
            (ProbeProtocol::Rnsp, entry)
        };
        let port_ok = addr
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
        if !port_ok {
            return Err(Error::Config(format!(
                "nat_probe_addrs: {entry:?} is not host:port (optionally stun: or rnsp:)"
            )));
        }
        out.push(NatProbeServer {
            addr: addr.to_string(),
            protocol,
        });
    }
    Ok(out)
}

/// Read an `answer_probes` value the way Reticulum reads a boolean.
pub(crate) fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "on" | "1" => Some(true),
        "no" | "false" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// How long the other servers' answers are waited for once the
/// facilitator has answered (a session) or two distinct addresses have (a
/// re-probe). The punch waits on the facilitator alone; this bounds what
/// the NAT probe adds to it.
const NAT_PROBE_GRACE: Duration = Duration::from_millis(500);

/// How long a probe server's name may take to resolve before it is left
/// out of this probe.
const NAT_PROBE_RESOLVE: Duration = Duration::from_secs(1);

/// Direct-link peers probed at once, at most (one per distinct address).
const PEERS_PROBED_MAX: usize = 4;

/// How a server is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ask {
    /// A facilitator or STUN server: the bare request every version knows.
    Server(ProbeProtocol),
    /// A peer's direct socket: the padded RNSP request.
    Peer,
}

/// One probe request and what identifies its answer.
enum Attempt {
    Rnsp([u8; 16]),
    RnspPeer([u8; 16]),
    Stun([u8; 12]),
}

impl Attempt {
    fn new(ask: Ask, rng: &mut impl RngCore) -> Self {
        let mut nonce = [0u8; 16];
        match ask {
            Ask::Server(ProbeProtocol::Stun) => {
                let mut txn = [0u8; 12];
                rng.fill_bytes(&mut txn);
                Attempt::Stun(txn)
            }
            Ask::Server(ProbeProtocol::Rnsp) => {
                rng.fill_bytes(&mut nonce);
                Attempt::Rnsp(nonce)
            }
            Ask::Peer => {
                rng.fill_bytes(&mut nonce);
                Attempt::RnspPeer(nonce)
            }
        }
    }

    fn request(&self) -> Vec<u8> {
        match self {
            Attempt::Rnsp(nonce) => wire::rnsp_request(nonce).to_vec(),
            Attempt::RnspPeer(nonce) => {
                leviculum_core::direct_link::nat::rnsp_peer_request(nonce).to_vec()
            }
            Attempt::Stun(txn) => wire::stun_binding_request(txn).to_vec(),
        }
    }

    fn answer(&self, data: &[u8]) -> Option<SocketAddr> {
        match self {
            Attempt::Rnsp(nonce) | Attempt::RnspPeer(nonce) => {
                wire::parse_rnsp_response(data, nonce)
            }
            Attempt::Stun(txn) => wire::parse_stun_binding_response(data, txn),
        }
    }
}

/// Probe each of `servers` from one socket at once, so the mappings can be
/// compared. Every unanswered server is asked again each attempt. Returns
/// once all have answered, or [`NAT_PROBE_GRACE`] after the first server
/// answered (`primary`: a session's facilitator, which the punch waits on)
/// or after two distinct addresses have (otherwise), or when the attempts
/// run out. The probes come back in the order given.
pub(crate) async fn probe_many(
    socket: &UdpSocket,
    servers: &[(SocketAddr, Ask)],
    primary: bool,
) -> Vec<NatProbe> {
    let mut rng = rand_core::OsRng;
    let mut buf = [0u8; 512];
    let mut probes: Vec<NatProbe> = servers
        .iter()
        .map(|(server, _)| NatProbe {
            server: *server,
            mapped: None,
        })
        .collect();
    if servers.is_empty() {
        return probes;
    }
    let mut stop_at: Option<tokio::time::Instant> = None;
    let enough = |probes: &[NatProbe]| {
        let mut ips: Vec<IpAddr> = Vec::new();
        for p in probes.iter().filter(|p| p.mapped.is_some()) {
            if !ips.contains(&p.server.ip()) {
                ips.push(p.server.ip());
            }
        }
        ips.len() >= 2
    };
    for _ in 0..PROBE_ATTEMPTS {
        let attempts: Vec<Option<Attempt>> = servers
            .iter()
            .zip(&probes)
            .map(|((_, ask), p)| p.mapped.is_none().then(|| Attempt::new(*ask, &mut rng)))
            .collect();
        for ((server, _), attempt) in servers.iter().zip(&attempts) {
            if let Some(attempt) = attempt {
                if let Err(e) = socket
                    .send_to(&attempt.request(), on_socket(socket, *server))
                    .await
                {
                    tracing::debug!("direct link: probe of {server} not sent: {e}");
                }
            }
        }
        let deadline = tokio::time::Instant::now() + PROBE_WAIT;
        loop {
            let until = stop_at.map_or(deadline, |s| s.min(deadline));
            match tokio::time::timeout_at(until, socket.recv_from(&mut buf)).await {
                Err(_) => break,
                Ok(Err(e)) => {
                    tracing::debug!("direct link: probe recv error: {e}");
                    break;
                }
                Ok(Ok((len, src))) => {
                    let src = canonical(src);
                    for (i, ((server, _), attempt)) in servers.iter().zip(&attempts).enumerate() {
                        if probes[i].mapped.is_some() || canonical(*server) != src {
                            continue;
                        }
                        if let Some(mapped) = attempt.as_ref().and_then(|a| a.answer(&buf[..len])) {
                            probes[i].mapped = Some(canonical(mapped));
                            let start_grace = if primary { i == 0 } else { enough(&probes) };
                            if start_grace && stop_at.is_none() {
                                stop_at = Some(tokio::time::Instant::now() + NAT_PROBE_GRACE);
                            }
                        }
                    }
                    if probes.iter().all(|p| p.mapped.is_some()) {
                        return probes;
                    }
                }
            }
            if stop_at.is_some_and(|s| tokio::time::Instant::now() >= s) {
                return probes;
            }
        }
        if stop_at.is_some_and(|s| tokio::time::Instant::now() >= s) {
            return probes;
        }
    }
    probes
}

/// Resolve the probe servers, leaving out any that does not resolve within
/// [`NAT_PROBE_RESOLVE`], and any already in `taken`.
async fn resolve_servers(
    taken: &[SocketAddr],
    servers: &[NatProbeServer],
) -> Vec<(SocketAddr, Ask)> {
    let lookups = servers.iter().map(|s| async move {
        match tokio::time::timeout(NAT_PROBE_RESOLVE, resolve_facilitator(&s.addr)).await {
            Ok(Ok(addr)) => Some((addr, Ask::Server(s.protocol))),
            Ok(Err(e)) => {
                tracing::info!("direct link: a NAT probe server did not resolve: {e}");
                None
            }
            Err(_) => {
                tracing::info!("direct link: a NAT probe server timed out resolving");
                None
            }
        }
    });
    let mut out: Vec<(SocketAddr, Ask)> = Vec::new();
    for (addr, ask) in futures::future::join_all(lookups)
        .await
        .into_iter()
        .flatten()
    {
        let addr = canonical(addr);
        if !taken.contains(&addr) && !out.iter().any(|(a, _)| *a == addr) {
            out.push((addr, ask));
        }
    }
    out
}

/// The peers worth probing: one per distinct address, none at an address
/// already being probed, [`PEERS_PROBED_MAX`] at most.
fn peers_to_probe(taken: &[SocketAddr], peers: &[SocketAddr]) -> Vec<(SocketAddr, Ask)> {
    let mut out: Vec<(SocketAddr, Ask)> = Vec::new();
    for peer in peers.iter().map(|p| canonical(*p)) {
        if out.len() >= PEERS_PROBED_MAX {
            break;
        }
        if !taken.contains(&peer) && !out.iter().any(|(a, _)| a.ip() == peer.ip()) {
            out.push((peer, Ask::Peer));
        }
    }
    out
}

/// Probe `primary` (a session's facilitator, if any), the probe `servers`
/// and the direct-link `peers` from `socket`, and classify the result. The
/// report's times are left at zero for the node to stamp.
pub(crate) async fn probe_nat_type(
    socket: &UdpSocket,
    primary: Option<(SocketAddr, ProbeProtocol)>,
    servers: &[NatProbeServer],
    peers: &[SocketAddr],
) -> NatReport {
    let mut list: Vec<(SocketAddr, Ask)> = primary
        .iter()
        .map(|(addr, protocol)| (canonical(*addr), Ask::Server(*protocol)))
        .collect();
    let taken: Vec<SocketAddr> = list.iter().map(|(a, _)| *a).collect();
    list.extend(resolve_servers(&taken, servers).await);
    let taken: Vec<SocketAddr> = list.iter().map(|(a, _)| *a).collect();
    list.extend(peers_to_probe(&taken, peers));
    let probes = probe_many(socket, &list, primary.is_some()).await;
    let local_port = socket.local_addr().map_or(0, |a| a.port());
    let candidates = list
        .first()
        .map(|(toward, _)| host_candidates(socket, *toward))
        .unwrap_or_default();
    NatReport::from_probes(probes, local_port, candidates, 0, None)
}

/// A token bucket: `burst` at once, one more per `refill`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bucket {
    tokens: u32,
    burst: u32,
    refill: Duration,
    last: Instant,
}

impl Bucket {
    pub(crate) fn new(burst: u32, refill: Duration) -> Self {
        Bucket {
            tokens: burst,
            burst,
            refill,
            last: Instant::now(),
        }
    }

    fn top_up(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last);
        let earned = (elapsed.as_millis() / self.refill.as_millis().max(1)) as u32;
        if earned > 0 {
            self.tokens = (self.tokens + earned).min(self.burst);
            self.last += self.refill * earned;
        }
        if self.tokens == self.burst {
            self.last = now;
        }
    }

    /// Spend one token if there is one.
    pub(crate) fn take(&mut self, now: Instant) -> bool {
        self.top_up(now);
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }

    fn is_full(&mut self, now: Instant) -> bool {
        self.top_up(now);
        self.tokens == self.burst
    }
}

/// A [`Bucket`] per source address, for the facilitator. At most
/// [`SOURCES_MAX`] sources are tracked; past that, idle ones are dropped,
/// and a new source waits until one is.
pub(crate) struct PerSource {
    buckets: HashMap<IpAddr, Bucket>,
    burst: u32,
    refill: Duration,
}

/// Sources a facilitator tracks at once.
const SOURCES_MAX: usize = 4096;

impl PerSource {
    pub(crate) fn new(burst: u32, refill: Duration) -> Self {
        PerSource {
            buckets: HashMap::new(),
            burst,
            refill,
        }
    }

    /// Whether `ip` may be answered now.
    pub(crate) fn take(&mut self, ip: IpAddr, now: Instant) -> bool {
        if !self.buckets.contains_key(&ip) && self.buckets.len() >= SOURCES_MAX {
            self.buckets.retain(|_, b| !b.is_full(now));
            if self.buckets.len() >= SOURCES_MAX {
                return false;
            }
        }
        let (burst, refill) = (self.burst, self.refill);
        self.buckets
            .entry(ip)
            .or_insert_with(|| Bucket::new(burst, refill))
            .take(now)
    }
}

/// Direct-link settings the host sets beside [`DirectLinkSettings`] (which
/// stays as it was, so a host's struct literal still builds).
#[derive(Debug, Clone)]
pub(crate) struct DirectLinkExtras {
    /// The keepalive interval and silence bound of a fast-liveness
    /// interface, when the host set them (leviculum#99).
    pub(crate) keepalive: Option<(Duration, Duration)>,
    /// The host's NAT probe servers.
    pub(crate) nat_probe_servers: Vec<NatProbeServer>,
    /// Answer direct-link peers' NAT probes (on by default).
    pub(crate) answer_probes: bool,
    /// The live direct-link peers' addresses, shared between the event
    /// loop (which writes them) and [`crate::driver::ReticulumNode::probe_nat`].
    pub(crate) peers: Arc<Mutex<Vec<SocketAddr>>>,
}

impl Default for DirectLinkExtras {
    fn default() -> Self {
        DirectLinkExtras {
            keepalive: None,
            nat_probe_servers: Vec::new(),
            answer_probes: true,
            peers: Arc::default(),
        }
    }
}

/// What the builder holds of [`DirectLinkExtras`] until it builds; `None`
/// reads the config.
#[derive(Debug, Clone, Default)]
pub(crate) struct DirectLinkExtrasConfig {
    pub(crate) keepalive: Option<(Duration, Duration)>,
    pub(crate) nat_probe_servers: Option<Vec<String>>,
    pub(crate) answer_probes: Option<bool>,
}

impl DirectLinkExtrasConfig {
    /// Check each, and read the config where the host gave nothing.
    pub(crate) fn resolve(
        self,
        config: &crate::config::ReticulumConfig,
    ) -> Result<DirectLinkExtras, Error> {
        if let Some((interval, dead_after)) = self.keepalive {
            if interval.is_zero() || dead_after <= interval {
                return Err(Error::Config(format!(
                    "direct_link_keepalive: interval {interval:?} must be above zero and below \
                     dead_after {dead_after:?}"
                )));
            }
        }
        let nat_probe_servers = match self.nat_probe_servers {
            Some(servers) => parse_nat_probe_servers(&servers.join(","))?,
            None => parse_nat_probe_servers(
                config.direct_link.nat_probe_addrs.as_deref().unwrap_or(""),
            )?,
        };
        if let Some(bad) = &config.direct_link.invalid_answer_probes {
            return Err(Error::Config(format!(
                "answer_probes must be yes or no, not {bad:?}"
            )));
        }
        Ok(DirectLinkExtras {
            keepalive: self.keepalive,
            nat_probe_servers,
            answer_probes: self
                .answer_probes
                .or(config.direct_link.answer_probes)
                .unwrap_or(true),
            peers: Arc::default(),
        })
    }
}

/// The servers a probe asks beside its facilitator: this node's own
/// facilitator (`probe_addr`), which a session skips when it is the one it
/// probes anyway, then `configured`. A responder whose peer named another
/// facilitator so classifies with no extra key.
pub(crate) fn nat_servers(
    settings: &DirectLinkSettings,
    configured: Vec<NatProbeServer>,
) -> Vec<NatProbeServer> {
    let mut out: Vec<NatProbeServer> = settings
        .facilitator
        .iter()
        .map(|addr| NatProbeServer {
            addr: addr.clone(),
            protocol: settings.protocol,
        })
        .collect();
    for server in configured {
        if !out.iter().any(|s| s.addr == server.addr) {
            out.push(server);
        }
    }
    out
}

/// A NAT probe outside any session, from a fresh socket: this node's
/// facilitator, the host's servers and the live direct-link peers.
/// An error when there is nothing to probe.
pub(crate) async fn probe_nat_now(
    settings: &DirectLinkSettings,
    extras: &DirectLinkExtras,
) -> Result<NatReport, Error> {
    let servers = nat_servers(settings, extras.nat_probe_servers.clone());
    let peers = extras.peers.lock_recover().clone();
    if servers.is_empty() && peers.is_empty() {
        return Err(Error::DirectLink(
            leviculum_core::node::DirectLinkError::NoFacilitator,
        ));
    }
    let toward = match servers.first() {
        Some(s) => resolve_facilitator(&s.addr).await?,
        None => peers[0],
    };
    let socket = UdpSocket::from_std(bind_session_socket(toward)?)?;
    Ok(probe_nat_type(&socket, None, &servers, &peers).await)
}

/// The first wait between two re-probes, and the longest it grows to.
const REPROBE_BACKOFF_MIN: Duration = Duration::from_secs(30);
const REPROBE_BACKOFF_MAX: Duration = Duration::from_secs(30 * 60);
/// Peer addresses remembered as probed, at most; past it the memory starts
/// over (a re-probe or two more, not a storm: the backoff still holds).
const PROBED_MAX: usize = 256;

/// When to probe the NAT again: when a direct-link peer appears at an
/// address not probed before, no sooner than the backoff allows. The
/// backoff doubles with each re-probe, from [`REPROBE_BACKOFF_MIN`] to
/// [`REPROBE_BACKOFF_MAX`].
#[derive(Debug)]
pub(crate) struct Reprobe {
    probed: HashSet<IpAddr>,
    due: Option<Instant>,
    next_allowed: Instant,
    backoff: Duration,
}

impl Reprobe {
    pub(crate) fn new(now: Instant) -> Self {
        Reprobe {
            probed: HashSet::new(),
            due: None,
            next_allowed: now,
            backoff: REPROBE_BACKOFF_MIN,
        }
    }

    /// These addresses were just probed (a session's probe included them).
    pub(crate) fn probed(&mut self, ips: impl IntoIterator<Item = IpAddr>) {
        for ip in ips {
            if self.probed.len() >= PROBED_MAX {
                self.probed.clear();
            }
            self.probed.insert(ip);
        }
    }

    /// A direct-link peer is live at `ip`: a re-probe is due if it is new.
    pub(crate) fn peer_seen(&mut self, ip: IpAddr, now: Instant) {
        if self.probed.contains(&ip) {
            return;
        }
        let at = now.max(self.next_allowed);
        self.due = Some(self.due.map_or(at, |d| d.min(at)));
    }

    /// Whether a re-probe starts now; if so, the backoff grows.
    pub(crate) fn take_due(&mut self, now: Instant) -> bool {
        if !self.due.is_some_and(|d| d <= now) {
            return false;
        }
        self.due = None;
        self.next_allowed = now + self.backoff;
        self.backoff = (self.backoff * 2).min(REPROBE_BACKOFF_MAX);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn bound(ip: &str) -> Option<UdpSocket> {
        UdpSocket::bind(format!("{ip}:0")).await.ok()
    }

    /// A fake RNSP server on `ip` that reports each prober's address with its
    /// port moved by `shift`: a NAT giving each destination its own port.
    async fn shifting_server(
        ip: &str,
        shift: u16,
    ) -> Option<(SocketAddr, tokio::task::JoinHandle<()>)> {
        let socket = bound(ip).await?;
        let addr = socket.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut buf = [0u8; 576];
            while let Ok((len, src)) = socket.recv_from(&mut buf).await {
                if let Some(nonce) = wire::parse_rnsp_request(&buf[..len]) {
                    let seen = SocketAddr::new(src.ip(), src.port().wrapping_add(shift));
                    let _ = socket
                        .send_to(&wire::rnsp_response(&nonce, &seen), src)
                        .await;
                }
            }
        });
        Some((addr, task))
    }

    fn server(addr: SocketAddr) -> NatProbeServer {
        NatProbeServer {
            addr: addr.to_string(),
            protocol: ProbeProtocol::Rnsp,
        }
    }

    const RNSP: ProbeProtocol = ProbeProtocol::Rnsp;

    /// Servers on one loopback address, however many ports, are one server:
    /// unknown, with what each saw kept.
    #[tokio::test]
    async fn servers_on_one_address_never_classify() {
        use leviculum_core::direct_link::{NatMapping, NatUnknown};
        let (a, ta) = shifting_server("127.0.0.1", 0).await.unwrap();
        let (b, tb) = shifting_server("127.0.0.1", 0).await.unwrap();
        let (moved, tc) = shifting_server("127.0.0.1", 7).await.unwrap();
        let socket = bound("127.0.0.1").await.unwrap();
        let local = socket.local_addr().unwrap();
        for second in [b, moved] {
            let r = probe_nat_type(&socket, Some((a, RNSP)), &[server(second)], &[]).await;
            assert_eq!(r.mapping, NatMapping::Unknown(NatUnknown::OneServer));
            assert!(r.servers_share_ip);
            assert_eq!(r.probes[0].mapped, Some(local));
            assert!(r.probes[1].mapped.is_some());
        }
        let alone = probe_nat_type(&socket, Some((a, RNSP)), &[], &[]).await;
        assert_eq!(alone.mapping, NatMapping::Unknown(NatUnknown::OneServer));
        // The facilitator itself is not probed twice.
        let dup = probe_nat_type(&socket, Some((a, RNSP)), &[server(a)], &[]).await;
        assert_eq!(dup.probes.len(), 1);
        for t in [ta, tb, tc] {
            t.abort();
        }
    }

    /// Two servers at distinct addresses (127.0.0.1 and 127.0.0.2, where
    /// the host's loopback has both; Linux does, macOS does not) classify:
    /// the same mapping is endpoint-independent, a moved port
    /// endpoint-dependent, and a second address that never answers leaves
    /// it unknown without waiting out every attempt.
    #[tokio::test]
    async fn servers_at_two_addresses_classify() {
        use leviculum_core::direct_link::{NatMapping, NatUnknown};
        let Some((other_same, t1)) = shifting_server("127.0.0.2", 0).await else {
            return; // no second loopback address here
        };
        let (other_moved, t2) = shifting_server("127.0.0.2", 7).await.unwrap();
        let (facilitator, t3) = shifting_server("127.0.0.1", 0).await.unwrap();
        let silent = bound("127.0.0.2").await.unwrap().local_addr().unwrap();
        let socket = bound("0.0.0.0").await.unwrap();
        let p = Some((facilitator, RNSP));
        let cone = probe_nat_type(&socket, p, &[server(other_same)], &[]).await;
        assert_eq!(cone.mapping, NatMapping::EndpointIndependent);
        assert!(!cone.servers_share_ip);
        let sym = probe_nat_type(&socket, p, &[server(other_moved)], &[]).await;
        assert_eq!(sym.mapping, NatMapping::EndpointDependent);
        let started = tokio::time::Instant::now();
        let half = probe_nat_type(&socket, p, &[server(silent)], &[]).await;
        assert_eq!(half.mapping, NatMapping::Unknown(NatUnknown::OneAnswered));
        assert!(started.elapsed() < PROBE_WAIT, "{:?}", started.elapsed());
        for t in [t1, t2, t3] {
            t.abort();
        }
    }

    /// A facilitator that answers after the second server still gives the
    /// session its reflexive address.
    #[tokio::test]
    async fn the_facilitator_answer_is_waited_for_whatever_the_others_do() {
        let (other, task) = shifting_server("127.0.0.1", 0).await.unwrap();
        let slow = bound("127.0.0.1").await.unwrap();
        let slow_addr = slow.local_addr().unwrap();
        let slow_task = tokio::spawn(async move {
            let mut buf = [0u8; 576];
            if let Ok((len, src)) = slow.recv_from(&mut buf).await {
                tokio::time::sleep(Duration::from_millis(300)).await;
                if let Some(nonce) = wire::parse_rnsp_request(&buf[..len]) {
                    let _ = slow.send_to(&wire::rnsp_response(&nonce, &src), src).await;
                }
            }
        });
        let socket = bound("127.0.0.1").await.unwrap();
        let probes = probe_many(
            &socket,
            &[(slow_addr, Ask::Server(RNSP)), (other, Ask::Server(RNSP))],
            true,
        )
        .await;
        assert!(probes
            .iter()
            .all(|p| p.mapped == Some(socket.local_addr().unwrap())));
        task.abort();
        slow_task.abort();
    }

    #[test]
    fn nat_probe_addrs_parse_and_a_missing_port_is_refused() {
        let parsed = parse_nat_probe_servers(
            " rns2.example.org:4343, stun:stun.example.net:3478 ,rnsp:[2001:db8::1]:4343,",
        )
        .unwrap();
        assert_eq!(
            parsed
                .iter()
                .map(|s| (s.addr.as_str(), s.protocol))
                .collect::<Vec<_>>(),
            vec![
                ("rns2.example.org:4343", RNSP),
                ("stun.example.net:3478", ProbeProtocol::Stun),
                ("[2001:db8::1]:4343", RNSP),
            ]
        );
        assert!(parse_nat_probe_servers("").unwrap().is_empty());
        for bad in ["rns2.example.org", "stun:", ":4343", "host:port"] {
            assert!(parse_nat_probe_servers(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_probe_asks_the_own_facilitator_then_the_configured_servers() {
        let settings = DirectLinkSettings {
            facilitator: Some("own.example:4343".into()),
            ..Default::default()
        };
        let configured = parse_nat_probe_servers("own.example:4343, two.example:4343").unwrap();
        let servers: Vec<String> = nat_servers(&settings, configured)
            .into_iter()
            .map(|s| s.addr)
            .collect();
        assert_eq!(servers, vec!["own.example:4343", "two.example:4343"]);
        assert!(nat_servers(&DirectLinkSettings::default(), Vec::new()).is_empty());
    }

    /// One peer per address is probed, none at an address already asked,
    /// and no more than four.
    #[test]
    fn peers_are_probed_once_per_address() {
        let a = |s: &str| -> SocketAddr { s.parse().unwrap() };
        let peers = [
            a("198.51.100.1:1000"),
            a("198.51.100.1:2000"),
            a("198.51.100.2:1000"),
            a("198.51.100.3:1000"),
            a("198.51.100.4:1000"),
            a("198.51.100.5:1000"),
        ];
        let chosen = peers_to_probe(&[a("198.51.100.2:1000")], &peers);
        assert_eq!(
            chosen.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            vec![
                a("198.51.100.1:1000"),
                a("198.51.100.3:1000"),
                a("198.51.100.4:1000"),
                a("198.51.100.5:1000"),
            ]
        );
        assert!(chosen.iter().all(|(_, ask)| *ask == Ask::Peer));
    }

    /// A new peer address makes a re-probe due at once the first time, then
    /// no sooner than a doubling backoff; an address already probed, or
    /// seen again, makes none.
    #[test]
    fn a_new_peer_address_reprobes_with_a_backoff() {
        let t0 = Instant::now();
        let ip = |n: u8| IpAddr::from([198, 51, 100, n]);
        let mut r = Reprobe::new(t0);
        r.probed([ip(1)]);
        r.peer_seen(ip(1), t0);
        assert!(!r.take_due(t0), "already probed");
        r.peer_seen(ip(2), t0);
        assert!(r.take_due(t0), "a new address, the first time: now");
        r.probed([ip(2)]);
        r.peer_seen(ip(2), t0);
        assert!(!r.take_due(t0 + Duration::from_secs(1)));
        r.peer_seen(ip(3), t0 + Duration::from_secs(1));
        assert!(
            !r.take_due(t0 + Duration::from_secs(29)),
            "inside the backoff"
        );
        assert!(r.take_due(t0 + REPROBE_BACKOFF_MIN));
        // Churn: one new address after another waits ever longer.
        r.peer_seen(ip(4), t0 + REPROBE_BACKOFF_MIN);
        assert!(!r.take_due(t0 + REPROBE_BACKOFF_MIN * 2));
        assert!(r.take_due(t0 + REPROBE_BACKOFF_MIN * 3));
        // And the wait stops growing at the cap.
        let mut at = t0 + REPROBE_BACKOFF_MIN * 3;
        for n in 5..20 {
            at += REPROBE_BACKOFF_MAX;
            r.peer_seen(ip(n), at);
            assert!(r.take_due(at), "{n}");
        }
    }

    #[test]
    fn a_bucket_allows_its_burst_then_one_per_refill() {
        let t0 = Instant::now();
        let mut b = Bucket::new(3, Duration::from_secs(2));
        assert!((0..3).all(|_| b.take(t0)));
        assert!(!b.take(t0));
        assert!(!b.take(t0 + Duration::from_secs(1)));
        assert!(b.take(t0 + Duration::from_secs(2)));
        assert!(!b.take(t0 + Duration::from_secs(3)));
        let mut sources = PerSource::new(2, Duration::from_secs(2));
        let (x, y) = (IpAddr::from([10, 0, 0, 1]), IpAddr::from([10, 0, 0, 2]));
        assert!(sources.take(x, t0) && sources.take(x, t0));
        assert!(!sources.take(x, t0), "x spent its burst");
        assert!(sources.take(y, t0), "y has its own");
    }

    #[test]
    fn answer_probes_reads_like_a_reticulum_boolean() {
        for (v, want) in [
            ("Yes", Some(true)),
            ("off", Some(false)),
            ("0", Some(false)),
            ("maybe", None),
        ] {
            assert_eq!(parse_bool(v), want, "{v}");
        }
    }
}
