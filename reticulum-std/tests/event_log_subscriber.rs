//! Phase-A unit tests for the structured event-log subscriber.
//! See `reticulum-std/src/test_support/event_log.rs`.
//!
//! # Cross-test pollution
//!
//! The subscriber lives behind a process-global tracing layer.  Every
//! active `EventLogHandle` receives every event the layer sees,
//! regardless of which test emitted it.  To stay deterministic when
//! tests run in parallel, each test uses a **disjoint event name**
//! (`EV_BASIC`, `EV_VIOLATION` (= `PKT_RX` since the production
//! catalogue is needed for the schema-violation test), `EV_UNKNOWN`,
//! `EV_PANIC`, `EV_MACRO`, `EV_WS`) and filters the dump to lines
//! that reference its own event name before asserting.

use std::panic;

use reticulum_std::test_support::event_log::{
    init_event_log, init_event_log_to_file, init_event_log_with_extra_schemas, EventLogHandle,
    EventSchema,
};

/// Test-only catalogue extension for `test_assert_no_schema_violations_macro_red`.
/// Per-handle (lives in the handle's `extra_schemas`); other handles
/// don't see it.
const TEST_MACRO_SCHEMAS: &[EventSchema] = &[EventSchema {
    name: "EV_MACRO",
    required_keys: &["k1", "k2"],
}];

/// Filter `handle.dump()` for lines that concern the named event:
/// either the canonical event line (starts with `"EVENT_NAME "`) or
/// a `*_VIOLATION` line that names the event in its `event=...`
/// field.
fn lines_for(handle: &EventLogHandle, event_name: &str) -> Vec<String> {
    let event_prefix = format!("{event_name} ");
    let event_kv = format!("event={event_name} ");
    handle
        .dump()
        .into_iter()
        .filter(|l| l.starts_with(&event_prefix) || l.contains(&event_kv))
        .collect()
}

/// Test 1 — basic format.  Three `EV_BASIC` events with the PKT_RX-
/// shape field set; assert format on each.  EV_BASIC is not in the
/// production catalogue, so no schema check fires.
#[test]
fn test_basic_event_format() {
    let handle = init_event_log();
    for _ in 0..3 {
        tracing::debug!(
            event = "EV_BASIC",
            iface = "tcp0",
            r#type = "Data",
            dst = "ab",
            hops = 1u8,
            len = 64usize,
        );
    }
    let lines = lines_for(&handle, "EV_BASIC");
    assert_eq!(
        lines.len(),
        3,
        "expected 3 EV_BASIC lines, got {}: {lines:?}",
        lines.len()
    );
    for line in &lines {
        assert!(
            line.starts_with("EV_BASIC node=local "),
            "wrong prefix: {line}"
        );
        assert!(
            line.contains(" dst=ab ")
                && line.contains(" hops=1 ")
                && line.contains(" iface=tcp0 ")
                && line.contains(" len=64 ")
                && line.contains(" type=Data "),
            "missing canonical key=value: {line}",
        );
        // node first; then alphabetical: dst < hops < iface < len < type; t last.
        let node_pos = line.find("node=").unwrap();
        let dst_pos = line.find("dst=").unwrap();
        let hops_pos = line.find("hops=").unwrap();
        let iface_pos = line.find("iface=").unwrap();
        let len_pos = line.find("len=").unwrap();
        let type_pos = line.find("type=").unwrap();
        let t_pos = line.rfind(" t=").unwrap();
        assert!(
            node_pos < dst_pos
                && dst_pos < hops_pos
                && hops_pos < iface_pos
                && iface_pos < len_pos
                && len_pos < type_pos
                && type_pos < t_pos,
            "non-canonical key order in: {line}",
        );
    }
}

/// Test 2 — schema violation.  PKT_RX is the production-catalogue
/// entry; emit a PKT_RX missing `hops` and `len` and assert the
/// synthetic `EVENT_SCHEMA_VIOLATION` line is appended.
#[test]
fn test_schema_violation_emitted() {
    let handle = init_event_log();
    tracing::debug!(
        event = "PKT_RX",
        iface = "tcp0",
        r#type = "Data",
        dst = "ab",
        // hops + len intentionally missing
    );
    let lines = lines_for(&handle, "PKT_RX");
    // 1 event line + 1 violation line referencing PKT_RX.
    assert_eq!(
        lines.len(),
        2,
        "expected 2 PKT_RX-related lines, got {}: {lines:?}",
        lines.len(),
    );
    assert!(
        lines[0].starts_with("PKT_RX node=local "),
        "first line should be the event line: {}",
        lines[0]
    );
    let violation = &lines[1];
    assert!(
        violation.starts_with("EVENT_SCHEMA_VIOLATION event=PKT_RX "),
        "violation prefix wrong: {violation}",
    );
    assert!(
        violation.contains("missing=[hops,len]"),
        "missing list wrong: {violation}",
    );
    assert!(
        violation.contains("caller=event_log_subscriber.rs:"),
        "caller field wrong: {violation}",
    );
}

/// Test 3 — unknown events pass through.  An event whose name is
/// not in the catalogue gets emitted as-is, no schema check fires,
/// no field-value violation expected.
#[test]
fn test_unknown_event_passes_through() {
    let handle = init_event_log();
    tracing::debug!(event = "EV_UNKNOWN", x = 1u32);
    let lines = lines_for(&handle, "EV_UNKNOWN");
    assert_eq!(
        lines.len(),
        1,
        "expected exactly 1 EV_UNKNOWN line, got: {lines:?}"
    );
    assert!(
        lines[0].starts_with("EV_UNKNOWN node=local "),
        "wrong prefix: {}",
        lines[0]
    );
    assert!(lines[0].contains("x=1"), "missing field: {}", lines[0]);
}

/// Test 4 — drop on panic.  A handle that goes out of scope while
/// the thread is panicking dumps the buffer to the configured target.
/// Use the file-target variant so we can read the dump back
/// deterministically.
#[test]
fn test_dump_on_panic() {
    let dump_path = std::env::temp_dir().join(format!("event-log-dump-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&dump_path);

    let dump_path_for_panic = dump_path.clone();
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _handle = init_event_log_to_file(dump_path_for_panic);
        tracing::debug!(event = "EV_PANIC", note = "before_panic");
        panic!("intentional panic for test_dump_on_panic");
    }));
    assert!(result.is_err(), "panic was expected to propagate");

    let dumped = std::fs::read_to_string(&dump_path)
        .expect("dump file should have been written by the Drop");
    assert!(
        dumped.contains("=== EVENT LOG DUMP"),
        "missing banner: {dumped}",
    );
    assert!(
        dumped.contains("EV_PANIC node=local note=before_panic"),
        "missing event line: {dumped}",
    );

    let _ = std::fs::remove_file(&dump_path);
}

/// Test 5 — `assert_no_schema_violations!` macro.  Emits an EV_MACRO
/// event missing required keys (the test handle adds an EV_MACRO
/// schema via extra_schemas; other handles don't see it).  Macro
/// catches the violation and panics with a count message.
#[test]
fn test_assert_no_schema_violations_macro_red() {
    let result = panic::catch_unwind(|| {
        let handle = init_event_log_with_extra_schemas(TEST_MACRO_SCHEMAS, None);
        // EV_MACRO requires k1 and k2; we send only k1 → schema violation
        // appears in this handle's buffer (because EV_MACRO is in this
        // handle's extra_schemas).
        tracing::debug!(event = "EV_MACRO", k1 = "v1");
        reticulum_std::assert_no_schema_violations!(handle);
    });
    let err = result.expect_err("the macro should have panicked on the violation");
    let msg = err
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| err.downcast_ref::<&'static str>().copied())
        .unwrap_or("<non-string panic payload>");
    assert!(
        msg.contains("schema violations"),
        "panic message should mention violations, got: {msg}",
    );
    assert!(
        msg.contains('1'),
        "panic message should mention the violation count (1): {msg}",
    );
}

/// Test 6 — field-value violations.  Field values containing
/// whitespace, `=`, or non-printable characters trigger an
/// `EVENT_FIELD_VIOLATION` line in addition to the original event.
#[test]
fn test_field_value_whitespace_violation() {
    let handle = init_event_log();
    tracing::debug!(event = "EV_WS", note = "has space");
    tracing::debug!(event = "EV_WS", extra = "key=val");
    tracing::debug!(event = "EV_WS", clean = "ok");

    let lines = lines_for(&handle, "EV_WS");
    // 3 event lines + 2 violation lines (one per offending field).
    assert_eq!(
        lines.len(),
        5,
        "expected 5 EV_WS lines (3 events + 2 violations), got: {lines:?}",
    );

    let violations: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("EVENT_FIELD_VIOLATION"))
        .collect();
    assert_eq!(
        violations.len(),
        2,
        "expected exactly 2 EVENT_FIELD_VIOLATION lines, got: {violations:?}",
    );

    let v_ws = violations
        .iter()
        .find(|l| l.contains("field=note") && l.contains("value_problem=whitespace"))
        .unwrap_or_else(|| panic!("missing whitespace violation: {violations:?}"));
    assert!(
        v_ws.contains("event=EV_WS"),
        "wrong event in violation: {v_ws}",
    );
    assert!(
        v_ws.contains("caller=event_log_subscriber.rs:"),
        "missing caller: {v_ws}",
    );

    let v_eq = violations
        .iter()
        .find(|l| l.contains("field=extra") && l.contains("value_problem=equals"))
        .unwrap_or_else(|| panic!("missing equals violation: {violations:?}"));
    assert!(
        v_eq.contains("event=EV_WS"),
        "wrong event in violation: {v_eq}",
    );
}
