//! This node's NAT type and how its direct-link attempts ended (+ciris,
//! leviculum#100).
//!
//! The driver probes two servers off each session's socket and reports the
//! verdict here ([`NodeCore::record_nat_report`]). The node keeps the latest
//! report, raises [`NodeEvent::NatTypeChanged`] when the classification
//! changes, and records each attempt's end beside that session's verdict, so
//! a host can count punch outcomes per NAT type ([`NodeCore::direct_link_outcomes`]).
//! It also logs each phase of a session on one info line: the signals
//! (`READY`, `COMPLETE`), the verdict and the outcome. The driver logs the
//! first punch heard.

use alloc::collections::VecDeque;
use core::fmt;
use core::net::SocketAddr;

use rand_core::CryptoRngCore;

use crate::direct_link::{
    DirectLinkOutcome, DirectLinkOutcomes, DirectLinkRecord, NatMapping, NatProbe, NatReport,
    SessionId, Signal,
};
use crate::hex_fmt::HexShort;
use crate::link::LinkId;
use crate::traits::{Clock, Storage};

use super::event::NodeEvent;
use super::NodeCore;

/// How many sessions' verdicts and peers the node remembers, for the
/// outcome that comes later. Sessions run one per link and end within
/// seconds; a held move can outlast this many newer sessions only on a node
/// upgrading dozens of links at once, and its record then says `nat: None`.
const SESSION_NOTES: usize = 32;

/// What the node knows of one recent session, for its outcome record.
#[derive(Debug, Clone, Copy)]
struct SessionNote {
    session: SessionId,
    nat: Option<NatMapping>,
    /// The peer's reflexive address: debug lines only.
    peer: Option<SocketAddr>,
    /// The peer's identity hash (or, before it identified, the destination
    /// this node dialled): what info lines name the peer by.
    peer_hash: Option<[u8; 16]>,
}

/// The node's NAT reports and direct-link outcomes.
#[derive(Debug, Default)]
pub(crate) struct NatState {
    latest: Option<NatReport>,
    /// The last classification that was not `Unknown`, for the change event.
    last_known: Option<NatMapping>,
    notes: VecDeque<SessionNote>,
    outcomes: DirectLinkOutcomes,
}

impl NatState {
    fn note(&mut self, session: &SessionId) -> &mut SessionNote {
        if let Some(i) = self.notes.iter().position(|n| &n.session == session) {
            return &mut self.notes[i];
        }
        if self.notes.len() >= SESSION_NOTES {
            self.notes.pop_front();
        }
        self.notes.push_back(SessionNote {
            session: *session,
            nat: None,
            peer: None,
            peer_hash: None,
        });
        self.notes.back_mut().expect("just pushed")
    }

    fn noted(&self, session: &SessionId) -> Option<&SessionNote> {
        self.notes.iter().find(|n| &n.session == session)
    }

    /// Heap held, for the node's census.
    pub(super) fn heap_bytes(&self) -> usize {
        use crate::heap_census as hc;
        hc::vec_deque_bytes(&self.notes)
            + self.outcomes.recent.capacity() * core::mem::size_of::<DirectLinkRecord>()
            + self.latest.as_ref().map_or(0, |r| {
                r.probes.capacity() * core::mem::size_of::<NatProbe>()
                    + r.host_candidates.capacity() * core::mem::size_of::<SocketAddr>()
            })
    }
}

/// `server→mapped` for each probe, `-` for no answer: one log field.
struct Probes<'a>(&'a [NatProbe]);

impl fmt::Display for Probes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, p) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            match p.mapped {
                Some(m) => write!(f, "{}->{}", p.server, m)?,
                None => write!(f, "{}->-", p.server)?,
            }
        }
        Ok(())
    }
}

/// An address or `-`, for a log field.
struct Peer(Option<SocketAddr>);

impl fmt::Display for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(a) => write!(f, "{a}"),
            None => f.write_str("-"),
        }
    }
}

impl<R: CryptoRngCore, C: Clock, S: Storage> NodeCore<R, C, S> {
    /// +ciris (leviculum#100): the driver's NAT report for `session`'s probe
    /// socket, or for a probe outside any session (`None`). Kept as
    /// [`Self::nat_report`], stamped with the node's clock; raises [`NodeEvent::NatTypeChanged`] for the
    /// first report and for each known classification that differs from the
    /// last known one.
    pub fn record_nat_report(
        &mut self,
        session: Option<&SessionId>,
        mut report: NatReport,
    ) -> crate::transport::TickOutput {
        report.at_ms = self.transport.clock().now_ms();
        report.at_unix_secs = self.transport.clock().wall_unix_secs();
        let link = session.and_then(|s| self.direct_links.link_of_session(s));
        let answered = report.probes.iter().filter(|p| p.mapped.is_some()).count();
        // Addresses stay off info lines (leviculum#100): the verdict and
        // its shape there, what each server saw at debug.
        crate::tracing::info!(
            link = %Short(link.as_ref().map(|l| &l.as_bytes()[..])),
            session = %Short(session.map(|s| &s[..])),
            nat = report.mapping.name(),
            probed = report.probes.len(),
            answered,
            port_preserved = ?report.port_preserved,
            servers_share_ip = report.servers_share_ip,
            global_ipv6 = report.global_ipv6,
            "direct link: NAT type"
        );
        crate::tracing::debug!(
            session = %Short(session.map(|s| &s[..])),
            mapped = %Peer(report.mapped()),
            probes = %Probes(&report.probes),
            local_port = report.local_port,
            "direct link: NAT probe answers"
        );
        let state = &mut self.direct_links.nat;
        if let Some(session) = session {
            state.note(session).nat = Some(report.mapping);
        }
        let first = state.latest.is_none();
        let changed = report.mapping.is_known() && state.last_known != Some(report.mapping);
        if first || changed {
            self.events.push(NodeEvent::NatTypeChanged {
                previous: state.last_known,
                report: alloc::boxed::Box::new(report.clone()),
            });
        }
        if report.mapping.is_known() {
            state.last_known = Some(report.mapping);
        }
        state.latest = Some(report);
        self.process_events_and_actions()
    }

    /// +ciris (leviculum#100): this node's latest NAT report, if a probe
    /// has run.
    pub fn nat_report(&self) -> Option<NatReport> {
        self.direct_links.nat.latest.clone()
    }

    /// +ciris (leviculum#100): how this node's direct-link attempts ended,
    /// each beside its session's NAT verdict.
    pub fn direct_link_outcomes(&self) -> DirectLinkOutcomes {
        self.direct_links.nat.outcomes.clone()
    }

    /// +ciris (leviculum#100): the link a running session upgrades (its
    /// wire id, as the node's log lines print it), so the driver's lines for
    /// the session can name it too.
    pub fn direct_link_session_link(&self, session: &SessionId) -> Option<LinkId> {
        self.direct_links.link_of_session(session)
    }

    /// +ciris (leviculum#100): the hash a log line names `link_id`'s peer
    /// by: its identity hash once it identified, else (on the initiator)
    /// the destination this node dialled. Never an address.
    pub fn direct_link_peer_hash(&self, link_id: &LinkId) -> Option<[u8; 16]> {
        let link = self.links.get(link_id)?;
        match link.remote_identity() {
            Some(identity) => Some(*identity.hash()),
            None if link.is_initiator() => Some(*link.destination_hash().as_bytes()),
            None => None,
        }
    }

    /// Log a session's signal on one info line, and note the peer.
    pub(super) fn log_direct_link_signal(&mut self, link_id: &LinkId, signal: &Signal, sent: bool) {
        let session = *signal.session();
        let known = self
            .direct_links
            .sessions
            .get(link_id)
            .and_then(crate::direct_link::Session::peer_public);
        let (name, peer) = match signal {
            Signal::Ready {
                responder_public, ..
            } if !sent => ("READY", Some(*responder_public)),
            Signal::Ready { .. } => ("READY", known),
            Signal::Complete { .. } => ("COMPLETE", known),
            Signal::Accept { .. } => ("ACCEPT", known),
            Signal::Reject { .. } | Signal::Request { .. } => return,
        };
        let hash = self.direct_link_peer_hash(link_id);
        let note = self.direct_links.nat.note(&session);
        if peer.is_some() {
            note.peer = peer;
        }
        if hash.is_some() {
            note.peer_hash = hash;
        }
        let (peer, hash) = (note.peer, note.peer_hash);
        let verb = if sent { "sent" } else { "received" };
        crate::tracing::info!(
            link = %HexShort(link_id.as_bytes()),
            session = %HexShort(&session),
            peer = %Short(hash.as_ref().map(|h| &h[..])),
            "direct link: {} {}",
            name,
            verb
        );
        crate::tracing::debug!(
            session = %HexShort(&session),
            peer_addr = %Peer(peer),
            "direct link: {} {} (peer address)",
            name,
            verb
        );
    }

    /// Record how a session's attempt ended, and log it.
    pub(super) fn record_direct_link_outcome(
        &mut self,
        link_id: LinkId,
        session: SessionId,
        proposed: bool,
        outcome: DirectLinkOutcome,
    ) {
        let now_ms = self.transport.clock().now_ms();
        let visible = self
            .link_origin_ids
            .get(&link_id)
            .copied()
            .unwrap_or(link_id);
        let current = self.direct_link_peer_hash(&link_id);
        let state = &mut self.direct_links.nat;
        let (nat, peer, hash) = state
            .noted(&session)
            .map_or((None, None, None), |n| (n.nat, n.peer, n.peer_hash));
        let hash = hash.or(current);
        crate::tracing::debug!(
            session = %HexShort(&session),
            peer_addr = %Peer(peer),
            "direct link: outcome (peer address)"
        );
        crate::tracing::info!(
            link = %HexShort(link_id.as_bytes()),
            session = %HexShort(&session),
            peer = %Short(hash.as_ref().map(|h| &h[..])),
            proposed,
            nat = nat.map_or("-", |m| m.name()),
            outcome = outcome.name(),
            detail = ?outcome,
            "direct link: outcome"
        );
        state.outcomes.push(DirectLinkRecord {
            link_id: visible,
            session,
            proposed,
            nat,
            outcome,
            at_ms: now_ms,
        });
    }
}

/// An id in short hex, or `-`.
struct Short<'a>(Option<&'a [u8]>);

impl fmt::Display for Short<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(id) => write!(f, "{}", HexShort(id)),
            None => f.write_str("-"),
        }
    }
}
