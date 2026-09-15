//! Helper binary for the stdout/stderr split test in
//! `leviculum-std/tests/log_stream_split.rs`.
//!
//! Installs the production subscriber exactly as `lnmsg`, `lnsd` and
//! `lnpnd` do (`event_log::install_global_subscriber`), then drives the
//! real over-budget path: a node with a [`CoreProcessor`] whose
//! `on_tick` sleeps past [`PROCESSOR_TICK_BUDGET`], so the driver's
//! timer branch calls `report_budget` and it emits
//! `CORE_PROCESSOR_OVER_BUDGET` at WARN level.  Nothing about the
//! warning is faked here; the only thing the helper contributes is a
//! process whose two streams a parent can tell apart.
//!
//! On stdout it prints exactly one line, `PAYLOAD`, standing in for the
//! data every binary that installs this subscriber writes there —
//! `lnmsg`'s message bodies, `lncp`'s progress, `lnprobe`'s reply. The
//! test asserts stdout is that line and nothing else, which is the
//! assertion `lnmsg/tests/python_interop.rs` makes seven times over.
//!
//! Usage: `log-stream-helper <run-ms>` — start the node, wait `<run-ms>`
//! for at least one tick (the driver's timer branch is at most ~1 s
//! apart), exit 0.

use std::time::Duration;

use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;
use leviculum_std::driver::{
    CoreProcessor, ReticulumNodeBuilder, StdNodeCore, PROCESSOR_TICK_BUDGET,
};
use leviculum_std::event_log::install_global_subscriber;

/// Burns past the tick budget on every periodic slot. Sleeping rather
/// than spinning: `report_budget` measures wall time, which is what the
/// budget is about (the core lock is held for the duration either way).
struct OverBudget;

impl CoreProcessor for OverBudget {
    fn on_event(&mut self, _core: &mut StdNodeCore, _event: &NodeEvent) -> TickOutput {
        TickOutput::empty()
    }

    fn on_tick(&mut self, _core: &mut StdNodeCore, _now_ms: u64) -> TickOutput {
        std::thread::sleep(PROCESSOR_TICK_BUDGET * 3);
        TickOutput::empty()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let run_ms: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("usage: log-stream-helper <run-ms>");

    // `warn` is `lnmsg`'s default filter, i.e. the level at which the
    // failing line actually reached a user.
    install_global_subscriber("warn");

    let storage = std::env::temp_dir().join(format!("log-stream-helper-{}", std::process::id()));
    std::fs::create_dir_all(&storage)?;

    // No interfaces and no shared instance: the timer branch fires on its
    // own 1 s fallback, which is all the over-budget path needs.
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .share_instance(false)
        .without_events()
        .storage_path(storage.clone())
        .core_processor(OverBudget)
        .build()
        .await?;
    node.start().await?;

    println!("PAYLOAD");

    tokio::time::sleep(Duration::from_millis(run_ms)).await;
    node.stop().await?;

    let _ = std::fs::remove_dir_all(&storage);
    Ok(())
}
