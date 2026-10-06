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
//! direct_connect_policy = reject      # | accept_all | identified_only
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
}

impl DirectLinkSettings {
    /// Read the `[reticulum]` keys. Unknown policy or protocol words are a
    /// config error rather than a silent default: getting either wrong
    /// changes who learns this node's public address.
    pub fn from_config(config: &crate::config::ReticulumConfig) -> Result<Self, Error> {
        let policy = match config.direct_link.direct_connect_policy.as_deref().map(str::trim) {
            None | Some("") | Some("reject") => DirectLinkPolicy::Reject,
            Some("accept_all") => DirectLinkPolicy::AcceptAll,
            Some("identified_only") => DirectLinkPolicy::IdentifiedOnly,
            Some(other) => {
                return Err(Error::Config(format!(
                    "direct_connect_policy must be reject, accept_all or identified_only, not {other:?}"
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
    /// Which peers' direct-link requests to accept: `reject` (default),
    /// `accept_all` or `identified_only`.
    #[serde(default)]
    pub direct_connect_policy: Option<String>,
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
        "probe_port" => config.direct_link.probe_port = value.trim().parse().ok(),
        "probe_addr" => config.direct_link.probe_addr = Some(value.trim().to_string()),
        "probe_protocol" => {
            config.direct_link.probe_protocol = Some(value.trim().to_ascii_lowercase())
        }
        "direct_connect_policy" => {
            config.direct_link.direct_connect_policy = Some(value.trim().to_ascii_lowercase())
        }
        _ => {}
    }
}

/// Resolve a `host:port` facilitator to the address put in the REQUEST.
/// The peer probes that address too, so it has to be an IP, not a name.
pub(crate) async fn resolve_facilitator(addr: &str) -> io::Result<SocketAddr> {
    tokio::net::lookup_host(addr)
        .await?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{addr} has no IPv4 address"),
            )
        })
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
    let mut rng = rand_core::OsRng;
    let mut buf = [0u8; 512];
    for _ in 0..PROBE_ATTEMPTS {
        // A fresh nonce per attempt, so a late answer to an earlier attempt
        // is not mistaken for this one (either is equally true, but matching
        // exactly keeps the rule simple).
        let attempt = ProbeAttempt::new(protocol, &mut rng);
        let request = attempt.request();
        socket.send_to(&request, server).await?;
        let deadline = tokio::time::Instant::now() + PROBE_WAIT;
        loop {
            match tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
                Err(_) => break,
                Ok(Err(e)) => {
                    tracing::debug!("direct link: probe recv error: {e}");
                    break;
                }
                Ok(Ok((len, src))) => {
                    if src != server {
                        continue;
                    }
                    if let Some(public) = attempt.answer(&buf[..len]) {
                        return Ok(public);
                    }
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("facilitator {server} did not answer"),
    ))
}

// Punch

/// Interval between punch frames, and the whole window. Same as rns-rs, so
/// the two ends of a mixed pair give up at about the same time.
const PUNCH_INTERVAL: Duration = Duration::from_millis(100);
const PUNCH_WINDOW: Duration = Duration::from_secs(10);

/// Punch toward `peer` until both directions are proven: a valid punch from
/// the peer has arrived (so its NAT lets it out to us) and a valid ack for
/// one of ours has arrived (so ours reach it). Returns the address the peer
/// was actually seen at, which a port-preserving NAT makes `peer` itself.
pub(crate) async fn punch(
    socket: &UdpSocket,
    peer: SocketAddr,
    session: &SessionId,
    token: &[u8; 32],
) -> Option<SocketAddr> {
    let deadline = tokio::time::Instant::now() + PUNCH_WINDOW;
    let mut heard_punch_from: Option<SocketAddr> = None;
    let mut acked_by: Option<SocketAddr> = None;
    let mut seq: u32 = 0;
    let mut tick = tokio::time::interval(PUNCH_INTERVAL);
    let mut buf = [0u8; 128];
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return None,
            _ = tick.tick() => {
                let frame = wire::punch_frame(PunchKind::Punch, session, token, seq);
                seq = seq.wrapping_add(1) % wire::KEEPALIVE_SEQ;
                let _ = socket.send_to(&frame, peer).await;
            }
            result = socket.recv_from(&mut buf) => {
                let Ok((len, src)) = result else { continue };
                match wire::parse_punch_frame(&buf[..len], session, token) {
                    Some((PunchKind::Punch, their_seq)) => {
                        let ack = wire::punch_frame(PunchKind::Ack, session, token, their_seq);
                        let _ = socket.send_to(&ack, src).await;
                        heard_punch_from = Some(src);
                    }
                    Some((PunchKind::Ack, _)) => acked_by = Some(src),
                    None => continue,
                }
                if let (Some(from), Some(_)) = (heard_punch_from, acked_by) {
                    return Some(from);
                }
            }
        }
    }
}

// Facilitator

/// Answer RNSP and STUN Binding requests on `port`, telling each prober the
/// address its datagram arrived from. Stateless; runs until aborted.
pub(crate) async fn spawn_facilitator(port: u16) -> io::Result<AbortHandle> {
    let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?;
    tracing::info!(
        "direct link: facilitator listening on {}",
        socket.local_addr()?
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
    },
    Punched {
        session: SessionId,
        peer: Option<SocketAddr>,
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
    timing: DirectUdpTiming,
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
            timing: DirectUdpTiming::default(),
        }
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
                token,
            } => self.start_punch(session, peer, token),
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
        let bind = if server.is_ipv4() {
            SocketAddr::from(([0, 0, 0, 0], 0))
        } else {
            SocketAddr::from(([0u16; 8], 0))
        };
        let socket = match std::net::UdpSocket::bind(bind)
            .and_then(|s| s.set_nonblocking(true).map(|_| s))
            .and_then(UdpSocket::from_std)
        {
            Ok(s) => Arc::new(s),
            Err(e) => {
                tracing::warn!("direct link: cannot bind a probe socket: {e}");
                let _ = outcomes.try_send(Outcome::Probed {
                    session,
                    public: None,
                });
                return;
            }
        };
        self.sockets.insert(session, Arc::clone(&socket));
        let task = tokio::spawn(async move {
            let public = match probe(&socket, server, protocol).await {
                Ok(public) => {
                    tracing::info!("direct link: reflexive address {public} (via {server})");
                    Some(public)
                }
                Err(e) => {
                    tracing::info!("direct link: probe failed: {e}");
                    None
                }
            };
            let _ = outcomes.send(Outcome::Probed { session, public }).await;
        });
        self.replace_task(session, task.abort_handle());
    }

    fn start_punch(&mut self, session: SessionId, peer: SocketAddr, token: [u8; 32]) {
        let outcomes = self.outcomes.clone();
        let Some(socket) = self.sockets.get(&session).cloned() else {
            // No probe socket (it failed to bind, or was released): there is
            // no NAT mapping to punch from.
            let _ = outcomes.try_send(Outcome::Punched {
                session,
                peer: None,
            });
            return;
        };
        self.tokens.insert(session, token);
        let task = tokio::spawn(async move {
            tracing::info!("direct link: punching toward {peer}");
            let seen = punch(&socket, peer, &session, &token).await;
            match seen {
                Some(at) => tracing::info!("direct link: punch through to {at}"),
                None => tracing::info!("direct link: punch toward {peer} failed"),
            }
            let _ = outcomes
                .send(Outcome::Punched {
                    session,
                    peer: seen,
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
    }

    /// The punch for `session` succeeded toward `peer`: build its interface
    /// under `id`. The caller sends the handle for registration and, once it
    /// is registered, reports the index to the node via
    /// [`Self::registered`].
    pub(crate) fn interface_for(
        &mut self,
        session: SessionId,
        peer: SocketAddr,
        id: InterfaceId,
    ) -> Option<InterfaceHandle> {
        self.tasks.remove(&session);
        let socket = self.sockets.remove(&session)?;
        let token = self.tokens.remove(&session)?;
        let (handle, task) =
            spawn_direct_udp_interface(id, socket, peer, session, token, self.timing);
        self.registering.insert(id, session);
        self.interfaces.insert(id.0, task);
        Some(handle)
    }

    /// A handle just got registered; if it was a direct interface, the
    /// session it belongs to.
    pub(crate) fn registered(&mut self, id: InterfaceId) -> Option<SessionId> {
        self.registering.remove(&id)
    }

    /// An interface disconnected; forget its task if it was a direct one.
    pub(crate) fn interface_gone(&mut self, id: InterfaceId) {
        self.interfaces.remove(&id.0);
        self.registering.remove(&id);
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
            punch(&a, b_addr, &session, &token),
            punch(&b, a_addr, &session, &token)
        );
        assert_eq!((ra, rb), (Some(b_addr), Some(a_addr)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_punch_with_the_wrong_token_never_completes() {
        let a = loopback().await;
        let b = loopback().await;
        let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        let session = [3u8; 16];
        let (ra, rb) = tokio::join!(
            punch(&a, b_addr, &session, &[4u8; 32]),
            punch(&b, a_addr, &session, &[5u8; 32])
        );
        assert_eq!((ra, rb), (None, None));
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
    fn a_misspelt_policy_is_a_config_error() {
        let err =
            DirectLinkSettings::from_config(&reticulum(&[("direct_connect_policy", "accept-all")]))
                .unwrap_err();
        assert!(matches!(err, Error::Config(_)));
        assert!(DirectLinkSettings::from_config(&reticulum(&[("probe_protocol", "ice")])).is_err());
    }
}
