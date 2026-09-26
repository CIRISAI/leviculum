//! Per-test capture of formatted `tracing` output into a buffer.
//!
//! Three interface test modules grew the same helper independently
//! (`interfaces::rnode`, `interfaces::serial`, `interfaces::local`): a
//! `fmt` subscriber writing into an `Arc<Mutex<Vec<u8>>>`, installed with
//! the thread-local [`tracing::subscriber::set_default`].  Thread-local is
//! the right choice for them — a `#[tokio::test]` runs its current-thread
//! runtime, so every task it spawns is polled on the test thread and emits
//! through the test's own dispatcher.  Measurement on 2026-09-26 confirmed
//! that directly: in a failing run of
//! `interfaces::rnode::tests::test_drop_direct_ingress_announces_arming_at_the_rnode_boundary`
//! the io task emitted from the same `ThreadId` that installed the
//! subscriber, with the identical `Dispatch::Scoped` pointer current and
//! `LevelFilter::current() == debug`.
//!
//! # Why the helper must pin the callsite-interest cache (Codeberg #290)
//!
//! What the same measurement showed missing was ONE line: the `info!` at
//! `crate::interfaces::log_direct_ingress_filter_armed`.  A freshly
//! minted `info!` emitted from the very same task, on the same thread,
//! through the same dispatcher, landed in the buffer.  So the suppression
//! is not per-thread and not per-dispatcher — it is per CALLSITE.
//!
//! `tracing` caches an `Interest` per callsite, process-wide, the first
//! time the callsite is hit, and `tracing-core` takes a shortcut while at
//! most one dispatcher is registered: `callsite.rs`'s `Rebuilder::JustOne`
//! asks `dispatcher::get_default()` — the dispatcher current on the
//! *hitting* thread — instead of consulting a list.  A sibling test that
//! hits a shared callsite with no subscriber installed therefore resolves
//! to `NoSubscriber`, whose `register_callsite` is `Interest::never()`, and
//! that `never` is cached for the whole process.  Every later capture of
//! that line is dead, on any thread, however correctly it installs its own
//! subscriber.
//!
//! That is exactly what happened: the armed line is emitted from the io
//! task of both `rnode` and `serial`, and
//! `test_drop_direct_ingress_filters_hops0_at_the_rnode_boundary` drives
//! an rnode io task with the knob on while installing no subscriber at
//! all.  Whichever of the two tests reached the callsite first decided its
//! interest, which is why the assertion failed 4 times in 25 runs of
//! `cargo test -p leviculum-std --lib interfaces::rnode::` and never in
//! isolation.
//!
//! [`capture_logs`] closes that by keeping a second dispatcher registered
//! for the life of the process, so `tracing-core` never takes the
//! current-thread shortcut again and every callsite settles on
//! `Interest::sometimes()` — the answer that defers to whichever
//! dispatcher is current when the event is actually emitted, which is what
//! a per-test thread-local capture needs.  The cost is the loss of the
//! interest cache's fast path inside test binaries; nothing is logged
//! differently.
//!
//! Sibling approach for a different problem: [`super::warn_capture`]
//! captures plain WARN messages out of the ONE global subscriber, because
//! its tests run under a `multi_thread` runtime where a thread-local
//! subscriber genuinely cannot see the emitting worker.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::{DefaultGuard, Interest};
use tracing::{Event, Metadata, Subscriber};

/// The buffer a capture writes into.
pub type CaptureBuffer = Arc<Mutex<Vec<u8>>>;

/// Lock the buffer, recovering from a panic in another holder: a poisoned
/// capture buffer still holds the lines a failing test wants to print.
fn lock_buffer(buf: &Mutex<Vec<u8>>) -> MutexGuard<'_, Vec<u8>> {
    buf.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone)]
struct LogSink(CaptureBuffer);

impl std::io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        lock_buffer(&self.0).extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogSink {
    type Writer = LogSink;
    fn make_writer(&'a self) -> LogSink {
        self.clone()
    }
}

/// A registered-but-never-default subscriber whose only job is to answer
/// `register_callsite` with `Interest::sometimes()`.
///
/// It is never installed as anyone's default, so none of its recording
/// methods is ever reached; only `register_callsite` and `max_level_hint`
/// are consulted, by `tracing-core`'s interest rebuild.  `sometimes` is
/// the decisive answer: combined with any other dispatcher's verdict it
/// yields `sometimes` (`Interest::and` collapses disagreement to
/// `sometimes`), so no callsite can ever be cached as `never`, and the
/// per-event `enabled()` question reaches the dispatcher that is actually
/// current at emission time.
struct AskAtEmissionTime;

impl Subscriber for AskAtEmissionTime {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }

    /// Unbounded, so this subscriber never lowers the process-wide maximum
    /// level below what a capture asked for.
    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }

    fn enabled(&self, _: &Metadata<'_>) -> bool {
        false
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, _: &Event<'_>) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

/// Keep a second dispatcher registered for the life of the process.
///
/// Registering it is the whole effect: `tracing::Dispatch::new` enters it
/// into `tracing-core`'s dispatcher registry and rebuilds every callsite's
/// cached interest, and the `OnceLock` keeps it alive so the registry's
/// weak reference never lapses.  From then on at least two dispatchers are
/// live whenever a capture is installed, the `JustOne` current-thread
/// shortcut is off, and a subscriber-less thread can no longer cache
/// `Interest::never()` on a callsite another test asserts on.
fn pin_callsite_interest() {
    static PINNED: OnceLock<tracing::Dispatch> = OnceLock::new();
    PINNED.get_or_init(|| tracing::Dispatch::new(AskAtEmissionTime));
}

/// Capture formatted `tracing` output at DEBUG and above for the lifetime
/// of the returned guard, into the returned buffer.
///
/// The subscriber is thread-local, so concurrent tests do not see each
/// other's records; see the module docs for why that is sound under
/// `#[tokio::test]` and for what the helper has to do about the
/// process-wide callsite-interest cache.
#[must_use]
pub fn capture_logs() -> (CaptureBuffer, DefaultGuard) {
    pin_callsite_interest();
    let buf: CaptureBuffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(LogSink(Arc::clone(&buf)))
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (buf, guard)
}

/// Everything captured so far, as text.
#[must_use]
pub fn captured(buf: &CaptureBuffer) -> String {
    String::from_utf8_lossy(&lock_buffer(buf)).into_owned()
}
