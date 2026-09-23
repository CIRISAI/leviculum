//! Where [`leviculum_settle_budget::FRAME_BUDGET_US`] comes from, and why it
//! is the fast PHY and not the corpus's slowest.
//!
//! The crate itself is dependency-free arithmetic, so the budget sits in it as
//! a constant. A constant is a claim, and this file is where the claim is made
//! to answer to the radio: both candidate budgets are DERIVED here from
//! `leviculum_core::rnode::airtime_ms_with_preamble` — the same function the
//! interface charges its own transmissions and its CSMA spacing with — so a
//! change to the airtime arithmetic moves this budget or makes this file red.
//!
//! Run: `cargo test -p leviculum-settle-budget --test frame_budget -- --nocapture`

use leviculum_core::rnode::airtime_ms_with_preamble;
use leviculum_settle_budget::{
    price_pass, SettlePolicy, FRAME_BUDGET_US, WORKBLOCK_ROUNDS_PN, WORKBLOCK_US_NRF52840,
};

/// The frame this defect lost: the client's link request, 84 bytes on the air
/// (`T114_SX_RX len=84 rssi=-34`, `lnode_a`, 2026-09-23).
const LINK_REQUEST_BYTES: u32 = 84;

/// SF7/BW125/CR4:5 with the derived 24-symbol preamble — what
/// `lora_pn_board_offer_past_the_link` and `lora_pn_board_sync` configure, and
/// therefore the PHY every propagation-node measurement so far was taken on.
fn pn_cell_frame_ms() -> u64 {
    airtime_ms_with_preamble(LINK_REQUEST_BYTES, 125_000, 7, 5, 24)
}

/// SF12/BW125/CR4:8 with the 18-symbol floor — `lora_path_discovery_slowest_mixed`,
/// the slowest setting the rig can drive.
fn slowest_corpus_frame_ms() -> u64 {
    airtime_ms_with_preamble(LINK_REQUEST_BYTES, 125_000, 12, 8, 18)
}

/// The pinned budget is the PN cells' own frame, derived and not written down.
#[test]
fn the_budget_is_one_link_request_at_the_propagation_cells_phy() {
    let derived_us = pn_cell_frame_ms() * 1_000;
    println!(
        "FRAME_BUDGET sf=7 cr=5 bw=125000 preamble=24 bytes={LINK_REQUEST_BYTES} \
         airtime_ms={} pinned_us={FRAME_BUDGET_US}",
        pn_cell_frame_ms()
    );
    assert_eq!(
        derived_us,
        u64::from(FRAME_BUDGET_US),
        "FRAME_BUDGET_US no longer equals the airtime it was derived from"
    );
}

/// **Why not the slowest PHY.** The instruction's ceiling — one frame at the
/// corpus's slowest setting — is 5448 ms, and the defect it was meant to catch
/// is 3727 ms. A test written against that ceiling would have been green on
/// the firmware that went deaf for 17.9 s.
///
/// The reason is structural rather than incidental: a fixed absence costs more
/// frames the shorter the frames are, so the binding PHY is the fastest one in
/// play, not the slowest. This test is the arithmetic that says so, and it is
/// red the day a slow-PHY budget is substituted for the fast one.
#[test]
fn the_slowest_corpus_phy_would_not_have_caught_the_defect() {
    let slowest_us = slowest_corpus_frame_ms() * 1_000;
    let defect = price_pass(
        SettlePolicy::WholeWorkblock,
        WORKBLOCK_ROUNDS_PN,
        WORKBLOCK_US_NRF52840,
    );
    println!(
        "SLOW_CEILING sf=12 cr=8 bw=125000 preamble=18 bytes={LINK_REQUEST_BYTES} \
         airtime_ms={} defect_us={} verdict={}",
        slowest_corpus_frame_ms(),
        defect.absence_us,
        if defect.absence_us <= slowest_us {
            "green"
        } else {
            "red"
        }
    );
    assert!(
        defect.absence_us <= slowest_us,
        "if the defect ever exceeds the slow-PHY ceiling too, this file's \
         argument for the fast PHY needs rewriting rather than deleting"
    );
    assert!(
        u64::from(FRAME_BUDGET_US) < slowest_us,
        "the pinned budget must be the tighter of the two"
    );
}

/// Both budgets in one line, for the record the next post-mortem reads.
#[test]
fn the_two_candidate_budgets_are_reported() {
    let fast = pn_cell_frame_ms();
    let slow = slowest_corpus_frame_ms();
    println!(
        "BUDGETS fast_ms={fast} slow_ms={slow} ratio={}",
        slow / fast
    );
    assert!(slow > fast);
}
