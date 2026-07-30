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

use std::collections::{BTreeMap, BTreeSet};
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
    // Interface-death frame loss is internal reliability observability.
    "FramesDropped",
];

/// Variants whose `destination_hash` is deliberately not projected into
/// `dest_hash`. Add a name here (with a reason) only when a C application
/// cannot use it.
const DEST_HASH_NOT_PROJECTED: &[&str] = &[
    // Not projected at all (see INTENTIONALLY_OTHER); path requests are the
    // transport's business.
    "PathRequestReceived",
];

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Variant names declared in the core `NodeEvent` enum (4-space-indented
/// `Name {` lines between `pub enum NodeEvent {` and its closing brace).
fn node_event_variants() -> BTreeSet<String> {
    let src = read("../leviculum-core/src/node/event.rs");
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

/// Variant name → its field names, for the variants of `NodeEvent`.
fn node_event_fields() -> BTreeMap<String, BTreeSet<String>> {
    let src = read("../leviculum-core/src/node/event.rs");
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut in_enum = false;
    let mut current: Option<String> = None;
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
        if indent == 4 {
            current = trimmed
                .strip_suffix(" {")
                .filter(|n| n.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
                .filter(|n| n.chars().all(|c| c.is_ascii_alphanumeric()))
                .map(str::to_string);
            if let Some(name) = &current {
                out.entry(name.clone()).or_default();
            }
            continue;
        }
        if indent == 8 {
            if let Some(name) = &current {
                if let Some(field) = trimmed.split(':').next() {
                    let field = field.trim();
                    if !field.is_empty()
                        && field
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
                    {
                        out.entry(name.clone())
                            .or_default()
                            .insert(field.to_string());
                    }
                }
            }
        }
    }
    out
}

/// The body of each `project()` match arm, keyed by variant name.
fn projection_arms() -> BTreeMap<String, String> {
    let src = read("src/events.rs");
    let mut out = BTreeMap::new();
    let mut current: Option<(String, String)> = None;
    for line in src.lines() {
        let l = line.trim_start();
        if let Some(rest) = l.strip_prefix("NodeEvent::") {
            if rest.contains('{') || rest.contains("=>") {
                if let Some((name, body)) = current.take() {
                    out.insert(name, body);
                }
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect();
                if !name.is_empty() {
                    current = Some((name, String::new()));
                    continue;
                }
            }
        }
        if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some((name, body)) = current.take() {
        out.insert(name, body);
    }
    out
}

/// Field-level counterpart to the variant-level check above. The variant guard
/// only asks whether a `project()` arm exists, so a *field* added to an
/// already-projected variant reaches C only if someone remembers to widen that
/// arm — and every arm that binds with `..` swallows the omission silently.
/// This pins the one field the pattern keeps recurring on: a `destination_hash`
/// on an event has to reach `dest_hash`, so a responder hosting several
/// destinations can tell them apart (Codeberg #134's `LinkEstablished`, #137's
/// `RequestReceived`).
#[test]
fn every_projected_event_with_a_destination_sets_dest_hash() {
    let fields = node_event_fields();
    let arms = projection_arms();
    let skip: BTreeSet<String> = DEST_HASH_NOT_PROJECTED
        .iter()
        .map(|s| s.to_string())
        .collect();

    let carriers: Vec<&String> = fields
        .iter()
        .filter(|(_, f)| f.contains("destination_hash") || f.contains("destination"))
        .map(|(name, _)| name)
        .collect();
    assert!(
        carriers.len() >= 6,
        "parsed only {} destination-carrying variants — the parser likely drifted",
        carriers.len()
    );

    let mut missing: Vec<String> = carriers
        .iter()
        .filter(|name| !skip.contains(**name))
        .filter(|name| {
            arms.get(**name)
                .is_some_and(|body| !body.contains("e.dest_hash ="))
        })
        .map(|name| (*name).clone())
        .collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "projected NodeEvent variants that carry a destination but never set \
         `e.dest_hash` (a C consumer cannot tell which destination the event \
         belongs to): {missing:?}. Either set it in the project() arm, or add \
         the name to DEST_HASH_NOT_PROJECTED with a reason."
    );

    // Keep this allowlist honest too.
    let stale: Vec<&str> = DEST_HASH_NOT_PROJECTED
        .iter()
        .filter(|a| !carriers.iter().any(|c| c.as_str() == **a))
        .copied()
        .collect();
    assert!(
        stale.is_empty(),
        "stale DEST_HASH_NOT_PROJECTED entries (no such destination-carrying \
         variant): {stale:?}"
    );
}
