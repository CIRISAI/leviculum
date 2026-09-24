//! The receipt window this crate reasons about is the one the uploader
//! actually computes. Both numbers are restated in the crate so the firmware
//! can depend on it alone; here they are held against their source.

use leviculum_core::constants::{RAW_RECEIPT_TIMEOUT_FLOOR_MS, TRAFFIC_TIMEOUT_FACTOR};
use leviculum_upload_proof as up;

#[test]
fn the_factor_and_the_floor_are_the_stacks_own() {
    assert_eq!(
        up::TRAFFIC_TIMEOUT_FACTOR,
        TRAFFIC_TIMEOUT_FACTOR,
        "the window this crate prices must be the window leviculum-core enforces"
    );
    assert_eq!(up::RECEIPT_FLOOR_MS, RAW_RECEIPT_TIMEOUT_FLOOR_MS);
}

#[test]
fn a_proof_that_reaches_the_deadline_is_late() {
    let rtt = 300;
    // 1800 ms. The return flight is half a round trip, so the node's own
    // budget ends there; one millisecond either side of it decides the upload.
    let window = up::receipt_window_ms(rtt);
    assert!(up::proof_lands(rtt, window - rtt / 2 - 1));
    assert!(!up::proof_lands(rtt, window - rtt / 2));
    assert!(!up::proof_lands(rtt, window));
}
