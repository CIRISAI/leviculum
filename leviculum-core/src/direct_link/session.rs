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

use super::wire::{ProbeProtocol, SessionId, Signal, REJECT_UNSUPPORTED};

/// How long each waiting phase may last before the session gives up.
///
/// Punching gets the driver's own punch window (10 s) plus slack, so the
/// driver's verdict normally arrives before this timer fires.
pub const DISCOVER_TIMEOUT_MS: u64 = 10_000;
pub const PROPOSE_TIMEOUT_MS: u64 = 10_000;
pub const READY_TIMEOUT_MS: u64 = 10_000;
pub const PUNCH_TIMEOUT_MS: u64 = 12_000;

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
    /// Punch from the probe's socket toward `peer`.
    Punch { peer: SocketAddr },
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
        if self.phase != Phase::Discovering {
            return Vec::new();
        }
        match self.role {
            Role::Initiator => {
                self.enter(Phase::Proposing, now_ms);
                alloc::vec![Step::Send(Signal::Request {
                    session: self.id,
                    facilitator: self.facilitator,
                    initiator_public: public,
                    protocol: self.protocol,
                })]
            }
            Role::Responder => {
                // Constructed with the initiator's address, so it is there.
                let Some(peer) = self.peer_public else {
                    return alloc::vec![Step::Failed(Failure::Timeout)];
                };
                self.enter(Phase::Punching, now_ms);
                alloc::vec![
                    Step::Send(Signal::Ready {
                        session: self.id,
                        responder_public: public,
                    }),
                    Step::Punch { peer },
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
                    responder_public, ..
                },
            ) => {
                self.peer_public = Some(*responder_public);
                self.enter(Phase::Punching, now_ms);
                alloc::vec![Step::Punch {
                    peer: *responder_public,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// The driver's verdict on the punch.
    pub fn punched(&mut self, succeeded: bool, now_ms: u64) -> Vec<Step> {
        if self.phase != Phase::Punching {
            return Vec::new();
        }
        if succeeded {
            self.enter(Phase::Connected, now_ms);
            alloc::vec![Step::Established]
        } else {
            alloc::vec![Step::Failed(Failure::PunchFailed)]
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
        } = request_from(&steps).clone()
        else {
            unreachable!()
        };
        assert_eq!(initiator_public, a_pub());

        let (mut b, b_steps) =
            Session::respond(session, TOKEN, facilitator, initiator_public, protocol, 150);
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
        assert_eq!(b_steps[1], Step::Punch { peer: a_pub() });

        let a_steps = deliver(&b_steps, &mut a, 210);
        assert_eq!(a_steps, [Step::Punch { peer: b_pub() }]);
        assert_eq!(a.peer_public(), Some(b_pub()));

        assert_eq!(a.punched(true, 300), [Step::Established]);
        assert_eq!(b.punched(true, 300), [Step::Established]);
        assert_eq!(a.deadline_ms(), None);
        assert!(
            a.poll(u64::MAX).is_empty(),
            "a connected session never times out"
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
        let (mut b, _) = Session::respond(ID, TOKEN, t(), a_pub(), ProbeProtocol::Rnsp, 0);
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
        };
        assert!(a.signal(&ready, 2).is_empty());
        assert_eq!(a.phase(), Phase::Proposing);
        // A responder takes no signal at all after the REQUEST.
        let (mut b, _) = Session::respond(ID, TOKEN, t(), a_pub(), ProbeProtocol::Rnsp, 0);
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

        let (mut b, _) = Session::respond(ID, TOKEN, t(), a_pub(), ProbeProtocol::Rnsp, 0);
        b.probed(b_pub(), 7);
        assert_eq!(
            b.poll(7 + PUNCH_TIMEOUT_MS),
            [Step::Failed(Failure::PunchFailed)]
        );
    }

    #[test]
    fn a_failed_punch_fails_the_session() {
        let (mut b, _) = Session::respond(ID, TOKEN, t(), a_pub(), ProbeProtocol::Rnsp, 0);
        b.probed(b_pub(), 1);
        assert_eq!(b.punched(false, 2), [Step::Failed(Failure::PunchFailed)]);
    }
}
