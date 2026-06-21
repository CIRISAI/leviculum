//! Guards against the "silently dropped event" bug class.
//!
//! Every `NodeEvent` variant the engine can emit must be either projected to a
//! typed `LEV_EVENT_*` by `events::project()` or explicitly listed here as
//! intentionally mapped to `LEV_EVENT_OTHER`. A new engine event that is
//! neither fails this test, instead of vanishing into `LEV_EVENT_OTHER`
//! unnoticed (the class of bug that hid the missing `PathFound` projection).
//!
//! `NodeEvent` is `#[non_exhaustive]`, so the compiler cannot enforce this in
//! the FFI crate; this source-level check stands in for that, the same way
//! `guard_coverage.rs` enforces the panic-guard invariant.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Variants deliberately not given their own event type: pure observability or
/// transport-internal signals that project to `LEV_EVENT_OTHER` on purpose.
/// Add a name here (with a reason) only when exposing it would not help a C
/// application.
const INTENTIONALLY_OTHER: &[&str] = &[
    // "the transport layer already handles" path requests; informational only.
    "PathRequestReceived",
    // Channel retransmit is internal reliability observability.
    "ChannelRetransmit",
];

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Variant names declared in the core `NodeEvent` enum (4-space-indented
/// `Name {` lines between `pub enum NodeEvent {` and its closing brace).
fn node_event_variants() -> BTreeSet<String> {
    let src = read("../reticulum-core/src/node/event.rs");
    let mut variants = BTreeSet::new();
    let mut in_enum = false;
    for line in src.lines() {
        if line.contains("pub enum NodeEvent") {
            in_enum = true;
            continue;
        }
        if in_enum && line == "}" {
            break;
        }
        if !in_enum {
            continue;
        }
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if indent != 4 {
            continue;
        }
        if let Some(name) = trimmed.strip_suffix(" {") {
            if name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && name.chars().all(|c| c.is_ascii_alphanumeric())
            {
                variants.insert(name.to_string());
            }
        }
    }
    variants
}

/// Variant names handled by a `project()` match arm in `events.rs`.
fn projected_variants() -> BTreeSet<String> {
    let src = read("src/events.rs");
    src.lines()
        .filter_map(|line| {
            let l = line.trim_start();
            // Match-arm heads only: `NodeEvent::Name {` or `NodeEvent::Name =>`.
            let rest = l.strip_prefix("NodeEvent::")?;
            if !rest.contains('{') && !rest.contains("=>") {
                return None;
            }
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

#[test]
fn every_node_event_variant_is_projected_or_allowlisted() {
    let variants = node_event_variants();
    assert!(
        variants.len() >= 20,
        "parsed only {} NodeEvent variants — the parser likely drifted",
        variants.len()
    );

    let projected = projected_variants();
    let allow: BTreeSet<String> = INTENTIONALLY_OTHER.iter().map(|s| s.to_string()).collect();

    let mut unaccounted: Vec<String> = variants
        .iter()
        .filter(|v| !projected.contains(*v) && !allow.contains(*v))
        .cloned()
        .collect();
    unaccounted.sort();
    assert!(
        unaccounted.is_empty(),
        "NodeEvent variants neither projected nor allowlisted (they silently \
         become LEV_EVENT_OTHER): {unaccounted:?}. Either add a project() arm \
         with a LEV_EVENT_* type, or add the name to INTENTIONALLY_OTHER."
    );

    // Keep the allowlist honest: every entry must still name a real variant.
    let stale: Vec<&str> = INTENTIONALLY_OTHER
        .iter()
        .filter(|a| !variants.contains(**a))
        .copied()
        .collect();
    assert!(
        stale.is_empty(),
        "stale INTENTIONALLY_OTHER entries (no such NodeEvent variant): {stale:?}"
    );
}
