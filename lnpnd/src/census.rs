//! The propagation role's heap census, `PN_CENSUS` (order 1515, LV-1000).
//!
//! lnpnd grew to 398 MB anonymous RSS on miauhaus and the OOM killer took
//! it twice in four days. The SIGUSR1 dump it shares with lnsd counts the
//! core's tables and the process's mappings, but not one table of the
//! propagation role, so the dump could say that the heap grew and not
//! which table grew. This line names them: one byte estimate per table the
//! daemon holds, the process's resident set beside it, every
//! [`CENSUS_PERIOD_MS`] into the event log and once more on SIGUSR1.
//!
//! The columns, all heap bytes except the counts at the end:
//!
//! | column | what it counts |
//! |---|---|
//! | `peers` | the peer table, its traffic counters and names, the inbound throttle |
//! | `store_index` | the message store's four indexes, each entry's path included |
//! | `store_writer` | appends and purges the store's writer has not confirmed |
//! | `sync_out` | the outbound round's offer plan |
//! | `sync_in` | payloads queued for the stamp worker and for the store |
//! | `dup` | the role's duplicate list: processed ids, departed ids; the synced messages' origins |
//! | `links` | per-link state: proofs owed, open links, validated links |
//! | `mailbox` | the own mailbox's router tables |
//! | `config` | lists read from the config: allow lists, ignored, static peers |
//! | `eventbuf` | engine events sent and not yet taken by the printer |
//!
//! [`FIELD_COLUMNS`] maps every field of the structs that hold the daemon's
//! state to the column that counts it, and a unit test reads the structs'
//! source: a table added without a census column fails it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SendError, Sender};
use std::sync::Arc;

use crate::engine::EngineEvent;

/// Spacing of the periodic `PN_CENSUS` line: the order's ten minutes.
pub const CENSUS_PERIOD_MS: u64 = 10 * 60 * 1000;

/// The columns of the line, in its order (module docs).
pub const COLUMNS: &[&str] = &[
    "peers",
    "store_index",
    "store_writer",
    "sync_out",
    "sync_in",
    "dup",
    "links",
    "mailbox",
    "config",
    "eventbuf",
];

/// Every field of the structs that hold the daemon's state, and the column
/// that counts it: `-` for a scalar or a fixed-size value with no heap
/// behind it, `*` for a field whose own struct is listed here field by
/// field, several columns joined by `+` for one that holds more than one
/// table.
pub const FIELD_COLUMNS: &[(&str, &str, &str)] = &[
    ("Engine", "state", "*"),
    ("Engine", "events", "eventbuf"),
    ("Engine", "pending_proofs", "links"),
    ("Engine", "inbound_links", "links"),
    ("Engine", "next_announce_at", "-"),
    ("Engine", "announce_interval_secs", "-"),
    ("Engine", "announce_delay_secs", "-"),
    ("Engine", "start_announce_owed", "-"),
    ("Engine", "next_announce_occasion", "-"),
    ("Engine", "sync_interval_ms", "-"),
    ("Engine", "rotation_interval_ms", "-"),
    ("Engine", "next_maintenance_at", "-"),
    ("Engine", "expiry_backlog", "-"),
    ("Engine", "validation", "sync_in"),
    ("Engine", "pending", "sync_in"),
    ("Engine", "unproven", "links"),
    ("Engine", "profile", "-"),
    ("Engine", "last_profile", "-"),
    ("Engine", "departures_path", "-"),
    ("Engine", "departures_dirty", "-"),
    ("Engine", "status_asked_at", "links"),
    ("Engine", "census", "-"),
    ("Engine", "next_census_at", "-"),
    ("Ready", "node", "store_index+store_writer+dup"),
    ("Ready", "destination_hash", "-"),
    ("Ready", "min_cost", "-"),
    ("Ready", "peering", "*"),
    ("Ready", "control_hash", "-"),
    ("Ready", "control_allowed", "config"),
    ("Ready", "auth_allowed", "config"),
    ("Ready", "mailbox", "*"),
    ("Ready", "started_unix", "-"),
    ("Ready", "client_received", "-"),
    ("Ready", "client_served", "-"),
    ("Ready", "store_limit_bytes", "-"),
    ("Ready", "delivery_limit_kb", "-"),
    ("PeeringRuntime", "table", "peers"),
    ("PeeringRuntime", "gate", "peers"),
    ("PeeringRuntime", "origins", "dup"),
    ("PeeringRuntime", "peer_store", "-"),
    ("PeeringRuntime", "our_identity", "-"),
    ("PeeringRuntime", "our_identity_hash", "-"),
    ("PeeringRuntime", "validated_links", "links"),
    ("PeeringRuntime", "inbound_transfers", "links"),
    ("PeeringRuntime", "inbound_link_facts", "links"),
    ("PeeringRuntime", "store_full", "-"),
    ("PeeringRuntime", "outbound", "sync_out"),
    ("PeeringRuntime", "mining", "-"),
    ("PeeringRuntime", "last_synced", "-"),
    ("PeeringRuntime", "next_sync_at_ms", "-"),
    ("PeeringRuntime", "sync_interval_ms", "-"),
    ("PeeringRuntime", "next_rotation_at_ms", "-"),
    ("PeeringRuntime", "rotation_interval_ms", "-"),
    ("PeeringRuntime", "traffic", "peers"),
    ("PeeringRuntime", "names", "peers"),
    ("PeeringRuntime", "unpeered_incoming", "-"),
    ("PeeringRuntime", "unpeered_rx_bytes", "-"),
    ("PeeringRuntime", "forced_next", "-"),
    ("PeeringRuntime", "static_paths_pending", "config"),
    ("PeeringRuntime", "events", "eventbuf"),
    ("MailboxRuntime", "router", "mailbox"),
    ("MailboxRuntime", "delivery_hash", "-"),
    ("MailboxRuntime", "config", "config"),
    ("MailboxRuntime", "announce_schedule", "-"),
    ("MailboxRuntime", "events", "eventbuf"),
];

/// One census: bytes per column, then the counts that read them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PnCensus {
    pub peers: usize,
    pub store_index: usize,
    pub store_writer: usize,
    pub sync_out: usize,
    pub sync_in: usize,
    pub dup: usize,
    pub links: usize,
    pub mailbox: usize,
    pub config: usize,
    pub eventbuf: usize,
    /// `VmRSS` of the process in kB, 0 where `/proc` cannot say.
    pub rss_kb: u64,
    /// Messages in the store.
    pub stored: usize,
    /// Peers in the table.
    pub peered: usize,
    /// Engine events sent and not yet taken.
    pub events_queued: usize,
}

impl PnCensus {
    /// The columns' values in [`COLUMNS`] order.
    pub fn columns(&self) -> [(&'static str, usize); 10] {
        [
            ("peers", self.peers),
            ("store_index", self.store_index),
            ("store_writer", self.store_writer),
            ("sync_out", self.sync_out),
            ("sync_in", self.sync_in),
            ("dup", self.dup),
            ("links", self.links),
            ("mailbox", self.mailbox),
            ("config", self.config),
            ("eventbuf", self.eventbuf),
        ]
    }

    /// The sum of the columns.
    pub fn tables(&self) -> usize {
        self.columns().iter().map(|(_, bytes)| bytes).sum()
    }

    /// The line as the event log and the soak print it.
    pub fn line(&self) -> String {
        let mut line = String::from("PN_CENSUS");
        for (name, bytes) in self.columns() {
            line.push_str(&format!(" {name}={bytes}"));
        }
        line.push_str(&format!(
            " tables={} rss_kb={} stored={} peered={} events_queued={}",
            self.tables(),
            self.rss_kb,
            self.stored,
            self.peered,
            self.events_queued
        ));
        line
    }

    /// Write the census into the structured log, one `PN_CENSUS` event.
    pub fn log(&self, occasion: &'static str) {
        tracing::info!(
            event = "PN_CENSUS",
            occasion = occasion,
            peers = self.peers,
            store_index = self.store_index,
            store_writer = self.store_writer,
            sync_out = self.sync_out,
            sync_in = self.sync_in,
            dup = self.dup,
            links = self.links,
            mailbox = self.mailbox,
            config = self.config,
            eventbuf = self.eventbuf,
            tables = self.tables(),
            rss_kb = self.rss_kb,
            stored = self.stored,
            peered = self.peered,
            events_queued = self.events_queued,
        );
    }
}

/// `VmRSS` of this process in kB, 0 where `/proc/self/status` cannot say.
pub fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
        })
        .unwrap_or(0)
}

/// Engine events sent and taken, for the `eventbuf` column.
#[derive(Debug, Default)]
struct EventCounts {
    sent: AtomicUsize,
    taken: AtomicUsize,
}

/// The engine's event channel, counting what it sends, so the census can
/// say how many events wait for the printer: the channel is unbounded, and
/// a printer blocked on its stderr would grow it without a trace.
#[derive(Clone)]
pub struct EventSender {
    tx: Sender<EngineEvent>,
    counts: Arc<EventCounts>,
}

impl EventSender {
    pub fn send(&self, event: EngineEvent) -> Result<(), SendError<EngineEvent>> {
        self.tx.send(event)?;
        self.counts.sent.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Events sent and not yet marked taken ([`CensusHandle::event_taken`]).
    pub fn queued(&self) -> usize {
        let sent = self.counts.sent.load(Ordering::Relaxed);
        sent.saturating_sub(self.counts.taken.load(Ordering::Relaxed))
    }
}

impl From<Sender<EngineEvent>> for EventSender {
    fn from(tx: Sender<EngineEvent>) -> Self {
        Self {
            tx,
            counts: Arc::default(),
        }
    }
}

/// What the daemon's main loop holds of the census: a SIGUSR1 asks for a
/// line at the next tick, and the event printer marks what it has taken.
#[derive(Clone)]
pub struct CensusHandle {
    requested: Arc<AtomicBool>,
    counts: Arc<EventCounts>,
}

impl CensusHandle {
    pub(crate) fn new(events: &EventSender) -> Self {
        Self {
            requested: Arc::default(),
            counts: Arc::clone(&events.counts),
        }
    }

    /// Ask for a `PN_CENSUS` line at the engine's next tick.
    pub fn request(&self) {
        self.requested.store(true, Ordering::Relaxed);
    }

    /// Whether a line was asked for, clearing the request.
    pub(crate) fn take_request(&self) -> bool {
        self.requested.swap(false, Ordering::Relaxed)
    }

    /// One engine event left the channel.
    pub fn event_taken(&self) {
        self.counts.taken.fetch_add(1, Ordering::Relaxed);
    }
}

/// Heap bytes of a `HashMap` or `HashSet` spine: the table's buckets and
/// their control bytes, at its capacity.
pub(crate) fn hash_table_bytes(capacity: usize, entry_size: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    // hashbrown allocates a power of two of buckets at 7/8 load, one
    // control byte a bucket.
    let buckets = (capacity * 8 / 7).next_power_of_two();
    buckets * (entry_size + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The fields of `struct <name>` in `source`, by name, with their types.
    fn struct_fields(source: &str, name: &str) -> Vec<(String, String)> {
        let start = [
            format!("pub struct {name}<"),
            format!("pub struct {name} {{"),
            format!("pub(crate) struct {name} {{"),
            format!("struct {name}<"),
            format!("struct {name} {{"),
        ]
        .iter()
        .find_map(|head| source.find(head.as_str()))
        .unwrap_or_else(|| panic!("struct {name} not found"));
        let body_start = start + source[start..].find('{').expect("struct body") + 1;
        let mut depth = 1;
        let mut end = body_start;
        for (offset, ch) in source[body_start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = body_start + offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        source[body_start..end]
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("//") && !line.starts_with('#'))
            .filter_map(|line| {
                let line = line.strip_prefix("pub(crate) ").unwrap_or(line);
                let line = line.strip_prefix("pub ").unwrap_or(line);
                let (field, ty) = line.split_once(':')?;
                let field = field.trim();
                if field.is_empty() || !field.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    return None;
                }
                Some((field.to_string(), ty.trim().to_string()))
            })
            .collect()
    }

    /// A type that holds heap behind it, which no `-` may name.
    fn holds_a_table(ty: &str) -> bool {
        [
            "HashMap",
            "HashSet",
            "BTreeMap",
            "BTreeSet",
            "Vec<",
            "VecDeque",
            "PropagationNode",
            "PeerTable",
            "InboundGate",
            "LxmfRouter",
            "Sender",
            "Worker",
            "OutboundSync",
            "MailboxConfig",
        ]
        .iter()
        .any(|kind| ty.contains(kind))
    }

    /// Every field of every state-holding struct has a census column, and
    /// every column it names is one the line prints: a table added without
    /// a census entry fails here, not on a field node.
    #[test]
    fn the_census_line_names_every_table_the_daemon_holds() {
        let sources = [
            ("Engine", include_str!("engine.rs")),
            ("Ready", include_str!("engine.rs")),
            ("PeeringRuntime", include_str!("peering.rs")),
            ("MailboxRuntime", include_str!("mailbox.rs")),
        ];
        let mut missing = Vec::new();
        let mut unbacked = Vec::new();
        let mut seen = BTreeSet::new();
        for (name, source) in sources {
            for (field, ty) in struct_fields(source, name) {
                seen.insert((name, field.clone()));
                match FIELD_COLUMNS
                    .iter()
                    .find(|(owner, listed, _)| *owner == name && *listed == field)
                {
                    None => missing.push(format!("{name}.{field}: {ty}")),
                    Some((_, _, "-")) if holds_a_table(&ty) => {
                        unbacked.push(format!("{name}.{field}: {ty}"))
                    }
                    Some(_) => {}
                }
            }
        }
        assert!(
            missing.is_empty(),
            "fields without a census column (add them to FIELD_COLUMNS and count them): {missing:#?}"
        );
        assert!(
            unbacked.is_empty(),
            "fields marked `-` that hold heap: {unbacked:#?}"
        );
        let stale: Vec<_> = FIELD_COLUMNS
            .iter()
            .filter(|(owner, field, _)| !seen.contains(&(*owner, field.to_string())))
            .collect();
        assert!(stale.is_empty(), "census entries for no field: {stale:?}");
        let line = PnCensus::default().line();
        for (_, _, columns) in FIELD_COLUMNS {
            for column in columns.split('+').filter(|c| *c != "-" && *c != "*") {
                assert!(COLUMNS.contains(&column), "unknown column {column}");
                assert!(line.contains(&format!(" {column}=")), "{line}");
            }
        }
        for column in COLUMNS {
            assert!(
                FIELD_COLUMNS
                    .iter()
                    .any(|(_, _, columns)| columns.split('+').any(|c| c == *column)),
                "column {column} counts no field"
            );
        }
    }

    #[test]
    fn the_line_carries_every_column_and_the_rss() {
        let census = PnCensus {
            peers: 1,
            store_index: 2,
            dup: 3,
            rss_kb: 4,
            ..PnCensus::default()
        };
        let line = census.line();
        assert!(
            line.starts_with("PN_CENSUS peers=1 store_index=2 "),
            "{line}"
        );
        assert!(line.contains(" dup=3 "), "{line}");
        assert!(line.contains(" tables=6 rss_kb=4 "), "{line}");
    }

    #[test]
    fn the_event_backlog_counts_sent_minus_taken() {
        let (tx, rx) = std::sync::mpsc::channel();
        let events = EventSender::from(tx);
        let handle = CensusHandle::new(&events);
        for _ in 0..3 {
            events.send(EngineEvent::Announced).expect("open channel");
        }
        assert_eq!(events.queued(), 3);
        rx.recv().expect("an event");
        handle.event_taken();
        assert_eq!(events.queued(), 2);
    }
}
