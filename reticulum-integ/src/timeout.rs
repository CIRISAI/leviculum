//! Wall-clock per-test timeout helper for hardware-bound LoRa scenarios.
//!
//! Wraps a closure on a worker thread and bounds it with `mpsc::recv_timeout`
//! on the test thread.  On timeout the test panics with a clear message and
//! the harness moves on to the next test.  See Codeberg #50.
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

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Run `f` on a worker thread; panic with a clear message if it does not
/// complete within `secs`.  Panics raised inside `f` are re-raised on the
/// test thread so `#[should_panic]` and the default test reporter behave
/// normally.
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
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "LoRa test '{name}' timed out after {secs}s — \
             hardware/firmware did not progress (Codeberg #50)"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("LoRa test '{name}' worker thread died without producing a result")
        }
    }
}
