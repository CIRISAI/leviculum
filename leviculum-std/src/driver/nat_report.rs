//! This node's NAT type and how its direct-link attempts ended (+ciris,
//! leviculum#100); see [`leviculum_core::direct_link::NatReport`].

use leviculum_core::direct_link::{DirectLinkOutcomes, NatReport};

use super::{dispatch_committed, ReticulumNode, ReticulumNodeBuilder};
use crate::error::Error;
use crate::sync_ext::MutexRecover;

impl ReticulumNode {
    /// The latest NAT report: the verdict of the newest two-server probe
    /// (a direct-link session's, or [`probe_nat`](Self::probe_nat)'s), the
    /// observations behind it, when it ran and whether the socket had a
    /// global IPv6 address. `None` until a probe has run. A change of
    /// verdict also arrives as `NodeEvent::NatTypeChanged`. Takes the core
    /// lock.
    pub fn nat_report(&self) -> Option<NatReport> {
        self.inner.lock_recover().nat_report()
    }

    /// How this node's direct-link attempts ended (established, punch
    /// failed, timed out, refused, ...), counted since start, and the latest
    /// few each beside its session's NAT verdict, to correlate NAT type with
    /// outcome. Takes the core lock.
    pub fn direct_link_outcomes(&self) -> DirectLinkOutcomes {
        self.inner.lock_recover().direct_link_outcomes()
    }

    /// Probe the facilitator (`probe_addr`) and the NAT probe servers from a
    /// fresh socket now, outside any session, and record the result as
    /// [`nat_report`](Self::nat_report) would. For a host that wants its
    /// NAT type before (or without) proposing. Sends one small datagram to
    /// each server per attempt; no address leaves for any peer. Errors
    /// without a facilitator, or when the node is not running.
    pub async fn probe_nat(&self) -> Result<NatReport, Error> {
        let report = crate::direct_link::probe_nat_now(
            &self.direct_link,
            &self.direct_link_extras.nat_probe_servers,
        )
        .await?;
        dispatch_committed(&self.action_dispatch_tx(), || {
            let mut core = self.inner.lock_recover();
            let output = core.record_nat_report(None, report);
            let stamped = core.nat_report().expect("just recorded");
            Ok((stamped, output))
        })
        .await
    }
}

impl ReticulumNodeBuilder {
    /// More servers to probe from each direct-link session's socket beside
    /// its facilitator, so the node can tell its NAT type
    /// ([`ReticulumNode::nat_report`]), overriding the config's
    /// `nat_probe_addrs`. Each is `host:port`, probed with RNSP, or with STUN
    /// when prefixed `stun:`; `build` refuses an entry with no port. With
    /// none, a session still probes this node's own facilitator when the
    /// peer named another, and otherwise reads `Unknown(OneServer)`.
    pub fn nat_probe_servers(mut self, servers: Vec<String>) -> Self {
        self.direct_link_extras.nat_probe_servers = Some(servers);
        self
    }
}
