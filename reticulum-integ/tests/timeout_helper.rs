//! Unit tests for `reticulum_integ::timeout::run_with_timeout`.
//! Covers the three required behaviours (Codeberg #50 Bug B):
//! normal completion returns, panics propagate, and timeout fires.

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
#[should_panic(expected = "timed out after 1s")]
fn test_timeout_fires() {
    run_with_timeout("test", 1, || {
        std::thread::sleep(std::time::Duration::from_secs(10));
    });
}
