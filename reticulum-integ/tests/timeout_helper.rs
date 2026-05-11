//! Unit tests for `reticulum_integ::timeout::run_with_timeout`.
//! Covers the four required behaviours (Codeberg #50 Bug B + Codeberg #52
//! disambiguation): normal completion returns, panics propagate, wedge
//! timeout fires (worker still running), wrapper-tight timeout fires (worker
//! completed just past the deadline).

use reticulum_integ::timeout::run_with_timeout;

#[test]
fn test_timeout_returns_normal_completion() {
    run_with_timeout("test", 1, || {});
}

#[test]
#[should_panic(expected = "inner")]
fn test_timeout_propagates_panic() {
    run_with_timeout("test", 1, || {
        panic!("inner");
    });
}

#[test]
#[should_panic(expected = "wedge: worker still active after 1s")]
fn test_timeout_fires_wedge() {
    // Worker sleeps 10 s, wrapper times out at 1 s. After the primary timeout
    // and the 50 ms grace the worker is still running — wedge branch.
    run_with_timeout("test", 1, || {
        std::thread::sleep(std::time::Duration::from_secs(10));
    });
}

#[test]
#[should_panic(
    expected = "wrapper-tight: worker completed but cleanup pushed total wallclock past 1s"
)]
fn test_timeout_fires_wrapper_tight() {
    // Worker sleeps 1010 ms, wrapper times out at 1 s. After the primary
    // timeout the 50 ms grace period catches the worker's completion.
    // This proves the disambiguation distinguishes wedge from wrapper-tight.
    run_with_timeout("test", 1, || {
        std::thread::sleep(std::time::Duration::from_millis(1010));
    });
}
