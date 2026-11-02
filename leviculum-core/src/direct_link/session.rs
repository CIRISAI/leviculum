//! One direct-link upgrade attempt, as a sans-I/O state machine.
//!
//! The session never touches a socket or the channel. Each input (a probe
//! result, a signal, a punch result, the clock) returns the [`Step`]s the
//! caller must carry out: send a signal on the link, probe the facilitator
//! from a fresh UDP socket, or punch from that same socket. The socket has to
//! be the same one for probe and punch, because the reflexive address the
//! probe learns is the NAT mapping of that socket and no other.
//!
//! ```text
//!  initiator                                responder
//!  Discovering --probe ok--> Proposing
//!            REQUEST ------------------------> (policy) Discovering
//!            <------------------------- ACCEPT
//!  AwaitingReady                                   --probe ok-->
//!            <-------------------------- READY      Punching
//!  Punching  <====== punch frames, both ways ======> Punching
//!  Connected                                        Connected
//! ```
//!
//! Every waiting phase has a deadline; missing it fails the session, and the
//! link carries on over the path it already had.

use alloc::vec::Vec;
use core::net::SocketAddr;

use super::wire::{ProbeProtocol, SessionId, Signal, REJECT_PUNCH_FAILED, REJECT_UNSUPPORTED};

/// How long each waiting phase may last before the session gives up.
///
/// Punching gets the driver's own punch window (10 s) plus slack, so the
/// driver's verdict normally arrives before this timer fires.
pub const DISCOVER_TIMEOUT_MS: u64 = 10_000;
pub const PROPOSE_TIMEOUT_MS: u64 = 10_000;
pub const READY_TIMEOUT_MS: u64 = 10_000;
pub const PUNCH_TIMEOUT_MS: u64 = 12_000;
/// +ciris (leviculum#89): how long a side whose punch succeeded waits for the
/// peer's verdict. The peer's punch window ends within this, and its COMPLETE
/// or REJECT travels the relay.
pub const COMMIT_TIMEOUT_MS: u64 = 12_000;

/// Which end of the upgrade this node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}

/// Where the session is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for this node's reflexive address from the facilitator.
    Discovering,
    /// Initiator: REQUEST sent, waiting for ACCEPT or REJECT.
    Proposing,
    /// Initiator: accepted, waiting for the responder's READY.
    AwaitingReady,
    /// Both addresses known; the driver is punching.
    Punching,
    /// +ciris (leviculum#89): this side's punch succeeded and COMPLETE is
    /// sent; waiting for the peer's COMPLETE before either side moves.
    Committing,
    /// The punch succeeded.
    Connected,
}

/// Why a session ended without a direct path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The peer sent REJECT with this reason byte.
    Rejected(u8),
    /// The facilitator did not answer this node's probe.
    ProbeFailed,
    /// The peer stopped answering partway through.
    Timeout,
    /// No punch frame got through in the punch window (symmetric NAT,
    /// a firewall dropping inbound UDP, or no route between the two).
    PunchFailed,
}

/// +ciris — the peer's host candidates worth punching:
/// - its global IPv6 addresses, always: most mobile and many home networks
///   give hosts one, with no NAT in the way, only a stateful firewall that a
///   simultaneous send opens like any punch;
/// - its IPv4 host addresses only when both ends report the SAME public IP
///   (one NAT; a punch to that IP needs the router to hairpin, which many
///   home routers do not). A private address behind another NAT is
///   unreachable, and punching it only leaks datagrams onto this node's own
///   network.
#[must_use]
pub fn same_nat_alternates(
    own_public: SocketAddr,
    peer_public: SocketAddr,
    peer_local: &[SocketAddr],
) -> Vec<SocketAddr> {
    let same_nat = own_public.ip() == peer_public.ip();
    peer_local
        .iter()
        .copied()
        .filter(|a| *a != peer_public && a.port() != 0)
        .filter(|a| match a.ip() {
            core::net::IpAddr::V6(ip) => is_global_v6(&ip),
            core::net::IpAddr::V4(_) => same_nat,
        })
        .collect()
}

/// +ciris — a global unicast IPv6 address (2000::/3), the only kind a peer
/// elsewhere can reach.
#[must_use]
pub fn is_global_v6(ip: &core::net::Ipv6Addr) -> bool {
    ip.segments()[0] & 0xe000 == 0x2000
}

/// Something the caller must do for the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Send this signal on the link's channel.
    Send(Signal),
    /// Bind a UDP socket and learn its reflexive address from `server`.
    Probe {
        server: SocketAddr,
        protocol: ProbeProtocol,
    },
    /// Punch from the probe's socket toward `peer`, and toward each of
    /// `alternates` at once: the peer's host candidates, when both ends sit
    /// behind the same NAT (+ciris). The first that proves both directions
    /// is the path.
    Punch {
        peer: SocketAddr,
        alternates: Vec<SocketAddr>,
    },
    /// The direct path is up.
    Established,
    /// The session is over without one.
    Failed(Failure),
}

/// One upgrade attempt on one link.
#[derive(Debug, Clone)]
pub struct Session {
    id: SessionId,
    role: Role,
    phase: Phase,
    phase_since_ms: u64,
    /// The punch token, bound to the link key and `id`.
    token: [u8; 32],
    facilitator: SocketAddr,
    protocol: ProbeProtocol,
    /// The other side's reflexive address, once known.
    peer_public: Option<SocketAddr>,
    /// +ciris: the other side's host candidates, as it sent them.
    peer_local: Vec<SocketAddr>,
    /// +ciris: this node's reflexive address, once probed (the initiator
    /// needs it when READY arrives).
    own_public: Option<SocketAddr>,
    /// +ciris (leviculum#89): both ends agreed to the two-sided commit: a
    /// side moves the link only once its own punch and the peer's both
    /// succeeded. Offered by every leviculum REQUEST; an rns-rs peer never
    /// sends `"c"`, so with one the session keeps the one-sided rule.
    commit: bool,
    /// The peer's COMPLETE arrived (while this side was still punching).
    peer_committed: bool,
    /// +ciris (leviculum#88): opaque bytes the REQUEST carries for the
    /// responder's host.
    host_payload: Option<Vec<u8>>,
}

impl Session {
    /// Start an upgrade as the initiator: probe first, then propose.
    pub fn initiate(
        id: SessionId,
        token: [u8; 32],
        facilitator: SocketAddr,
        protocol: ProbeProtocol,
        now_ms: u64,
    ) -> (Self, Vec<Step>) {
        let session = Session {
            id,
            role: Role::Initiator,
            phase: Phase::Discovering,
            phase_since_ms: now_ms,
            token,
            facilitator,
            protocol,
            peer_public: None,
            peer_local: Vec::new(),
            own_public: None,
            commit: true,
            peer_committed: false,
            host_payload: None,
        };
        let steps = alloc::vec![Step::Probe {
            server: facilitator,
            protocol,
        }];
        (session, steps)
    }

    /// Take a peer's REQUEST as the responder: accept, then probe the
    /// facilitator the request names, with the protocol it names.
    ///
    /// The caller has already applied its policy; a session only exists for
    /// a request that policy let through.
    pub fn respond(
        id: SessionId,
        token: [u8; 32],
        facilitator: SocketAddr,
        initiator_public: SocketAddr,
        initiator_local: Vec<SocketAddr>,
        protocol: ProbeProtocol,
        now_ms: u64,
    ) -> (Self, Vec<Step>) {
        let session = Session {
            id,
            role: Role::Responder,
            phase: Phase::Discovering,
            phase_since_ms: now_ms,
            token,
            facilitator,
            protocol,
            peer_public: Some(initiator_public),
            peer_local: initiator_local,
            own_public: None,
            // Agreed only if the REQUEST offered it: see `set_commit`.
            commit: false,
            peer_committed: false,
            host_payload: None,
        };
        let steps = alloc::vec![
            Step::Send(Signal::Accept { session: id }),
            Step::Probe {
                server: facilitator,
                protocol,
            },
        ];
        (session, steps)
    }

    pub fn id(&self) -> &SessionId {
        &self.id
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn token(&self) -> &[u8; 32] {
        &self.token
    }

    /// +ciris (leviculum#89): whether the two-sided commit is in force (as
    /// far as this side knows so far).
    pub fn commit(&self) -> bool {
        self.commit
    }

    /// +ciris (leviculum#89): a responder agrees to the two-sided commit
    /// when the REQUEST offered it. An initiator offers it by default; it
    /// turns it off here only to talk to a peer as rns-rs does.
    pub fn set_commit(&mut self, commit: bool) {
        self.commit = commit;
    }

    /// +ciris (leviculum#88): bytes the initiator's REQUEST carries for the
    /// responder's host. Set before the probe answers.
    pub fn set_host_payload(&mut self, payload: Option<Vec<u8>>) {
        self.host_payload = payload;
    }

    /// The peer's reflexive address, once the session has learned it.
    pub fn peer_public(&self) -> Option<SocketAddr> {
        self.peer_public
    }

    /// When the current phase times out, or `None` once connected.
    pub fn deadline_ms(&self) -> Option<u64> {
        let budget = match self.phase {
            Phase::Discovering => DISCOVER_TIMEOUT_MS,
            Phase::Proposing => PROPOSE_TIMEOUT_MS,
            Phase::AwaitingReady => READY_TIMEOUT_MS,
            Phase::Punching => PUNCH_TIMEOUT_MS,
            Phase::Committing => COMMIT_TIMEOUT_MS,
            Phase::Connected => return None,
        };
        Some(self.phase_since_ms.saturating_add(budget))
    }

    fn enter(&mut self, phase: Phase, now_ms: u64) {
        self.phase = phase;
        self.phase_since_ms = now_ms;
    }

    /// The probe learned this node's reflexive address.
    pub fn probed(&mut self, public: SocketAddr, now_ms: u64) -> Vec<Step> {
        self.probed_with_local(public, Vec::new(), now_ms)
    }

    /// +ciris — [`Self::probed`], with this node's host candidates: sent to
    /// the peer, which punches them if it turns out to share this node's NAT.
    pub fn probed_with_local(
        &mut self,
        public: SocketAddr,
        local: Vec<SocketAddr>,
        now_ms: u64,
    ) -> Vec<Step> {
        if self.phase != Phase::Discovering {
            return Vec::new();
        }
        match self.role {
            Role::Initiator => {
                self.enter(Phase::Proposing, now_ms);
                self.own_public = Some(public);
                alloc::vec![Step::Send(Signal::Request {
                    session: self.id,
                    facilitator: self.facilitator,
                    initiator_public: public,
                    protocol: self.protocol,
                    initiator_local: local,
                    commit: self.commit,
                    host_payload: self.host_payload.take(),
                })]
            }
            Role::Responder => {
                // Constructed with the initiator's address, so it is there.
                let Some(peer) = self.peer_public else {
                    return alloc::vec![Step::Failed(Failure::Timeout)];
                };
                self.enter(Phase::Punching, now_ms);
                let alternates = same_nat_alternates(public, peer, &self.peer_local);
                alloc::vec![
                    Step::Send(Signal::Ready {
                        session: self.id,
                        responder_public: public,
                        responder_local: local,
                        commit: self.commit,
                    }),
                    Step::Punch { peer, alternates },
                ]
            }
        }
    }

    /// The probe got no answer.
    ///
    /// A responder tells the initiator rather than leaving it to time out.
    pub fn probe_failed(&mut self) -> Vec<Step> {
        if self.phase != Phase::Discovering {
            return Vec::new();
        }
        let mut steps = Vec::new();
        if self.role == Role::Responder {
            steps.push(Step::Send(Signal::Reject {
                session: self.id,
                reason: REJECT_UNSUPPORTED,
            }));
        }
        steps.push(Step::Failed(Failure::ProbeFailed));
        steps
    }

    /// A signal for this session arrived on the link.
    ///
    /// Signals that do not fit the current phase are ignored: the channel is
    /// reliable and ordered, so one can only be a peer bug or a stale
    /// session's leftover, and neither should end a live session.
    pub fn signal(&mut self, signal: &Signal, now_ms: u64) -> Vec<Step> {
        if signal.session() != &self.id {
            return Vec::new();
        }
        match (self.role, self.phase, signal) {
            (Role::Initiator, Phase::Proposing, Signal::Accept { .. }) => {
                self.enter(Phase::AwaitingReady, now_ms);
                Vec::new()
            }
            // A responder whose probe failed rejects after it accepted, so a
            // REJECT is valid while waiting for READY as well.
            (
                Role::Initiator,
                Phase::Proposing | Phase::AwaitingReady,
                Signal::Reject { reason, .. },
            ) => alloc::vec![Step::Failed(Failure::Rejected(*reason))],
            (
                Role::Initiator,
                Phase::AwaitingReady,
                Signal::Ready {
                    responder_public,
                    responder_local,
                    commit,
                    ..
                },
            ) => {
                // In force only when the responder agreed too.
                self.commit = self.commit && *commit;
                self.peer_public = Some(*responder_public);
                self.peer_local.clone_from(responder_local);
                self.enter(Phase::Punching, now_ms);
                let alternates = match self.own_public {
                    Some(own) => same_nat_alternates(own, *responder_public, responder_local),
                    None => Vec::new(),
                };
                alloc::vec![Step::Punch {
                    peer: *responder_public,
                    alternates,
                }]
            }
            // +ciris (leviculum#89): the peer's half of the two-sided commit.
            (_, Phase::Punching, Signal::Complete { .. }) if self.commit => {
                self.peer_committed = true;
                Vec::new()
            }
            (_, Phase::Committing, Signal::Complete { .. }) => {
                self.enter(Phase::Connected, now_ms);
                alloc::vec![Step::Established]
            }
            (_, Phase::Punching | Phase::Committing, Signal::Reject { reason, .. })
                if self.commit =>
            {
                alloc::vec![Step::Failed(Failure::Rejected(*reason))]
            }
            _ => Vec::new(),
        }
    }

    /// The driver's verdict on the punch.
    pub fn punched(&mut self, succeeded: bool, now_ms: u64) -> Vec<Step> {
        if self.phase != Phase::Punching {
            return Vec::new();
        }
        if !self.commit {
            // One-sided, as rns-rs decides: this side's verdict alone.
            return if succeeded {
                self.enter(Phase::Connected, now_ms);
                alloc::vec![Step::Established]
            } else {
                alloc::vec![Step::Failed(Failure::PunchFailed)]
            };
        }
        // +ciris (leviculum#89): two-sided. A punch can succeed on one side
        // only, and a side that moved alone sent its replies into a socket
        // the other had already released. So each side tells the other its
        // verdict, and moves only when both succeeded.
        if succeeded {
            let complete = Step::Send(Signal::Complete { session: self.id });
            if self.peer_committed {
                self.enter(Phase::Connected, now_ms);
                alloc::vec![complete, Step::Established]
            } else {
                self.enter(Phase::Committing, now_ms);
                alloc::vec![complete]
            }
        } else {
            alloc::vec![
                Step::Send(Signal::Reject {
                    session: self.id,
                    reason: REJECT_PUNCH_FAILED,
                }),
                Step::Failed(Failure::PunchFailed),
            ]
        }
    }

    /// Fail the session if its phase deadline has passed.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Step> {
        match self.deadline_ms() {
            Some(deadline) if now_ms >= deadline => {
                let failure = if self.phase == Phase::Discovering {
                    Failure::ProbeFailed
                } else if self.phase == Phase::Punching {
                    Failure::PunchFailed
                } else if self.phase == Phase::Committing {
                    // The peer never said how its punch went.
                    Failure::Timeout
                } else {
                    Failure::Timeout
                };
                alloc::vec![Step::Failed(failure)]
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::direct_link::wire::{REJECT_BUSY, REJECT_POLICY};

    const ID: SessionId = [0x42; 16];
    const TOKEN: [u8; 32] = [0x24; 32];

    fn t() -> SocketAddr {
        "203.0.113.7:4343".parse().unwrap()
    }
    fn a_pub() -> SocketAddr {
        "198.51.100.1:5000".parse().unwrap()
    }
    fn b_pub() -> SocketAddr {
        "192.0.2.9:6000".parse().unwrap()
    }

    /// Feed every `Send` from one side into the other, as the link would.
    fn deliver(steps: &[Step], to: &mut Session, now: u64) -> Vec<Step> {
        steps
            .iter()
            .filter_map(|s| match s {
                Step::Send(sig) => Some(to.signal(sig, now)),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn request_from(steps: &[Step]) -> &Signal {
        steps
            .iter()
            .find_map(|s| match s {
                Step::Send(sig @ Signal::Request { .. }) => Some(sig),
                _ => None,
            })
            .expect("a REQUEST")
    }

    #[test]
    fn full_upgrade_both_sides() {
        let (mut a, steps) = Session::initiate(ID, TOKEN, t(), ProbeProtocol::Rnsp, 0);
        assert_eq!(
            steps,
            [Step::Probe {
                server: t(),
                protocol: ProbeProtocol::Rnsp
            }]
        );

        let steps = a.probed(a_pub(), 100);
        assert_eq!(a.phase(), Phase::Proposing);
        let Signal::Request {
            session,
            facilitator,
            initiator_public,
            protocol,
            initiator_local,
            commit: true,
            host_payload: None,
        } = request_from(&steps).clone()
        else {
            unreachable!()
        };
        assert_eq!(initiator_public, a_pub());

        let (mut b, b_steps) = Session::respond(
            session,
            TOKEN,
            facilitator,
            initiator_public,
            initiator_local,
            protocol,
            150,
        );
        assert_eq!(
            b_steps[1],
            Step::Probe {
                server: t(),
                protocol: ProbeProtocol::Rnsp
            }
        );
        assert!(deliver(&b_steps, &mut a, 160).is_empty());
        assert_eq!(a.phase(), Phase::AwaitingReady);

        let b_steps = b.probed(b_pub(), 200);
        assert_eq!(b.phase(), Phase::Punching);
        assert_eq!(
            b_steps[1],
            Step::Punch {
                peer: a_pub(),
                alternates: Vec::new()
            }
        );

        let a_steps = deliver(&b_steps, &mut a, 210);
        assert_eq!(
            a_steps,
            [Step::Punch {
                peer: b_pub(),
                alternates: Vec::new()
            }]
        );
        assert_eq!(a.peer_public(), Some(b_pub()));

        assert_eq!(a.punched(true, 300), [Step::Established]);
        assert_eq!(b.punched(true, 300), [Step::Established]);
        assert_eq!(a.deadline_ms(), None);
        assert!(
            a.poll(u64::MAX).is_empty(),
            "a connected session never times out"
        );
    }

    /// +ciris: two nodes behind one NAT (the same public IP) punch each
    /// other's host candidates too; a router that does not hairpin never
    /// delivers a punch sent to its own public address. Behind different NATs
    /// a host candidate is never punched.
    #[test]
    fn behind_one_nat_each_side_punches_the_others_host_candidate() {
        use core::net::{IpAddr, Ipv4Addr};
        let shared = IpAddr::V4(Ipv4Addr::new(73, 45, 127, 73));
        let (a_pub, b_pub) = (
            SocketAddr::new(shared, 48098),
            SocketAddr::new(shared, 55418),
        );
        let a_lan = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)), 40001);
        let b_lan = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 11)), 40002);

        let (mut a, _) = Session::initiate(ID, TOKEN, t(), ProbeProtocol::Rnsp, 0);
        let steps = a.probed_with_local(a_pub, alloc::vec![a_lan], 1);
        let Signal::Request {
            session,
            facilitator,
            initiator_public,
            protocol,
            initiator_local,
            commit: true,
            host_payload: None,
        } = request_from(&steps).clone()
        else {
            unreachable!()
        };
        assert_eq!(
            initiator_local,
            [a_lan],
            "the request carries A's host candidate"
        );
        let (mut b, b_steps) = Session::respond(
            session,
            TOKEN,
            facilitator,
            initiator_public,
            initiator_local,
            protocol,
            2,
        );
        deliver(&b_steps, &mut a, 3);
        let b_steps = b.probed_with_local(b_pub, alloc::vec![b_lan], 4);
        assert_eq!(
            b_steps[1],
            Step::Punch {
                peer: a_pub,
                alternates: alloc::vec![a_lan]
            },
            "B punches A's public address and A's host candidate"
        );
        let a_steps = deliver(&b_steps, &mut a, 5);
        assert_eq!(
            a_steps,
            [Step::Punch {
                peer: b_pub,
                alternates: alloc::vec![b_lan]
            }],
            "A punches B's public address and B's host candidate"
        );

        // Different public IPs: a host candidate is never punched.
        let other: SocketAddr = "192.0.2.9:6000".parse().unwrap();
        assert!(same_nat_alternates(a_pub, other, &[b_lan]).is_empty());
    }

    /// +ciris: a peer's global IPv6 address is punched across NATs too; its
    /// link-local or unique-local ones never are.
    #[test]
    fn a_global_ipv6_candidate_is_punched_whatever_the_public_ipv4() {
        let a_pub: SocketAddr = "198.51.100.1:5000".parse().unwrap();
        let b_pub: SocketAddr = "203.0.113.7:6000".parse().unwrap();
        let global: SocketAddr = "[2001:db8:1::5]:6000".parse().unwrap();
        let link_local: SocketAddr = "[fe80::1]:6000".parse().unwrap();
        let ula: SocketAddr = "[fd00::5]:6000".parse().unwrap();
        let lan: SocketAddr = "192.168.1.5:6000".parse().unwrap();
        assert_eq!(
            same_nat_alternates(a_pub, b_pub, &[lan, global, link_local, ula]),
            alloc::vec![global]
        );
    }

    #[test]
    fn reject_ends_the_initiator_in_either_waiting_phase() {
        for accept_first in [false, true] {
            let (mut a, _) = Session::initiate(ID, TOKEN, t(), ProbeProtocol::Stun, 0);
            a.probed(a_pub(), 1);
            if accept_first {
                a.signal(&Signal::Accept { session: ID }, 2);
            }
            let steps = a.signal(
                &Signal::Reject {
                    session: ID,
                    reason: REJECT_POLICY,
                },
                3,
            );
            assert_eq!(steps, [Step::Failed(Failure::Rejected(REJECT_POLICY))]);
        }
    }

    #[test]
    fn a_responder_whose_probe_fails_rejects() {
        let (mut b, _) =
            Session::respond(ID, TOKEN, t(), a_pub(), Vec::new(), ProbeProtocol::Rnsp, 0);
        assert_eq!(
            b.probe_failed(),
            [
                Step::Send(Signal::Reject {
                    session: ID,
                    reason: REJECT_UNSUPPORTED
                }),
                Step::Failed(Failure::ProbeFailed)
            ]
        );
    }

    #[test]
    fn signals_for_another_session_or_phase_are_ignored() {
        let (mut a, _) = Session::initiate(ID, TOKEN, t(), ProbeProtocol::Rnsp, 0);
        a.probed(a_pub(), 1);
        let other = [0x43; 16];
        assert!(a.signal(&Signal::Accept { session: other }, 2).is_empty());
        assert_eq!(a.phase(), Phase::Proposing);
        // READY before ACCEPT does not jump the phase.
        let ready = Signal::Ready {
            session: ID,
            responder_public: b_pub(),
            responder_local: Vec::new(),
            commit: false,
        };
        assert!(a.signal(&ready, 2).is_empty());
        assert_eq!(a.phase(), Phase::Proposing);
        // A responder takes no signal at all after the REQUEST.
        let (mut b, _) =
            Session::respond(ID, TOKEN, t(), a_pub(), Vec::new(), ProbeProtocol::Rnsp, 0);
        let busy = Signal::Reject {
            session: ID,
            reason: REJECT_BUSY,
        };
        assert!(b.signal(&busy, 1).is_empty());
    }

    #[test]
    fn each_waiting_phase_times_out_with_its_own_cause() {
        let (mut a, _) = Session::initiate(ID, TOKEN, t(), ProbeProtocol::Rnsp, 1_000);
        assert!(a.poll(1_000 + DISCOVER_TIMEOUT_MS - 1).is_empty());
        assert_eq!(
            a.poll(1_000 + DISCOVER_TIMEOUT_MS),
            [Step::Failed(Failure::ProbeFailed)]
        );

        let (mut a, _) = Session::initiate(ID, TOKEN, t(), ProbeProtocol::Rnsp, 0);
        a.probed(a_pub(), 5);
        assert_eq!(a.deadline_ms(), Some(5 + PROPOSE_TIMEOUT_MS));
        assert_eq!(
            a.poll(5 + PROPOSE_TIMEOUT_MS),
            [Step::Failed(Failure::Timeout)]
        );

        let (mut b, _) =
            Session::respond(ID, TOKEN, t(), a_pub(), Vec::new(), ProbeProtocol::Rnsp, 0);
        b.probed(b_pub(), 7);
        assert_eq!(
            b.poll(7 + PUNCH_TIMEOUT_MS),
            [Step::Failed(Failure::PunchFailed)]
        );
    }

    #[test]
    fn a_failed_punch_fails_the_session() {
        let (mut b, _) =
            Session::respond(ID, TOKEN, t(), a_pub(), Vec::new(), ProbeProtocol::Rnsp, 0);
        b.probed(b_pub(), 1);
        assert_eq!(b.punched(false, 2), [Step::Failed(Failure::PunchFailed)]);
    }
}
