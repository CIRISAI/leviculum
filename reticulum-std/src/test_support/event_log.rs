//! Structured event-log subscriber for mvr / integration tests.
//!
//! # Format
//!
//! Each emitted event renders to a single line in canonical form:
//!
//! ```text
//! EVENT_NAME node=<n> key1=val1 key2=val2 ... t=<rel-ms>
//! ```
//!
//! - `EVENT_NAME` = string value of the `event` field passed to
//!   `tracing::debug!(event = "FOO", ...)`.
//! - `node=` is reserved (Stage 6 commit 3 wires it from
//!   `LEVICULUM_EVENT_NODE`).  Currently always `node=local`.
//! - All other fields are alphabetically sorted.
//! - `t=` is always last; relative milliseconds since the layer was
//!   registered (process-global init time).
//!
//! Records that do not carry an `event = "..."` field are silently
//! ignored — the legacy printf-style `tracing::debug!("[FOO] ...")`
//! sites stay compatible.
//!
//! # Architecture: process-global layer + active-handles list
//!
//! A single [`EventLogLayer`] is registered once in the process via
//! [`crate::test_support::tracing_setup::init_tracing_with_event_log`]
//! — driven by `Once`.  All threads, including `tokio::test(multi_thread)`
//! workers, route events through it.
//!
//! Per-test buffer isolation is built on top of the global layer: an
//! [`EventLogHandle`] returned from [`init_event_log`] (or its
//! variants) registers an `ActiveHandle` in the layer's shared list.
//! `on_event` iterates the active list and pushes the formatted line
//! to every active buffer.  When the test's handle drops, it removes
//! itself from the list.
//!
//! Concurrency consequence: every active buffer receives every event,
//! regardless of which test emitted it.  Tests that assert on buffer
//! contents must filter by event name to avoid cross-test
//! pollution.  Use disjoint event names per test (`EV_BASIC`,
//! `EV_VIOLATION`, …); mvr tests already enforce
//! `--test-threads=1` so this only affects unit tests.
//!
//! # Validation
//!
//! Two violation classes, both non-blocking — original event lines
//! are never suppressed.
//!
//! ## Schema validation (per-handle)
//!
//! [`EVENT_CATALOG`] declares required keys per event name.
//! Per consumed event, the layer iterates each active handle's
//! catalogue (production + the handle's `extra_schemas`).  If the
//! event is catalogued and required keys are missing, a synthetic
//! line is appended to that handle's buffer:
//!
//! ```text
//! EVENT_SCHEMA_VIOLATION event=PKT_RX missing=[hops,len] caller=transport.rs:1049 t=<rel-ms>
//! ```
//!
//! ## Field-value validation (per-event, per-handle)
//!
//! Token-based parsers (Stage 7's `jl`/`jldiff`) split lines on
//! whitespace.  Field values containing whitespace, `=`, or
//! non-printable characters break that contract.  The visitor
//! detects them and the layer emits one synthetic line per
//! offending field, into every active buffer:
//!
//! ```text
//! EVENT_FIELD_VIOLATION event=PKT_RX field=note value_problem=whitespace caller=transport.rs:1049 t=<rel-ms>
//! ```
//!
//! `value_problem` ∈ {`whitespace`, `equals`, `non_printable`}.
//!
//! # Wiring a test
//!
//! ```ignore
//! use reticulum_std::test_support::event_log::init_event_log;
//!
//! #[tokio::test]
//! async fn my_mvr() {
//!     let _evlog = init_event_log();
//!     // ... test body emits tracing::debug!(event = "...", ...) calls ...
//! }
//! ```
//!
//! On `Drop`, if the thread is panicking, the buffer dumps to
//! stderr (or to the configured file via [`init_event_log_to_file`])
//! with a `=== EVENT LOG DUMP …` banner.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Registry};

const NODE_ENV_VAR: &str = "LEVICULUM_EVENT_NODE";
const LOG_FILE_ENV_VAR: &str = "LEVICULUM_EVENT_LOG";

/// Schema for one structured event.  Declares the keys that MUST be
/// present on every emission of this event name.
pub struct EventSchema {
    pub name: &'static str,
    pub required_keys: &'static [&'static str],
}

/// Production catalogue.  Stage 6 commit 4 expands this to one
/// entry per converted call site; until then only `PKT_RX` is
/// catalogued.  Adding entries without a live emitter is the
/// "stale catalogue" failure mode (Variant 3 can't detect it),
/// so we keep this minimal.
pub const EVENT_CATALOG: &[EventSchema] = &[EventSchema {
    name: "PKT_RX",
    required_keys: &["iface", "type", "dst", "hops", "len"],
}];

/// Where the buffer is dumped on a panicking drop.
enum DumpTarget {
    Stderr,
    File(PathBuf),
}

/// Per-handle bookkeeping kept in the layer's active list.
struct ActiveHandle {
    buffer: Arc<Mutex<Vec<String>>>,
    extra_schemas: &'static [EventSchema],
}

/// Handle returned from [`init_event_log`] / variants.  While alive,
/// every event the global layer sees is appended to `buffer`.  On
/// drop, the handle removes itself from the layer's active list and
/// — if the thread is panicking — dumps the buffer to the configured
/// target.
pub struct EventLogHandle {
    buffer: Arc<Mutex<Vec<String>>>,
    dump_target: DumpTarget,
    /// Reference to the layer's shared active-handles list, used by
    /// `Drop` to remove this handle's entry.
    active: Arc<Mutex<Vec<ActiveHandle>>>,
}

impl EventLogHandle {
    /// Snapshot the current buffer.  Useful for assertions in
    /// non-panicking tests.  Other parallel tests may have
    /// contributed lines — filter by event name.
    pub fn dump(&self) -> Vec<String> {
        self.buffer.lock().unwrap().clone()
    }
}

impl Drop for EventLogHandle {
    fn drop(&mut self) {
        // Remove our active entry first so subsequent events don't
        // race against a partly-torn-down handle.
        if let Ok(mut active) = self.active.lock() {
            active.retain(|h| !Arc::ptr_eq(&h.buffer, &self.buffer));
        }

        if !std::thread::panicking() {
            return;
        }

        let buffer = self.buffer.lock().unwrap();
        let body = buffer.join("\n");
        let dump = format!(
            "=== EVENT LOG DUMP (test panicked, {} lines) ===\n{}\n=== END EVENT LOG DUMP ===\n",
            buffer.len(),
            body,
        );
        match &self.dump_target {
            DumpTarget::Stderr => eprintln!("{dump}"),
            DumpTarget::File(p) => {
                // Best-effort — failure to write the dump must not
                // shadow the original panic.
                let _ = std::fs::write(p, dump);
            }
        }
    }
}

/// Initialise the subscriber with the production catalogue.
/// Panic-dump goes to stderr.
#[must_use]
pub fn init_event_log() -> EventLogHandle {
    init_inner(DumpTarget::Stderr, &[])
}

/// Initialise the subscriber with the production catalogue.
/// Panic-dump is written to `path` instead of stderr.  Useful when
/// a test wants to assert on the dumped content.
#[must_use]
pub fn init_event_log_to_file(path: PathBuf) -> EventLogHandle {
    init_inner(DumpTarget::File(path), &[])
}

/// Test-only entry point: extends the production catalogue with the
/// supplied extra schemas (per-handle, not global).  Used by
/// `tests/event_log_subscriber.rs` to exercise the schema-violation
/// path with synthetic event names that don't pollute the production
/// catalogue.
#[doc(hidden)]
#[must_use]
pub fn init_event_log_with_extra_schemas(
    extra: &'static [EventSchema],
    file_path: Option<PathBuf>,
) -> EventLogHandle {
    let target = match file_path {
        Some(p) => DumpTarget::File(p),
        None => DumpTarget::Stderr,
    };
    init_inner(target, extra)
}

fn init_inner(dump_target: DumpTarget, extra_schemas: &'static [EventSchema]) -> EventLogHandle {
    // Make sure the global layer is installed (idempotent via Once).
    crate::test_support::tracing_setup::init_tracing_with_event_log();

    let buffer: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let active = active_list();
    {
        let mut list = active.lock().unwrap();
        list.push(ActiveHandle {
            buffer: Arc::clone(&buffer),
            extra_schemas,
        });
    }
    EventLogHandle {
        buffer,
        dump_target,
        active: Arc::clone(active),
    }
}

/// Build the layer used by `tracing_setup::init_tracing_with_event_log`.
/// One global layer per process; the active-handles list it owns is
/// shared with every [`EventLogHandle`] via [`active_list`].
pub fn layer() -> EventLogLayer {
    EventLogLayer {
        active: Arc::clone(active_list()),
        init_time: Instant::now(),
    }
}

/// Global active-handles list.  Lazily allocated on first access so
/// the order of `tracing_setup::init_tracing_with_event_log` and
/// `init_event_log` doesn't matter.
fn active_list() -> &'static Arc<Mutex<Vec<ActiveHandle>>> {
    static ACTIVE: OnceLock<Arc<Mutex<Vec<ActiveHandle>>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Arc::new(Mutex::new(Vec::new())))
}

/// Process-wide node identifier.  Read from `LEVICULUM_EVENT_NODE`
/// at first access; defaults to `local`.  Cached via OnceLock so
/// every emitted event gets a consistent prefix even if the env
/// changes mid-run.
fn node_name() -> &'static str {
    static NODE: OnceLock<String> = OnceLock::new();
    NODE.get_or_init(|| std::env::var(NODE_ENV_VAR).unwrap_or_else(|_| "local".to_string()))
}

/// Process-wide append-only event log file.  Returns `Some` only when
/// `LEVICULUM_EVENT_LOG=<path>` is set in the environment at first
/// access AND the file opens successfully.  Cached via OnceLock so
/// the env-var lookup + file-open happens exactly once per process.
fn event_log_file() -> Option<&'static Mutex<File>> {
    static FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    FILE.get_or_init(|| {
        std::env::var(LOG_FILE_ENV_VAR).ok().and_then(|p| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
                .map(Mutex::new)
        })
    })
    .as_ref()
}

/// Read every input file as text, parse the trailing `t=<n>` token of
/// each non-empty line, and return all lines sorted by parsed `n`
/// (stable on tie).  Lines that fail `t=` parsing sort to the end
/// with a synthetic timestamp of `u128::MAX`, preserving their
/// relative order.
pub fn merge_event_logs(paths: &[PathBuf]) -> Vec<String> {
    let mut lines: Vec<(u128, usize, String)> = Vec::new();
    let mut tie_breaker: usize = 0;
    for path in paths {
        let Ok(file) = File::open(path) else { continue };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            let t = parse_t(&line).unwrap_or(u128::MAX);
            lines.push((t, tie_breaker, line));
            tie_breaker += 1;
        }
    }
    lines.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    lines.into_iter().map(|(_, _, l)| l).collect()
}

fn parse_t(line: &str) -> Option<u128> {
    line.split_whitespace()
        .rev()
        .find_map(|tok| tok.strip_prefix("t="))
        .and_then(|n| n.parse::<u128>().ok())
}

/// Install a global tracing subscriber for production daemons (`lnsd`,
/// the helper bin, etc.) that combines a standard fmt layer with the
/// event-log layer when `LEVICULUM_EVENT_LOG` is set.  Unset → only
/// fmt installed; runtime overhead matches the previous standalone
/// `tracing_subscriber::fmt().init()` call.
///
/// `default_filter` is the env-filter directive used when `RUST_LOG`
/// is unset (e.g. `"info"`, `"debug"`, …).
pub fn install_global_subscriber(default_filter: &str) {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let fmt_layer = fmt::layer().compact().with_filter(env_filter);
    if std::env::var(LOG_FILE_ENV_VAR).is_ok() {
        let _ = Registry::default().with(fmt_layer).with(layer()).try_init();
    } else {
        let _ = Registry::default().with(fmt_layer).try_init();
    }
}

/// The layer registered into the global subscriber chain.  Driven by
/// the active-handles list above.
pub struct EventLogLayer {
    active: Arc<Mutex<Vec<ActiveHandle>>>,
    init_time: Instant,
}

impl EventLogLayer {
    fn caller(&self, event: &Event<'_>) -> String {
        let meta = event.metadata();
        match (meta.file(), meta.line()) {
            (Some(f), Some(l)) => {
                let basename = f.rsplit('/').next().unwrap_or(f);
                format!("{basename}:{l}")
            }
            _ => "?".to_string(),
        }
    }
}

impl<S: Subscriber> Layer<S> for EventLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);

        let Some(event_name) = visitor.event_name else {
            return;
        };

        let t_ms = self.init_time.elapsed().as_millis();

        // Build the canonical line.  node= reserved as the first
        // field, sourced from LEVICULUM_EVENT_NODE (default "local").
        // Other fields alphabetical; t= last.
        let mut line = String::with_capacity(64);
        line.push_str(&event_name);
        line.push(' ');
        line.push_str("node=");
        line.push_str(node_name());
        for (k, v) in &visitor.fields {
            // `node` from a tracing call would conflict with the
            // reserved prefix — env-var wins, user-supplied skipped.
            if k == "node" {
                continue;
            }
            line.push(' ');
            line.push_str(k);
            line.push('=');
            line.push_str(v);
        }
        line.push_str(&format!(" t={t_ms}"));

        let caller = self.caller(event);

        // Build the field-violation lines once; they have no per-
        // handle component, so all consumers (file + every active
        // buffer) receive the same text.
        let field_violation_lines: Vec<String> = visitor
            .field_violations
            .iter()
            .map(|(field, problem)| {
                format!(
                    "EVENT_FIELD_VIOLATION event={} field={} value_problem={} caller={} t={}",
                    event_name, field, problem, caller, t_ms,
                )
            })
            .collect();

        // Process-wide append-only file (when LEVICULUM_EVENT_LOG is
        // set).  Production daemons + helper bin write here.  Schema
        // violations are per-handle so they don't appear in the file.
        if let Some(file) = event_log_file() {
            if let Ok(mut f) = file.lock() {
                let _ = writeln!(f, "{line}");
                for v in &field_violation_lines {
                    let _ = writeln!(f, "{v}");
                }
                let _ = f.flush();
            }
        }

        // Distribute to every active handle.  Per-handle:
        //   1. push the canonical line
        //   2. push one EVENT_FIELD_VIOLATION per offending field
        //   3. push one EVENT_SCHEMA_VIOLATION if the handle's
        //      catalogue (production + extra_schemas) declares this
        //      event with required keys absent from the record.
        let active = self.active.lock().unwrap();
        for handle in active.iter() {
            let mut buf = handle.buffer.lock().unwrap();
            buf.push(line.clone());

            for v in &field_violation_lines {
                buf.push(v.clone());
            }

            let schema = EVENT_CATALOG
                .iter()
                .chain(handle.extra_schemas.iter())
                .find(|s| s.name == event_name);
            if let Some(s) = schema {
                let missing: Vec<&str> = s
                    .required_keys
                    .iter()
                    .filter(|k| !visitor.fields.contains_key(**k))
                    .copied()
                    .collect();
                if !missing.is_empty() {
                    let v = format!(
                        "EVENT_SCHEMA_VIOLATION event={} missing=[{}] caller={} t={}",
                        event_name,
                        missing.join(","),
                        caller,
                        t_ms,
                    );
                    buf.push(v);
                }
            }
        }
    }
}

/// Detect non-scalar values that would break a whitespace/`=`-token
/// parser.  Returns the kind of problem, or `None` if safe.
fn field_value_problem(value: &str) -> Option<&'static str> {
    for c in value.chars() {
        if c.is_ascii_whitespace() {
            return Some("whitespace");
        }
        if c == '=' {
            return Some("equals");
        }
        if !c.is_ascii_graphic() {
            return Some("non_printable");
        }
    }
    None
}

#[derive(Default)]
struct EventVisitor {
    event_name: Option<String>,
    fields: BTreeMap<String, String>,
    field_violations: Vec<(String, &'static str)>,
}

impl EventVisitor {
    fn record(&mut self, field: &Field, value: String) {
        if field.name() == "event" {
            self.event_name = Some(value);
            return;
        }
        if let Some(problem) = field_value_problem(&value) {
            self.field_violations
                .push((field.name().to_string(), problem));
        }
        self.fields.insert(field.name().to_string(), value);
    }
}

impl Visit for EventVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.to_string());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record(field, value.to_string());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record(field, value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record(field, value.to_string());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.record(field, value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let v = format!("{value:?}");
        let v = v.trim_matches('"').to_string();
        self.record(field, v);
    }
}

/// Asserts that the buffer contains no `EVENT_SCHEMA_VIOLATION` lines
/// referencing an event the test cares about.  Filter caller-side
/// before passing the dump (or accept a generic check that any
/// violation panics).  Use at the end of a test where catalogue
/// completeness is part of the contract.
#[macro_export]
macro_rules! assert_no_schema_violations {
    ($handle:expr) => {{
        let __dump = $handle.dump();
        let __violations: Vec<&String> = __dump
            .iter()
            .filter(|l| l.starts_with("EVENT_SCHEMA_VIOLATION"))
            .collect();
        if !__violations.is_empty() {
            panic!(
                "schema violations: {} (first: {:?})",
                __violations.len(),
                __violations.first(),
            );
        }
    }};
}
