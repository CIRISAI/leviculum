//! What a raw SAADC count means in millivolts at the battery terminal
//! (Codeberg #380).
//!
//! # Why the gain is in here and not in a comment
//!
//! A battery reading is a product of three numbers: the ADC's full-scale
//! input voltage, the board's external divider, and the raw count. Only
//! the last of the three is measured; the other two are configuration,
//! and until this crate existed exactly one of them was written down.
//!
//! The full scale is `reference / gain`. The firmware never set either:
//! `ChannelConfig::single_ended` *happens* to pick `Reference::INTERNAL`
//! (0.6 V) and `Gain::GAIN1_6` (`embassy-nrf-0.9.0/src/saadc.rs:100`),
//! which makes the full scale 3.6 V, which is the 3600 the conversion
//! divided by. Nothing said so, nothing checked it, and the day that
//! default moves — an embassy release, an `_nrf54l`-style cfg, a second
//! channel added with a different gain — every battery reading on every
//! board shifts by a fixed factor and stays entirely plausible. A
//! half-charged pack reads as a full one and nobody can tell.
//!
//! So the gain lives here as data, [`AdcGain`]; the firmware sets the
//! channel from it *and* converts with it, and [`AdcGain::full_scale_mv`]
//! derives the millivolts. There is one number, and changing it changes
//! the register and the arithmetic together — or fails a test.
//!
//! # Why the divider is not in here
//!
//! It is a property of two resistors on a particular board, so it lives
//! in that board's file (`boards/rak4631.rs`, `boards/t114.rs`) next to
//! the pin it is wired to, and arrives here as a constructor argument.
//! Both numbers are the Meshtastic variant's, and both are stated there
//! with the divider they come from:
//! `heltec_mesh_node_t114/variant.h:213` gives 4.916 and line 219 the
//! divider `AIN2 = VBAT * (100/490)`; `rak4631_epaper/variant.h:218`
//! gives 1.73 the same way. The variants' `AREF_VOLTAGE 3.0` is *their*
//! ADC gain choice and not ours — gain and divider are independent, so
//! our 3600 stays right beside their 4.916.
//!
//! # What this crate cannot see
//!
//! Nothing here is a brownout detector. The board that reset twice on a
//! field walk (#380) took its log with it; a sampler running at 1 Hz
//! sees a sustained sag under transmit load, not the microsecond
//! transient that trips the regulator. What it buys is the *margin* —
//! how close the pack sits to the edge before the edge is reached —
//! which is the thing we have never once measured.

#![cfg_attr(not(test), no_std)]

use core::fmt;

/// The nRF52840 SAADC's internal band-gap reference, in millivolts
/// (nRF52840 PS v1.8, §6.23: 0.6 V). The only other reference,
/// `VDD1_4`, is a fraction of a supply rail and therefore has no
/// constant full scale, which is why this crate models the internal one
/// alone: it is the one the firmware sets.
pub const INTERNAL_REFERENCE_MV: u32 = 600;

/// Raw count that means full scale at 12-bit resolution
/// (`Resolution::_12BIT`, set alongside the gain in `battery.rs`).
pub const RAW_FULL_SCALE: u32 = 4095;

/// The SAADC gain setting, as the factor the input is multiplied by
/// before the comparator sees it — so a *smaller* gain accepts a
/// *larger* input voltage.
///
/// Mirrors `embassy_nrf::saadc::Gain` for the nRF52840 rather than
/// depending on it: this crate builds for the host, `embassy-nrf` does
/// not. The firmware maps one to the other at a single `match`, which
/// is also where the compiler catches a variant this table has missed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdcGain {
    /// `GAIN1_6` — full scale 3.6 V against the internal reference.
    OneSixth,
    /// `GAIN1_5` — full scale 3.0 V.
    OneFifth,
    /// `GAIN1_4` — full scale 2.4 V.
    OneQuarter,
    /// `GAIN1_3` — full scale 1.8 V.
    OneThird,
    /// `GAIN1_2` — full scale 1.2 V.
    OneHalf,
    /// `GAIN1` — full scale 0.6 V.
    Unity,
    /// `GAIN2` — full scale 0.3 V.
    Two,
    /// `GAIN4` — full scale 0.15 V.
    Four,
}

impl AdcGain {
    /// The gain as `(numerator, denominator)`.
    const fn ratio(self) -> (u32, u32) {
        match self {
            AdcGain::OneSixth => (1, 6),
            AdcGain::OneFifth => (1, 5),
            AdcGain::OneQuarter => (1, 4),
            AdcGain::OneThird => (1, 3),
            AdcGain::OneHalf => (1, 2),
            AdcGain::Unity => (1, 1),
            AdcGain::Two => (2, 1),
            AdcGain::Four => (4, 1),
        }
    }

    /// Millivolts at the ADC pin that read [`RAW_FULL_SCALE`].
    ///
    /// `reference / gain`: the comparator sees `V_pin × gain` and tops
    /// out at the reference, so the pin tops out at `reference` divided
    /// by the gain.
    pub const fn full_scale_mv(self) -> u32 {
        let (num, den) = self.ratio();
        INTERNAL_REFERENCE_MV * den / num
    }
}

/// The gain the firmware's battery channel is configured with, and the
/// gain its conversion is derived from — one constant serving both, so
/// they cannot disagree.
///
/// `OneSixth` because it is the widest input range the nRF52840 offers
/// (3.6 V at the pin) and both boards' dividers were sized against a
/// reference ADC that had the same range. Changing it changes the
/// register the firmware writes *and* the millivolts every conversion
/// divides by; the tests below pin both boards' resulting full scales,
/// so a change here fails a test rather than silently rescaling every
/// reading in the field.
pub const CONFIGURED_GAIN: AdcGain = AdcGain::OneSixth;

/// Everything between a raw count and a millivolt reading at the
/// battery terminal: the ADC's configured gain and the board's external
/// divider.
///
/// Constructed once per board from that board's `ADC_MULTIPLIER`, and
/// then used for both jobs — the firmware asks it for [`Self::gain`] to
/// configure the channel and calls [`Self::raw_to_battery_mv`] to read
/// it, so the register and the arithmetic cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryScale {
    gain: AdcGain,
    /// The board's divider multiplier in thousandths. Integer, because
    /// the conversion is integer: 1.73 → 1730, 4.916 → 4916.
    multiplier_milli: u32,
    /// Millivolts at the *battery terminal* that read [`RAW_FULL_SCALE`],
    /// i.e. the pin's full scale times the divider. Precomputed so the
    /// per-sample conversion is one multiply and one divide.
    terminal_full_scale_mv: u32,
}

impl BatteryScale {
    /// Build the scale for one board.
    ///
    /// `divider_multiplier` is that board's `ADC_MULTIPLIER`: the factor
    /// from volts at the pin back to volts at the terminal. It is `f32`
    /// because the board files state it as one; it is rounded to
    /// thousandths here, which is exact for both boards we have (1.730
    /// and 4.916) and is 2 mV of resolution at a 6 V full scale for any
    /// board we do not.
    pub const fn new(gain: AdcGain, divider_multiplier: f32) -> Self {
        let multiplier_milli = (divider_multiplier * 1000.0 + 0.5) as u32;
        Self {
            gain,
            multiplier_milli,
            terminal_full_scale_mv: (gain.full_scale_mv() * multiplier_milli + 500) / 1000,
        }
    }

    /// The scale for a board whose divider multiplier is
    /// `divider_multiplier`, at the gain the firmware actually
    /// configures ([`CONFIGURED_GAIN`]).
    ///
    /// The one constructor the firmware uses: a board file states its
    /// multiplier and nothing else, and the ADC side of the arithmetic
    /// is not a per-board choice.
    pub const fn for_board(divider_multiplier: f32) -> Self {
        Self::new(CONFIGURED_GAIN, divider_multiplier)
    }

    /// The gain the channel must be configured with for this scale to be
    /// true.
    pub const fn gain(&self) -> AdcGain {
        self.gain
    }

    /// The board's divider multiplier in thousandths (1.73 → 1730).
    pub const fn multiplier_milli(&self) -> u32 {
        self.multiplier_milli
    }

    /// Millivolts at the battery terminal that read [`RAW_FULL_SCALE`].
    ///
    /// The RAK's 1.73 divider puts this at 6228 mV — a 1S pack uses
    /// two thirds of the range. The T114's 4.916 puts it at 14 698 mV,
    /// so a 1S pack at 4.2 V sits at 0.854 V on the pin and one LSB is
    /// 3.6 mV at the terminal. Ample either way; a reading *above* this
    /// is impossible and one near it means the input is floating.
    pub const fn terminal_full_scale_mv(&self) -> u32 {
        self.terminal_full_scale_mv
    }

    /// Convert one raw sample to millivolts at the battery terminal.
    ///
    /// `Saadc::sample` returns `i16`; a single-ended positive input
    /// cannot be negative, so a negative value is rail noise and clamps
    /// to zero.
    pub fn raw_to_battery_mv(&self, raw: i16) -> u16 {
        let r = raw.max(0) as u32;
        // r ≤ 4095 and the terminal full scale is ≤ 14 698 for any board
        // we build, so the product stays inside u32 by four orders of
        // magnitude; the saturating cast is for a board that does not
        // exist yet rather than for one that does.
        let mv = (r * self.terminal_full_scale_mv) / RAW_FULL_SCALE;
        if mv > u16::MAX as u32 {
            u16::MAX
        } else {
            mv as u16
        }
    }
}

/// Per-cell open-circuit-voltage curve (mV → percent), borrowed verbatim
/// from `meshtastic/src/power.h:13-22`. Piecewise-linear between
/// adjacent points.
const OCV_CURVE: [(u16, u8); 11] = [
    (4190, 100),
    (4050, 90),
    (3990, 80),
    (3890, 65),
    (3800, 50),
    (3720, 35),
    (3630, 20),
    (3530, 10),
    (3420, 5),
    (3300, 0),
    (3000, 0), // sentinel — anything below 3.0 V/cell stays at 0 %
];

/// Map a per-cell millivolt reading to a 0–100 percent estimate via the
/// LiPo OCV curve. Above the top breakpoint clamps to 100 %, below the
/// bottom to 0 %.
pub fn cell_mv_to_percent(cell_mv: u16) -> u8 {
    if cell_mv >= OCV_CURVE[0].0 {
        return 100;
    }
    for i in 0..OCV_CURVE.len() - 1 {
        let (hi_mv, hi_pct) = OCV_CURVE[i];
        let (lo_mv, lo_pct) = OCV_CURVE[i + 1];
        if cell_mv >= lo_mv {
            let span_mv = (hi_mv - lo_mv) as u32;
            let span_pct = (hi_pct - lo_pct) as u32;
            let above = (cell_mv - lo_mv) as u32;
            return (lo_pct as u32 + above * span_pct / span_mv.max(1)) as u8;
        }
    }
    0
}

/// A live 1S pack never reads below this. A LiPo's own protection
/// circuit opens near 2.5 V and the board's regulator gives out before
/// that, so a reading under it is not a battery: it is a floating input,
/// a divider whose enable pin never went high, or a sample taken before
/// the divider settled.
pub const PLAUSIBLE_FLOOR_MV: u16 = 2500;

/// Where 2S begins. A freshly charged 1S sits at 4.2 V and we allow it
/// a wide margin; a 2S pack floors near 6.0 V even at deep discharge, so
/// the gap between the two is about 1.6 V wide and nothing legitimate
/// lands in it.
pub const TWO_S_FLOOR_MV: u16 = 6000;

/// A 2S pack tops out at 8.4 V charging. Above this margin the reading
/// is not a pack — on the T114 the scale reaches 14.7 V, so a floating
/// or stuck-high input lands here and used to be classified as 2S,
/// halving every subsequent per-cell voltage.
pub const PLAUSIBLE_CEILING_MV: u16 = 9000;

/// What one reading says about the pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellClass {
    /// Cells in series: 1 or 2. Never anything else — an implausible
    /// reading gets 1, the configuration that cannot make a good pack
    /// look empty.
    pub cells: u8,
    /// False when the reading is outside anything a LiPo pack can be, in
    /// which case [`Self::cells`] is a fallback and not a measurement.
    /// The caller says so in the log; silently guessing is how a wrong
    /// multiplier or an unenabled divider stays invisible for a year.
    pub plausible: bool,
}

/// Decide whether the pack is 1S or 2S from a single millivolt reading.
///
/// The doc on the old version of this promised a warning on an
/// in-between reading and the code had no way to give one — it returned
/// a bare `u8`, so every reading looked equally trustworthy including
/// the ones that were not readings at all. The guard matters more now
/// than it did: with the RAK's 1.73 divider the whole range tops out at
/// 6.2 V, so a floating input could barely reach the 2S threshold; with
/// the T114's 4.916 it reaches 14.7 V, and an input that is not
/// connected classifies as 2S with total confidence.
pub fn classify_cell_count(pack_mv: u16) -> CellClass {
    if !(PLAUSIBLE_FLOOR_MV..=PLAUSIBLE_CEILING_MV).contains(&pack_mv) {
        CellClass {
            cells: 1,
            plausible: false,
        }
    } else if pack_mv >= TWO_S_FLOOR_MV {
        CellClass {
            cells: 2,
            plausible: true,
        }
    } else {
        CellClass {
            cells: 1,
            plausible: true,
        }
    }
}

/// The smoothing filter behind the published pack voltage.
///
/// Meshtastic runs an α = 0.5 low-pass on this same signal
/// (`power.cpp:392`) and so did we, once per 5 s reading. Sampling moved
/// to 1 Hz so a transmit sag is visible in `min_mv`, and keeping α = 0.5
/// there would have shortened the filter's time constant fivefold and
/// pushed that same sag onto the status display, which quantises the
/// voltage to 100 mV and would have redrawn on every packet. So the
/// per-sample retention is 7/8 instead: `0.875⁵ = 0.513`, i.e. after
/// five 1 Hz samples the filter has moved as far as one old 5 s step
/// did, to within a percent. The display sees exactly what it saw
/// before; only the sampler underneath got faster.
#[derive(Debug, Clone, Copy)]
pub struct BatteryEwma {
    mv: u32,
}

impl BatteryEwma {
    /// Seed the filter with the first sample rather than with zero, so
    /// the published voltage is right immediately instead of climbing
    /// out of an invented empty pack over the first half minute.
    pub const fn new(first_mv: u16) -> Self {
        Self {
            mv: first_mv as u32,
        }
    }

    /// Fold in one sample and return the filtered value. The `+ 4`
    /// rounds to nearest: truncating would bias the output about half a
    /// millivolt low per step, which on a monotonically discharging pack
    /// accumulates in one direction.
    pub fn step(&mut self, sample_mv: u16) -> u16 {
        self.mv = (self.mv * 7 + sample_mv as u32 + 4) / 8;
        self.mv as u16
    }

    /// The current filtered value, without folding anything in.
    pub const fn mv(&self) -> u16 {
        self.mv as u16
    }
}

/// The extremes seen since the last report.
///
/// The report period holds these rather than a mean because the mean is
/// what hides the interesting event: a pack that averages 3.9 V while
/// dropping to 3.2 V under every transmission is a pack about to reset
/// the board, and it looks identical to a healthy one until the minimum
/// is kept.
#[derive(Debug, Clone, Copy)]
pub struct BatteryWindow {
    min_mv: u16,
    max_mv: u16,
}

impl BatteryWindow {
    /// Open a window on its first sample. There is no "empty" window:
    /// an unopened one would have to report `min = u16::MAX`, and that
    /// value reaching a log line once is one wrong reading too many.
    pub const fn new(first_mv: u16) -> Self {
        Self {
            min_mv: first_mv,
            max_mv: first_mv,
        }
    }

    /// Fold in one sample.
    pub fn observe(&mut self, mv: u16) {
        if mv < self.min_mv {
            self.min_mv = mv;
        }
        if mv > self.max_mv {
            self.max_mv = mv;
        }
    }

    /// Lowest sample in the window.
    pub const fn min_mv(&self) -> u16 {
        self.min_mv
    }

    /// Highest sample in the window.
    pub const fn max_mv(&self) -> u16 {
        self.max_mv
    }

    /// Start the next window, seeded with the sample that just ended
    /// this one, so the new window is never empty and no sample falls
    /// between two windows.
    pub fn restart(&mut self, mv: u16) {
        self.min_mv = mv;
        self.max_mv = mv;
    }
}

/// The `BATTERY` line's body, byte-exact.
///
/// Rendered through [`fmt::Display`] so a host test can assert the bytes
/// a field capture will be grepped for. The trailing `t=<ms>` stamp is
/// NOT part of it: the board's log layer appends it to every line
/// (`leviculum-log-line`), and duplicating it here would produce two.
pub struct BatteryLine {
    /// Filtered pack voltage — the same number the display shows.
    pub mv: u16,
    /// Lowest and highest raw samples of the period, the pair that makes
    /// a sag under load visible instead of averaged away.
    pub min_mv: u16,
    /// See [`Self::min_mv`].
    pub max_mv: u16,
    /// Charge estimate from the OCV curve, per cell.
    pub percent: u8,
    /// Cells in series, as classified at boot.
    pub cells: u8,
}

impl fmt::Display for BatteryLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BATTERY mv={} min_mv={} max_mv={} pct={} cells={}S",
            self.mv, self.min_mv, self.max_mv, self.percent, self.cells,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    /// The gain the firmware configures, spelled out. If this number
    /// moves, every reading in the field moves with it, so it fails a
    /// test instead.
    #[test]
    fn configured_gain_gives_a_full_scale_of_thirty_six_hundred_millivolts() {
        assert_eq!(AdcGain::OneSixth.full_scale_mv(), 3600);
    }

    /// The gain the firmware writes into the channel, and what it makes
    /// of each board's divider. All four numbers move together or not at
    /// all, which is the entire point of the crate: a gain changed
    /// without noticing rescales every reading on every board and every
    /// one of them stays plausible.
    #[test]
    fn the_configured_gain_pins_both_boards_full_scales() {
        assert_eq!(CONFIGURED_GAIN, AdcGain::OneSixth);
        assert_eq!(CONFIGURED_GAIN.full_scale_mv(), 3600);
        // RAK4631 / WisMesh Pocket V2, divider 1.73.
        assert_eq!(BatteryScale::for_board(1.73).terminal_full_scale_mv(), 6228);
        // Heltec Mesh Node T114, divider 4.916.
        assert_eq!(
            BatteryScale::for_board(4.916).terminal_full_scale_mv(),
            17698
        );
    }

    /// The rest of the table, so a mis-transcribed ratio cannot hide
    /// behind the one variant we use.
    #[test]
    fn every_gain_derives_its_full_scale_from_the_reference() {
        assert_eq!(AdcGain::OneFifth.full_scale_mv(), 3000);
        assert_eq!(AdcGain::OneQuarter.full_scale_mv(), 2400);
        assert_eq!(AdcGain::OneThird.full_scale_mv(), 1800);
        assert_eq!(AdcGain::OneHalf.full_scale_mv(), 1200);
        assert_eq!(AdcGain::Unity.full_scale_mv(), 600);
        assert_eq!(AdcGain::Two.full_scale_mv(), 300);
        assert_eq!(AdcGain::Four.full_scale_mv(), 150);
    }

    /// A changed gain must change the conversion, not just the register.
    /// Halving the full scale halves every reading — which is precisely
    /// the silent field-wide shift this crate exists to prevent, so it
    /// is asserted rather than assumed.
    #[test]
    fn a_different_gain_moves_the_conversion_with_it() {
        let six = BatteryScale::new(AdcGain::OneSixth, 1.73);
        let three = BatteryScale::new(AdcGain::OneThird, 1.73);
        assert_eq!(six.terminal_full_scale_mv(), 6228);
        assert_eq!(three.terminal_full_scale_mv(), 3114);
        assert_eq!(
            three.raw_to_battery_mv(4095) * 2,
            six.raw_to_battery_mv(4095)
        );
    }

    /// The refactor must change nothing on the board that works today:
    /// the RAK's scale has to reproduce the old hard-coded
    /// `raw * 6228 / 4095` for every raw value, bit for bit.
    #[test]
    fn rak_multiplier_reproduces_the_previous_conversion_exactly() {
        fn previous(raw: i16) -> u16 {
            let r = raw.max(0) as u32;
            ((r * 6228) / 4095) as u16
        }
        let scale = BatteryScale::new(AdcGain::OneSixth, 1.73);
        for raw in [0i16, 1, 100, 1000, 2048, 2731, 2850, 3000, 4094, 4095] {
            assert_eq!(
                scale.raw_to_battery_mv(raw),
                previous(raw),
                "raw={raw} diverged from the pre-refactor conversion"
            );
        }
        // And across the whole range, not just the handful above.
        for raw in 0..=4095i16 {
            assert_eq!(scale.raw_to_battery_mv(raw), previous(raw), "raw={raw}");
        }
    }

    /// A negative count is rail noise on a single-ended input, not a
    /// negative voltage.
    #[test]
    fn negative_raw_counts_clamp_to_zero() {
        let scale = BatteryScale::new(AdcGain::OneSixth, 1.73);
        assert_eq!(scale.raw_to_battery_mv(-1), 0);
        assert_eq!(scale.raw_to_battery_mv(i16::MIN), 0);
    }

    /// The T114's divider, from `variant.h:213`. The full scale lands at
    /// 14.7 V and a 1S pack at 4.2 V reads back as 4.2 V through it.
    #[test]
    fn t114_multiplier_reads_a_full_pack_as_a_full_pack() {
        let scale = BatteryScale::new(AdcGain::OneSixth, 4.916);
        assert_eq!(scale.multiplier_milli(), 4916);
        assert_eq!(scale.terminal_full_scale_mv(), 17698);
        // 4.2 V at the terminal is 4200 / 4.916 = 854.3 mV at the pin,
        // which is 854.3 / 3600 × 4095 = 972 counts.
        let mv = scale.raw_to_battery_mv(972);
        assert!(
            (4190..=4210).contains(&mv),
            "972 counts should read as a full 1S pack, got {mv} mV"
        );
    }

    /// One LSB at the T114's scale, so the resolution claim in the doc
    /// is a checked number and not a hope.
    #[test]
    fn t114_resolution_is_a_few_millivolts_per_count() {
        let scale = BatteryScale::new(AdcGain::OneSixth, 4.916);
        let step = scale.raw_to_battery_mv(1001) - scale.raw_to_battery_mv(1000);
        assert!(step <= 5, "one count moved the reading by {step} mV");
    }

    #[test]
    fn a_normal_single_cell_pack_classifies_as_one_cell() {
        for mv in [3000u16, 3700, 4200, 4400] {
            let c = classify_cell_count(mv);
            assert_eq!(c.cells, 1, "{mv} mV");
            assert!(c.plausible, "{mv} mV");
        }
    }

    #[test]
    fn a_normal_two_cell_pack_classifies_as_two_cells() {
        for mv in [6000u16, 7400, 8400] {
            let c = classify_cell_count(mv);
            assert_eq!(c.cells, 2, "{mv} mV");
            assert!(c.plausible, "{mv} mV");
        }
    }

    /// The guard the doc comment promised and the code never had. A
    /// T114 whose divider-enable pin never went high, or whose input is
    /// floating, reads far above any pack; the old code called that 2S
    /// and then halved every per-cell voltage for the rest of the boot.
    #[test]
    fn an_out_of_range_reading_classifies_as_one_cell_not_two() {
        let floating = classify_cell_count(14_500);
        assert_eq!(floating.cells, 1);
        assert!(!floating.plausible);

        let dead_input = classify_cell_count(0);
        assert_eq!(dead_input.cells, 1);
        assert!(!dead_input.plausible);

        // The full scale of the T114's own divider — the exact value a
        // stuck-high input produces on that board.
        let full_scale = BatteryScale::new(AdcGain::OneSixth, 4.916).terminal_full_scale_mv();
        let stuck = classify_cell_count(full_scale as u16);
        assert_eq!(stuck.cells, 1);
        assert!(!stuck.plausible);
    }

    /// The boundaries themselves, so a later edit to either constant has
    /// to be deliberate.
    #[test]
    fn classification_boundaries_are_where_the_constants_say() {
        assert!(!classify_cell_count(PLAUSIBLE_FLOOR_MV - 1).plausible);
        assert!(classify_cell_count(PLAUSIBLE_FLOOR_MV).plausible);
        assert_eq!(classify_cell_count(TWO_S_FLOOR_MV - 1).cells, 1);
        assert_eq!(classify_cell_count(TWO_S_FLOOR_MV).cells, 2);
        assert!(classify_cell_count(PLAUSIBLE_CEILING_MV).plausible);
        assert!(!classify_cell_count(PLAUSIBLE_CEILING_MV + 1).plausible);
    }

    /// The percent curve is unchanged by the move; its endpoints and one
    /// interpolated point pin it.
    #[test]
    fn the_ocv_curve_maps_its_endpoints_and_interpolates_between() {
        assert_eq!(cell_mv_to_percent(4200), 100);
        assert_eq!(cell_mv_to_percent(4190), 100);
        assert_eq!(cell_mv_to_percent(3300), 0);
        assert_eq!(cell_mv_to_percent(2900), 0);
        // Midway between (3800, 50) and (3890, 65): 45 mV up a 90 mV
        // span is half of the 15-point step.
        assert_eq!(cell_mv_to_percent(3845), 57);
    }

    /// Five steps of the 1 Hz filter must land where one step of the old
    /// 5 s filter landed, or the display's smoothing changed under it.
    #[test]
    fn five_fast_steps_match_one_old_slow_step() {
        let old_one_step = (4000u32 + 3000) / 2; // α = 0.5, as before
        let mut ewma = BatteryEwma::new(4000);
        let mut mv = 0;
        for _ in 0..5 {
            mv = ewma.step(3000);
        }
        let delta = (mv as i32 - old_one_step as i32).abs();
        assert!(
            delta <= 15,
            "five 1 Hz steps reached {mv} mV, one 5 s step reached {old_one_step} mV"
        );
    }

    /// A filter seeded with the first reading must not drift off it
    /// while the reading holds — rounding that biased in one direction
    /// would walk a resting pack downhill.
    #[test]
    fn a_steady_signal_does_not_drift() {
        let mut ewma = BatteryEwma::new(3812);
        for _ in 0..1000 {
            ewma.step(3812);
        }
        assert_eq!(ewma.mv(), 3812);
    }

    #[test]
    fn the_window_keeps_the_extremes_and_restarts_on_one_sample() {
        let mut w = BatteryWindow::new(3800);
        w.observe(3750);
        w.observe(3900);
        w.observe(3820);
        assert_eq!(w.min_mv(), 3750);
        assert_eq!(w.max_mv(), 3900);
        w.restart(3810);
        assert_eq!(w.min_mv(), 3810);
        assert_eq!(w.max_mv(), 3810);
    }

    /// The bytes a field capture is grepped for.
    #[test]
    fn the_line_renders_the_documented_shape() {
        let line = BatteryLine {
            mv: 3812,
            min_mv: 3604,
            max_mv: 3840,
            percent: 51,
            cells: 1,
        };
        assert_eq!(
            line.to_string(),
            "BATTERY mv=3812 min_mv=3604 max_mv=3840 pct=51 cells=1S"
        );
    }
}
