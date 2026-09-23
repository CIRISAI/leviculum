#![no_std]
//! How long one propagation-engine settle pass may keep the board's main loop
//! away from its radio, and the slicing that holds it there.
//!
//! # The defect this crate pins (leviculum#425)
//!
//! `leviculum_nrf::pn::Engine::settle` is called inline from the single main
//! loop of `t114` / `rak4631`. Until the slicing it awaited a whole 1000-round
//! propagation stamp workblock in one pass — 3655 to 3727 ms measured on the
//! nRF52840 (`ble_pn_board_upload` 2026-09-16, `lora_pn_board_offer_past_the_link`
//! 2026-09-23). For that whole span the loop is not in its `select`, so
//! nothing drains `LORA_INCOMING`.
//!
//! That channel holds four frames and its producer is a **blocking** send
//! (`leviculum-nrf/src/lora.rs:106`): a LoRa task that finds it full parks in
//! the hand-off with no receive window standing. The board is then deaf, and
//! the night run measured exactly that — `SX_RX_ARM site=ack dark_ms=10284`
//! and `dark_ms=17870` on the serving T114 while `lnode_b` logged ten
//! consecutive `PN_STAMP ms=3655..3727`, and while the client's one link
//! request crossed the air (`lnode_a` overheard it, `T114_SX_RX len=84
//! rssi=-34`) and was never received. A propagation node that goes deaf for 10
//! to 18 s while it accepts mail cannot be dialled by the phone whose mail it
//! is accepting.
//!
//! # What is modelled here, and what is not
//!
//! This crate is arithmetic over the main loop's schedule. It does not run the
//! engine, and it does not hash anything — the firmware crate cross-compiles
//! to `thumbv7em-none-eabihf` and has no test target, which is why every
//! decision in it that can be made a pure function lives in a sibling crate
//! like this one.
//!
//! Two quantities:
//!
//! * [`Pass::absence_us`] — how long one settle pass keeps the loop away from
//!   its `select`. This is the term the fix moves, and it is the term the
//!   `PN_STAMP ms=` line measures directly.
//! * [`dark_span_us`] — how long the receiver is consequently dark. An
//!   absence is not yet a dark receiver: the hand-off channel absorbs the
//!   first few frames. The receiver goes dark only once the LoRa task has more
//!   frames in hand than the channel has free slots, and it stays dark for one
//!   absence per frame it still has to hand up. That is the bridge from the
//!   3.7 s absence to the 10.3 s dark span the rig measured, and it is why
//!   this crate models the channel at all.
//!
//! # The budget
//!
//! A dark receiver loses every frame that starts while it is dark. The bound
//! this crate asserts against is therefore ONE FRAME: the loop may be away for
//! less than the time a frame spends on the air, so a frame that starts while
//! the loop is away is still being received when the loop comes back and
//! re-arms is owed at most once per frame.
//!
//! Which frame, and which PHY, is a choice, and it is the choice that decides
//! whether the bound catches anything:
//!
//! * At the corpus's slowest PHY — SF12/BW125/CR4:8,
//!   `lora_path_discovery_slowest_mixed` — one 84-byte frame is 5448 ms of
//!   airtime. A bound of 5448 ms is passed by the 3727 ms defect. It would
//!   not have caught this.
//! * At the PHY the propagation cells actually run — SF7/BW125/CR4:5, which is
//!   what `lora_pn_board_offer_past_the_link` and `lora_pn_board_sync`
//!   configure — the same frame is 166 ms. The defect misses it by a factor of
//!   22.
//!
//! The binding case is the FAST PHY, and for the reason that makes the slow
//! one vacuous: a fixed absence costs more frames the shorter the frames are.
//! [`FRAME_BUDGET_US`] is the fast one, and `tests/frame_budget.rs` derives
//! both from `leviculum_core::rnode::airtime_ms_with_preamble` rather than
//! restating either, so a change to the airtime arithmetic moves this budget
//! with it.

/// Rounds in the propagation-node stamp workblock
/// (`leviculum_lxmf::constants::WORKBLOCK_EXPAND_ROUNDS_PN`). Restated as a
/// `u32` here so this crate stays dependency-free; `tests/frame_budget.rs`
/// is not where it is checked — `leviculum_nrf::pn` uses the constant itself
/// and this value only prices it.
pub const WORKBLOCK_ROUNDS_PN: u32 = 1000;

/// The slowest whole-workblock validation measured on an nRF52840, in
/// microseconds: 3727 ms, the worst of the ten `PN_STAMP ms=` samples
/// `lora_pn_board_offer_past_the_link` logged on 2026-09-23. The best of the
/// same ten was 3655 ms.
pub const WORKBLOCK_US_NRF52840: u32 = 3_727_000;

/// The airtime of the frame this defect lost — the 84-byte link request
/// `lnode_a` overheard — at the PHY the propagation cells run, SF7/BW125/CR4:5
/// with the derived 18/24-symbol preamble. Derived rather than asserted in
/// `tests/frame_budget.rs`; see the module docs for why this PHY and not the
/// corpus's slowest.
pub const FRAME_BUDGET_US: u32 = 166_000;

/// Workblock rounds `leviculum_nrf::pn` expands in one settle pass.
///
/// The firmware reads this constant rather than keeping its own, so the number
/// the board runs and the number the tests below pin cannot drift apart. It is
/// the largest slice that fits the half of [`FRAME_BUDGET_US`] the loop's own
/// turnaround does not claim; [`slice_rounds_for`] is the sizing rule and
/// `the_firmwares_slice_is_the_largest_that_fits_its_half_frame` is where the
/// two are made to agree.
pub const GRIND_SLICE_ROUNDS: u32 = 20;

/// Slots in `LORA_INCOMING` (`leviculum-nrf/src/lora.rs:106`). The producer
/// blocks when they are full, which is what turns a busy main loop into a dark
/// receiver rather than a dropped packet.
pub const HANDOFF_SLOTS: u32 = 4;

/// What one settle pass spends on stamp validation before it returns to the
/// main loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlePolicy {
    /// The pre-fix schedule: the pass awaits the whole workblock.
    WholeWorkblock,
    /// The fix: the pass expands at most `rounds` of the workblock, parks the
    /// stream, and returns. The next pass resumes it.
    Sliced {
        /// Workblock rounds per pass. Zero would never finish, so it is
        /// clamped to one wherever it is priced.
        rounds: u32,
    },
}

impl SettlePolicy {
    /// Workblock rounds this policy expands in one pass, given a workblock of
    /// `total` rounds.
    pub fn rounds_per_pass(&self, total: u32) -> u32 {
        match self {
            SettlePolicy::WholeWorkblock => total,
            SettlePolicy::Sliced { rounds } => (*rounds).max(1).min(total),
        }
    }

    /// Passes one whole workblock of `total` rounds costs, at least one.
    pub fn passes_per_workblock(&self, total: u32) -> u32 {
        let per = self.rounds_per_pass(total);
        total.div_ceil(per.max(1)).max(1)
    }
}

/// What one settle pass costs the main loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pass {
    /// Microseconds the loop is away from its `select`.
    pub absence_us: u64,
    /// Workblock rounds this pass expanded.
    pub rounds: u32,
}

/// Price one settle pass under `policy`, given a workblock of `rounds` rounds
/// costing `workblock_us` in total.
///
/// The cost is linear in the rounds and is measured, not modelled: 1000 rounds
/// at 3727 ms is 3.727 ms per round on this core, and a slice of `n` rounds is
/// `n` of them. Integer arithmetic throughout, rounding up, so a slice is
/// never priced below what it costs.
pub fn price_pass(policy: SettlePolicy, rounds: u32, workblock_us: u32) -> Pass {
    let per_pass = policy.rounds_per_pass(rounds);
    let absence_us =
        (u64::from(per_pass) * u64::from(workblock_us)).div_ceil(u64::from(rounds.max(1)));
    Pass {
        absence_us,
        rounds: per_pass,
    }
}

/// How long the receiver stays dark, given one pass's `absence_us`.
///
/// The LoRa task hands `frames_in_hand` reassembled payloads up through a
/// channel with `slots` free slots. The first `slots` of them cost it nothing:
/// the send completes, the task returns to the radio, the window is re-armed.
/// Every frame beyond that parks the task in the hand-off until the main loop
/// comes back to its `select` and drains one — one absence per frame, with
/// `turnaround_us` for the loop's own turn through the `select` and its
/// dispatch.
///
/// With nothing over the slots the answer is zero: an absence that nobody is
/// waiting out costs no darkness, which is why an idle board with a validating
/// engine is not a bug.
pub fn dark_span_us(absence_us: u64, turnaround_us: u32, frames_in_hand: u32, slots: u32) -> u64 {
    let blocked = frames_in_hand.saturating_sub(slots);
    u64::from(blocked) * (absence_us + u64::from(turnaround_us))
}

/// The longest the loop is away while a batch of `messages` validations
/// drains, and the passes it takes.
///
/// The longest absence is one pass's — the engine validates at most one
/// message per pass either way, so the batch length does not lengthen a single
/// absence. It multiplies them, which is what [`dark_span_us`] prices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drain {
    /// Longest single absence from the `select`, microseconds.
    pub longest_absence_us: u64,
    /// Settle passes the whole batch takes.
    pub passes: u32,
}

/// Drain a batch of `messages` stamp validations under `policy`.
pub fn drain(policy: SettlePolicy, messages: u32, rounds: u32, workblock_us: u32) -> Drain {
    let pass = price_pass(policy, rounds, workblock_us);
    Drain {
        longest_absence_us: pass.absence_us,
        passes: messages.saturating_mul(policy.passes_per_workblock(rounds)),
    }
}

/// The largest slice, in workblock rounds, that still fits a `budget_us`
/// absence — the sizing rule `leviculum_nrf::pn::GRIND_SLICE_ROUNDS` answers.
///
/// At least one round: a slice of zero rounds is a loop that never finishes a
/// stamp, which is a worse failure than a deaf one.
pub fn slice_rounds_for(budget_us: u32, rounds: u32, workblock_us: u32) -> u32 {
    if workblock_us == 0 {
        return rounds.max(1);
    }
    let fit = (u64::from(budget_us) * u64::from(rounds)) / u64::from(workblock_us);
    (fit as u32).clamp(1, rounds.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loop's own turn through the nine-arm `select` and the action
    /// dispatch that follows a pass. Not measured on the board — it cannot be,
    /// from here — so it is used as an ALLOWANCE and always spent against us:
    /// every assertion below charges it on top of the absence, and the
    /// allowance is half the frame budget, so a slice sized under the other
    /// half is green however the real turnaround falls inside it.
    const TURNAROUND_ALLOWANCE_US: u32 = FRAME_BUDGET_US / 2;

    /// **Red before the fix.** One settle pass that awaits the whole workblock
    /// keeps the loop away for the whole workblock: 3727 ms, against a 166 ms
    /// frame at the PHY the propagation cells run.
    #[test]
    fn an_unsliced_pass_is_away_for_a_whole_workblock() {
        let pass = price_pass(
            SettlePolicy::WholeWorkblock,
            WORKBLOCK_ROUNDS_PN,
            WORKBLOCK_US_NRF52840,
        );
        assert_eq!(pass.absence_us, u64::from(WORKBLOCK_US_NRF52840));
        assert!(
            pass.absence_us > u64::from(FRAME_BUDGET_US),
            "the pre-fix absence must exceed the frame budget, or this file \
             asserts nothing: absence {} us, budget {} us",
            pass.absence_us,
            FRAME_BUDGET_US
        );
        // The factor, so the margin is in the record and not only the verdict.
        assert_eq!(pass.absence_us / u64::from(FRAME_BUDGET_US), 22);
    }

    /// **Green after the fix.** The slice the firmware runs, plus the whole
    /// turnaround allowance, fits inside one frame.
    #[test]
    fn a_sliced_pass_returns_inside_one_frame() {
        let pass = price_pass(
            SettlePolicy::Sliced {
                rounds: GRIND_SLICE_ROUNDS,
            },
            WORKBLOCK_ROUNDS_PN,
            WORKBLOCK_US_NRF52840,
        );
        let gap = pass.absence_us + u64::from(TURNAROUND_ALLOWANCE_US);
        assert!(
            gap <= u64::from(FRAME_BUDGET_US),
            "sliced gap {} us must fit the {} us frame budget (absence {} us, \
             turnaround allowance {} us)",
            gap,
            FRAME_BUDGET_US,
            pass.absence_us,
            TURNAROUND_ALLOWANCE_US
        );
        // 20 of 1000 rounds at 3727 ms is 74.54 ms, rounded up.
        assert_eq!(pass.absence_us, 74_540);
    }

    /// **The control.** A slice as wide as the workblock is the pre-fix
    /// schedule under another name, and it fails the same assertion — the
    /// harness can go red.
    #[test]
    fn a_slice_the_width_of_the_workblock_is_the_unsliced_policy() {
        let sliced = price_pass(
            SettlePolicy::Sliced {
                rounds: WORKBLOCK_ROUNDS_PN,
            },
            WORKBLOCK_ROUNDS_PN,
            WORKBLOCK_US_NRF52840,
        );
        let whole = price_pass(
            SettlePolicy::WholeWorkblock,
            WORKBLOCK_ROUNDS_PN,
            WORKBLOCK_US_NRF52840,
        );
        assert_eq!(sliced, whole);
        assert!(
            sliced.absence_us + u64::from(TURNAROUND_ALLOWANCE_US) > u64::from(FRAME_BUDGET_US)
        );
    }

    /// A slice wider than the workblock is the workblock, and a zero slice is
    /// one round — neither shape can hang the loop.
    #[test]
    fn slice_widths_are_clamped_at_both_ends() {
        assert_eq!(
            SettlePolicy::Sliced { rounds: 0 }.rounds_per_pass(WORKBLOCK_ROUNDS_PN),
            1
        );
        assert_eq!(
            SettlePolicy::Sliced { rounds: 9_000 }.rounds_per_pass(WORKBLOCK_ROUNDS_PN),
            WORKBLOCK_ROUNDS_PN
        );
        assert_eq!(
            SettlePolicy::Sliced { rounds: 0 }.passes_per_workblock(WORKBLOCK_ROUNDS_PN),
            WORKBLOCK_ROUNDS_PN
        );
    }

    /// **The rig's number, reproduced.** Seven reassembled frames in the LoRa
    /// task's hand against four channel slots is three blocked hand-offs, one
    /// per settle pass: `3 * 3727 ms` is 11.2 s, and the night run measured
    /// `dark_ms=10284` on the same three-stamp span. The model is a bound on
    /// the observation, and the observation lands under it.
    #[test]
    fn the_nights_dark_span_follows_from_the_unsliced_absence() {
        let pass = price_pass(
            SettlePolicy::WholeWorkblock,
            WORKBLOCK_ROUNDS_PN,
            WORKBLOCK_US_NRF52840,
        );
        let dark = dark_span_us(pass.absence_us, 0, HANDOFF_SLOTS + 3, HANDOFF_SLOTS);
        assert_eq!(dark, 11_181_000);
        assert!(
            dark >= 10_284_000,
            "the model must cover the measured 10284 ms dark span, got {} us",
            dark
        );
    }

    /// The same seven frames with the slicing: the dark span collapses from
    /// 11.2 s to under a quarter of a second, and stays inside three frames —
    /// one per frame the task still owes the loop, which is the floor this
    /// mechanism has.
    #[test]
    fn slicing_collapses_the_dark_span_to_one_frame_per_blocked_handoff() {
        let pass = price_pass(
            SettlePolicy::Sliced {
                rounds: GRIND_SLICE_ROUNDS,
            },
            WORKBLOCK_ROUNDS_PN,
            WORKBLOCK_US_NRF52840,
        );
        let blocked = 3;
        let dark = dark_span_us(
            pass.absence_us,
            TURNAROUND_ALLOWANCE_US,
            HANDOFF_SLOTS + blocked,
            HANDOFF_SLOTS,
        );
        assert!(
            dark <= u64::from(blocked) * u64::from(FRAME_BUDGET_US),
            "{} us must stay inside {} frames",
            dark,
            blocked
        );
        assert_eq!(dark, 472_620);
    }

    /// Nothing beyond the channel's slots is nothing dark: a validating engine
    /// on a quiet channel costs the receiver no window.
    #[test]
    fn an_absence_nobody_waits_out_costs_no_darkness() {
        assert_eq!(
            dark_span_us(
                u64::from(WORKBLOCK_US_NRF52840),
                1_000,
                HANDOFF_SLOTS,
                HANDOFF_SLOTS
            ),
            0
        );
        assert_eq!(
            dark_span_us(u64::from(WORKBLOCK_US_NRF52840), 1_000, 0, HANDOFF_SLOTS),
            0
        );
    }

    /// Batch length multiplies passes, never one absence. Ten messages is ten
    /// unsliced passes or 10 x 50 sliced ones, and in both cases the longest
    /// single absence is one pass's.
    #[test]
    fn a_batch_multiplies_passes_and_not_the_longest_absence() {
        let whole = drain(
            SettlePolicy::WholeWorkblock,
            10,
            WORKBLOCK_ROUNDS_PN,
            WORKBLOCK_US_NRF52840,
        );
        assert_eq!(whole.passes, 10);
        assert_eq!(whole.longest_absence_us, u64::from(WORKBLOCK_US_NRF52840));

        let sliced = drain(
            SettlePolicy::Sliced {
                rounds: GRIND_SLICE_ROUNDS,
            },
            10,
            WORKBLOCK_ROUNDS_PN,
            WORKBLOCK_US_NRF52840,
        );
        assert_eq!(sliced.passes, 500);
        assert_eq!(sliced.longest_absence_us, 74_540);
    }

    /// The sizing rule the firmware's constant answers: 20 rounds is the
    /// largest slice that fits half a frame, which is the half the turnaround
    /// allowance does not take.
    #[test]
    fn the_firmwares_slice_is_the_largest_that_fits_its_half_frame() {
        let fit = slice_rounds_for(
            FRAME_BUDGET_US - TURNAROUND_ALLOWANCE_US,
            WORKBLOCK_ROUNDS_PN,
            WORKBLOCK_US_NRF52840,
        );
        assert_eq!(fit, 22);
        assert!(
            GRIND_SLICE_ROUNDS <= fit,
            "the firmware's slice ({GRIND_SLICE_ROUNDS}) must fit the derived \
             maximum ({fit})"
        );
    }

    /// Degenerate inputs are answered, not divided by.
    #[test]
    fn a_free_workblock_and_a_zero_budget_are_both_answered() {
        assert_eq!(
            slice_rounds_for(0, WORKBLOCK_ROUNDS_PN, WORKBLOCK_US_NRF52840),
            1
        );
        assert_eq!(
            slice_rounds_for(1_000, WORKBLOCK_ROUNDS_PN, 0),
            WORKBLOCK_ROUNDS_PN
        );
        assert_eq!(
            price_pass(SettlePolicy::WholeWorkblock, 0, 1_000).absence_us,
            0
        );
    }
}
