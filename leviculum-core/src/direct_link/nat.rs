//! This node's NAT type, from two probe servers (+ciris, leviculum#100).
//!
//! A probe asks a server which `ip:port` its datagram arrived from: the
//! socket's mapping on the NAT in front of it. One server cannot tell an
//! endpoint-independent mapping (the same public port whoever the socket
//! sends to; RFC 4787 4.1, "cone") from an endpoint-dependent one (a fresh
//! port per destination, "symmetric"). Two servers probed from the SAME
//! socket can: the mapping either stays or it moves.
//!
//! Everything here is observation, kept apart from any model of the mesh:
//! [`NatReport`] carries what each server answered next to the verdict, so a
//! reader can check the verdict against the raw numbers.

use alloc::vec::Vec;
use core::net::SocketAddr;

/// How the NAT in front of a socket maps it, as two probes saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NatMapping {
    /// A server saw the socket's own host address and port: no NAT (a public
    /// address, or a global IPv6 address with at most a firewall).
    NoNat,
    /// Every server saw the same `ip:port`: the NAT keeps one mapping for
    /// all destinations (cone). A peer can punch the address a facilitator
    /// reported.
    EndpointIndependent,
    /// The servers saw different ports (or addresses): the NAT maps each
    /// destination anew (symmetric, "hard"). The address a facilitator
    /// reported is not the one the peer's punch has to reach.
    EndpointDependent,
    /// Not enough answers to say.
    Unknown(NatUnknown),
}

/// Why a [`NatMapping`] is [`NatMapping::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NatUnknown {
    /// Only one probe server is configured, and its answer was not the
    /// socket's own address.
    OneServer,
    /// Two or more servers were probed and only one answered.
    OneAnswered,
    /// No server answered.
    NoAnswer,
}

impl NatMapping {
    /// A short stable name, for logs and counters.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            NatMapping::NoNat => "no-nat",
            NatMapping::EndpointIndependent => "endpoint-independent",
            NatMapping::EndpointDependent => "endpoint-dependent",
            NatMapping::Unknown(NatUnknown::OneServer) => "unknown-one-server",
            NatMapping::Unknown(NatUnknown::OneAnswered) => "unknown-one-answered",
            NatMapping::Unknown(NatUnknown::NoAnswer) => "unknown-no-answer",
        }
    }

    /// Whether this says anything about the NAT (not [`NatMapping::Unknown`]).
    #[must_use]
    pub fn is_known(&self) -> bool {
        !matches!(self, NatMapping::Unknown(_))
    }
}

impl core::fmt::Display for NatMapping {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

/// One server's answer to a probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NatProbe {
    /// The server probed.
    pub server: SocketAddr,
    /// The address it saw the probe come from; `None` if it never answered.
    pub mapped: Option<SocketAddr>,
}

/// What a two-server probe found: the verdict and the observations behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatReport {
    /// The verdict.
    pub mapping: NatMapping,
    /// Each server's answer, in the order probed; the first is the
    /// facilitator the session used.
    pub probes: Vec<NatProbe>,
    /// The probe socket's local port.
    pub local_port: u16,
    /// The socket's host addresses (on `local_port`): the LAN IPv4 address
    /// it sends from, and a global IPv6 address if it has one.
    pub host_candidates: Vec<SocketAddr>,
    /// Every answer kept the local port (a port-preserving NAT, which makes
    /// a punch easier to predict). `None` when nothing answered.
    pub port_preserved: Option<bool>,
    /// The servers that answered share one IP address. Two ports on one
    /// host tell a port-dependent mapping from an independent one, but not
    /// an address-dependent one (RFC 4787's rarer middle case), which then
    /// reads as [`NatMapping::EndpointIndependent`].
    pub servers_share_ip: bool,
    /// The socket has a global IPv6 host candidate, which a peer can punch
    /// whatever the IPv4 NAT does.
    pub global_ipv6: bool,
    /// When it was observed, on the node's monotonic clock (ms). The node
    /// stamps it when the report is recorded.
    pub at_ms: u64,
    /// When it was observed, as Unix seconds, if the node knows the time.
    pub at_unix_secs: Option<u64>,
}

impl NatReport {
    /// Classify `probes` taken from one socket bound to `local_port`, whose
    /// host addresses are `host_candidates`. `at_ms` and `at_unix_secs`
    /// stamp it.
    #[must_use]
    pub fn from_probes(
        probes: Vec<NatProbe>,
        local_port: u16,
        host_candidates: Vec<SocketAddr>,
        at_ms: u64,
        at_unix_secs: Option<u64>,
    ) -> Self {
        let mapping = classify(&probes, &host_candidates);
        let answers: Vec<(SocketAddr, SocketAddr)> = probes
            .iter()
            .filter_map(|p| p.mapped.map(|m| (p.server, m)))
            .collect();
        let port_preserved =
            (!answers.is_empty()).then(|| answers.iter().all(|(_, m)| m.port() == local_port));
        let servers_share_ip =
            answers.len() >= 2 && answers.iter().all(|(s, _)| s.ip() == answers[0].0.ip());
        let global_ipv6 = host_candidates.iter().any(|a| match a.ip() {
            core::net::IpAddr::V6(ip) => super::session::is_global_v6(&ip),
            core::net::IpAddr::V4(_) => false,
        });
        NatReport {
            mapping,
            probes,
            local_port,
            host_candidates,
            port_preserved,
            servers_share_ip,
            global_ipv6,
            at_ms,
            at_unix_secs,
        }
    }

    /// The first answer: the reflexive address a peer is told.
    #[must_use]
    pub fn mapped(&self) -> Option<SocketAddr> {
        self.probes.iter().find_map(|p| p.mapped)
    }
}

/// The verdict on `probes` from one socket with host addresses
/// `host_candidates`.
///
/// Only answers in the first answer's address family are compared: an IPv4
/// and an IPv6 mapping are different sockets' worth of NAT, not a moved
/// mapping.
#[must_use]
pub fn classify(probes: &[NatProbe], host_candidates: &[SocketAddr]) -> NatMapping {
    let mut answers = probes.iter().filter_map(|p| p.mapped);
    let Some(first) = answers.next() else {
        return NatMapping::Unknown(NatUnknown::NoAnswer);
    };
    let same_family: Vec<SocketAddr> = core::iter::once(first)
        .chain(answers.filter(|a| a.is_ipv4() == first.is_ipv4()))
        .collect();
    if same_family.iter().any(|a| *a != first) {
        return NatMapping::EndpointDependent;
    }
    if host_candidates.contains(&first) {
        return NatMapping::NoNat;
    }
    if same_family.len() >= 2 {
        return NatMapping::EndpointIndependent;
    }
    if probes.len() >= 2 {
        NatMapping::Unknown(NatUnknown::OneAnswered)
    } else {
        NatMapping::Unknown(NatUnknown::OneServer)
    }
}

/// How one direct-link attempt ended, for correlating with [`NatMapping`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectLinkOutcome {
    /// The punch went through and the link moved onto the direct path.
    Established,
    /// No punch got through in the window, on this side or (a REJECT with
    /// reason PUNCH_FAILED) on the peer's; the link stays on the relay.
    PunchFailed,
    /// The peer (or the facilitator, for the peer) went quiet partway.
    TimedOut,
    /// The peer refused, with this REJECT reason byte (any but
    /// PUNCH_FAILED, which is [`DirectLinkOutcome::PunchFailed`]).
    Refused(u8),
    /// The facilitator never answered this node's probe.
    ProbeFailed,
    /// The punch went through but the link fell back to the relay before
    /// or instead of moving (the interface went down, a transfer held it too
    /// long, or the peer went back first).
    FellBack(super::Failure),
    /// A crossing proposal from the peer won; that one runs instead.
    Superseded,
    /// An established direct path was later lost; the link is back on the
    /// relay.
    Lost,
}

impl DirectLinkOutcome {
    /// The outcome of a session that ended with `failure`.
    #[must_use]
    pub fn of_failure(failure: super::Failure) -> Self {
        use super::Failure;
        match failure {
            // The peer's verdict on its own punch, not a refusal.
            Failure::Rejected(super::wire::REJECT_PUNCH_FAILED) => DirectLinkOutcome::PunchFailed,
            Failure::Rejected(reason) => DirectLinkOutcome::Refused(reason),
            Failure::ProbeFailed => DirectLinkOutcome::ProbeFailed,
            Failure::Timeout => DirectLinkOutcome::TimedOut,
            Failure::PunchFailed => DirectLinkOutcome::PunchFailed,
            Failure::Glare => DirectLinkOutcome::Superseded,
            Failure::InterfaceLost | Failure::HeldTooLong | Failure::PeerFellBack => {
                DirectLinkOutcome::FellBack(failure)
            }
        }
    }

    /// A short stable name, for logs.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            DirectLinkOutcome::Established => "established",
            DirectLinkOutcome::PunchFailed => "punch-failed",
            DirectLinkOutcome::TimedOut => "timed-out",
            DirectLinkOutcome::Refused(_) => "refused",
            DirectLinkOutcome::ProbeFailed => "probe-failed",
            DirectLinkOutcome::FellBack(_) => "fell-back",
            DirectLinkOutcome::Superseded => "superseded",
            DirectLinkOutcome::Lost => "lost",
        }
    }
}

/// One direct-link attempt's end, with this node's NAT verdict for the
/// session's socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectLinkRecord {
    /// The link, under the id the application holds.
    pub link_id: crate::link::LinkId,
    /// The upgrade session.
    pub session: super::SessionId,
    /// This node proposed the upgrade.
    pub proposed: bool,
    /// This node's NAT verdict on the session's probe socket; `None` when no
    /// report reached the node for it (an old driver, or a probe that never
    /// ran).
    pub nat: Option<NatMapping>,
    /// How it ended.
    pub outcome: DirectLinkOutcome,
    /// When, on the node's monotonic clock (ms).
    pub at_ms: u64,
}

/// Direct-link outcomes since the node started, and the latest few in full.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectLinkOutcomes {
    pub established: u64,
    pub punch_failed: u64,
    pub timed_out: u64,
    pub refused: u64,
    pub probe_failed: u64,
    pub fell_back: u64,
    pub superseded: u64,
    pub lost: u64,
    /// The latest [`OUTCOME_RECORDS`] outcomes, oldest first.
    pub recent: Vec<DirectLinkRecord>,
}

/// How many [`DirectLinkRecord`]s a node keeps.
pub const OUTCOME_RECORDS: usize = 32;

impl DirectLinkOutcomes {
    /// Count `record` and keep it, dropping the oldest past
    /// [`OUTCOME_RECORDS`].
    pub fn push(&mut self, record: DirectLinkRecord) {
        let counter = match record.outcome {
            DirectLinkOutcome::Established => &mut self.established,
            DirectLinkOutcome::PunchFailed => &mut self.punch_failed,
            DirectLinkOutcome::TimedOut => &mut self.timed_out,
            DirectLinkOutcome::Refused(_) => &mut self.refused,
            DirectLinkOutcome::ProbeFailed => &mut self.probe_failed,
            DirectLinkOutcome::FellBack(_) => &mut self.fell_back,
            DirectLinkOutcome::Superseded => &mut self.superseded,
            DirectLinkOutcome::Lost => &mut self.lost,
        };
        *counter = counter.saturating_add(1);
        if self.recent.len() >= OUTCOME_RECORDS {
            self.recent.remove(0);
        }
        self.recent.push(record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn probe(server: &str, mapped: Option<&str>) -> NatProbe {
        NatProbe {
            server: addr(server),
            mapped: mapped.map(addr),
        }
    }

    const LAN: &str = "10.0.1.2:40000";

    fn report(probes: Vec<NatProbe>) -> NatReport {
        NatReport::from_probes(probes, 40000, vec![addr(LAN)], 0, None)
    }

    #[test]
    fn the_same_mapping_from_two_servers_is_endpoint_independent() {
        let r = report(vec![
            probe("198.51.100.1:4343", Some("203.0.113.1:40000")),
            probe("198.51.100.2:4343", Some("203.0.113.1:40000")),
        ]);
        assert_eq!(r.mapping, NatMapping::EndpointIndependent);
        assert_eq!(r.port_preserved, Some(true));
        assert!(!r.servers_share_ip);
        assert_eq!(r.mapped(), Some(addr("203.0.113.1:40000")));
    }

    #[test]
    fn a_different_port_per_server_is_endpoint_dependent() {
        let r = report(vec![
            probe("198.51.100.1:4343", Some("203.0.113.1:51000")),
            probe("198.51.100.2:4343", Some("203.0.113.1:51007")),
        ]);
        assert_eq!(r.mapping, NatMapping::EndpointDependent);
        assert_eq!(r.port_preserved, Some(false));
    }

    #[test]
    fn a_different_public_ip_per_server_is_endpoint_dependent() {
        let r = report(vec![
            probe("198.51.100.1:4343", Some("203.0.113.1:40000")),
            probe("198.51.100.2:4343", Some("203.0.113.9:40000")),
        ]);
        assert_eq!(r.mapping, NatMapping::EndpointDependent);
        assert_eq!(r.port_preserved, Some(true));
    }

    #[test]
    fn the_mapped_address_equal_to_the_host_address_is_no_nat() {
        let both = report(vec![
            probe("198.51.100.1:4343", Some(LAN)),
            probe("198.51.100.2:4343", Some(LAN)),
        ]);
        assert_eq!(both.mapping, NatMapping::NoNat);
        // One server is enough to see there is no NAT.
        let one = report(vec![probe("198.51.100.1:4343", Some(LAN))]);
        assert_eq!(one.mapping, NatMapping::NoNat);
    }

    #[test]
    fn too_few_answers_are_unknown_with_the_reason() {
        let one_server = report(vec![probe("198.51.100.1:4343", Some("203.0.113.1:40000"))]);
        assert_eq!(
            one_server.mapping,
            NatMapping::Unknown(NatUnknown::OneServer)
        );
        let one_answered = report(vec![
            probe("198.51.100.1:4343", Some("203.0.113.1:40000")),
            probe("198.51.100.2:4343", None),
        ]);
        assert_eq!(
            one_answered.mapping,
            NatMapping::Unknown(NatUnknown::OneAnswered)
        );
        // The second server answering alone still counts as one answer.
        let second_only = report(vec![
            probe("198.51.100.1:4343", None),
            probe("198.51.100.2:4343", Some("203.0.113.1:40000")),
        ]);
        assert_eq!(
            second_only.mapping,
            NatMapping::Unknown(NatUnknown::OneAnswered)
        );
        let none = report(vec![
            probe("198.51.100.1:4343", None),
            probe("198.51.100.2:4343", None),
        ]);
        assert_eq!(none.mapping, NatMapping::Unknown(NatUnknown::NoAnswer));
        assert_eq!(none.port_preserved, None);
        assert_eq!(none.mapped(), None);
    }

    #[test]
    fn two_ports_on_one_server_are_flagged() {
        let r = report(vec![
            probe("198.51.100.1:3478", Some("203.0.113.1:40000")),
            probe("198.51.100.1:3479", Some("203.0.113.1:40000")),
        ]);
        assert_eq!(r.mapping, NatMapping::EndpointIndependent);
        assert!(r.servers_share_ip);
    }

    #[test]
    fn mappings_of_another_family_are_not_compared() {
        let r = report(vec![
            probe("198.51.100.1:4343", Some("203.0.113.1:40000")),
            probe("[2001:db8::1]:4343", Some("[2001:db8::2]:40000")),
        ]);
        assert_eq!(r.mapping, NatMapping::Unknown(NatUnknown::OneAnswered));
    }

    #[test]
    fn a_global_ipv6_host_candidate_is_reported() {
        let r = NatReport::from_probes(
            vec![probe("198.51.100.1:4343", Some("203.0.113.1:40000"))],
            40000,
            vec![addr(LAN), addr("[2001:db8::5]:40000")],
            7,
            Some(1_800_000_000),
        );
        assert!(r.global_ipv6);
        assert_eq!((r.at_ms, r.at_unix_secs), (7, Some(1_800_000_000)));
        let link_local =
            NatReport::from_probes(vec![], 40000, vec![addr("[fe80::1]:40000")], 0, None);
        assert!(!link_local.global_ipv6);
    }

    #[test]
    fn outcomes_are_counted_and_the_latest_kept() {
        let mut outcomes = DirectLinkOutcomes::default();
        for i in 0..(OUTCOME_RECORDS + 3) {
            outcomes.push(DirectLinkRecord {
                link_id: crate::link::LinkId::new([i as u8; 16]),
                session: [0; 16],
                proposed: true,
                nat: Some(NatMapping::EndpointDependent),
                outcome: if i % 2 == 0 {
                    DirectLinkOutcome::Established
                } else {
                    DirectLinkOutcome::of_failure(super::super::Failure::PunchFailed)
                },
                at_ms: i as u64,
            });
        }
        assert_eq!(outcomes.established, 18);
        assert_eq!(outcomes.punch_failed, 17);
        assert_eq!(outcomes.recent.len(), OUTCOME_RECORDS);
        assert_eq!(outcomes.recent[0].at_ms, 3);
    }

    /// The NAT lab's first run (leviculum#100) recorded the side whose peer
    /// failed to punch as `refused`: the peer's REJECT PUNCH_FAILED is a
    /// failed punch, any other reason a refusal.
    #[test]
    fn a_peers_failed_punch_is_a_failed_punch_not_a_refusal() {
        use super::super::wire::{REJECT_POLICY, REJECT_PUNCH_FAILED};
        use super::super::Failure;
        assert_eq!(
            DirectLinkOutcome::of_failure(Failure::Rejected(REJECT_PUNCH_FAILED)),
            DirectLinkOutcome::PunchFailed
        );
        assert_eq!(
            DirectLinkOutcome::of_failure(Failure::Rejected(REJECT_POLICY)),
            DirectLinkOutcome::Refused(REJECT_POLICY)
        );
    }
}
