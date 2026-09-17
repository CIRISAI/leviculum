//! `scripts/analyze-lnpnd.py` against a log the real writer produced.
//!
//! The analysis is the observation tooling the swap of `leviculum.network`
//! depends on, and it has to exist BEFORE the swap rather than after. Two
//! things can quietly break it, and both are pinned here:
//!
//! 1. **The line format.** The events below go through
//!    `leviculum_std::event_log`'s layer, the same writer the daemon uses,
//!    so what the analysis parses is what a daemon writes and not a shape
//!    invented in a fixture.
//! 2. **The event names.** An event `lnpnd` emits that the analysis has
//!    never heard of is counted and named in its own phase, but only if the
//!    two lists agree about what exists. The second test reads the names
//!    straight out of `lnpnd/src` and requires each one to appear in the
//!    script.
//!
//! Its own test binary because the event-log sink is decided once per
//! process, on the first event, from `LEVICULUM_EVENT_LOG` — a second
//! target in the same process would find the sink already chosen.

use std::path::PathBuf;
use std::process::Command;

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/analyze-lnpnd.py")
}

/// The report the script prints for `log`.
fn analyse(log: &std::path::Path) -> String {
    let output = Command::new("python3")
        .arg(script())
        .arg(log)
        .output()
        .expect("python3 runs");
    assert!(
        output.status.success(),
        "the analysis failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// One line of the report, by its leading label.
fn field<'a>(report: &'a str, label: &str) -> &'a str {
    report
        .lines()
        .find(|line| line.trim_start().starts_with(label))
        .unwrap_or_else(|| panic!("the report has no '{label}' line:\n{report}"))
        .trim()
}

#[test]
fn the_analysis_reads_what_the_writer_writes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let log = dir.path().join("events.log");
    // Before the first event: the sink is a process-wide OnceLock filled on
    // first use (`event_log_file`, leviculum-std/src/event_log.rs:668-684).
    std::env::set_var("LEVICULUM_EVENT_LOG", &log);
    std::env::set_var("LEVICULUM_EVENT_NODE", "analysis");
    leviculum_std::event_log::install_global_subscriber("debug");

    // The events and keys lnpnd emits, in the shapes it emits them
    // (lnpnd/src/engine.rs, lnpnd/src/peering.rs, lnpnd/src/mailbox.rs).
    let peer_a = "e17f833c4ddf8890dd3a79a6fea8161d";
    let peer_b = "5a2d0029b6e5ec87020abaea0d746da4";
    for index in 0..3u64 {
        tracing::debug!(
            event = "PN_ACCEPT",
            tid = format!("{index:016x}"),
            dst = "00112233445566778899aabbccddeeff",
            bytes = 1000u64,
            value = 13u64,
            dup = false,
            via = "resource",
        );
    }
    tracing::debug!(
        event = "PN_ACCEPT",
        tid = "0000000000000000",
        dst = "00112233445566778899aabbccddeeff",
        bytes = 1000u64,
        value = 13u64,
        dup = true,
        via = "resource",
    );
    tracing::debug!(
        event = "PN_REJECT",
        reason = "invalid_stamp",
        via = "resource"
    );
    tracing::debug!(event = "PN_REJECT", reason = "invalid_stamp", via = "sync");
    tracing::debug!(
        event = "PN_REJECT",
        reason = "store_failed",
        via = "resource"
    );
    tracing::debug!(
        event = "PN_EVICT",
        tid = "0000000000000001",
        bytes = 1000u64,
        age_s = 2_592_000u64,
        reason = "expired",
    );
    tracing::debug!(
        event = "PN_STORE",
        used = 2000u64,
        limit = 10000u64,
        count = 2u64
    );
    tracing::debug!(
        event = "PN_OFFER",
        peer = peer_a,
        dir = "out",
        offered = 5u64,
        wanted = 2u64
    );
    tracing::debug!(
        event = "PN_OFFER",
        peer = peer_b,
        dir = "in",
        offered = 7u64,
        wanted = 0u64
    );
    tracing::debug!(
        event = "PN_SYNC",
        peer = peer_a,
        dir = "out",
        transferred = 2u64,
        bytes = 2000u64,
        result = "ok",
    );
    tracing::debug!(
        event = "PN_SYNC",
        peer = peer_b,
        dir = "out",
        transferred = 0u64,
        bytes = 0u64,
        result = "link_failed",
    );
    tracing::debug!(
        event = "PN_GET",
        dst = "00112233445566778899aabbccddeeff",
        form = "fetch",
        count = 2u64,
        bytes = 2000u64,
        purged = 2usize,
    );
    tracing::debug!(
        event = "PN_MAILBOX",
        src = "00112233445566778899aabbccddeeff",
        bytes = 500u64,
    );

    let report = analyse(&log);

    // Phase 2: accepted and rejected, rejections split by reason.
    assert!(field(&report, "accepted ").contains('3'), "{report}");
    assert!(field(&report, "duplicates").contains('1'), "{report}");
    assert!(field(&report, "rejected ").contains('3'), "{report}");
    assert!(field(&report, "invalid_stamp").contains('2'), "{report}");
    assert!(field(&report, "store_failed").contains('1'), "{report}");

    // Phase 3: store size against the limit, and evictions.
    let store = field(&report, "last reported");
    assert!(store.contains("20.0 %"), "store utilisation: {store}");
    assert!(field(&report, "evicted ").contains('1'), "{report}");

    // Phase 4: per-peer rounds, and a round that ended other than ok.
    assert!(report.contains(&peer_a[..16]), "peer A missing:\n{report}");
    assert!(report.contains(&peer_b[..16]), "peer B missing:\n{report}");
    assert!(
        report.contains("out:link_failed=1"),
        "a failed sync round must be visible per peer:\n{report}"
    );
    assert!(
        report.contains("1 sync round(s) ended other than ok"),
        "and called out:\n{report}"
    );

    // Phase 5: client deliveries.
    assert!(field(&report, "messages served").contains('2'), "{report}");
    assert!(field(&report, "own mail in").contains('1'), "{report}");

    // Phase 6/7: the heartbeat is recognised, and nothing is unaccounted for.
    assert!(field(&report, "heartbeats").contains('1'), "{report}");
    assert!(
        report.contains("every PN_* event in this log is one the analysis accounts for"),
        "{report}"
    );
    assert!(field(&report, "unparsed lines").ends_with('0'), "{report}");
}

/// Every `PN_*` event name in `lnpnd/src` has to be one the analysis knows.
/// An event added without a line here is an event that shows up in the
/// report's "does not know" phase on a production log, months later.
#[test]
fn the_analysis_knows_every_event_lnpnd_emits() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let script_text = std::fs::read_to_string(script()).expect("the script is there");
    // The KNOWN set specifically, not the whole file: every name also
    // appears in a `consume` branch, so searching the file would pass for
    // a name that had been dropped from the set.
    let known = script_text
        .split_once("KNOWN = {")
        .and_then(|(_, rest)| rest.split_once('}'))
        .map(|(body, _)| body.to_string())
        .expect("analyze-lnpnd.py declares a KNOWN set");

    let mut names = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(&src).expect("src reads") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("source reads");
        for after in text.split("event = \"PN_").skip(1) {
            let tail: String = after.chars().take_while(|c| *c != '"').collect();
            names.insert(format!("PN_{tail}"));
        }
    }

    assert!(
        names.len() >= 8,
        "the scan found only {names:?}; it is meant to find every PN_* emitter"
    );
    for name in &names {
        assert!(
            known.contains(&format!("\"{name}\"")),
            "{name} is emitted by lnpnd but not listed in analyze-lnpnd.py's KNOWN set"
        );
    }
}
