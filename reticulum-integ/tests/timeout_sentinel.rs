//! Sentinel: end-to-end proof that the timeout mechanism deterministically
//! converts a wedged closure into a test failure.  Codeberg #50 Bug B.
//! Updated 2026-05-07 to match the disambiguated panic message landed for
//! Codeberg #52 (wedge vs wrapper-tight).

#[test]
#[should_panic(expected = "wedge: worker still active after 1s")]
fn lora_test_timeout_sentinel() {
    reticulum_integ::timeout::run_with_timeout("sentinel", 1, || {
        std::thread::sleep(std::time::Duration::from_secs(60));
    });
}
