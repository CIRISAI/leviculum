//! mvr for Codeberg #290 — a per-test log capture that misses one line.
//!
//! # The failure this reproduces
//!
//! `cargo test -p leviculum-std --lib interfaces::rnode::` failed 4 times in
//! 25 runs on an untouched tree (2550450a) in
//! `interfaces::rnode::tests::test_drop_direct_ingress_announces_arming_at_the_rnode_boundary`,
//! and never in isolation. Every failure had the same shape: the capture
//! buffer held the io task's LATE lines (`serial port EOF`, `goodbye write
//! failed`) and not its FIRST one, the `DIRECT_INGRESS_FILTER armed` INFO
//! emitted from `interfaces::log_direct_ingress_filter_armed`.
//!
//! Instrumenting the failing runs refuted every per-thread explanation: the
//! io task emitted from the same `ThreadId` that installed the subscriber,
//! with the identical `Dispatch::Scoped` pointer current and
//! `LevelFilter::current() == debug`, and a freshly minted `info!` emitted
//! from that same task in that same moment DID land in the buffer. The
//! suppression is per CALLSITE, and it is decided process-wide, once, by
//! whichever thread reaches the callsite first:
//!
//! `tracing` caches an `Interest` per callsite, and while at most one
//! dispatcher is registered `tracing-core` takes a shortcut —
//! `callsite.rs`'s `Rebuilder::JustOne` asks `dispatcher::get_default()`,
//! the dispatcher current on the *hitting* thread, rather than consulting a
//! list. A thread with no subscriber installed resolves to `NoSubscriber`,
//! whose `register_callsite` answers `Interest::never()`, and that verdict
//! is cached for the rest of the process. In the rnode module the sibling
//! test `test_drop_direct_ingress_filters_hops0_at_the_rnode_boundary`
//! drives an io task with the same knob on while installing no subscriber
//! at all, so the two tests raced for the callsite and the loser's
//! assertion could never pass.
//!
//! # How this test bounds it
//!
//! One process, one thread pair, no tokio, no network, no timing. It owns
//! its own callsite so nothing else in the corpus can have registered it,
//! and it forces the exact order the race produces: capture installed
//! first, then the bare thread registers the callsite, then the capture's
//! own emission. In its own test binary, because the verdict is
//! process-global state — a sibling test holding a second capture would
//! switch the shortcut off by accident and hide the failure, which is
//! precisely why the bug looked flaky.

use leviculum_std::test_support::log_capture::{capture_logs, captured};

/// The shared callsite, standing in for
/// `interfaces::log_direct_ingress_filter_armed`: one `info!` reachable
/// from two different tests.
fn emit_the_shared_line(tag: &str) {
    tracing::info!("MVR_SHARED_CALLSITE tag={tag}");
}

#[test]
fn a_bare_thread_registering_the_callsite_must_not_kill_the_captured_line() {
    let (buf, _guard) = capture_logs();

    // The sibling test: same callsite, no subscriber installed, its own
    // thread. It is the first to reach the callsite, so it is the one whose
    // dispatcher decides the cached interest.
    std::thread::spawn(|| emit_the_shared_line("no_subscriber"))
        .join()
        .expect("the bare thread must not panic");

    emit_the_shared_line("under_capture");

    let logs = captured(&buf);
    assert!(
        logs.contains("MVR_SHARED_CALLSITE tag=under_capture"),
        "the capture lost its own line to the callsite-interest cache; logs:\n{logs}"
    );
    // The bare thread has no subscriber, so its record belongs to nobody —
    // a capture that collected it would be leaking across tests.
    assert!(
        !logs.contains("tag=no_subscriber"),
        "a record emitted with no subscriber installed reached this capture; logs:\n{logs}"
    );
}
