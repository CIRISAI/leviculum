//! Metrics through the `metrics` facade (+ciris, leviculum#77).
//!
//! The no_std core keeps its counter structs; this module bridges them onto
//! the facade once, on std hosts, as CIRISServer's unified-telemetry FSD
//! asks (§3.2: libraries emit, the host installs the recorder and the
//! exporter). With no recorder installed every macro here is a no-op.
//!
//! The bridge is pulled, not pushed: the host calls
//! [`crate::driver::ReticulumNode::publish_metrics`] on its own cadence,
//! typically right before each scrape, and every value is set from the
//! node's snapshots at that moment.
//!
//! Counters are process totals. Each node remembers what it last published
//! ([`CounterBaseline`]) and adds only its new growth to one process-wide
//! sum per series, which is what `absolute` records. Two nodes in one
//! process therefore sum, a node that replaces another carries on instead
//! of sitting under the old total (`absolute` keeps the largest value it
//! has seen; Codex review on #76), and a recorder installed late still
//! reads full totals. Gauges are levels and are simply set: with several
//! nodes in one process the last publish wins.
//!
//! [`METRIC_CATALOG`] is the one list of names. Every name the code records
//! must be in it and every entry must be recorded somewhere, with only the
//! label keys it lists; `event_catalog_completeness` checks both against the
//! source, the way it checks `EVENT_CATALOG`. Label values are always bounded
//! enumerations (role, reason, plane, component): no link id, destination or
//! peer key ever becomes a label.

use leviculum_core::heap_census::NodeHeapCensus;
use leviculum_core::node::{link_close_reason_name, LinkCensus, LinkLifecycle, LINK_CLOSE_REASONS};
use leviculum_core::transport::{DropReason, TransportStats};
use metrics::{counter, describe_counter, describe_gauge, gauge, Unit};

use crate::driver::PlaneStats;

/// Counter or gauge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    /// Monotonic, cumulative since the node started.
    Counter,
    /// A level read now.
    Gauge,
}

/// One metric this crate records.
#[derive(Debug, Clone, Copy)]
pub struct MetricSpec {
    /// OpenTelemetry-style dotted name; a Prometheus exporter renders
    /// `leviculum.link.closed` as `leviculum_link_closed_total`.
    pub name: &'static str,
    pub kind: MetricKind,
    pub unit: Unit,
    /// The only label keys the metric may carry. Their values are bounded
    /// enumerations.
    pub labels: &'static [&'static str],
    pub description: &'static str,
}

const fn counter_spec(
    name: &'static str,
    unit: Unit,
    labels: &'static [&'static str],
    description: &'static str,
) -> MetricSpec {
    MetricSpec {
        name,
        kind: MetricKind::Counter,
        unit,
        labels,
        description,
    }
}

const fn gauge_spec(
    name: &'static str,
    unit: Unit,
    labels: &'static [&'static str],
    description: &'static str,
) -> MetricSpec {
    MetricSpec {
        name,
        kind: MetricKind::Gauge,
        unit,
        labels,
        description,
    }
}

/// Every metric leviculum-std records.
pub const METRIC_CATALOG: &[MetricSpec] = &[
    // Link lifecycle (P0)
    counter_spec(
        "leviculum.link.established",
        Unit::Count,
        &["role"],
        "Links established, by this node's role (initiator: it dialled; responder: a peer did).",
    ),
    counter_spec(
        "leviculum.link.closed",
        Unit::Count,
        &["reason"],
        "Established links that ended, by close reason.",
    ),
    counter_spec(
        "leviculum.link.handshake_failed",
        Unit::Count,
        &["reason"],
        "Links that ended before they were established, by close reason, plus \
         reason=rejected for requests the application refused.",
    ),
    gauge_spec(
        "leviculum.link.live",
        Unit::Count,
        &["role"],
        "Established links (active or stale) now, by role. The initiator keeps a link alive \
         with its keepalives, so a rising initiator count is this node not closing what it dialled.",
    ),
    gauge_spec(
        "leviculum.link.pending",
        Unit::Count,
        &[],
        "Links still establishing.",
    ),
    gauge_spec(
        "leviculum.link.destinations",
        Unit::Count,
        &[],
        "Destinations with at least one established link.",
    ),
    gauge_spec(
        "leviculum.link.max_per_destination",
        Unit::Count,
        &[],
        "Established links to the busiest destination. Per-destination detail is on \
         ReticulumNode::link_census, never a label.",
    ),
    // The mirror invariant (P0)
    gauge_spec(
        "leviculum.link.mirror",
        Unit::Count,
        &[],
        "Links the driver's completion mirror holds as established.",
    ),
    gauge_spec(
        "leviculum.link.mirror_skew",
        Unit::Count,
        &[],
        "Completion mirror minus the core's established links. Zero in steady state; \
         a value that stays non-zero is a lost LinkEstablished or LinkClosed.",
    ),
    counter_spec(
        "leviculum.link.mirror_divergence_alarms",
        Unit::Count,
        &[],
        "Times the mirror and the core disagreed for longer than the alarm's grace \
         (each also logged as LINK_MIRROR_DIVERGED).",
    ),
    // Transport
    counter_spec(
        "leviculum.transport.packets",
        Unit::Count,
        &["direction"],
        "Packets sent, received, forwarded, and forwarded on a link.",
    ),
    counter_spec(
        "leviculum.transport.announces_processed",
        Unit::Count,
        &[],
        "Announces processed.",
    ),
    counter_spec(
        "leviculum.transport.dropped",
        Unit::Count,
        &["reason"],
        "Packets dropped, by classified reason. overheard-transport-id is normal on a \
         shared medium; the others are loss.",
    ),
    counter_spec(
        "leviculum.transport.suppressed",
        Unit::Count,
        &["kind"],
        "Work withheld on purpose, not loss: link requests redirected to a local client, \
         path requests batched onto a pending discovery, discovery retries withheld.",
    ),
    counter_spec(
        "leviculum.known_destination.evictions",
        Unit::Count,
        &[],
        "Known destinations evicted at the identity cap.",
    ),
    // Driver planes and queues
    gauge_spec(
        "leviculum.events.queued",
        Unit::Count,
        &["plane"],
        "Node events waiting for the application, per plane.",
    ),
    gauge_spec(
        "leviculum.events.capacity",
        Unit::Count,
        &["plane"],
        "Capacity of each node-event plane.",
    ),
    counter_spec(
        "leviculum.events.dropped",
        Unit::Count,
        &["plane"],
        "Node events dropped because the application did not drain its plane.",
    ),
    gauge_spec(
        "leviculum.retry.queued",
        Unit::Count,
        &[],
        "Packets queued for retry across all interfaces.",
    ),
    counter_spec(
        "leviculum.retry.dropped",
        Unit::Count,
        &[],
        "Packets dropped because an interface's retry queue was full.",
    ),
    counter_spec(
        "leviculum.shed.packets",
        Unit::Count,
        &[],
        "Packets shed by an open per-peer circuit breaker.",
    ),
    gauge_spec(
        "leviculum.completion.recent",
        Unit::Count,
        &[],
        "Terminal outcomes held in the completion registry's recent ring.",
    ),
    // Memory (P1: per-link memory on std)
    gauge_spec(
        "leviculum.memory.bytes",
        Unit::Bytes,
        &["component"],
        "Heap the node core accounts for, by component (the core's own census, an \
         estimate from container sizes).",
    ),
    gauge_spec(
        "leviculum.memory.link_bytes_mean",
        Unit::Bytes,
        &[],
        "Accounted link-table bytes per link.",
    ),
];

/// The node's state at one moment, everything a publish records.
pub(crate) struct Snapshot {
    pub lifecycle: LinkLifecycle,
    pub census: LinkCensus,
    pub mirror: usize,
    pub mirror_alarms: u64,
    pub transport: TransportStats,
    pub known_evictions: u64,
    pub plane: PlaneStats,
    pub heap: NodeHeapCensus,
}

/// Register every catalogue entry's unit and description with whatever
/// recorder is installed now. Done on every publish, not once per process:
/// a recorder installed (or swapped for a local one) after the first
/// publish must still learn them (Codex review on #76). Twenty-four calls,
/// against a scrape.
fn describe_all() {
    for spec in METRIC_CATALOG {
        match spec.kind {
            MetricKind::Counter => describe_counter!(spec.name, spec.unit, spec.description),
            MetricKind::Gauge => describe_gauge!(spec.name, spec.unit, spec.description),
        }
    }
}

/// The counter totals one node last published, per series, so the next
/// publish adds only the difference to the process-wide total.
#[derive(Debug, Default)]
pub(crate) struct CounterBaseline(std::collections::HashMap<(&'static str, &'static str), u64>);

/// Every node's contributions to each counter series, summed over the life
/// of the process.
static PROCESS_TOTALS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<(&'static str, &'static str), u64>>,
> = std::sync::OnceLock::new();

impl CounterBaseline {
    /// Add what this node's series `name{label}` gained since its last
    /// publish to the process-wide total, and return that total.
    ///
    /// Recording the process total with `absolute` keeps every property at
    /// once: two nodes in one process sum, a node that replaces another
    /// carries on from where the series stood, and a recorder installed
    /// after earlier publishes still reads full totals. A node's own totals
    /// never fall, so a smaller value adds nothing.
    fn accumulate(&mut self, name: &'static str, label: &'static str, total: u64) -> u64 {
        let last = self.0.insert((name, label), total).unwrap_or(0);
        let delta = total.saturating_sub(last);
        let mut totals = PROCESS_TOTALS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = totals.entry((name, label)).or_insert(0);
        *entry += delta;
        *entry
    }
}

/// Record a snapshot onto whatever recorder is installed.
pub(crate) fn record(s: &Snapshot, b: &mut CounterBaseline) {
    describe_all();

    // Link lifecycle
    let lc = &s.lifecycle;
    counter!("leviculum.link.established", "role" => "initiator").absolute(b.accumulate(
        "leviculum.link.established",
        "initiator",
        lc.established_initiator,
    ));
    counter!("leviculum.link.established", "role" => "responder").absolute(b.accumulate(
        "leviculum.link.established",
        "responder",
        lc.established_responder,
    ));
    for reason in LINK_CLOSE_REASONS {
        let name = link_close_reason_name(reason);
        counter!("leviculum.link.closed", "reason" => name).absolute(b.accumulate(
            "leviculum.link.closed",
            name,
            lc.closed(reason),
        ));
        counter!("leviculum.link.handshake_failed", "reason" => name).absolute(b.accumulate(
            "leviculum.link.handshake_failed",
            name,
            lc.handshake_failed(reason),
        ));
    }
    // A request the application refused ended before it established too.
    counter!("leviculum.link.handshake_failed", "reason" => "rejected").absolute(b.accumulate(
        "leviculum.link.handshake_failed",
        "rejected",
        lc.rejected,
    ));
    gauge!("leviculum.link.live", "role" => "initiator").set(s.census.initiator as f64);
    gauge!("leviculum.link.live", "role" => "responder").set(s.census.responder as f64);
    gauge!("leviculum.link.pending").set(s.census.pending as f64);
    gauge!("leviculum.link.destinations").set(s.census.by_destination.len() as f64);
    let busiest = s
        .census
        .by_destination
        .first()
        .map(|d| d.initiator + d.responder)
        .unwrap_or(0);
    gauge!("leviculum.link.max_per_destination").set(busiest as f64);

    // The mirror invariant
    let core = s.census.initiator + s.census.responder;
    gauge!("leviculum.link.mirror").set(s.mirror as f64);
    gauge!("leviculum.link.mirror_skew").set(s.mirror as f64 - core as f64);
    counter!("leviculum.link.mirror_divergence_alarms").absolute(b.accumulate(
        "leviculum.link.mirror_divergence_alarms",
        "",
        s.mirror_alarms,
    ));

    // Transport
    let t = &s.transport;
    counter!("leviculum.transport.packets", "direction" => "sent").absolute(b.accumulate(
        "leviculum.transport.packets",
        "sent",
        t.packets_sent(),
    ));
    counter!("leviculum.transport.packets", "direction" => "received").absolute(b.accumulate(
        "leviculum.transport.packets",
        "received",
        t.packets_received(),
    ));
    counter!("leviculum.transport.packets", "direction" => "forwarded").absolute(b.accumulate(
        "leviculum.transport.packets",
        "forwarded",
        t.packets_forwarded(),
    ));
    counter!("leviculum.transport.packets", "direction" => "forwarded_link").absolute(
        b.accumulate(
            "leviculum.transport.packets",
            "forwarded_link",
            t.packets_forwarded_link(),
        ),
    );
    counter!("leviculum.transport.announces_processed").absolute(b.accumulate(
        "leviculum.transport.announces_processed",
        "",
        t.announces_processed(),
    ));
    for reason in DropReason::ALL {
        counter!("leviculum.transport.dropped", "reason" => reason.kebab()).absolute(b.accumulate(
            "leviculum.transport.dropped",
            reason.kebab(),
            t.drops_for(reason),
        ));
    }
    counter!("leviculum.transport.suppressed", "kind" => "lr_local_client_redirect").absolute(
        b.accumulate(
            "leviculum.transport.suppressed",
            "lr_local_client_redirect",
            t.lr_local_client_redirects(),
        ),
    );
    counter!("leviculum.transport.suppressed", "kind" => "path_request_pending").absolute(
        b.accumulate(
            "leviculum.transport.suppressed",
            "path_request_pending",
            t.path_request_pending_suppressions(),
        ),
    );
    counter!("leviculum.transport.suppressed", "kind" => "path_request_retry").absolute(
        b.accumulate(
            "leviculum.transport.suppressed",
            "path_request_retry",
            t.path_request_retry_withholds(),
        ),
    );
    counter!("leviculum.known_destination.evictions").absolute(b.accumulate(
        "leviculum.known_destination.evictions",
        "",
        s.known_evictions,
    ));

    // Planes and queues
    let p = &s.plane;
    gauge!("leviculum.events.queued", "plane" => "control").set(p.control_queued as f64);
    gauge!("leviculum.events.queued", "plane" => "data").set(p.data_queued as f64);
    gauge!("leviculum.events.capacity", "plane" => "control").set(p.control_capacity as f64);
    gauge!("leviculum.events.capacity", "plane" => "data").set(p.data_capacity as f64);
    counter!("leviculum.events.dropped", "plane" => "control").absolute(b.accumulate(
        "leviculum.events.dropped",
        "control",
        p.control_dropped_total,
    ));
    counter!("leviculum.events.dropped", "plane" => "data").absolute(b.accumulate(
        "leviculum.events.dropped",
        "data",
        p.data_dropped_total,
    ));
    gauge!("leviculum.retry.queued").set(p.retry_queued as f64);
    counter!("leviculum.retry.dropped").absolute(b.accumulate(
        "leviculum.retry.dropped",
        "",
        p.retry_dropped_total,
    ));
    counter!("leviculum.shed.packets").absolute(b.accumulate(
        "leviculum.shed.packets",
        "",
        p.shed_packets_total,
    ));
    gauge!("leviculum.completion.recent").set(p.recent_outcomes as f64);

    // Memory
    let h = &s.heap;
    for (component, bytes) in [
        ("node", h.node_struct),
        ("links", h.links),
        ("resources", h.resources),
        ("events", h.events),
        ("requests", h.requests),
        ("destinations", h.destinations),
        ("transport", h.transport),
        ("storage", h.storage),
    ] {
        gauge!("leviculum.memory.bytes", "component" => component).set(bytes as f64);
    }
    let per_link = if h.link_count == 0 {
        0.0
    } else {
        h.links as f64 / h.link_count as f64
    };
    gauge!("leviculum.memory.link_bytes_mean").set(per_link);
}

/// How often the event loop compares the completion mirror with the core's
/// established-link count.
pub(crate) const MIRROR_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
/// How long the two may disagree before it is a divergence rather than
/// events in flight.
pub(crate) const MIRROR_DIVERGENCE_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

/// The completion mirror's view of the established links beside the core's
/// (leviculum#77). See [`crate::driver::ReticulumNode::link_count_check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkCountCheck {
    /// Links the completion mirror holds as established.
    pub mirror: usize,
    /// Links the core holds as established (active or stale).
    pub core: usize,
    /// Sustained divergences alarmed on since the node started.
    pub alarms: u64,
}

/// The completion-mirror invariant's alarm (leviculum#77): the mirror and the
/// core must agree on the established-link count. They differ for an instant
/// while events are in flight, so a disagreement is a divergence only once it
/// has lasted `grace`, and it is alarmed once per episode.
#[derive(Debug)]
pub(crate) struct MirrorWatch {
    grace: std::time::Duration,
    diverged_since: Option<std::time::Instant>,
    alarmed: bool,
}

/// A divergence that has outlasted the grace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MirrorAlarm {
    pub mirror: usize,
    pub core: usize,
    pub for_secs: u64,
}

impl MirrorWatch {
    pub(crate) fn new(grace: std::time::Duration) -> Self {
        MirrorWatch {
            grace,
            diverged_since: None,
            alarmed: false,
        }
    }

    /// One comparison. Returns an alarm the first time a disagreement has
    /// lasted `grace`; agreement ends the episode.
    pub(crate) fn observe(
        &mut self,
        mirror: usize,
        core: usize,
        now: std::time::Instant,
    ) -> Option<MirrorAlarm> {
        if mirror == core {
            self.diverged_since = None;
            self.alarmed = false;
            return None;
        }
        let since = *self.diverged_since.get_or_insert(now);
        let lasted = now.saturating_duration_since(since);
        if self.alarmed || lasted < self.grace {
            return None;
        }
        self.alarmed = true;
        Some(MirrorAlarm {
            mirror,
            core,
            for_secs: lasted.as_secs(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_brief_disagreement_is_not_a_divergence() {
        let mut w = MirrorWatch::new(Duration::from_secs(30));
        let t = Instant::now();
        assert_eq!(w.observe(5, 4, t), None);
        assert_eq!(w.observe(5, 4, t + Duration::from_secs(10)), None);
        assert_eq!(w.observe(5, 5, t + Duration::from_secs(20)), None);
        // The clock starts again after agreement.
        assert_eq!(w.observe(6, 5, t + Duration::from_secs(40)), None);
    }

    #[test]
    fn a_lasting_divergence_alarms_once_per_episode() {
        let mut w = MirrorWatch::new(Duration::from_secs(30));
        let t = Instant::now();
        assert_eq!(w.observe(7, 4, t), None);
        assert_eq!(
            w.observe(7, 4, t + Duration::from_secs(30)),
            Some(MirrorAlarm {
                mirror: 7,
                core: 4,
                for_secs: 30
            })
        );
        assert_eq!(w.observe(8, 4, t + Duration::from_secs(60)), None, "once");
        assert_eq!(w.observe(4, 4, t + Duration::from_secs(70)), None);
        // A new episode alarms again.
        assert_eq!(w.observe(3, 4, t + Duration::from_secs(80)), None);
        assert!(w.observe(3, 4, t + Duration::from_secs(110)).is_some());
    }
}
