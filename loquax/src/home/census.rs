//! The mailbox's heap census, `MAILBOX_CENSUS` (order 1585).
//!
//! loquax-mailbox held about 630 MiB (RSS and swap) on miauhaus after 28 h
//! for a personal mailbox with a 9.8 MB history, and grew about 22 MB/h. A
//! SIGUSR1 killed it, so nothing could say which table grew. This line
//! names them, as lnpnd's `PN_CENSUS` does for the propagation role: one
//! byte estimate per table the daemon holds, every [`CENSUS_PERIOD`] into
//! the log and on SIGUSR1, followed on SIGUSR1 by every embedded node's
//! transport census and the process sections lnsd's dump has (allocator,
//! mappings, the `gap:` line), through
//! [`leviculum_std::census::compose_multi_node_dump`].
//!
//! The columns, heap bytes:
//!
//! | column | what it counts |
//! |---|---|
//! | `outbox` | the home's own outbox, each packed message included |
//! | `group_outbox` | the hosted groups' copies in flight, the same way |
//! | `sender` | the messages told they have no node, the nodes of the propagated legs, the proofs owed to writers |
//! | `relations` | the related peers and the names heard ([`super::relation::HEARD_CAP`]) |
//! | `asker` | the uploads asked about at their propagation node |
//! | `wakes` | the watching links |
//! | `costs` | the device links and their keepalive ledger |
//! | `pn_asks` | the `/lev/status` questions in flight |
//! | `router` | every engine's LXMF router tables ([`leviculum_lxmf::router::LxmfRouter::heap_bytes`]) |
//! | `engine` | every engine's own maps and queues |
//!
//! After `tables` (their sum) come counts: `rss_kb`, `nodes` (the embedded
//! nodes: the home's and one per hosted group), `outbox_n`, `group_outbox_n`,
//! `told_n`, `related_n`, `heard_n`, `asker_n`, `watchers_n`, `devices_n`,
//! `tracked_n`, `recipients_n`, `unheard_n`, `events_queued`, the
//! archive's `archive_kb` on disk (SQLite pages are not heap; its cache is
//! bounded by SQLite's own `cache_size`), `answered`, how many engines'
//! answers the line holds (one under [`ANSWER_WAIT`] passed without them),
//! and two wake counts since the start (order 1723): `turns`, the daemon
//! loop's, and `engine_ticks`, the answering engines' ticks summed. An idle
//! mailbox wakes a few times a second; a loop or a tick deadline that spins
//! shows as a count far above that, measured in wakes and not in CPU time,
//! so the host's speed does not move it.
//!
//! [`RUN_LOCALS`] maps every `let mut` of the daemon's `run` to the column
//! that counts it, and a unit test reads `run` out of its source: a table
//! added there without a census column fails it.

use std::time::Duration;

/// Spacing of the periodic `MAILBOX_CENSUS` line: ten minutes, lnpnd's.
pub const CENSUS_PERIOD: Duration = Duration::from_secs(10 * 60);

/// How long the dump waits for the engines' answers to a census request
/// before it prints what it has.
pub const ANSWER_WAIT: Duration = Duration::from_secs(1);

/// The columns of the line, in its order (module docs).
pub const COLUMNS: &[&str] = &[
    "outbox",
    "group_outbox",
    "sender",
    "relations",
    "asker",
    "wakes",
    "costs",
    "pn_asks",
    "router",
    "engine",
];

/// Every `let mut` binding of `daemon::run`, and the column that counts
/// it: `-` for one with no heap of its own or one counted elsewhere (the
/// nodes are the transport census's), `*` for the group engines, whose
/// outboxes, routers and engines are the `group_outbox`, `router` and
/// `engine` columns.
pub const RUN_LOCALS: &[(&str, &str)] = &[
    ("store", "-"),
    ("relations", "relations"),
    ("outgoing", "outbox"),
    ("asker", "asker"),
    ("attached", "-"),
    ("serving", "-"),
    ("sender", "sender"),
    ("groups", "*"),
    ("wakes", "wakes"),
    ("costs", "costs"),
    ("costs_look", "-"),
    ("next_summary", "-"),
    ("outside_version", "-"),
    ("fanout", "-"),
    ("schedule", "-"),
    ("fetch", "-"),
    ("pn_round", "-"),
    ("pn_asks", "pn_asks"),
    ("selected_pn", "-"),
    ("engine_ready", "-"),
    ("next_pump", "-"),
    ("quiet_pumps", "-"),
    ("outbox_tick", "-"),
    ("outside", "-"),
    ("next_announce", "-"),
    ("next_group_announce", "-"),
    ("terminate", "-"),
    ("stop", "-"),
    ("census", "-"),
    ("next_census", "-"),
    ("census_ask", "-"),
    ("usr1", "-"),
    ("unstarted", "-"),
    ("turns", "-"),
];

/// One census: bytes per column, then the counts.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MailboxCensus {
    pub outbox: usize,
    pub group_outbox: usize,
    pub sender: usize,
    pub relations: usize,
    pub asker: usize,
    pub wakes: usize,
    pub costs: usize,
    pub pn_asks: usize,
    pub router: usize,
    pub engine: usize,
    pub rss_kb: u64,
    pub nodes: usize,
    pub outbox_n: usize,
    pub group_outbox_n: usize,
    pub told_n: usize,
    pub related_n: usize,
    pub heard_n: usize,
    pub asker_n: usize,
    pub watchers_n: usize,
    pub devices_n: usize,
    pub tracked_n: usize,
    pub recipients_n: usize,
    pub unheard_n: usize,
    pub events_queued: usize,
    pub archive_kb: u64,
    pub answered: usize,
    pub turns: u64,
    pub engine_ticks: u64,
}

impl MailboxCensus {
    /// The columns' values in [`COLUMNS`] order.
    pub fn columns(&self) -> [(&'static str, usize); 10] {
        [
            ("outbox", self.outbox),
            ("group_outbox", self.group_outbox),
            ("sender", self.sender),
            ("relations", self.relations),
            ("asker", self.asker),
            ("wakes", self.wakes),
            ("costs", self.costs),
            ("pn_asks", self.pn_asks),
            ("router", self.router),
            ("engine", self.engine),
        ]
    }

    /// The sum of the columns.
    pub fn tables(&self) -> usize {
        self.columns().iter().map(|(_, bytes)| bytes).sum()
    }

    /// Fold one engine's answer in.
    pub fn engine(&mut self, answer: &crate::outbox::EngineCensus) {
        self.router += answer.router_bytes;
        self.engine += answer.engine_bytes;
        self.tracked_n += answer.tracked;
        self.recipients_n += answer.recipients;
        self.unheard_n += answer.unheard;
        self.events_queued += answer.events_queued;
        self.engine_ticks = self.engine_ticks.saturating_add(answer.ticks);
        self.answered += 1;
    }

    /// The line as the log and the soak print it.
    pub fn line(&self) -> String {
        let mut line = String::from("MAILBOX_CENSUS");
        for (name, bytes) in self.columns() {
            line.push_str(&format!(" {name}={bytes}"));
        }
        line.push_str(&format!(
            " tables={} rss_kb={} nodes={} outbox_n={} group_outbox_n={} told_n={} \
             related_n={} heard_n={} asker_n={} watchers_n={} devices_n={} tracked_n={} \
             recipients_n={} unheard_n={} events_queued={} archive_kb={} answered={} \
             turns={} engine_ticks={}",
            self.tables(),
            self.rss_kb,
            self.nodes,
            self.outbox_n,
            self.group_outbox_n,
            self.told_n,
            self.related_n,
            self.heard_n,
            self.asker_n,
            self.watchers_n,
            self.devices_n,
            self.tracked_n,
            self.recipients_n,
            self.unheard_n,
            self.events_queued,
            self.archive_kb,
            self.answered,
            self.turns,
            self.engine_ticks,
        ));
        line
    }
}

impl MailboxCensus {
    /// Write the census into the structured log, one `MAILBOX_CENSUS`
    /// event; `occasion` is `period` or `signal`.
    pub fn log(&self, occasion: &'static str) {
        tracing::info!(
            event = "MAILBOX_CENSUS",
            occasion = occasion,
            outbox = self.outbox,
            group_outbox = self.group_outbox,
            sender = self.sender,
            relations = self.relations,
            asker = self.asker,
            wakes = self.wakes,
            costs = self.costs,
            pn_asks = self.pn_asks,
            router = self.router,
            engine = self.engine,
            tables = self.tables(),
            rss_kb = self.rss_kb,
            nodes = self.nodes,
            outbox_n = self.outbox_n,
            group_outbox_n = self.group_outbox_n,
            told_n = self.told_n,
            related_n = self.related_n,
            heard_n = self.heard_n,
            asker_n = self.asker_n,
            watchers_n = self.watchers_n,
            devices_n = self.devices_n,
            tracked_n = self.tracked_n,
            recipients_n = self.recipients_n,
            unheard_n = self.unheard_n,
            events_queued = self.events_queued,
            archive_kb = self.archive_kb,
            answered = self.answered,
            turns = self.turns,
            engine_ticks = self.engine_ticks,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The `let mut` bindings of `pub async fn run` in `source`, up to the
    /// end of its body.
    fn run_locals(source: &str) -> Vec<String> {
        let start = source
            .find("pub async fn run(")
            .expect("daemon::run in its source");
        let body = &source[start..];
        let open = body.find('{').expect("run's body");
        let mut depth = 0usize;
        let mut end = body.len();
        for (offset, ch) in body[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        body[open..end]
            .lines()
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("let mut ")?;
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                (!name.is_empty()).then_some(name)
            })
            .collect()
    }

    /// Every `let mut` of the daemon's loop has a census column, and every
    /// column is one the line prints and some binding feeds: a table added
    /// to the loop without a census entry fails here.
    #[test]
    fn the_census_line_names_every_table_the_mailbox_holds() {
        let locals = run_locals(include_str!("daemon.rs"));
        assert!(locals.len() > 10, "run's bindings read: {locals:?}");
        let missing: Vec<_> = locals
            .iter()
            .filter(|name| !RUN_LOCALS.iter().any(|(listed, _)| listed == name))
            .collect();
        assert!(
            missing.is_empty(),
            "bindings of daemon::run without a census column (add them to RUN_LOCALS and count them): {missing:?}"
        );
        let seen: BTreeSet<&str> = locals.iter().map(String::as_str).collect();
        let stale: Vec<_> = RUN_LOCALS
            .iter()
            .filter(|(name, _)| !seen.contains(name))
            .collect();
        assert!(stale.is_empty(), "census entries for no binding: {stale:?}");
        let line = MailboxCensus::default().line();
        for column in COLUMNS {
            assert!(line.contains(&format!(" {column}=")), "{line}");
        }
        // `router` and `engine` are fed by the group engines and the
        // home's own; the rest by a binding each.
        for column in COLUMNS
            .iter()
            .filter(|c| !matches!(**c, "router" | "engine" | "group_outbox"))
        {
            assert!(
                RUN_LOCALS.iter().any(|(_, listed)| listed == column),
                "column {column} counts no binding"
            );
        }
    }

    /// The control: a loop that grows an uncounted table fails the guard.
    #[test]
    fn an_uncounted_binding_is_named() {
        let source = "pub async fn run(x: u8) {\n    let mut ledger = Vec::new();\n    let mut store = 1;\n}\n";
        let locals = run_locals(source);
        assert_eq!(locals, ["ledger", "store"]);
        assert!(!RUN_LOCALS.iter().any(|(name, _)| *name == "ledger"));
    }

    #[test]
    fn the_line_sums_the_columns_and_folds_the_engines() {
        let mut census = MailboxCensus {
            outbox: 10,
            relations: 5,
            ..MailboxCensus::default()
        };
        census.engine(&crate::outbox::EngineCensus {
            router_bytes: 100,
            engine_bytes: 7,
            tracked: 2,
            ..Default::default()
        });
        census.engine(&crate::outbox::EngineCensus {
            router_bytes: 1,
            ticks: 5,
            ..Default::default()
        });
        census.turns = 9;
        let line = census.line();
        assert!(line.starts_with("MAILBOX_CENSUS outbox=10 "), "{line}");
        assert!(line.contains(" router=101 engine=7 tables=123 "), "{line}");
        assert!(line.contains(" tracked_n=2 "), "{line}");
        assert!(
            line.ends_with(" answered=2 turns=9 engine_ticks=5"),
            "{line}"
        );
    }

    /// 1732: a census without answers says so and counts no tick; two
    /// answers are counted, and their ticks sum without wrapping.
    #[test]
    fn the_tail_counts_the_answers_and_saturates_the_ticks() {
        let mut census = MailboxCensus {
            turns: 3,
            ..MailboxCensus::default()
        };
        let line = census.line();
        assert!(
            line.ends_with(" answered=0 turns=3 engine_ticks=0"),
            "{line}"
        );
        census.engine(&crate::outbox::EngineCensus {
            ticks: u64::MAX,
            ..Default::default()
        });
        census.engine(&crate::outbox::EngineCensus {
            ticks: 7,
            ..Default::default()
        });
        let line = census.line();
        assert!(
            line.ends_with(" answered=2 turns=3 engine_ticks=18446744073709551615"),
            "{line}"
        );
    }
}
