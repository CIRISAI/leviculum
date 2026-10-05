//! The boot's QSPI deadline and its line.

use leviculum_qspi_boot::{
    budget_ms, expected_step_us, outcome, Timeout, BOOT_STEP_BUDGET_MS, HEADER_READS, IDENTIFY_US,
    STEP_MS,
};

#[test]
fn expected_step_is_identify_plus_512_header_reads() {
    assert_eq!(HEADER_READS, 512);
    // 48 us overhead + 12 bytes at 16 MHz (6 us) per header read.
    assert_eq!(expected_step_us(), IDENTIFY_US + 512 * 54);
    assert_eq!(expected_step_us(), 28_748);
    assert_eq!(STEP_MS, 29);
}

#[test]
fn budget_is_four_steps_rounded_up_to_a_second() {
    assert_eq!(budget_ms(0), 0);
    assert_eq!(budget_ms(1), 1000);
    assert_eq!(budget_ms(29), 1000);
    assert_eq!(budget_ms(250), 1000);
    assert_eq!(budget_ms(251), 2000);
    assert_eq!(BOOT_STEP_BUDGET_MS, 1000);
}

#[test]
fn the_budget_clears_the_expected_step_four_times_over() {
    const { assert!(BOOT_STEP_BUDGET_MS >= 4 * STEP_MS) };
    assert_eq!(outcome(u64::from(4 * STEP_MS), BOOT_STEP_BUDGET_MS), None);
}

/// Positive control: a zero budget must produce the line, or the test
/// below proves nothing about the deadline.
#[test]
fn a_zero_budget_times_out() {
    let line = outcome(u64::from(STEP_MS), 0).expect("a zero budget must time out");
    assert_eq!(line, Timeout { after_ms: 0 });
    assert_eq!(line.to_string(), "state=timeout after_ms=0");
}

#[test]
fn a_budget_above_the_step_does_not_time_out() {
    assert_eq!(outcome(u64::from(STEP_MS), STEP_MS + 1), None);
    assert_eq!(outcome(u64::from(STEP_MS), BOOT_STEP_BUDGET_MS), None);
}

#[test]
fn the_deadline_takes_the_boundary_millisecond() {
    assert_eq!(outcome(999, 1000), None);
    assert_eq!(outcome(1000, 1000), Some(Timeout { after_ms: 1000 }));
    assert_eq!(outcome(u64::MAX, 1000), Some(Timeout { after_ms: 1000 }));
}

#[test]
fn line_shape() {
    assert_eq!(
        Timeout { after_ms: 1003 }.to_string(),
        "state=timeout after_ms=1003"
    );
}
