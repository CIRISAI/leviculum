//! Wall-clock per-test timeout helper for hardware-bound LoRa scenarios.
//!
//! Wraps a closure on a worker thread and bounds it with `mpsc::recv_timeout`
//! on the test thread.  On timeout the test panics with a clear message and
//! the harness moves on to the next test.  See Codeberg #50.
//!
//! # Panic-message disambiguation (Codeberg #52)
//!
//! When the primary `recv_timeout(secs)` returns `Timeout`, the helper does a
//! brief follow-up `recv_timeout(50ms)` to distinguish two cases:
//!
//! - **Worker still active** — follow-up also returns `Timeout`. Real wedge:
//!   "hardware/firmware did not progress (Codeberg #50 class)". Forensic
//!   capture script fires.
//! - **Worker just completed** — follow-up returns `Ok(_)`. The test scenario
//!   reached its closing point but the runner cleanup pushed the wallclock
//!   past the wrapper budget. The cause is wrapper-too-tight, not a wedge:
//!   "raise wrapper budget for this test (Codeberg #52-style)". No forensic
//!   capture (the test wasn't stuck).
//!
//! The disambiguation lets the next reader see at a glance whether to fix the
//! firmware/hardware or to bump the wrapper budget. Mis-attribution to
//! Codeberg #50 was the trigger for landing this — the audit on Codeberg #52
//! reproduced wrapper-too-tight failures that spuriously named #50.
//!
//! # Leaked-thread caveat
//!
//! If the closure exceeds the timeout, the worker thread is detached — we
//! cannot safely cancel arbitrary native code (FFI into pyserial, blocking
//! `tty_wait_until_sent` syscalls, etc.) from the outside.  The leak is
//! bounded by the lifetime of the `cargo test` process: when the test
//! binary exits, the leaked thread goes with it.
//!
//! This is acceptable for the use case (CI runner) and avoids the
//! correctness hazards of forced cancellation.  An async refactor of the
//! LoRa tests would let us cancel co-operatively, but is out of scope —
//! the firmware fix (Codeberg #50 Bug A) is the durable cure for the
//! actual wedge.
//!
//! # Timeout calibration is empirical, not authoritative
//!
//! The per-test `secs` budgets in `executor.rs` are first-cut
//! estimates from TOML-budget arithmetic, NOT measured runtimes.
//! They are deliberately generous — a false timeout from a tight
//! margin produces a future debugging session that re-derives
//! this calibration context from scratch, which is much more
//! expensive than the cost of a test taking 3 minutes when it
//! could take 1.
//!
//! **If you observe a false timeout** (test that ran fine
//! before, suddenly trips the deadline because hardware was a
//! bit slower), the right fix is to **bump the budget**, not to
//! investigate the test.  Re-tightening should only happen after
//! we have a healthy-hardware runtime histogram across many
//! nightlies — see Codeberg #50 for the empirical calibration
//! follow-up.
//!
//! Real wedges (firmware bug, hardware non-responsive) will
//! surface long before the test budget runs out: a healthy run
//! under stress is single-digit minutes; a wedge waits the full
//! budget.  The signal-to-noise ratio of "timeout fired" is
//! still strongly biased toward "real wedge" even at the
//! generous default.

use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Run `f` on a worker thread; panic with a clear message if it does not
/// complete within `secs`.  Panics raised inside `f` are re-raised on the
/// test thread so `#[should_panic]` and the default test reporter behave
/// normally.
///
/// On timeout, fires `scripts/_capture-wedge-forensics.sh` to capture
/// T114 USB-serial-id, dmesg tail, lsusb, wchan, and any
/// `LEVICULUM_EVENT_LOG` file before the panic — best-effort, never
/// shadows the original timeout.  See Codeberg #50.
pub fn run_with_timeout<F>(name: &str, secs: u64, f: F)
where
    F: FnOnce() + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let _worker = thread::Builder::new()
        .name(format!("lora-test:{name}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            let _ = tx.send(result);
        })
        .expect("spawn worker");

    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => std::panic::resume_unwind(payload),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Brief grace period: did the worker actually complete just past
            // our deadline? If so, the cause is wrapper-too-tight (Codeberg
            // #52-style), not a hardware/firmware wedge (Codeberg #50 class).
            // See the module-level "Panic-message disambiguation" doc-comment.
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(())) => panic!(
                    "LoRa test '{name}' wrapper-tight: worker completed but \
                     cleanup pushed total wallclock past {secs}s — \
                     raise wrapper budget for this test (Codeberg #52-style)"
                ),
                Ok(Err(payload)) => std::panic::resume_unwind(payload),
                Err(_) => {
                    capture_wedge_forensics(name);
                    panic!(
                        "LoRa test '{name}' wedge: worker still active after {secs}s — \
                         likely hardware/firmware did not progress (Codeberg #50 class)"
                    )
                }
            }
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            capture_wedge_forensics(name);
            panic!("LoRa test '{name}' worker thread died without producing a result")
        }
    }
}

/// Best-effort invocation of the forensic capture script.  Failures are
/// swallowed — the timeout-panic must not be shadowed by capture
/// problems.
fn capture_wedge_forensics(name: &str) {
    // Repo-root resolution: CARGO_MANIFEST_DIR points at
    // `<repo>/reticulum-integ` at compile time; the script lives at
    // `<repo>/scripts/_capture-wedge-forensics.sh`.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let script = format!("{manifest_dir}/../scripts/_capture-wedge-forensics.sh");
    let _ = Command::new("bash").arg(&script).arg(name).output();
}
