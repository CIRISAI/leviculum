//! The event-log layer answers a callsite once, and only visits records
//! that can possibly carry an event.
//!
//! # What is being asserted, and why in this shape
//!
//! `EventLogLayer` consumes records that carry `event = "NAME"` and
//! discards every other record. Discovering that a record has no `event`
//! field used to cost a full visit: a `BTreeMap<String, String>` and a
//! `String` per field name and per field value, built and dropped. There
//! are ~800 `tracing` sites in `leviculum-core` + `leviculum-std` and 127
//! of them carry `event =`, so the overwhelming majority of that work was
//! allocated in order to be thrown away.
//!
//! A callsite's field names are static — they live in its `Metadata` — so
//! the question "can a record from this site ever carry `event`?" has one
//! answer for the lifetime of the process. The layer now answers it once,
//! through a per-layer `Filter`.
//!
//! **Per-layer, not `Layer::enabled`.** That distinction is the whole
//! safety argument and it is why this file exists. A `Layer::enabled` /
//! `Layer::register_callsite` returning `Interest::never()` disables the
//! callsite for the ENTIRE subscriber — `Layered::pick_interest` returns
//! the outer layer's `never` without consulting the inner layers
//! (`tracing-subscriber-0.3.22/src/layer/layered.rs:442`). The fmt layer
//! sits below the event-log layer, so a global filter here would silently
//! delete ordinary `RUST_LOG` output. A per-layer `Filter` is scoped: the
//! `Filtered` wrapper adds its interest to the per-callsite sum and returns
//! `Interest::always()` upward so the layers beneath it keep their say
//! (`filter/layer_filters/mod.rs:741-763`).
//!
//! So there are four assertions here and all four are required: records
//! without an `event` field are not visited; records with one still are;
//! their rendered lines are unchanged byte for byte; and a record this
//! layer declined still reaches a layer below it.

use std::sync::{Mutex, MutexGuard, PoisonError};

use leviculum_std::event_log::visit_count;
use leviculum_std::test_support::event_log::{init_event_log, EventLogHandle};
use leviculum_std::test_support::warn_capture::register_warn_capture;

/// Serialises this file's tests: `visit_count()` is process-global, so a
/// second test emitting concurrently would be counted into the first
/// one's delta.
static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Lines of `handle`'s buffer that concern one event name, with the
/// trailing `t=<rel-ms>` removed — the only part of a line that is not a
/// function of the record.
fn lines_without_t(handle: &EventLogHandle, event_name: &str) -> Vec<String> {
    let prefix = format!("{event_name} ");
    let kv = format!("event={event_name} ");
    handle
        .dump()
        .into_iter()
        .filter(|l| l.starts_with(&prefix) || l.contains(&kv))
        .map(|l| match l.rfind(" t=") {
            Some(i) => l[..i].to_string(),
            None => l,
        })
        .collect()
}

/// A record with no `event` field costs no visit.
///
/// Every level is exercised, because the two ends of the range fail
/// differently: `warn!`/`info!` pass the fmt layer's default `info`
/// `EnvFilter` and so stay globally enabled whatever this layer says,
/// while `debug!`/`trace!` were globally enabled only because this layer
/// used to declare interest in everything.
#[test]
fn records_without_an_event_field_are_not_visited() {
    let _lock = lock();
    let _handle = init_event_log();

    let before = visit_count();
    for i in 0..200 {
        tracing::trace!("plain trace {i}");
        tracing::debug!(hash = "deadbeef", hops = 3u8, "plain debug");
        tracing::info!(iface = "tcp0", "plain info");
        tracing::warn!("plain warn {i}");
    }
    let visited = visit_count() - before;

    assert_eq!(
        visited, 0,
        "the layer visited {visited} records that carry no `event` field; \
         each visit is a BTreeMap<String, String> built to be discarded",
    );
}

/// A record that does carry `event` is still visited, at every level,
/// including the ones `RUST_LOG=info` drops from the fmt layer.
#[test]
fn records_with_an_event_field_are_still_visited() {
    let _lock = lock();
    let _handle = init_event_log();

    let before = visit_count();
    for _ in 0..50 {
        tracing::trace!(event = "EV_FILTER_TRACE", k = 1u8);
        tracing::debug!(event = "EV_FILTER_DEBUG", k = 2u8);
        tracing::info!(event = "EV_FILTER_INFO", k = 3u8);
        tracing::warn!(event = "EV_FILTER_WARN", k = 4u8);
    }
    let visited = visit_count() - before;

    assert_eq!(
        visited, 200,
        "an `event`-carrying record was dropped by the callsite filter; \
         silencing a real event is a worse bug than the churn it saves",
    );
}

/// The rendered line is unchanged, byte for byte, for every shape the
/// visitor treats specially: plain scalars, a `Debug` value, a value with
/// whitespace (violation + sanitisation), a name-type field (sanitised
/// silently), a user-supplied `node` that must lose to the env-var
/// prefix, and a catalogued event missing a required key.
#[test]
fn event_lines_are_rendered_byte_for_byte() {
    let _lock = lock();
    let handle = init_event_log();

    tracing::debug!(event = "EV_FILTER_FMT", b = 2u8, a = "x", c = true);
    assert_eq!(
        lines_without_t(&handle, "EV_FILTER_FMT"),
        vec!["EV_FILTER_FMT node=local a=x b=2 c=true"],
    );

    tracing::debug!(event = "EV_FILTER_DBG", v = ?Some("abc"), n = ?Option::<u8>::None);
    assert_eq!(
        lines_without_t(&handle, "EV_FILTER_DBG"),
        vec!["EV_FILTER_DBG node=local n=None v=abc"],
    );

    // `caller=` carries this file's own line number, so the expectation
    // reads it from the source rather than hardcoding a number that any
    // edit above would falsify.
    let ws_line = line!() + 1;
    tracing::debug!(
        event = "EV_FILTER_WS",
        note = "two words",
        iface = "Dark Doodad"
    );
    assert_eq!(
        lines_without_t(&handle, "EV_FILTER_WS"),
        vec![
            "EV_FILTER_WS node=local iface=Dark_Doodad note=two_words".to_string(),
            format!(
                "EVENT_FIELD_VIOLATION event=EV_FILTER_WS field=note \
                 value_problem=whitespace caller=event_log_callsite_filter.rs:{ws_line}"
            ),
        ],
    );

    tracing::debug!(event = "EV_FILTER_NODE", node = "ignored", k = 9u8);
    assert_eq!(
        lines_without_t(&handle, "EV_FILTER_NODE"),
        vec!["EV_FILTER_NODE node=local k=9"],
    );

    // PKT_RX is in the production catalogue; with `hops` and `len` absent
    // the schema-violation line must still be appended.
    let pkt_line = line!() + 1;
    tracing::debug!(
        event = "PKT_RX",
        iface = "tcp0",
        r#type = "Data",
        dst = "ab",
        ph = "00112233aabbccdd"
    );
    assert_eq!(
        lines_without_t(&handle, "PKT_RX"),
        vec![
            "PKT_RX node=local dst=ab iface=tcp0 ph=00112233aabbccdd type=Data".to_string(),
            format!(
                "EVENT_SCHEMA_VIOLATION event=PKT_RX missing=[hops,len] \
                 caller=event_log_callsite_filter.rs:{pkt_line}"
            ),
        ],
    );
}

/// A record the event-log layer declines still reaches the layers below
/// it.
///
/// This is the regression test for the hazard in the module docs: written
/// as `Layer::enabled`/`Layer::register_callsite` the filter would return
/// `Interest::never()` for this callsite — it carries no `event` field —
/// and `Layered` would short-circuit, so neither the warn-capture layer
/// nor the fmt layer below would ever see it. The message here is the
/// shape Codeberg #38's LRPROOF assertion depends on: a plain
/// `tracing::warn!` from a `leviculum_core` target with no `event` field
/// at all.
#[test]
fn a_declined_record_still_reaches_the_layer_below() {
    let _lock = lock();
    let capture = register_warn_capture();

    tracing::warn!(target: "leviculum_core::transport", "callsite filter must not eat this");

    assert!(
        capture
            .snapshot()
            .contains("callsite filter must not eat this"),
        "a plain WARN with no `event` field did not reach the warn-capture \
         layer; the event-log filter is disabling callsites globally \
         instead of only for itself. Captured: {:?}",
        capture.snapshot(),
    );
}
