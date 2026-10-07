//! Link telemetry (+ciris, leviculum#77): the live link list and the
//! cumulative lifecycle counters a host reads to see what its links are
//! doing in the field.
//!
//! Both are plain snapshots. The counters only grow (for a monotonic
//! metrics counter), and nothing here allocates on the hot path: a close
//! or an establishment bumps a fixed-size counter.

use alloc::vec::Vec;

use rand_core::CryptoRngCore;

use crate::constants::MS_PER_SECOND;
use crate::destination::DestinationHash;
use crate::link::{LinkCloseReason, LinkId, LinkState};
use crate::traits::{Clock, Storage};

use super::NodeCore;

/// Which end of a link this node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkRole {
    /// This node dialled the link; its keepalives keep it alive.
    Initiator,
    /// A peer opened the link to one of this node's destinations.
    Responder,
}

/// One live link, as a host lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    /// The id the application was given (a re-keyed link reports its
    /// original id, the same one its events carry).
    pub link_id: LinkId,
    /// For an initiator link the destination dialled; for a responder link
    /// this node's own destination.
    pub destination_hash: DestinationHash,
    pub role: LinkRole,
    pub state: LinkState,
    /// Seconds since establishment, or `None` while still establishing.
    pub age_secs: Option<u64>,
    /// Seconds since anything arrived on the link (one-second resolution),
    /// or `None` if nothing has yet.
    pub idle_secs: Option<u64>,
    /// Measured round-trip time in milliseconds, or `None` before a
    /// sample exists (the link's working fallback is not a measurement).
    pub rtt_ms: Option<u64>,
    /// The interface the link is sent on, if attached.
    pub interface_index: Option<usize>,
}

/// Why a link ended, as a counter index. One slot per [`LinkCloseReason`].
const REASONS: usize = 7;

fn reason_slot(reason: LinkCloseReason) -> usize {
    match reason {
        LinkCloseReason::Normal => 0,
        LinkCloseReason::Timeout => 1,
        LinkCloseReason::InvalidProof => 2,
        LinkCloseReason::PeerClosed => 3,
        LinkCloseReason::Stale => 4,
        LinkCloseReason::ChannelExhausted => 5,
        LinkCloseReason::Blackholed => 6,
    }
}

/// Every [`LinkCloseReason`], in counter order.
pub const LINK_CLOSE_REASONS: [LinkCloseReason; REASONS] = [
    LinkCloseReason::Normal,
    LinkCloseReason::Timeout,
    LinkCloseReason::InvalidProof,
    LinkCloseReason::PeerClosed,
    LinkCloseReason::Stale,
    LinkCloseReason::ChannelExhausted,
    LinkCloseReason::Blackholed,
];

/// A stable lowercase name for a close reason, for a metric label.
pub fn link_close_reason_name(reason: LinkCloseReason) -> &'static str {
    match reason {
        LinkCloseReason::Normal => "normal",
        LinkCloseReason::Timeout => "timeout",
        LinkCloseReason::InvalidProof => "invalid_proof",
        LinkCloseReason::PeerClosed => "peer_closed",
        LinkCloseReason::Stale => "stale",
        LinkCloseReason::ChannelExhausted => "channel_exhausted",
        LinkCloseReason::Blackholed => "blackholed",
    }
}

/// Cumulative link lifecycle counters since the node started.
///
/// Every link that ends is counted once: in `closed` if it had been
/// established, in `handshake_failed` if it never was, or in `rejected` if
/// the application refused its request. So
/// `established - closed_total()` is the established links alive now, and
/// a census that disagrees with it is a bookkeeping fault.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkLifecycle {
    /// Links established with this node as initiator.
    pub established_initiator: u64,
    /// Links established with this node as responder.
    pub established_responder: u64,
    /// Incoming link requests the application refused (`reject_link`).
    pub rejected: u64,
    closed: [u64; REASONS],
    handshake_failed: [u64; REASONS],
}

impl LinkLifecycle {
    /// Links established, either role.
    pub fn established(&self) -> u64 {
        self.established_initiator + self.established_responder
    }

    /// Established links that ended with `reason`.
    pub fn closed(&self, reason: LinkCloseReason) -> u64 {
        self.closed[reason_slot(reason)]
    }

    /// Links that ended with `reason` before they were established.
    pub fn handshake_failed(&self, reason: LinkCloseReason) -> u64 {
        self.handshake_failed[reason_slot(reason)]
    }

    /// Established links that ended, any reason.
    pub fn closed_total(&self) -> u64 {
        self.closed.iter().sum()
    }

    /// Links that never established, any reason.
    pub fn handshake_failed_total(&self) -> u64 {
        self.handshake_failed.iter().sum()
    }
}

/// The node's telemetry state: the counters, and whether the link being
/// torn down right now had been established (noted when it leaves the
/// table, read when its close is reported).
#[derive(Debug, Default)]
pub(crate) struct LinkTelemetry {
    pub(super) lifecycle: LinkLifecycle,
    pub(super) removing_was_established: Option<bool>,
}

impl<R: CryptoRngCore, C: Clock, S: Storage> NodeCore<R, C, S> {
    /// Every link in the table, live or establishing, with its role, state,
    /// age and idle time (leviculum#77).
    pub fn link_list(&self) -> Vec<LinkInfo> {
        let now_secs = self.transport.clock().now_ms() / MS_PER_SECOND;
        self.links
            .iter()
            .filter(|(_, link)| link.state() != LinkState::Closed)
            .map(|(wire_id, link)| {
                let established = link.established_at_secs();
                LinkInfo {
                    link_id: self
                        .link_origin_ids
                        .get(wire_id)
                        .copied()
                        .unwrap_or(*wire_id),
                    destination_hash: *link.destination_hash(),
                    role: if link.is_initiator() {
                        LinkRole::Initiator
                    } else {
                        LinkRole::Responder
                    },
                    state: link.state(),
                    age_secs: established.map(|at| now_secs.saturating_sub(at)),
                    idle_secs: established
                        .map(|_| now_secs.saturating_sub(link.last_inbound_secs())),
                    rtt_ms: link.rtt_us().map(|us| us / 1000),
                    interface_index: link.attached_interface(),
                }
            })
            .collect()
    }

    /// Cumulative link lifecycle counters (leviculum#77).
    pub fn link_lifecycle(&self) -> LinkLifecycle {
        self.link_telemetry.lifecycle
    }

    /// Called by `reject_link`: a request refused before it established,
    /// which reports no close (Codex review on #76).
    pub(super) fn note_link_rejected(&mut self) {
        self.link_telemetry.removing_was_established = None;
        self.link_telemetry.lifecycle.rejected += 1;
    }

    /// Called by `remove_link` as a link leaves the table.
    pub(super) fn note_link_removing(&mut self, was_established: bool) {
        self.link_telemetry.removing_was_established = Some(was_established);
    }

    /// Called by `emit_link_closed`, right after the matching removal.
    pub(super) fn note_link_closed(&mut self, reason: LinkCloseReason) {
        // Every close follows its removal; an unmatched one is counted as an
        // established link closing, the conservative reading.
        let established = self
            .link_telemetry
            .removing_was_established
            .take()
            .unwrap_or(true);
        let lc = &mut self.link_telemetry.lifecycle;
        let slot = reason_slot(reason);
        if established {
            lc.closed[slot] += 1;
        } else {
            lc.handshake_failed[slot] += 1;
        }
    }
}

impl<R: CryptoRngCore, C: Clock, S: Storage> NodeCore<R, C, S> {
    /// Established links (Active or Stale), either role: the count the
    /// driver's completion mirror must agree with (leviculum#77). One pass
    /// over the table, no allocation.
    pub fn established_link_count(&self) -> usize {
        self.links
            .values()
            .filter(|l| matches!(l.state(), LinkState::Active | LinkState::Stale))
            .count()
    }
}

impl crate::transport::TransportStats {
    /// The drop counter for one reason (leviculum#77), so a metrics bridge
    /// can walk [`crate::transport::DropReason::ALL`] instead of naming
    /// twenty-one accessors. Defined here so the fork moves no line of
    /// `transport.rs`.
    pub fn drops_for(&self, reason: crate::transport::DropReason) -> u64 {
        use crate::transport::DropReason as D;
        match reason {
            D::OverheardTransportId => self.drops_overheard_transport_id,
            D::InvalidAnnounce => self.drops_invalid_announce,
            D::PlainGroupMultihop => self.drops_plain_group_multihop,
            D::NoPath => self.drops_no_path,
            D::Ifac => self.drops_ifac,
            D::Duplicate => self.drops_duplicate,
            D::AnnounceOverMaxHops => self.drops_announce_over_max_hops,
            D::AnnounceReplay => self.drops_announce_replay,
            D::AnnounceRateLimited => self.drops_announce_rate_limited,
            D::IngressBurstAnnounce => self.drops_ingress_burst_announce,
            D::LrproofInvalid => self.drops_lrproof_invalid,
            D::LrproofNoLink => self.drops_lrproof_no_link,
            D::LinkDataNoLink => self.drops_link_data_no_link,
            D::LinkRepeatEcho => self.drops_link_repeat_echo,
            D::ForwardMaxHops => self.drops_forward_max_hops,
            D::BlackholedAnnounce => self.drops_blackholed_announce,
            D::SingleDecryptFail => self.drops_single_decrypt_fail,
            D::GroupDecryptFail => self.drops_group_decrypt_fail,
            D::UnknownContext => self.drops_unknown_context,
            D::NoSuchInterface => self.drops_no_such_interface,
            D::NextHopIsRequestor => self.drops_next_hop_is_requestor,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::transport::{DropReason, TransportStats};

    #[test]
    fn drops_for_covers_every_reason_and_sums_to_the_total() {
        let mut stats = TransportStats::default();
        for (i, reason) in DropReason::ALL.iter().enumerate() {
            for _ in 0..=i {
                stats.record_drop(*reason);
            }
        }
        for (i, reason) in DropReason::ALL.iter().enumerate() {
            assert_eq!(stats.drops_for(*reason), i as u64 + 1, "{reason:?}");
        }
        let sum: u64 = DropReason::ALL.iter().map(|r| stats.drops_for(*r)).sum();
        assert_eq!(sum, stats.packets_dropped());
    }
}
