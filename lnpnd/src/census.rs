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
//! After the columns and `tables` (their sum) come counts, not bytes:
//! `rss_kb`, `stored`, `peered`, `events_queued`, and the `dup` column by
//! its parts (order 1558): `processed_ids`, `departed_ids`, `origin_ids`,
//! the bound the three are held to, `dup_cap_ids`, and the origins' hash
//! table buckets, `origin_slots`, which name a table doubling. Then the
//! `store_index` column by its parts (order 1648): `store_index_entries`,
//! the live entries' own bytes, and `store_index_spare`, what the
//! containers allocate beyond them; the two sum to the column.
//!
//! The SIGUSR1 dump carries the latest line too, and its `tables` in the
//! dump's total (order 1648): the `gap:` line subtracts the role's tables
//! with the transport's, so its `unaccounted` is what neither counts
//! ([`CensusHandle::latest`]).
//!
//! After them, the node's transport tables (order 1586): `transport`, the
//! core storage's own byte estimate (the SIGUSR1 dump's tracked total),
//! `transport_entries` across its tables, `transport_unbounded`, the
//! tables with no ceiling, and `transport_over`, the tables above theirs.
//! The tables are the core's, not the engine's, so they are not a column:
//! [`FIELD_COLUMNS`] counts the engine's fields. A `PN_TRANSPORT` line
//! beside the census names each table with its fill and its ceiling
//! ([`TransportCensus`]).
//!
//! [`FIELD_COLUMNS`] maps every field of the structs that hold the daemon's
//! state to the column that counts it, and a unit test reads the structs'
//! source: a table added without a census column fails it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SendError, Sender};
use std::sync::{Arc, Mutex, PoisonError};

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
    /// The `dup` column by its parts, in ids (order 1558): processed ids
    /// held, departures held, synced messages' origins held.
    pub processed_ids: usize,
    pub departed_ids: usize,
    pub origin_ids: usize,
    /// The bound the three are held to, in ids: the role's processed-id
    /// and departure caps (`leviculum_lxmf::dup_caps`) and, for the
    /// origins, one per stored message.
    pub dup_cap_ids: usize,
    /// Buckets of the origins' hash table: a jump in `dup` with this
    /// doubled is the table resizing, not growth.
    pub origin_slots: usize,
    /// The `store_index` column by its parts (order 1648): the live
    /// entries' own bytes, and what the containers allocate beyond them.
    pub store_index_entries: usize,
    pub store_index_spare: usize,
    /// The node's transport tables (order 1586, [`TransportCensus`]).
    pub transport: TransportCensus,
}

/// The node's transport tables: what the core storage holds, table by
/// table, beside the ceiling it holds each to (order 1586).
///
/// lnpnd attaches to the host's shared instance and runs the local client
/// profile (`TableCaps::local_client`, order 1585); before that it carried
/// a full copy of the instance's tables, which no line of `PN_CENSUS`
/// showed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TransportCensus {
    /// The storage's own byte estimate, the dump's tracked total.
    pub bytes: u64,
    /// Every table: name, entries, ceiling.
    pub tables: Vec<(&'static str, usize, Option<usize>)>,
}

impl TransportCensus {
    /// Read the census off a core storage.
    pub fn of(storage: &impl leviculum_core::traits::Storage) -> Self {
        Self {
            bytes: storage.diagnostic_dump().1,
            tables: storage
                .collection_counts()
                .into_iter()
                .map(|row| (row.name, row.entries, row.capacity))
                .collect(),
        }
    }

    /// Entries across every table.
    pub fn entries(&self) -> usize {
        self.tables.iter().map(|(_, entries, _)| entries).sum()
    }

    /// Tables with no ceiling.
    pub fn unbounded(&self) -> usize {
        self.tables
            .iter()
            .filter(|(_, _, cap)| cap.is_none())
            .count()
    }

    /// Tables holding more than their ceiling.
    pub fn over(&self) -> usize {
        self.tables
            .iter()
            .filter(|(_, entries, cap)| cap.is_some_and(|cap| *entries > cap))
            .count()
    }

    /// `name:entries/ceiling` per table, `-` for no ceiling, comma
    /// separated: one whitespace-free token the event log keeps as is.
    pub fn detail(&self) -> String {
        self.tables
            .iter()
            .map(|(name, entries, cap)| match cap {
                Some(cap) => format!("{name}:{entries}/{cap}"),
                None => format!("{name}:{entries}/-"),
            })
            .collect::<Vec<_>>()
            .join(",")
    }
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
            " tables={} rss_kb={} stored={} peered={} events_queued={} processed_ids={} \
             departed_ids={} origin_ids={} dup_cap_ids={} origin_slots={} \
             store_index_entries={} store_index_spare={} transport={} \
             transport_entries={} transport_unbounded={} transport_over={}",
            self.tables(),
            self.rss_kb,
            self.stored,
            self.peered,
            self.events_queued,
            self.processed_ids,
            self.departed_ids,
            self.origin_ids,
            self.dup_cap_ids,
            self.origin_slots,
            self.store_index_entries,
            self.store_index_spare,
            self.transport.bytes,
            self.transport.entries(),
            self.transport.unbounded(),
            self.transport.over(),
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
            processed_ids = self.processed_ids,
            departed_ids = self.departed_ids,
            origin_ids = self.origin_ids,
            dup_cap_ids = self.dup_cap_ids,
            origin_slots = self.origin_slots,
            store_index_entries = self.store_index_entries,
            store_index_spare = self.store_index_spare,
            transport = self.transport.bytes,
            transport_entries = self.transport.entries(),
            transport_unbounded = self.transport.unbounded(),
            transport_over = self.transport.over(),
        );
        tracing::info!(
            event = "PN_TRANSPORT",
            occasion = occasion,
            bytes = self.transport.bytes,
            entries = self.transport.entries(),
            unbounded = self.transport.unbounded(),
            over = self.transport.over(),
            tables = self.transport.detail().as_str(),
        );
    }
}

/// `VmRSS` of this process in kB, shared with every daemon's census.
pub use leviculum_std::census::rss_kb;

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
    latest: Arc<Mutex<Latest>>,
}

/// The last census the engine took, and how many it has taken.
#[derive(Default)]
struct Latest {
    taken: u64,
    census: Option<PnCensus>,
}

impl CensusHandle {
    pub(crate) fn new(events: &EventSender) -> Self {
        Self {
            requested: Arc::default(),
            counts: Arc::clone(&events.counts),
            latest: Arc::default(),
        }
    }

    /// The engine took a census: keep it for the SIGUSR1 dump.
    pub(crate) fn publish(&self, census: &PnCensus) {
        let mut latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        latest.taken += 1;
        latest.census = Some(census.clone());
    }

    /// How many censuses the engine has taken, and the last of them.
    pub fn latest(&self) -> (u64, Option<PnCensus>) {
        let latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        (latest.taken, latest.census.clone())
    }

    /// The SIGUSR1 dump's section of the role (order 1648): the census
    /// line, and its `tables` as the bytes the dump adds to its total.
    /// `None` before the engine took one.
    pub fn dump_section(&self) -> Option<(String, u64)> {
        let census = self.latest().1?;
        Some((
            format!("{}\n", census.line()),
            u64::try_from(census.tables()).unwrap_or(u64::MAX),
        ))
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

/// Heap bytes of a hash table's spine, shared with every daemon's census.
pub(crate) use leviculum_std::census::hash_table_bytes;

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

    /// Order 1586: the line carries the transport block, and the detail
    /// names every table with its fill and its ceiling.
    #[test]
    fn the_line_carries_the_transport_tables_and_their_bounds() {
        let census = PnCensus {
            transport: TransportCensus {
                bytes: 4096,
                tables: vec![
                    ("known_identities", 812, Some(4096)),
                    ("path_table", 5000, Some(4096)),
                    ("remembered_at", 3, None),
                ],
            },
            ..PnCensus::default()
        };
        let line = census.line();
        assert!(
            line.ends_with(
                " transport=4096 transport_entries=5815 transport_unbounded=1 transport_over=1"
            ),
            "{line}"
        );
        assert_eq!(
            census.transport.detail(),
            "known_identities:812/4096,path_table:5000/4096,remembered_at:3/-"
        );
    }

    /// The census of a real client storage: every table bounded, none
    /// over (the local client profile, order 1585).
    #[test]
    fn a_client_storage_reports_every_table_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = leviculum_std::driver::StdStorage::new_with_caps(
            dir.path(),
            leviculum_core::memory_storage::TableCaps::local_client(),
        )
        .expect("storage");
        let transport = TransportCensus::of(&storage);
        assert!(transport.tables.len() > 10, "{transport:?}");
        assert_eq!(transport.unbounded(), 0, "{}", transport.detail());
        assert_eq!(transport.over(), 0);
        assert!(
            transport.detail().contains("known_identities:0/2048"),
            "{}",
            transport.detail()
        );
    }

    /// Order 1648: the dump's section is the last census the engine took,
    /// its line and its `tables`, and nothing before the first.
    #[test]
    fn the_dump_section_is_the_last_census_taken() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let handle = CensusHandle::new(&EventSender::from(tx));
        assert_eq!(handle.latest().0, 0);
        assert_eq!(handle.dump_section(), None);
        for store_index in [10, 7000] {
            handle.publish(&PnCensus {
                store_index,
                store_index_entries: store_index - 3,
                store_index_spare: 3,
                peers: 5,
                ..PnCensus::default()
            });
        }
        assert_eq!(handle.latest().0, 2);
        let (line, bytes) = handle.dump_section().expect("a census was taken");
        assert_eq!(bytes, 7005);
        assert!(
            line.starts_with("PN_CENSUS peers=5 store_index=7000 "),
            "{line}"
        );
        assert!(
            line.contains(" store_index_entries=6997 store_index_spare=3 "),
            "{line}"
        );
        assert!(line.ends_with('\n'), "{line:?}");
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
