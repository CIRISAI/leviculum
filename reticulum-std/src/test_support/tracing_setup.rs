//! Process-global tracing subscriber installer for the test harness.
//!
//! Replaces the previous standalone `tracing_subscriber::fmt().init()`
//! in `tests/rnsd_interop/common.rs::init_tracing()` with a `Registry`
//! chain so subsequent layers (Stage 6 / Codeberg #39 piece 1's
//! event-log layer) can be attached without rewriting every test
//! that depends on the global subscriber.
//!
//! Idempotent via `std::sync::Once` — multiple test files can call
//! `init_tracing_with_event_log()` and the install runs at most once
//! per process.
//!
//! # Why a global subscriber, not per-thread
//!
//! `tracing::dispatcher::set_default()` returns a per-thread guard.
//! In `#[tokio::test(multi_thread)]` tests the spawned worker threads
//! have their own thread-local dispatcher and never see events
//! emitted on the test's main thread (and vice versa).
//!
//! Using `set_global_default` once per process means every thread —
//! main or worker — routes events through the same subscriber.  The
//! event-log layer (added in Stage 6 commit 2) builds per-test
//! buffer isolation on top of this global root via an active-handles
//! list.

use std::sync::Once;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Registry};

static INIT: Once = Once::new();

/// Install the process-global subscriber.  Composes a `fmt` layer
/// (with `with_test_writer` so libtest captures the output) with an
/// `EnvFilter` driven by `RUST_LOG`.  Stage-6 commit 2 will extend
/// this chain with the event-log layer; for now the chain is
/// fmt-layer-only and behaviourally equivalent to the previous
/// `tracing_subscriber::fmt().init()` call site.
pub fn init_tracing_with_event_log() {
    INIT.call_once(|| {
        let env_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let fmt_layer = fmt::layer().with_test_writer();
        Registry::default().with(env_filter).with(fmt_layer).init();
    });
}
