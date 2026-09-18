//! `node=` is the reserved first field of every canonical line and it comes
//! from `LEVICULUM_EVENT_NODE`, i.e. from whoever started the daemon. It is
//! written into the line directly, bypassing the field visitor, so nothing
//! downstream would rescue it: a node named after the room it stands in
//! ("back shed") would split EVERY line the process ever writes, including
//! the EVENT_FIELD_VIOLATION lines that are supposed to report such damage.
//!
//! Own test binary: `node_name()` caches the env var in a `OnceLock` at the
//! first emitted event, so the variable has to be set before any other test
//! in the process emits anything.

use leviculum_std::test_support::event_log::init_event_log;

#[test]
fn a_node_name_with_a_space_does_not_split_every_line() {
    std::env::set_var("LEVICULUM_EVENT_NODE", "back shed t114");

    let evlog = init_event_log();
    tracing::debug!(event = "OBS_NODE_NAME_PROBE", ok = true);

    let dump = evlog.dump();
    let line = dump
        .iter()
        .find(|l| l.starts_with("OBS_NODE_NAME_PROBE "))
        .unwrap_or_else(|| panic!("probe event missing; dump:\n{dump:#?}"));

    assert!(
        line.starts_with("OBS_NODE_NAME_PROBE node=back_shed_t114 "),
        "the node prefix must be one token: {line}"
    );
    let tokens: Vec<&str> = line.split_whitespace().skip(1).collect();
    assert!(
        tokens.iter().all(|t| t.contains('=')),
        "no bare tokens anywhere on the line: {line}"
    );
}
