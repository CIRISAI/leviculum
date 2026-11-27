//! The egress scheduler's public face on [`ReticulumNode`] (+ciris; see
//! [`crate::egress`] and `docs/egress-scheduler.md`).

use std::time::Duration;

use leviculum_core::LinkId;

use super::ReticulumNode;
use crate::egress::{EgressClass, EgressDepth, EgressFloor, EgressStats, MediaKind};
use crate::sync_ext::MutexRecover;

impl ReticulumNode {
    /// Pace everything this node sends on its network interfaces at
    /// `rate_bps` bits per second, or with `None` stop pacing (the default:
    /// packets leave as fast as the interfaces take them).
    ///
    /// The rate counts the frames as handed to the interfaces (IFAC
    /// included; HDLC, IP, UDP and TCP overhead not), so set it a little
    /// under the uplink. A rate below [`crate::egress::MIN_RATE_BPS`] is
    /// raised to it. Takes effect at once, for what is already queued too.
    pub fn set_egress_rate(&self, rate_bps: Option<u64>) {
        self.egress.set_rate(rate_bps);
    }

    /// The pacing rate in force, bits per second; `None` = unpaced.
    pub fn egress_rate(&self) -> Option<u64> {
        self.egress.rate()
    }

    /// What the egress queue holds now: per class, the queued bytes and
    /// packets and the age of the oldest packet. Allocates nothing.
    pub fn egress_depth(&self) -> EgressDepth {
        self.egress.depth()
    }

    /// Cumulative egress counters per class (sent, dropped by age,
    /// refused). Allocates nothing.
    pub fn egress_stats(&self) -> EgressStats {
        self.egress.stats()
    }

    /// Schedule a link's Control frames (Channel messages, requests,
    /// responses) as `class`, or with `None` as Control again. With
    /// [`EgressClass::Audio`] or [`EgressClass::Video`] the link's
    /// datagrams default to that kind too. Proofs, link setup and resource
    /// frames keep their own class, and a frame moved this way is never
    /// dropped by age: only a datagram is. Nothing changes on the wire.
    ///
    /// The hint is forgotten when the link closes.
    pub fn set_link_egress_class(&self, link_id: &LinkId, class: Option<EgressClass>) {
        let wire = self
            .inner
            .lock_recover()
            .wire_link_id(link_id)
            .unwrap_or(*link_id);
        self.egress.set_link_class(*wire.as_bytes(), class);
    }

    /// The age past which a queued datagram of `kind` is dropped at the head
    /// of the queue instead of sent. Default
    /// [`crate::egress::DEFAULT_MEDIA_AGE_BOUND`] (800 ms) for both.
    pub fn set_egress_media_age_bound(&self, kind: MediaKind, bound: Duration) {
        self.egress.set_media_age_bound(kind, bound);
    }

    /// The share Announce and Bulk are guaranteed together; see
    /// [`EgressFloor`].
    pub fn set_egress_floor(&self, floor: EgressFloor) {
        self.egress.set_floor(floor);
    }
}
