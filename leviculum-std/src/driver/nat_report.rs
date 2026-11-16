//! This node's NAT type and how its direct-link attempts ended (+ciris,
//! leviculum#100); see [`leviculum_core::direct_link::NatReport`].

use leviculum_core::direct_link::{DirectLinkOutcomes, NatReport};

use super::{dispatch_committed, ReticulumNode, ReticulumNodeBuilder};
use crate::error::Error;
use crate::sync_ext::MutexRecover;

impl ReticulumNode {
    /// The latest NAT report: the verdict of the newest probe (a
    /// direct-link session's, a re-probe's, or
    /// [`probe_nat`](Self::probe_nat)'s), the observations behind it, when
    /// it ran and whether the socket had a global IPv6 address. `None`
    /// until a probe has run; `Unknown` until servers at two distinct
    /// addresses have answered. A change of verdict also arrives as
    /// `NodeEvent::NatTypeChanged`. Takes the core lock.
    ///
    /// The report holds raw addresses (each server's and what it saw):
    /// the host decides what leaves the node. CIRISEdge exports only counts
    /// by class. The node's own info-level logs carry none.
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

    /// Probe now, from a fresh socket and outside any session: this node's
    /// facilitator (`probe_addr`), the host's NAT probe servers, and every
    /// live direct-link peer (which answers unless it set
    /// `answer_probes = no`). Recorded as [`nat_report`](Self::nat_report)
    /// is; the node also re-probes by itself when a peer at a new address
    /// appears. Sends one small datagram to each per attempt; no address
    /// leaves for any peer. Errors when there is nothing to probe, or when
    /// the node is not running.
    pub async fn probe_nat(&self) -> Result<NatReport, Error> {
        let report =
            crate::direct_link::nat::probe_nat_now(&self.direct_link, &self.direct_link_extras)
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

    /// Answer direct-link peers' NAT probes (default `true`), overriding the
    /// config's `answer_probes`. A node answers only a padded RNSP request
    /// from the IP address of a peer it holds a direct path with, on that
    /// path's socket, never with more bytes than the request and at most
    /// six at once, one per two seconds after. Off, peers cannot use this
    /// node as their second probe server.
    pub fn answer_probes(mut self, enabled: bool) -> Self {
        self.direct_link_extras.answer_probes = Some(enabled);
        self
    }
}
