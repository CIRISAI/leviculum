//! Node-level bookkeeping for direct-link upgrades (+ciris, leviculum#70).
//!
//! The protocol and one attempt's state machine live in
//! [`crate::direct_link`]; this file is where a session meets the node: the
//! policy that answers a peer's REQUEST, the signals intercepted off the
//! link's channel, the jobs handed to the driver (which owns every socket),
//! and the move of the link onto the punched interface and back off it.
//!
//! The driver's half of the contract:
//!
//! 1. After any call into the node, drain [`NodeCore::take_direct_link_jobs`].
//! 2. `Probe`: bind a UDP socket, keep it under the session id, learn its
//!    reflexive address, and report through [`NodeCore::direct_link_probed`].
//! 3. `Punch`: punch from that same socket. On success bring the socket up as
//!    an interface, register it with the node like any spawned interface,
//!    then report its index through [`NodeCore::direct_link_punched`]; on
//!    failure report `None`.
//! 4. `Release`: drop whatever the driver holds for the session.
//!    `CloseInterface`: tear the direct interface down (its link is gone).

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::net::SocketAddr;

use rand_core::CryptoRngCore;

use crate::constants::{MS_PER_SECOND, TRUNCATED_HASHBYTES};
use crate::direct_link::wire::{self, REJECT_BUSY, REJECT_POLICY};
use crate::direct_link::{Failure, ProbeProtocol, Role, Session, SessionId, Signal, Step};
use crate::hex_fmt::HexShort;
use crate::link::{LinkId, LinkState};
use crate::packet::PacketContext;
use crate::traits::{Clock, Storage};

use super::event::NodeEvent;
use super::NodeCore;

/// Minimum spacing between two proposals on the same link, so a caller
/// retrying in a loop cannot keep a peer probing its facilitator.
pub const PROPOSAL_COOLDOWN_MS: u64 = 60_000;

/// Which upgrade REQUESTs this node answers with ACCEPT.
///
/// Accepting reveals this node's public address to the peer, so the default
/// is to refuse: a node opts in by configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirectLinkPolicy {
    /// Refuse every request (REJECT with reason POLICY).
    #[default]
    Reject,
    /// Accept from any link peer.
    AcceptAll,
    /// Accept only when the peer's identity is known: it identified itself on
    /// the link (LINKIDENTIFY), or this node initiated the link and so knows
    /// whose destination answered.
    IdentifiedOnly,
}

/// Direct-link settings. The default refuses requests and cannot propose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DirectLinkConfig {
    /// How to answer a peer's REQUEST.
    pub policy: DirectLinkPolicy,
    /// Where this node probes when it proposes. `None` means this node cannot
    /// propose; it can still accept, probing the facilitator the peer names.
    pub facilitator: Option<SocketAddr>,
    /// How to probe `facilitator`.
    pub protocol: ProbeProtocol,
}

/// Why [`NodeCore::propose_direct_link`] did not start an upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectLinkError {
    /// No such link, or it is not active.
    NoActiveLink,
    /// No facilitator is configured.
    NoFacilitator,
    /// An upgrade is already running on this link, or it is already direct.
    AlreadyUpgrading,
    /// The last proposal on this link was under [`PROPOSAL_COOLDOWN_MS`] ago.
    CoolingDown,
    /// The signal could not be queued on the link's channel (window full).
    ChannelBusy,
}

impl core::fmt::Display for DirectLinkError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            DirectLinkError::NoActiveLink => "no such active link",
            DirectLinkError::NoFacilitator => "no facilitator configured",
            DirectLinkError::AlreadyUpgrading => "the link is already upgrading or direct",
            DirectLinkError::CoolingDown => "the last proposal on this link was too recent",
            DirectLinkError::ChannelBusy => "the link's channel is busy",
        })
    }
}

/// Work the driver does for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectLinkJob {
    /// Bind a UDP socket for `session` and learn its reflexive address.
    Probe {
        session: SessionId,
        server: SocketAddr,
        protocol: ProbeProtocol,
    },
    /// Punch from the session's socket toward `peer`.
    Punch {
        session: SessionId,
        peer: SocketAddr,
        token: [u8; 32],
    },
    /// The session is over: drop its socket and any task still running.
    Release { session: SessionId },
    /// The link a direct interface served is gone: tear the interface down.
    CloseInterface { interface_index: usize },
}

/// A link moved onto a direct interface, and where it was before.
#[derive(Debug, Clone, Copy)]
struct Attachment {
    link_id: LinkId,
    previous: Option<usize>,
}

/// The node's direct-link state.
#[derive(Debug, Default)]
pub(crate) struct DirectLinks {
    config: DirectLinkConfig,
    /// Running sessions, by wire link id. At most one per link.
    sessions: BTreeMap<LinkId, Session>,
    /// When each link last proposed, for the cooldown.
    last_proposal_ms: BTreeMap<LinkId, u64>,
    /// Established direct interfaces, by interface index.
    attached: BTreeMap<usize, Attachment>,
    jobs: VecDeque<DirectLinkJob>,
}

impl DirectLinks {
    fn link_of_session(&self, session: &SessionId) -> Option<LinkId> {
        self.sessions
            .iter()
            .find(|(_, s)| s.id() == session)
            .map(|(link_id, _)| *link_id)
    }

    fn interface_of_link(&self, link_id: &LinkId) -> Option<usize> {
        self.attached
            .iter()
            .find(|(_, a)| &a.link_id == link_id)
            .map(|(idx, _)| *idx)
    }
}

impl<R: CryptoRngCore, C: Clock, S: Storage> NodeCore<R, C, S> {
    /// Replace the direct-link settings. Running sessions keep going.
    pub fn set_direct_link_config(&mut self, config: DirectLinkConfig) {
        self.direct_links.config = config;
    }

    /// The direct-link settings.
    pub fn direct_link_config(&self) -> DirectLinkConfig {
        self.direct_links.config
    }

    /// The direct interface a link has been moved onto, if any.
    pub fn direct_link_interface(&self, link_id: &LinkId) -> Option<usize> {
        let link_id = self.resolve_link_id(link_id);
        self.direct_links.interface_of_link(&link_id)
    }

    /// Start upgrading `link_id` to a direct path.
    ///
    /// Only call this for a peer known to implement the upgrade (another
    /// leviculum node or an rns-rs node): a Python RNS peer cannot skip the
    /// REQUEST, and its channel stalls behind it (see [`crate::direct_link`]).
    ///
    /// The outcome arrives later as [`NodeEvent::DirectLinkEstablished`] or
    /// [`NodeEvent::DirectLinkFailed`].
    pub fn propose_direct_link(
        &mut self,
        link_id: &LinkId,
    ) -> Result<crate::transport::TickOutput, DirectLinkError> {
        let link_id = self.resolve_link_id(link_id);
        let now_ms = self.transport.clock().now_ms();
        let facilitator = self
            .direct_links
            .config
            .facilitator
            .ok_or(DirectLinkError::NoFacilitator)?;
        let link_key = match self.links.get(&link_id) {
            Some(link) if link.state() == LinkState::Active => {
                *link.link_key().ok_or(DirectLinkError::NoActiveLink)?
            }
            _ => return Err(DirectLinkError::NoActiveLink),
        };
        if self.direct_links.sessions.contains_key(&link_id)
            || self.direct_links.interface_of_link(&link_id).is_some()
        {
            return Err(DirectLinkError::AlreadyUpgrading);
        }
        if let Some(last) = self.direct_links.last_proposal_ms.get(&link_id) {
            if now_ms.saturating_sub(*last) < PROPOSAL_COOLDOWN_MS {
                return Err(DirectLinkError::CoolingDown);
            }
        }

        let mut session_id = [0u8; 16];
        self.rng.fill_bytes(&mut session_id);
        let token = wire::punch_token(&link_key, &session_id);
        let (session, steps) = Session::initiate(
            session_id,
            token,
            facilitator,
            self.direct_links.config.protocol,
            now_ms,
        );
        crate::tracing::info!(
            link = %HexShort(link_id.as_bytes()),
            session = %HexShort(&session_id),
            %facilitator,
            "direct link: proposing"
        );
        self.direct_links.last_proposal_ms.insert(link_id, now_ms);
        self.direct_links.sessions.insert(link_id, session);
        self.apply_direct_link_steps(link_id, steps, now_ms);
        Ok(self.process_events_and_actions())
    }

    /// Take the driver's pending work, oldest first.
    pub fn take_direct_link_jobs(&mut self) -> Vec<DirectLinkJob> {
        self.direct_links.jobs.drain(..).collect()
    }

    /// The driver's probe for `session` finished: the reflexive address, or
    /// `None` if the facilitator never answered.
    pub fn direct_link_probed(
        &mut self,
        session: &SessionId,
        public: Option<SocketAddr>,
    ) -> crate::transport::TickOutput {
        let now_ms = self.transport.clock().now_ms();
        if let Some(link_id) = self.direct_links.link_of_session(session) {
            let steps = match self.direct_links.sessions.get_mut(&link_id) {
                Some(s) => match public {
                    Some(addr) => s.probed(addr, now_ms),
                    None => s.probe_failed(),
                },
                None => Vec::new(),
            };
            self.apply_direct_link_steps(link_id, steps, now_ms);
        } else {
            // The session ended while the probe ran; its socket is orphaned.
            self.direct_links
                .jobs
                .push_back(DirectLinkJob::Release { session: *session });
        }
        self.process_events_and_actions()
    }

    /// The driver's punch for `session` finished. `Some(index)` is the direct
    /// interface the driver brought up and registered; `None` means the punch
    /// failed.
    ///
    /// If the session ended in the meantime (its link closed), an interface
    /// brought up for it is handed straight back as a `CloseInterface` job.
    pub fn direct_link_punched(
        &mut self,
        session: &SessionId,
        interface_index: Option<usize>,
    ) -> crate::transport::TickOutput {
        let now_ms = self.transport.clock().now_ms();
        match self.direct_links.link_of_session(session) {
            Some(link_id) => {
                let steps = match self.direct_links.sessions.get_mut(&link_id) {
                    Some(s) => s.punched(interface_index.is_some(), now_ms),
                    None => Vec::new(),
                };
                if steps.contains(&Step::Established) {
                    if let Some(index) = interface_index {
                        let proposed = self
                            .direct_links
                            .sessions
                            .get(&link_id)
                            .is_some_and(|s| s.role() == Role::Initiator);
                        self.attach_direct_link(link_id, index, proposed, now_ms);
                    }
                } else if let Some(index) = interface_index {
                    // The session was no longer punching, so nothing can use
                    // the interface.
                    self.direct_links
                        .jobs
                        .push_back(DirectLinkJob::CloseInterface {
                            interface_index: index,
                        });
                }
                self.apply_direct_link_steps(link_id, steps, now_ms);
            }
            None => {
                if let Some(index) = interface_index {
                    self.direct_links
                        .jobs
                        .push_back(DirectLinkJob::CloseInterface {
                            interface_index: index,
                        });
                }
                self.direct_links
                    .jobs
                    .push_back(DirectLinkJob::Release { session: *session });
            }
        }
        self.process_events_and_actions()
    }

    /// Move the link onto its new interface and say so.
    fn attach_direct_link(&mut self, link_id: LinkId, index: usize, proposed: bool, now_ms: u64) {
        let Some(link) = self.links.get_mut(&link_id) else {
            self.direct_links
                .jobs
                .push_back(DirectLinkJob::CloseInterface {
                    interface_index: index,
                });
            return;
        };
        let previous = link.attached_interface();
        link.set_attached_interface(index);
        // The direct path just proved itself both ways; do not let a stale
        // timer that was running on the relayed path fire against it.
        link.record_inbound(now_ms / MS_PER_SECOND);
        let is_initiator = link.is_initiator();
        let destination = *link.destination_hash();
        self.direct_links
            .attached
            .insert(index, Attachment { link_id, previous });
        crate::tracing::info!(
            link = %HexShort(link_id.as_bytes()),
            interface = index,
            previous = ?previous,
            "direct link: established, link moved onto the direct interface"
        );
        self.events.push(NodeEvent::DirectLinkEstablished {
            link_id,
            interface_index: index,
            proposed,
        });
        // The initiator linked to a destination it can name: ask for it over
        // the new interface, so the owner's announce answer installs a
        // one-hop path and fresh traffic to it (not just this link) goes
        // direct. Best effort; the responder side learns the initiator's
        // destinations from whatever the initiator announces.
        if is_initiator {
            let mut tag = [0u8; TRUNCATED_HASHBYTES];
            tag[..8].copy_from_slice(&now_ms.to_be_bytes());
            tag[8..].copy_from_slice(&destination.as_bytes()[..8]);
            if let Err(e) = self
                .transport
                .request_path(destination.as_bytes(), Some(index), &tag)
            {
                crate::tracing::debug!(%e, "direct link: path request failed (best-effort)");
            }
        }
    }

    /// A channel message in the upgrade's MSGTYPE range arrived on `link_id`.
    pub(super) fn on_direct_link_signal(
        &mut self,
        link_id: LinkId,
        msgtype: u16,
        data: &[u8],
        now_ms: u64,
    ) {
        let signal = match Signal::decode(msgtype, data) {
            Ok(signal) => signal,
            Err(e) => {
                crate::tracing::debug!(
                    link = %HexShort(link_id.as_bytes()),
                    msgtype,
                    ?e,
                    "direct link: undecodable signal dropped"
                );
                return;
            }
        };

        let Signal::Request {
            session,
            facilitator,
            initiator_public,
            protocol,
        } = signal
        else {
            let steps = match self.direct_links.sessions.get_mut(&link_id) {
                Some(s) => s.signal(&signal, now_ms),
                None => Vec::new(),
            };
            self.apply_direct_link_steps(link_id, steps, now_ms);
            return;
        };

        let Some(link) = self.links.get(&link_id) else {
            return;
        };
        let identified = link.is_initiator() || link.remote_identity().is_some();
        let link_key = link.link_key().copied();
        let allowed = match self.direct_links.config.policy {
            DirectLinkPolicy::Reject => false,
            DirectLinkPolicy::AcceptAll => true,
            DirectLinkPolicy::IdentifiedOnly => identified,
        };
        let busy = self.direct_links.sessions.contains_key(&link_id)
            || self.direct_links.interface_of_link(&link_id).is_some();
        let reject = if !allowed {
            Some(REJECT_POLICY)
        } else if busy {
            Some(REJECT_BUSY)
        } else {
            None
        };
        let (Some(link_key), None) = (link_key, reject) else {
            crate::tracing::info!(
                link = %HexShort(link_id.as_bytes()),
                reason = reject.unwrap_or(REJECT_POLICY),
                "direct link: refusing a peer's proposal"
            );
            self.send_direct_link_signal(
                &link_id,
                &Signal::Reject {
                    session,
                    reason: reject.unwrap_or(REJECT_POLICY),
                },
                now_ms,
            );
            return;
        };

        crate::tracing::info!(
            link = %HexShort(link_id.as_bytes()),
            session = %HexShort(&session),
            %facilitator,
            peer = %initiator_public,
            "direct link: accepting a peer's proposal"
        );
        let token = wire::punch_token(&link_key, &session);
        let (session, steps) = Session::respond(
            session,
            token,
            facilitator,
            initiator_public,
            protocol,
            now_ms,
        );
        self.direct_links.sessions.insert(link_id, session);
        self.apply_direct_link_steps(link_id, steps, now_ms);
    }

    /// Carry out what a session asked for.
    fn apply_direct_link_steps(&mut self, link_id: LinkId, steps: Vec<Step>, now_ms: u64) {
        for step in steps {
            let Some(session) = self.direct_links.sessions.get(&link_id) else {
                return;
            };
            let session_id = *session.id();
            match step {
                Step::Send(signal) => {
                    if !self.send_direct_link_signal(&link_id, &signal, now_ms) {
                        self.end_direct_link_session(link_id, Some(Failure::Timeout));
                        return;
                    }
                }
                Step::Probe { server, protocol } => {
                    self.direct_links.jobs.push_back(DirectLinkJob::Probe {
                        session: session_id,
                        server,
                        protocol,
                    });
                }
                Step::Punch { peer } => {
                    let token = *session.token();
                    self.direct_links.jobs.push_back(DirectLinkJob::Punch {
                        session: session_id,
                        peer,
                        token,
                    });
                }
                // The attach already happened in `direct_link_punched`; the
                // session has done its job.
                Step::Established => self.end_direct_link_session(link_id, None),
                Step::Failed(failure) => {
                    self.end_direct_link_session(link_id, Some(failure));
                    return;
                }
            }
        }
    }

    /// Drop a session; report it if it failed.
    fn end_direct_link_session(&mut self, link_id: LinkId, failure: Option<Failure>) {
        let Some(session) = self.direct_links.sessions.remove(&link_id) else {
            return;
        };
        // After success the driver's socket now belongs to the interface,
        // so only a failed session has anything to release.
        if let Some(failure) = failure {
            crate::tracing::info!(
                link = %HexShort(link_id.as_bytes()),
                session = %HexShort(session.id()),
                role = ?session.role(),
                ?failure,
                "direct link: upgrade failed, link stays on its current path"
            );
            self.direct_links.jobs.push_back(DirectLinkJob::Release {
                session: *session.id(),
            });
            self.events.push(NodeEvent::DirectLinkFailed {
                link_id,
                failure,
                proposed: session.role() == Role::Initiator,
            });
        }
    }

    /// Queue `signal` on the link's channel. False if it could not be.
    fn send_direct_link_signal(&mut self, link_id: &LinkId, signal: &Signal, now_ms: u64) -> bool {
        let Some(link) = self.links.get_mut(link_id) else {
            return false;
        };
        if link.state() != LinkState::Active {
            return false;
        }
        let link_mdu = link.mdu();
        let rtt_ms = link.rtt_ms();
        let envelope = match link.ensure_channel(rtt_ms).send_system_raw(
            signal.msgtype(),
            &signal.encode(),
            link_mdu,
            now_ms,
            rtt_ms,
        ) {
            Ok(envelope) => envelope,
            Err(e) => {
                crate::tracing::debug!(?e, "direct link: channel refused the signal");
                return false;
            }
        };
        let packet = match link.build_data_packet_with_context(
            &envelope,
            PacketContext::Channel,
            &mut self.rng,
        ) {
            Ok(packet) => packet,
            Err(_) => return false,
        };
        // Same receipt as an application channel message, so the channel's
        // own retransmission covers a lost signal.
        if let Some(seq) = link.channel().map(|ch| ch.last_sent_sequence()) {
            self.receipt_tracker
                .register(&packet, *link_id, seq, now_ms);
        }
        self.route_link_packet(link_id, &packet);
        true
    }

    /// Time out sessions whose peer or facilitator went quiet.
    pub(super) fn check_direct_link_timeouts(&mut self, now_ms: u64) {
        let due: Vec<(LinkId, Vec<Step>)> = self
            .direct_links
            .sessions
            .iter_mut()
            .map(|(link_id, s)| (*link_id, s.poll(now_ms)))
            .filter(|(_, steps)| !steps.is_empty())
            .collect();
        for (link_id, steps) in due {
            self.apply_direct_link_steps(link_id, steps, now_ms);
        }
    }

    /// The earliest session deadline.
    pub(super) fn direct_link_next_deadline(&self) -> Option<u64> {
        self.direct_links
            .sessions
            .values()
            .filter_map(Session::deadline_ms)
            .min()
    }

    /// The link is being removed: end its session and retire its interface.
    pub(super) fn direct_link_link_removed(&mut self, link_id: &LinkId) {
        if let Some(session) = self.direct_links.sessions.remove(link_id) {
            self.direct_links.jobs.push_back(DirectLinkJob::Release {
                session: *session.id(),
            });
        }
        self.direct_links.last_proposal_ms.remove(link_id);
        if let Some(index) = self.direct_links.interface_of_link(link_id) {
            self.direct_links.attached.remove(&index);
            self.direct_links
                .jobs
                .push_back(DirectLinkJob::CloseInterface {
                    interface_index: index,
                });
        }
    }

    /// An interface went down. If it was a direct one, put its link back on
    /// the interface it used before, which is the relayed path the upgrade
    /// started from.
    pub(super) fn direct_link_interface_down(&mut self, index: usize) {
        let Some(attachment) = self.direct_links.attached.remove(&index) else {
            return;
        };
        let Some(link) = self.links.get_mut(&attachment.link_id) else {
            return;
        };
        if let Some(previous) = attachment.previous {
            link.set_attached_interface(previous);
        }
        crate::tracing::info!(
            link = %HexShort(attachment.link_id.as_bytes()),
            interface = index,
            restored = ?attachment.previous,
            "direct link: direct interface lost, link back on its previous path"
        );
        self.events.push(NodeEvent::DirectLinkLost {
            link_id: attachment.link_id,
            interface_index: index,
        });
    }
}
