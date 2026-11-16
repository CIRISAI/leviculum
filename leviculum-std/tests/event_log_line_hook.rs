//! The event log's line hook (1655, walk 9, 48): a process with no file
//! and no terminal, the phone, hands each structured line to a function
//! of its own (logcat there), for the targets it names.
//!
//! One test in its own binary: the hook installs the global subscriber.

use std::sync::Mutex;

static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn keep(line: &str) {
    SEEN.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(line.to_string());
}

fn seen() -> Vec<String> {
    SEEN.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[test]
fn the_hook_gets_each_line_of_the_targets_it_names_and_no_other() {
    assert!(
        leviculum_std::event_log::install_line_subscriber("loquax", keep),
        "installed"
    );
    tracing::debug!(
        target: "loquax::events",
        event = "ROOMS_LINK",
        hub = "14d0b712",
        state = "ready",
    );
    // Another crate's event: not this hook's.
    tracing::debug!(
        target: "leviculum_core::transport",
        event = "ROOMS_LINK",
        hub = "00",
        state = "other",
    );
    // No `event` field: not a structured line.
    tracing::info!(target: "loquax::events", "a plain line");
    let lines = seen();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("ROOMS_LINK ")
            && lines[0].contains(" hub=14d0b712 ")
            && lines[0].contains(" state=ready "),
        "{lines:?}"
    );
    // A second install keeps the first and says so.
    assert!(!leviculum_std::event_log::install_line_subscriber(
        "loquax", keep
    ));
}
