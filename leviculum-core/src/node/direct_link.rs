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
use crate::link::channel::ChannelError;
use crate::link::{LinkId, LinkState};
use crate::packet::PacketContext;
use crate::traits::{Clock, Storage};

use super::event::NodeEvent;
use super::NodeCore;

/// Minimum spacing between two proposals on the same link, so a caller
/// retrying in a loop cannot keep a peer probing its facilitator.
pub const PROPOSAL_COOLDOWN_MS: u64 = 60_000;

/// After a link moves onto its direct interface, link packets still in
/// flight on the relayed path keep arriving for a while. Only past this grace
/// does such an arrival mean the peer has gone back to the relay.
pub const FALLBACK_GRACE_MS: u64 = 10_000;

/// Signals waiting on a link's channel window, at most. An upgrade needs two
/// or three; anything beyond that is a peer misbehaving.
const OUTBOX_MAX: usize = 8;

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

/// What became of one attempt to put a signal on a channel.
enum SignalSend {
    Sent,
    /// Backpressure: the window is full or the channel is pacing.
    NotYet,
    /// It will never go.
    Never,
}

/// A link moved onto a direct interface, and where it was before.
#[derive(Debug, Clone, Copy)]
struct Attachment {
    link_id: LinkId,
    /// The upgrade session that put the link here; named in the fallback
    /// nudge.
    session: SessionId,
    previous: Option<usize>,
    /// When the link moved onto the direct interface.
    attached_at_ms: u64,
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
    /// Signals the channel could not take yet (window full or pacing), per
    /// link, in order. Flushed on every tick.
    outbox: BTreeMap<LinkId, VecDeque<Signal>>,
    /// The interface the link packet being processed arrived on.
    rx_iface: Option<usize>,
    /// Punched links waiting on the relay for a resource transfer to finish
    /// before they move: (session, direct interface, proposed).
    deferred: BTreeMap<LinkId, (SessionId, usize, bool)>,
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

    #[cfg(test)]
    pub(crate) fn direct_link_outbox_len(&self) -> usize {
        self.direct_links.outbox.values().map(VecDeque::len).sum()
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
                        if self.resources_in_flight(&link_id) {
                            // A transfer cut for the link's present MTU may
                            // carry parts no UDP datagram can (Codex review
                            // on #74): the link moves once it is done.
                            self.direct_links
                                .deferred
                                .insert(link_id, (*session, index, proposed));
                        } else {
                            self.attach_direct_link(link_id, *session, index, proposed, now_ms);
                        }
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
    fn attach_direct_link(
        &mut self,
        link_id: LinkId,
        session: SessionId,
        index: usize,
        proposed: bool,
        now_ms: u64,
    ) {
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
        // A link that came up over TCP may have negotiated far more than a
        // UDP datagram on the open internet carries. Both ends lower it to
        // the same ceiling here, as rns-rs does, so they keep agreeing on the
        // link MDU (leviculum#70 review). Never raised.
        link.lower_mtu(crate::direct_link::DIRECT_LINK_MTU);
        // The direct path just proved itself both ways; do not let a stale
        // timer that was running on the relayed path fire against it.
        link.record_inbound(now_ms / MS_PER_SECOND);
        // A punch both ways is the peer answering: a link that went stale on
        // the relay while the punch ran is live again, as any authenticated
        // inbound traffic would make it (Codex review on #74).
        let recovered = link.state() == LinkState::Stale;
        if recovered {
            link.set_state(LinkState::Active);
        }
        let is_initiator = link.is_initiator();
        let destination = *link.destination_hash();
        self.direct_links.attached.insert(
            index,
            Attachment {
                link_id,
                session,
                previous,
                attached_at_ms: now_ms,
            },
        );
        crate::tracing::info!(
            link = %HexShort(link_id.as_bytes()),
            interface = index,
            previous = ?previous,
            "direct link: established, link moved onto the direct interface"
        );
        if recovered {
            self.events.push(NodeEvent::LinkRecovered { link_id });
        }
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
        // A session id is the driver's key for its socket and tasks, so one
        // already in use anywhere on the node is refused, not just on this
        // link: a peer must not be able to reuse another link's id and take
        // over its probe (Codex review on #74).
        let busy = self.direct_links.sessions.contains_key(&link_id)
            || self.direct_links.interface_of_link(&link_id).is_some()
            || self
                .direct_links
                .sessions
                .values()
                .any(|s| s.id() == &session);
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
        // Its signals still waiting on the channel would outlive it: a
        // REQUEST sent after the initiator gave up starts the peer on a
        // session nobody holds (Codex review on #74).
        if let Some(queue) = self.direct_links.outbox.get_mut(&link_id) {
            // A REJECT is the session's last word, not stale initiation: it
            // stays, so the peer hears the failure instead of timing out.
            queue.retain(|signal| {
                signal.session() != session.id() || matches!(signal, Signal::Reject { .. })
            });
            if queue.is_empty() {
                self.direct_links.outbox.remove(&link_id);
            }
        }
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

    /// Send `signal` on the link's channel, or hold it until the channel can
    /// take it. False only when it can never go (no active link, a full
    /// outbox, or a hard channel error).
    ///
    /// A full send window or channel pacing is ordinary backpressure on a
    /// busy link, not a reason to give the upgrade up: the signal waits in
    /// the link's outbox behind any already waiting there, and the session's
    /// own phase timer bounds how long that may take.
    pub(super) fn send_direct_link_signal(
        &mut self,
        link_id: &LinkId,
        signal: &Signal,
        now_ms: u64,
    ) -> bool {
        if self
            .direct_links
            .outbox
            .get(link_id)
            .is_some_and(|q| !q.is_empty())
        {
            return self.hold_direct_link_signal(link_id, signal.clone());
        }
        match self.try_send_direct_link_signal(link_id, signal, now_ms) {
            SignalSend::Sent => true,
            SignalSend::NotYet => self.hold_direct_link_signal(link_id, signal.clone()),
            SignalSend::Never => false,
        }
    }

    fn hold_direct_link_signal(&mut self, link_id: &LinkId, signal: Signal) -> bool {
        let queue = self.direct_links.outbox.entry(*link_id).or_default();
        if queue.len() >= OUTBOX_MAX {
            return false;
        }
        queue.push_back(signal);
        true
    }

    /// Send what each link's outbox holds, in order, until a channel pushes
    /// back again.
    fn flush_direct_link_outbox(&mut self, now_ms: u64) {
        let links: Vec<LinkId> = self.direct_links.outbox.keys().copied().collect();
        for link_id in links {
            while let Some(signal) = self
                .direct_links
                .outbox
                .get(&link_id)
                .and_then(|q| q.front().cloned())
            {
                match self.try_send_direct_link_signal(&link_id, &signal, now_ms) {
                    SignalSend::Sent => {
                        if let Some(q) = self.direct_links.outbox.get_mut(&link_id) {
                            q.pop_front();
                        }
                    }
                    SignalSend::NotYet => break,
                    SignalSend::Never => {
                        self.direct_links.outbox.remove(&link_id);
                        self.end_direct_link_session(link_id, Some(Failure::Timeout));
                        break;
                    }
                }
            }
            if self
                .direct_links
                .outbox
                .get(&link_id)
                .is_some_and(VecDeque::is_empty)
            {
                self.direct_links.outbox.remove(&link_id);
            }
        }
    }

    fn try_send_direct_link_signal(
        &mut self,
        link_id: &LinkId,
        signal: &Signal,
        now_ms: u64,
    ) -> SignalSend {
        let Some(link) = self.links.get_mut(link_id) else {
            return SignalSend::Never;
        };
        // Stale too: the fallback nudge goes out on a link that went stale
        // on its direct path.
        if !matches!(link.state(), LinkState::Active | LinkState::Stale) {
            return SignalSend::Never;
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
            Err(ChannelError::Busy | ChannelError::PacingDelay { .. }) => {
                return SignalSend::NotYet;
            }
            Err(e) => {
                crate::tracing::debug!(?e, "direct link: channel refused the signal");
                return SignalSend::Never;
            }
        };
        let packet = match link.build_data_packet_with_context(
            &envelope,
            PacketContext::Channel,
            &mut self.rng,
        ) {
            Ok(packet) => packet,
            Err(_) => return SignalSend::Never,
        };
        // Same receipt as an application channel message, so the channel's
        // own retransmission covers a lost signal.
        if let Some(seq) = link.channel().map(|ch| ch.last_sent_sequence()) {
            self.receipt_tracker
                .register(&packet, *link_id, seq, now_ms);
        }
        self.route_link_packet(link_id, &packet);
        SignalSend::Sent
    }

    /// Time out sessions whose peer or facilitator went quiet.
    pub(super) fn check_direct_link_timeouts(&mut self, now_ms: u64) {
        self.flush_direct_link_outbox(now_ms);
        let ready: Vec<LinkId> = self
            .direct_links
            .deferred
            .keys()
            .copied()
            .filter(|link_id| !self.resources_in_flight(link_id))
            .collect();
        for link_id in ready {
            if let Some((session, index, proposed)) = self.direct_links.deferred.remove(&link_id) {
                self.attach_direct_link(link_id, session, index, proposed, now_ms);
            }
        }
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

    /// Whether the link carries anything sized for its present MTU that the
    /// direct path could not: a resource transfer under way either way, or a
    /// channel message still awaiting its proof that is larger than the
    /// channel can send at `DIRECT_LINK_MTU`.
    fn resources_in_flight(&self, link_id: &LinkId) -> bool {
        self.links.get(link_id).is_some_and(|link| {
            link.outgoing_resource().is_some()
                || link.incoming_resource().is_some()
                || link.channel().is_some_and(|ch| {
                    ch.outstanding_exceeds_mtu(crate::direct_link::DIRECT_LINK_MTU)
                })
        }) || self.assembling_resources.contains_key(link_id)
    }

    /// Heap held by the direct-link bookkeeping (leviculum#77's census).
    pub(super) fn direct_link_heap_bytes(&self) -> usize {
        use crate::heap_census as hc;
        let d = &self.direct_links;
        // A signal is fixed-size: its queue's buffer is all it costs.
        let outbox: usize = d.outbox.values().map(hc::vec_deque_bytes).sum();
        hc::btree_map_bytes(&d.sessions)
            + hc::btree_map_bytes(&d.last_proposal_ms)
            + hc::btree_map_bytes(&d.attached)
            + hc::btree_map_bytes(&d.deferred)
            + hc::btree_map_bytes(&d.outbox)
            + outbox
            + hc::vec_deque_bytes(&d.jobs)
    }

    /// The link is being removed: end its session and retire its interface.
    pub(super) fn direct_link_link_removed(&mut self, link_id: &LinkId) {
        if let Some((_, index, _)) = self.direct_links.deferred.remove(link_id) {
            self.direct_links
                .jobs
                .push_back(DirectLinkJob::CloseInterface {
                    interface_index: index,
                });
        }
        if let Some(session) = self.direct_links.sessions.remove(link_id) {
            self.direct_links.jobs.push_back(DirectLinkJob::Release {
                session: *session.id(),
            });
        }
        self.direct_links.last_proposal_ms.remove(link_id);
        self.direct_links.outbox.remove(link_id);
        if let Some(index) = self.direct_links.interface_of_link(link_id) {
            self.direct_links.attached.remove(&index);
            self.direct_links
                .jobs
                .push_back(DirectLinkJob::CloseInterface {
                    interface_index: index,
                });
        }
    }

    /// An interface went down.
    ///
    /// If it was some link's fallback (the relayed interface the link left),
    /// that fallback is gone: forget it, so a later loss of the direct path
    /// cannot send the link back to an index that no longer exists.
    ///
    /// If it was a direct one, put its link back on the interface it used
    /// before. With no fallback left the link has no route at all, so it is
    /// closed (as `Stale`) rather than pinned to a dead interface.
    pub(super) fn direct_link_interface_down(&mut self, index: usize) {
        // A punched link still waiting to move never got its direct path.
        let waiting = self
            .direct_links
            .deferred
            .iter()
            .find(|(_, (_, i, _))| *i == index)
            .map(|(link_id, (_, _, proposed))| (*link_id, *proposed));
        if let Some((link_id, proposed)) = waiting {
            self.direct_links.deferred.remove(&link_id);
            self.events.push(NodeEvent::DirectLinkFailed {
                link_id,
                failure: Failure::Timeout,
                proposed,
            });
        }
        for attachment in self.direct_links.attached.values_mut() {
            if attachment.previous == Some(index) {
                attachment.previous = None;
            }
        }
        let Some(attachment) = self.direct_links.attached.remove(&index) else {
            return;
        };
        self.release_direct_attachment(index, attachment, "direct interface lost", false);
    }

    /// Return a link from its direct interface to its fallback, or close it
    /// when there is none, and report the loss.
    ///
    /// Every fallback to the relay nudges the peer over it, so a peer still
    /// on the direct path sees authenticated traffic arrive over the relay and
    /// comes back too (see [`Self::direct_link_inbound`]); without it a
    /// one-way failure strands the two ends on different paths. `retire` is
    /// for a fallback this node decides on while the direct interface still
    /// exists (the link went stale on it, or the peer came back over the
    /// relay): that interface is closed as well.
    fn release_direct_attachment(
        &mut self,
        index: usize,
        attachment: Attachment,
        why: &str,
        retire: bool,
    ) {
        let Some(link) = self.links.get_mut(&attachment.link_id) else {
            return;
        };
        crate::tracing::info!(
            link = %HexShort(attachment.link_id.as_bytes()),
            interface = index,
            fallback = ?attachment.previous,
            "direct link: {why}; link returns to its fallback, or closes without one"
        );
        self.events.push(NodeEvent::DirectLinkLost {
            link_id: attachment.link_id,
            interface_index: index,
        });
        match attachment.previous {
            Some(previous) => {
                link.set_attached_interface(previous);
                let nudge = link.build_keepalive_packet().ok();
                if retire {
                    self.direct_links
                        .jobs
                        .push_back(DirectLinkJob::CloseInterface {
                            interface_index: index,
                        });
                }
                // Tell the peer, whichever way this end found out: a peer
                // still on its working direction otherwise stays there while
                // this end waits on the relay (Codex review on #74). The
                // keepalive helps a stale link recover but is not encrypted,
                // so the peer does not act on it (see `direct_link_inbound`).
                // This signal is, and the peer ignores it as a protocol
                // message: COMPLETE for a session it no longer holds. What it
                // acts on is where the signal arrived from.
                if let Some(nudge) = nudge {
                    self.route_link_packet(&attachment.link_id, &nudge);
                }
                let now_ms = self.transport.clock().now_ms();
                self.send_direct_link_signal(
                    &attachment.link_id,
                    &Signal::Complete {
                        session: attachment.session,
                    },
                    now_ms,
                );
            }
            None => {
                // The link goes, so its direct interface must too: the
                // attachment is already gone, so `remove_link` cannot find it
                // (Codex review on #74).
                self.direct_links
                    .jobs
                    .push_back(DirectLinkJob::CloseInterface {
                        interface_index: index,
                    });
                let is_initiator = link.is_initiator();
                let destination = *link.destination_hash();
                link.close();
                self.remove_link(&attachment.link_id);
                self.emit_link_closed(
                    attachment.link_id,
                    crate::link::LinkCloseReason::Stale,
                    is_initiator,
                    destination,
                );
            }
        }
    }

    /// A link packet from interface `iface` is about to be processed.
    pub(super) fn direct_link_rx_begin(&mut self, iface: usize) {
        self.direct_links.rx_iface = Some(iface);
    }

    /// The link packet has been processed.
    pub(super) fn direct_link_rx_end(&mut self) {
        self.direct_links.rx_iface = None;
    }

    /// The packet being processed decrypted under `link_id`'s key: it came
    /// from the peer. Called from the channel path only after a successful
    /// decrypt; most handlers record liveness before decrypting, so liveness
    /// is no evidence here.
    pub(super) fn direct_link_authenticated(&mut self, link_id: &LinkId, now_ms: u64) {
        if let Some(iface) = self.direct_links.rx_iface {
            self.direct_link_inbound(link_id, iface, now_ms);
        }
    }

    /// An authenticated link packet for `link_id` arrived on interface
    /// `iface`: one that decrypted under the link key (a channel message;
    /// a keepalive is not encrypted and never counts). So nobody able to
    /// inject packets onto the relay can make this node drop a working
    /// direct path (Codex review on #74).
    ///
    /// If the link is on a direct interface and the packet came over the
    /// relayed interface the link left, the peer has fallen back (its side of
    /// the direct path stopped working). Follow it, after
    /// [`FALLBACK_GRACE_MS`], before which relayed packets from the moment of
    /// the upgrade are still draining.
    pub(super) fn direct_link_inbound(&mut self, link_id: &LinkId, iface: usize, now_ms: u64) {
        if self.direct_links.attached.is_empty() {
            return;
        }
        let Some(index) = self.direct_links.interface_of_link(link_id) else {
            return;
        };
        let Some(attachment) = self.direct_links.attached.get(&index).copied() else {
            return;
        };
        if attachment.previous != Some(iface)
            || now_ms < attachment.attached_at_ms.saturating_add(FALLBACK_GRACE_MS)
        {
            return;
        }
        self.direct_links.attached.remove(&index);
        self.release_direct_attachment(index, attachment, "peer came back over the relay", true);
    }

    /// The link went stale while on a direct interface: the direct path has
    /// stopped carrying the peer's traffic, at least toward this node. Fall
    /// back now rather than when the interface's own silence timer expires;
    /// on a fast link the link would be closed long before that.
    pub(super) fn direct_link_link_stale(&mut self, link_id: &LinkId) {
        let Some(index) = self.direct_links.interface_of_link(link_id) else {
            return;
        };
        let Some(attachment) = self.direct_links.attached.remove(&index) else {
            return;
        };
        self.release_direct_attachment(
            index,
            attachment,
            "link went stale on the direct path",
            true,
        );
    }

    /// Forget every session and direct interface: the driver's sockets are
    /// gone (a stopped node being started again). Links on a direct interface
    /// go back to their fallback, sessions in flight fail, and no job from
    /// before survives. Call before the driver starts taking jobs.
    pub fn abandon_direct_links(&mut self) -> crate::transport::TickOutput {
        self.direct_links.jobs.clear();
        self.direct_links.outbox.clear();
        for (link_id, (_, _, proposed)) in core::mem::take(&mut self.direct_links.deferred) {
            self.events.push(NodeEvent::DirectLinkFailed {
                link_id,
                failure: Failure::Timeout,
                proposed,
            });
        }
        let sessions = core::mem::take(&mut self.direct_links.sessions);
        for (link_id, session) in sessions {
            self.events.push(NodeEvent::DirectLinkFailed {
                link_id,
                failure: Failure::Timeout,
                proposed: session.role() == Role::Initiator,
            });
        }
        let attached = core::mem::take(&mut self.direct_links.attached);
        for (index, attachment) in attached {
            self.release_direct_attachment(index, attachment, "node restarted", false);
        }
        self.process_events_and_actions()
    }
}
