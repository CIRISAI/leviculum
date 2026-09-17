//! Helper binary for the multi-process event-log integration test.
//!
//! Usage (driven by env vars + argv):
//!
//! ```sh
//! LEVICULUM_EVENT_LOG=/tmp/foo.log \
//! LEVICULUM_EVENT_NODE=node-a \
//!     event-log-helper <pre-sleep-ms> <cadence-ms> [gate-file]
//! ```
//!
//! Sleeps `<pre-sleep-ms>`, then emits 3 `HELPER_TICK` events spaced
//! by `<cadence-ms>`, exits 0.  Designed to interleave deterministically
//! with a second instance using staggered (pre-sleep, cadence) values
//! so the merged log alternates `node=` keys.
//!
//! With a `gate-file` argument the helper blocks after the FIRST event
//! until that path exists.  The rotation test needs the log rotated
//! between two events of one process, and a sleep long enough to be
//! safe under load is a flake waiting for a busy machine: the gate makes
//! the ordering a fact instead of a bet.
//!
//! The structured event-log layer is installed via
//! `event_log::install_global_subscriber`; events go to the
//! per-process file `LEVICULUM_EVENT_LOG` points at.

use std::thread;
use std::time::Duration;

use leviculum_std::event_log::install_global_subscriber;

fn main() {
    let mut args = std::env::args().skip(1);
    let pre_sleep_ms: u64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("usage: event-log-helper <pre-sleep-ms> <cadence-ms>");
    let cadence_ms: u64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("usage: event-log-helper <pre-sleep-ms> <cadence-ms> [gate-file]");
    let gate = args.next().map(std::path::PathBuf::from);

    install_global_subscriber("debug");

    thread::sleep(Duration::from_millis(pre_sleep_ms));
    for i in 0..3 {
        tracing::debug!(event = "HELPER_TICK", i = i as u64);
        if i == 0 {
            if let Some(gate) = &gate {
                wait_for(gate);
            }
        }
        thread::sleep(Duration::from_millis(cadence_ms));
    }
}

/// Block until `path` exists.  Bounded, so a test that never opens the
/// gate fails as a timeout rather than hanging a suite forever.
fn wait_for(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        if std::time::Instant::now() > deadline {
            eprintln!("event-log-helper: gate {} never appeared", path.display());
            std::process::exit(2);
        }
        thread::sleep(Duration::from_millis(5));
    }
}
