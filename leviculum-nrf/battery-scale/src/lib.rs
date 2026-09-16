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
//! (0.6 V) and `Gain::GAIN1_6` (`embassy-nrf-0.9.0/src/saadc.rs:104`),
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
//! A board that knows its two resistors rather than only their ratio
//! passes them instead, as [`Divider`] ([`BatteryScale::for_divider`]).
//! That is not tidiness: the ratio decides the conversion, but the
//! *magnitude* decides how long the SAADC has to hold the input before
//! it converts ([`AcquisitionTime`]), and a multiplier alone cannot say.
//! `boards/solarnode.rs` states 1 MΩ over 510 kΩ, which divides like a
//! 2.9608 multiplier and samples like nothing else we have — 338 kΩ of
//! source where the T114's 490 kΩ chain presents 80 kΩ.
//!
//! # What the percentage guard costs
//!
//! The cell count is decided from one reading at boot, and for the rest
//! of that boot every per-cell voltage is the pack voltage divided by
//! it. A wrong count therefore halves or doubles the percentage, and the
//! percentage that goes on the air carries neither a unit nor the count
//! it was divided by — a remote reader cannot tell a wrong one from a
//! right one. So [`pack_percent`] declines to derive a percentage from a
//! pack voltage the classified cell count cannot produce, and the caller
//! reports nothing rather than a number.
//!
//! The band's edges come from the curve below rather than from constants
//! of their own, because the guard and the percentage have to agree
//! about what a cell is. A cell that reads more than one step of the
//! curve's own top segment above its 100 % point ([`CELL_CEILING_MV`],
//! 4.33 V) is not a cell of a pack this size; the step is the headroom
//! the table itself offers, and it clears the 4.2 V a charger holds
//! during constant-voltage without admitting a reading that is a whole
//! extra cell wide.
//!
//! The floor is the decision that costs something, and it is deliberate.
//! The curve's bottom point is 3.0 V per cell and everything at or below
//! it reads 0 %: that is a real pack state — a nearly-empty one — and
//! not a measurement fault. A node that went quiet about its battery
//! exactly when the battery was about to give out would be worse than
//! one that reported zero. So the curve's floor is NOT the guard's
//! floor. The guard's floor is the protection cut-off
//! ([`PLAUSIBLE_FLOOR_MV`], 2.5 V per cell), below which a LiPo's own
//! protection circuit has opened and no live pack delivers current, so a
//! reading there is a floating input or a divider that did not settle
//! rather than an empty battery. Between the two, 2.5 to 3.0 V per cell,
//! the percentage is reported and it is 0 — which is the whole span in
//! which a live pack can be flat.
//!
//! What the guard does not do is re-classify. A pack that leaves its
//! band says so once and stays classified as it was; deciding the cell
//! count again at runtime is a different question from declining to
//! build on the answer already given.
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

/// How long the SAADC holds its input connected before it converts, as
/// the `TACQ` field the channel is configured with.
///
/// Mirrors `embassy_nrf::saadc::Time` for the nRF52840 rather than
/// depending on it, for the same reason [`AdcGain`] mirrors
/// `saadc::Gain`: this crate builds for the host. The firmware maps one
/// to the other at a single `match`.
///
/// This is the second default that was never a statement. The
/// conversion's correctness rests on the sampling capacitor having
/// reached the pin's voltage by the end of the window, and how long that
/// takes is set by the source resistance the *board's divider* presents
/// — so it is a per-board number, and until this existed every board got
/// `Time::_10US` because that is what `ChannelConfig::single_ended`
/// happens to pick (`embassy-nrf-0.9.0/src/saadc.rs:109`). On a divider
/// of a few tens of kΩ that is right; on one of a few hundred it is
/// outside what the part specifies, and the failure mode is a reading
/// that is low by an amount nothing in the log distinguishes from a
/// discharged pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AcquisitionTime {
    /// `TACQ = 3 µs`.
    ThreeUs,
    /// `TACQ = 5 µs`.
    FiveUs,
    /// `TACQ = 10 µs` — what `ChannelConfig::single_ended` defaults to.
    TenUs,
    /// `TACQ = 15 µs`.
    FifteenUs,
    /// `TACQ = 20 µs`.
    TwentyUs,
    /// `TACQ = 40 µs` — the longest the part offers.
    FortyUs,
}

/// The acquisition times, shortest first, each with the largest source
/// resistance the nRF52840 specifies it for.
///
/// nRF52840 PS v1.8, §6.23 (SAADC electrical specification), the
/// `TACQ` / maximum-source-resistance table. Shortest first because
/// [`AcquisitionTime::for_source_ohms`] takes the first row that covers
/// a source: a longer window than needed is time the ADC holds the
/// divider enabled for nothing.
const ACQUISITION_TABLE: [(AcquisitionTime, u32); 6] = [
    (AcquisitionTime::ThreeUs, 10_000),
    (AcquisitionTime::FiveUs, 40_000),
    (AcquisitionTime::TenUs, 100_000),
    (AcquisitionTime::FifteenUs, 200_000),
    (AcquisitionTime::TwentyUs, 400_000),
    (AcquisitionTime::FortyUs, 800_000),
];

impl AcquisitionTime {
    /// The window in microseconds.
    pub const fn micros(self) -> u32 {
        match self {
            AcquisitionTime::ThreeUs => 3,
            AcquisitionTime::FiveUs => 5,
            AcquisitionTime::TenUs => 10,
            AcquisitionTime::FifteenUs => 15,
            AcquisitionTime::TwentyUs => 20,
            AcquisitionTime::FortyUs => 40,
        }
    }

    /// The largest source resistance the part specifies this window for.
    pub const fn max_source_ohms(self) -> u32 {
        let mut i = 0;
        while i < ACQUISITION_TABLE.len() {
            let (t, ohms) = ACQUISITION_TABLE[i];
            if t as u8 == self as u8 {
                return ohms;
            }
            i += 1;
        }
        // Unreachable: the table covers every variant, and a test asserts
        // it. Answering with the smallest bound rather than panicking in
        // a `const fn` keeps a missed row conservative.
        ACQUISITION_TABLE[0].1
    }

    /// The shortest window the part specifies for a source of
    /// `source_ohms`, or [`None`] when no window covers it.
    ///
    /// `None` is not a detail to paper over: above 800 kΩ the part
    /// specifies nothing at all, so the board's divider is the thing that
    /// has to change, not the register.
    pub const fn for_source_ohms(source_ohms: u32) -> Option<Self> {
        let mut i = 0;
        while i < ACQUISITION_TABLE.len() {
            let (t, ohms) = ACQUISITION_TABLE[i];
            if source_ohms <= ohms {
                return Some(t);
            }
            i += 1;
        }
        None
    }
}

/// A board's resistive battery divider, as the two resistors it is.
///
/// The multiplier a board file used to state as one `f32` is derivable
/// from these and so is the source resistance the ADC sees, which is the
/// number that decides [`AcquisitionTime`] — and which a bare multiplier
/// cannot express at all: 100 kΩ over 390 kΩ and 1 MΩ over 3.9 MΩ divide
/// identically and are two very different things to sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divider {
    high_ohms: u32,
    low_ohms: u32,
}

impl Divider {
    /// `high_ohms` is the resistor between the battery terminal and the
    /// ADC pin, `low_ohms` the one between the pin and the divider's
    /// return — ground, or an enable pin that sinks it (see
    /// `boards/solarnode.rs`).
    pub const fn new(high_ohms: u32, low_ohms: u32) -> Self {
        Self {
            high_ohms,
            low_ohms,
        }
    }

    /// The factor from volts at the pin back to volts at the terminal,
    /// in thousandths: `(high + low) / low`.
    ///
    /// Computed in `u64` and rounded to nearest, so it matches to the
    /// millivolt what a board file that states the same divider as a
    /// decimal arrives at, without either of them being the source of
    /// the other.
    pub const fn multiplier_milli(&self) -> u32 {
        let total = self.high_ohms as u64 + self.low_ohms as u64;
        let low = self.low_ohms as u64;
        ((total * 1000 + low / 2) / low) as u32
    }

    /// The resistance the ADC pin looks back into while the divider is
    /// enabled: the two resistors in parallel, since one goes to the
    /// pack and the other to the return and both are low-impedance ends.
    pub const fn source_ohms(&self) -> u32 {
        let high = self.high_ohms as u64;
        let low = self.low_ohms as u64;
        let sum = high + low;
        if sum == 0 {
            return 0;
        }
        (high * low / sum) as u32
    }

    /// The shortest acquisition window the part specifies for this
    /// divider, or [`None`] when it specifies none.
    pub const fn acquisition_time(&self) -> Option<AcquisitionTime> {
        AcquisitionTime::for_source_ohms(self.source_ohms())
    }
}

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
    /// The acquisition window the channel must be configured with for a
    /// sample through this divider to have settled.
    acquisition_time: AcquisitionTime,
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
    ///
    /// The acquisition window is [`AcquisitionTime::TenUs`], which is
    /// what the channel has always been configured with — see that
    /// type's doc. A multiplier alone cannot say whether that is right:
    /// it is the *magnitude* of the two resistors that decides, and this
    /// constructor is not told it. A board that states its resistors
    /// uses [`Self::for_divider`] and gets the window derived.
    pub const fn new(gain: AdcGain, divider_multiplier: f32) -> Self {
        let multiplier_milli = (divider_multiplier * 1000.0 + 0.5) as u32;
        Self::from_parts(gain, AcquisitionTime::TenUs, multiplier_milli)
    }

    /// The scale for a board whose divider multiplier is
    /// `divider_multiplier`, at the gain the firmware actually
    /// configures ([`CONFIGURED_GAIN`]).
    ///
    /// A board file states its multiplier and nothing else. Both boards
    /// that predate [`Divider`] are on this path and their readings are
    /// unchanged by its arrival, which is asserted below rather than
    /// asserted here.
    pub const fn for_board(divider_multiplier: f32) -> Self {
        Self::new(CONFIGURED_GAIN, divider_multiplier)
    }

    /// The scale for a board that states its divider as the two
    /// resistors it is, at the gain the firmware actually configures
    /// ([`CONFIGURED_GAIN`]).
    ///
    /// Both halves of the channel configuration come from the same pair
    /// of numbers: the multiplier the conversion divides by, and the
    /// acquisition window the source resistance needs. A divider the
    /// part specifies no window for falls back to the longest one it
    /// has, [`AcquisitionTime::FortyUs`] — the best available, and
    /// [`Divider::acquisition_time`] is the honest answer for anything
    /// that has to know the part is out of its envelope.
    pub const fn for_divider(divider: Divider) -> Self {
        let acq = match divider.acquisition_time() {
            Some(t) => t,
            None => AcquisitionTime::FortyUs,
        };
        Self::from_parts(CONFIGURED_GAIN, acq, divider.multiplier_milli())
    }

    const fn from_parts(
        gain: AdcGain,
        acquisition_time: AcquisitionTime,
        multiplier_milli: u32,
    ) -> Self {
        Self {
            gain,
            acquisition_time,
            multiplier_milli,
            terminal_full_scale_mv: (gain.full_scale_mv() * multiplier_milli + 500) / 1000,
        }
    }

    /// The gain the channel must be configured with for this scale to be
    /// true.
    pub const fn gain(&self) -> AdcGain {
        self.gain
    }

    /// The acquisition window the channel must be configured with for a
    /// sample through this board's divider to have settled.
    pub const fn acquisition_time(&self) -> AcquisitionTime {
        self.acquisition_time
    }

    /// The board's divider multiplier in thousandths (1.73 → 1730).
    pub const fn multiplier_milli(&self) -> u32 {
        self.multiplier_milli
    }

    /// Millivolts at the battery terminal that read [`RAW_FULL_SCALE`].
    ///
    /// The RAK's 1.73 divider puts this at 6228 mV — a 1S pack uses
    /// two thirds of the range. The T114's 4.916 puts it at 17 698 mV,
    /// so a 1S pack at 4.2 V sits at 0.854 V on the pin and one LSB is
    /// 4.3 mV at the terminal; the Solar Node's 1 M/510 k lands between
    /// them at 10 660 mV, 2.6 mV per count. Ample on all three; a
    /// reading *above* this is impossible and one near it means the
    /// input is floating.
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

/// The per-cell reading the curve calls 100 %: its top breakpoint.
pub const CURVE_TOP_MV: u16 = OCV_CURVE[0].0;

/// The per-cell reading the curve's bottom sentinel sits at. At or below
/// it [`cell_mv_to_percent`] answers 0 %, and that answer is a pack
/// state and not an error — see the crate doc for why this is not the
/// guard's floor.
pub const CURVE_FLOOR_MV: u16 = OCV_CURVE[OCV_CURVE.len() - 1].0;

/// The highest per-cell voltage [`pack_band_mv`] admits: the curve's top
/// point carried one step of the curve's own top segment further
/// (4190 + 140 mV). The curve clamps above its top point instead of
/// saying how far above is still a cell, so the step is the only
/// headroom the table offers; it clears the 4.2 V a charger holds during
/// constant-voltage and stops far short of the 6.0 V that would be a
/// second cell.
pub const CELL_CEILING_MV: u16 = CURVE_TOP_MV + (OCV_CURVE[0].0 - OCV_CURVE[1].0);

/// The lowest per-cell voltage [`pack_band_mv`] admits. Deliberately
/// *below* [`CURVE_FLOOR_MV`]: an over-discharged pack is a real state
/// that must still report 0 %, and only a reading under the protection
/// cut-off stops being a live pack at all. The crate doc argues it.
pub const CELL_FLOOR_MV: u16 = PLAUSIBLE_FLOOR_MV;

/// The pack voltages a `cells`-cell pack can produce, inclusive, or
/// `None` for a cell count no pack has (0, or one whose band leaves the
/// u16 millivolts the whole ADC path speaks).
///
/// This is the range the classification *implies*, which is a different
/// and much tighter thing than the range [`classify_cell_count`] accepts:
/// the classifier only has to place a reading on one side of the 1S/2S
/// gap, while this has to say whether the reading can be that many cells
/// at all. 5.5 V classifies as 1S — it is under the 2S floor — and is
/// not one cell of anything.
pub const fn pack_band_mv(cells: u8) -> Option<(u16, u16)> {
    if cells == 0 {
        return None;
    }
    let n = cells as u32;
    let hi = CELL_CEILING_MV as u32 * n;
    if hi > u16::MAX as u32 {
        return None;
    }
    Some(((CELL_FLOOR_MV as u32 * n) as u16, hi as u16))
}

/// The charge estimate for a pack reading, or `None` when the reading is
/// outside what `cells` cells can produce.
///
/// `None` is not "unknown battery": the voltage is a measurement and the
/// caller keeps publishing it. It is "this percentage would be derived
/// through a classification the evidence no longer supports", which is
/// the one case in which a number is worse than no number — a percentage
/// carries neither its unit nor its cell count, so a reader far away has
/// nothing to check it against.
pub fn pack_percent(pack_mv: u16, cells: u8) -> Option<u8> {
    let (lo, hi) = pack_band_mv(cells)?;
    if pack_mv < lo || pack_mv > hi {
        return None;
    }
    Some(cell_mv_to_percent(pack_mv / cells as u16))
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
    /// Charge estimate from the OCV curve, per cell, or `None` when the
    /// pack voltage is outside the band the classified cell count implies
    /// ([`pack_percent`]). It renders as `pct=none`: the key stays in the
    /// line so a field capture can still be split on it, and the absence
    /// is stated rather than left to a reader to notice.
    pub percent: Option<u8>,
    /// Cells in series, as classified at boot.
    pub cells: u8,
}

impl fmt::Display for BatteryLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BATTERY mv={} min_mv={} max_mv={} pct=",
            self.mv, self.min_mv, self.max_mv,
        )?;
        match self.percent {
            Some(percent) => write!(f, "{}", percent)?,
            None => write!(f, "none")?,
        }
        write!(f, " cells={}S", self.cells)
    }
}

/// The `BATTERY_PCT` line's body, byte-exact.
///
/// Said once when the percentage starts or stops being derivable, not
/// once per reading: the sampler runs at 1 Hz, and a pack resting just
/// outside its band would otherwise put a line a second into the capture
/// and bury the transition that is the whole information. The pack
/// voltage and the band it was judged against travel with it, so the line
/// says why on its own without a reader having to know the constants.
pub struct BatteryPercentLine {
    /// Whether a percentage is being reported from here on.
    pub reportable: bool,
    /// The reading that flipped it.
    pub pack_mv: u16,
    /// Cells in series, as classified at boot — unchanged by this, which
    /// is the point: the board declines to build on the classification,
    /// it does not revise it.
    pub cells: u8,
    /// The band `cells` implies, inclusive, as [`pack_band_mv`] gives it.
    pub lo_mv: u16,
    /// See [`Self::lo_mv`].
    pub hi_mv: u16,
}

impl BatteryPercentLine {
    /// Build the line for one reading and the boot's classification.
    ///
    /// The band comes from [`pack_band_mv`], so the line cannot state
    /// edges the guard does not use. A cell count that has no band at all
    /// renders as `0..0`, which is what it is — the classifier never
    /// produces one, and a line that lied about it would be worse than
    /// one that shows an empty band.
    pub fn new(pack_mv: u16, cells: u8, reportable: bool) -> Self {
        let (lo_mv, hi_mv) = pack_band_mv(cells).unwrap_or((0, 0));
        Self {
            reportable,
            pack_mv,
            cells,
            lo_mv,
            hi_mv,
        }
    }
}

impl fmt::Display for BatteryPercentLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BATTERY_PCT reportable={} pack_mv={} cells={}S band_lo_mv={} band_hi_mv={}",
            u8::from(self.reportable),
            self.pack_mv,
            self.cells,
            self.lo_mv,
            self.hi_mv,
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

    /// The band's edges are the curve's, not numbers of their own. If
    /// the table is ever re-borrowed from a newer Meshtastic, the guard
    /// moves with it — which is the reason it was derived rather than
    /// written down.
    #[test]
    fn the_band_edges_come_from_the_curve() {
        assert_eq!(CURVE_TOP_MV, 4190);
        assert_eq!(CURVE_FLOOR_MV, 3000);
        // The curve's top segment is (4050, 90) → (4190, 100): a 140 mV
        // step, carried once past full.
        assert_eq!(CELL_CEILING_MV, 4330);
        // Comfortably above the 4.2 V a charger holds, and nowhere near
        // the 6.0 V that would be a second cell. `const` blocks because
        // both sides are constants and clippy is right that the check
        // belongs at compile time.
        const { assert!(CELL_CEILING_MV > 4200) };
        const { assert!(CELL_CEILING_MV < TWO_S_FLOOR_MV) };
    }

    /// The floor is the protection cut-off and NOT the curve's floor.
    /// The crate doc argues why; this is the assertion that keeps a later
    /// "tidy-up" from moving it up to 3000 and silencing every
    /// nearly-empty node.
    #[test]
    fn the_bands_floor_sits_below_the_curves_floor() {
        assert_eq!(CELL_FLOOR_MV, PLAUSIBLE_FLOOR_MV);
        const { assert!(CELL_FLOOR_MV < CURVE_FLOOR_MV) };
        assert_eq!(pack_band_mv(1), Some((2500, 4330)));
        assert_eq!(pack_band_mv(2), Some((5000, 8660)));
    }

    /// A reading inside the band yields a percentage.
    #[test]
    fn a_reading_inside_the_band_yields_a_percentage() {
        assert_eq!(pack_percent(4200, 1), Some(100));
        assert_eq!(pack_percent(3800, 1), Some(50));
        // The same pack voltages doubled, on a 2S classification that is
        // actually right.
        assert_eq!(pack_percent(8400, 2), Some(100));
        assert_eq!(pack_percent(7600, 2), Some(50));
    }

    /// The case that motivated the guard (#380): the boot reading
    /// classified 2S, the pack is a 1S one. Dividing by two lands the
    /// per-cell voltage at 1.95 V, the curve clamps it to 0 %, and a
    /// remote reader sees a nearly-full pack reported as flat — with
    /// nothing in the number to say it is wrong.
    #[test]
    fn a_two_cell_classification_with_a_one_cell_voltage_yields_nothing() {
        let pack_mv = 3900;
        // What the old code would have published, for the record.
        assert_eq!(cell_mv_to_percent(pack_mv / 2), 0);
        assert_eq!(pack_percent(pack_mv, 2), None);
        // And the same reading against the classification it really is.
        assert_eq!(pack_percent(pack_mv, 1), Some(cell_mv_to_percent(3900)));
    }

    /// The mirror case: a 1S classification carrying a voltage no single
    /// cell reaches. It is inside the plausible band the boot classifier
    /// checks (2.5–9 V) and under the 2S floor, so the classifier calls
    /// it 1S and is happy; the curve clamps it to 100 % and a half-empty
    /// 2S pack reports full.
    #[test]
    fn a_one_cell_classification_with_an_over_cell_voltage_yields_nothing() {
        let pack_mv = 5500;
        assert!(classify_cell_count(pack_mv).plausible);
        assert_eq!(classify_cell_count(pack_mv).cells, 1);
        assert_eq!(cell_mv_to_percent(pack_mv), 100);
        assert_eq!(pack_percent(pack_mv, 1), None);
    }

    /// Both edges of both bands, from both sides.
    #[test]
    fn the_band_boundaries_hold_from_both_sides() {
        for cells in [1u8, 2] {
            let (lo, hi) = pack_band_mv(cells).expect("band for a real cell count");
            assert_eq!(pack_percent(lo - 1, cells), None, "{cells}S just under lo");
            assert!(pack_percent(lo, cells).is_some(), "{cells}S at lo");
            assert!(pack_percent(hi, cells).is_some(), "{cells}S at hi");
            assert_eq!(pack_percent(hi + 1, cells), None, "{cells}S just over hi");
        }
    }

    /// The floor decision, made concrete: a 1S pack between the
    /// protection cut-off and the curve's floor is a real, nearly-empty
    /// pack. It reports 0 %, because a node that goes quiet about its
    /// battery exactly then is worse than one that reports zero.
    #[test]
    fn a_nearly_empty_pack_reports_zero_rather_than_nothing() {
        assert_eq!(pack_percent(2900, 1), Some(0));
        assert_eq!(pack_percent(2500, 1), Some(0));
        // Same on 2S: 2.5 V/cell is flat, not a fault.
        assert_eq!(pack_percent(5000, 2), Some(0));
        // Below the cut-off no live pack delivers current, so this is a
        // measurement fault and not an empty battery.
        assert_eq!(pack_percent(2400, 1), None);
    }

    /// A cell count no classifier produces still has to answer, and the
    /// answer is "no band" rather than a division by zero.
    #[test]
    fn a_cell_count_of_zero_has_no_band_and_no_percentage() {
        assert_eq!(pack_band_mv(0), None);
        assert_eq!(pack_percent(3700, 0), None);
        // And one whose band would leave the millivolt range the ADC path
        // speaks at all.
        assert_eq!(pack_band_mv(16), None);
        assert_eq!(pack_percent(60_000, 16), None);
    }

    /// The guard admits every reading the classifier calls a good pack of
    /// the count it just classified — otherwise a board would boot into
    /// the withheld state on a perfectly ordinary pack.
    #[test]
    fn every_plausible_classification_agrees_with_its_own_band() {
        for pack_mv in PLAUSIBLE_FLOOR_MV..=PLAUSIBLE_CEILING_MV {
            let class = classify_cell_count(pack_mv);
            if !class.plausible {
                continue;
            }
            let got = pack_percent(pack_mv, class.cells);
            // The classifier's 1S range runs to 5999 mV, well past what
            // one cell can be; those are exactly the readings the guard
            // exists to catch, so they are allowed to be None.
            let (lo, hi) = pack_band_mv(class.cells).expect("band");
            if (lo..=hi).contains(&pack_mv) {
                assert!(got.is_some(), "{pack_mv} mV as {}S", class.cells);
            } else {
                assert!(got.is_none(), "{pack_mv} mV as {}S", class.cells);
            }
        }
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
            percent: Some(51),
            cells: 1,
        };
        assert_eq!(
            line.to_string(),
            "BATTERY mv=3812 min_mv=3604 max_mv=3840 pct=51 cells=1S"
        );
    }

    /// The withheld form of the same line. The voltage is untouched —
    /// only the derived number is gone — and the `pct=` key survives so a
    /// capture split on keys does not lose a column.
    #[test]
    fn a_withheld_percentage_renders_as_none_and_keeps_the_voltage() {
        let line = BatteryLine {
            mv: 3900,
            min_mv: 3860,
            max_mv: 3940,
            percent: None,
            cells: 2,
        };
        assert_eq!(
            line.to_string(),
            "BATTERY mv=3900 min_mv=3860 max_mv=3940 pct=none cells=2S"
        );
    }

    /// The bytes of the state-change line, both ways round, with the band
    /// it was judged against.
    #[test]
    fn the_percent_gate_line_renders_the_documented_shape() {
        assert_eq!(
            BatteryPercentLine::new(3900, 2, false).to_string(),
            "BATTERY_PCT reportable=0 pack_mv=3900 cells=2S band_lo_mv=5000 band_hi_mv=8660"
        );
        assert_eq!(
            BatteryPercentLine::new(7400, 2, true).to_string(),
            "BATTERY_PCT reportable=1 pack_mv=7400 cells=2S band_lo_mv=5000 band_hi_mv=8660"
        );
    }

    // ---- SenseCAP Solar Node P1-Pro (Codeberg #233) ----
    //
    // Its divider is the XIAO nRF52840's own, R17 = 1 MΩ over R18 =
    // 510 kΩ, stated in `seeed_xiao_nrf52840_kit/variant.h:202` and
    // quoted in `boards/solarnode.rs`. Every number below is that
    // divider's and differs from the T114's, which is the point: a test
    // these constants pass and 4.916 also passes is not a test of this
    // board. Several of them assert the T114 answer alongside, so the
    // divergence is visible rather than merely claimed.

    /// The board's divider, as its board file states it.
    const SOLARNODE: Divider = Divider::new(1_000_000, 510_000);

    /// The scale the solar node's `bin` builds.
    fn solarnode_scale() -> BatteryScale {
        BatteryScale::for_divider(SOLARNODE)
    }

    /// The resistors divide like the multiplier the board file's prose
    /// derives from them, to the thousandth — so the two statements of
    /// the same divider cannot drift, and the reading is 2.9608's and
    /// not 3.3's.
    #[test]
    fn the_solar_nodes_resistors_give_the_multiplier_its_board_file_states() {
        assert_eq!(SOLARNODE.multiplier_milli(), 2961);
        // (1000 + 510) / 510 = 2.9608, rounded to thousandths.
        assert_eq!(BatteryScale::for_board(2.9608).multiplier_milli(), 2961);
        // And NOT the variant's own `ADC_MULTIPLIER 3.3`, which is that
        // firmware's multiplier paired with its own wrong full scale.
        // Taken at ours it would read every pack 11 % high.
        assert_ne!(SOLARNODE.multiplier_milli(), 3300);
        assert_eq!(BatteryScale::for_board(3.3).terminal_full_scale_mv(), 11880);
    }

    /// The whole measurable range on this board, and that it is neither
    /// of the other two boards' ranges.
    #[test]
    fn the_solar_nodes_full_scale_is_its_own_and_not_the_t114s() {
        let sn = solarnode_scale();
        assert_eq!(sn.terminal_full_scale_mv(), 10_660);
        assert_eq!(
            BatteryScale::for_board(4.916).terminal_full_scale_mv(),
            17_698
        );
        assert_eq!(BatteryScale::for_board(1.73).terminal_full_scale_mv(), 6228);
        assert_eq!(sn.gain(), CONFIGURED_GAIN);
    }

    /// Both ends of the raw range, exactly.
    #[test]
    fn the_solar_nodes_scale_maps_both_ends_of_the_raw_range() {
        let sn = solarnode_scale();
        assert_eq!(sn.raw_to_battery_mv(0), 0);
        assert_eq!(sn.raw_to_battery_mv(-1), 0);
        assert_eq!(sn.raw_to_battery_mv(1), 2);
        assert_eq!(sn.raw_to_battery_mv(4094), 10_657);
        assert_eq!(sn.raw_to_battery_mv(4095), 10_660);
        // One count is 2.6 mV at the terminal here — the finest of the
        // three boards, since this divider throws away the least.
        let step = sn.raw_to_battery_mv(2001) - sn.raw_to_battery_mv(2000);
        assert!(step <= 3, "one count moved the reading by {step} mV");
    }

    /// A full single cell, read through this divider — and the same raw
    /// count read through the T114's, which is a different pack
    /// entirely. This is the assertion the instruction's rule is about:
    /// swap the constants and it fails, loudly.
    #[test]
    fn a_full_cell_on_the_solar_node_is_a_two_cell_pack_on_the_t114() {
        let sn = solarnode_scale();
        let t114 = BatteryScale::for_board(4.916);
        // 4.2 V at the terminal is 4200 / 2.9608 = 1418.5 mV at the pin,
        // which is 1418.5 / 3600 × 4095 = 1613 counts.
        let raw = 1613;
        assert_eq!(sn.raw_to_battery_mv(raw), 4198);
        assert_eq!(classify_cell_count(sn.raw_to_battery_mv(raw)).cells, 1);
        assert_eq!(pack_percent(sn.raw_to_battery_mv(raw), 1), Some(100));
        // The identical count with the T114's divider compiled in.
        assert_eq!(t114.raw_to_battery_mv(raw), 6971);
        assert_eq!(classify_cell_count(t114.raw_to_battery_mv(raw)).cells, 2);
    }

    /// The raw counts at which this board's readings cross each
    /// classification boundary. They are this divider's counts: the same
    /// counts on the T114 land on the other side of every one of them.
    #[test]
    fn the_classification_boundaries_sit_at_this_boards_raw_counts() {
        let sn = solarnode_scale();
        // The protection cut-off, 2.5 V.
        assert_eq!(sn.raw_to_battery_mv(960), 2499);
        assert!(!classify_cell_count(sn.raw_to_battery_mv(960)).plausible);
        assert_eq!(sn.raw_to_battery_mv(961), 2501);
        assert!(classify_cell_count(sn.raw_to_battery_mv(961)).plausible);
        // The 1S/2S split, 6.0 V.
        assert_eq!(sn.raw_to_battery_mv(2304), 5997);
        assert_eq!(classify_cell_count(sn.raw_to_battery_mv(2304)).cells, 1);
        assert_eq!(sn.raw_to_battery_mv(2305), 6000);
        assert_eq!(classify_cell_count(sn.raw_to_battery_mv(2305)).cells, 2);
        // The ceiling above which nothing is a pack, 9.0 V.
        assert_eq!(sn.raw_to_battery_mv(3457), 8999);
        assert!(classify_cell_count(sn.raw_to_battery_mv(3457)).plausible);
        assert_eq!(sn.raw_to_battery_mv(3458), 9001);
        assert!(!classify_cell_count(sn.raw_to_battery_mv(3458)).plausible);
        // The T114's divider at those same four counts is nowhere near
        // any of them.
        let t114 = BatteryScale::for_board(4.916);
        for raw in [960, 961, 2304, 2305, 3457, 3458] {
            assert_ne!(
                t114.raw_to_battery_mv(raw),
                sn.raw_to_battery_mv(raw),
                "raw={raw} read the same through both dividers"
            );
        }
    }

    /// What the enable pin getting it backwards looks like here, and that
    /// it is rejected rather than reported.
    ///
    /// `AdcCtrl` is P0.14, **active LOW**: it sinks the low side of the
    /// divider. Left inactive — driven high, which is what
    /// `EnableLine::new` does at construction and what a flipped
    /// polarity would leave it at during the sample — R18 no longer goes
    /// to a return at all; it goes to the 3.3 V rail, and the pin sits
    /// between the pack and that rail rather than at the pack's 1/2.9608
    /// share of it. For any live pack (3.0 to 4.2 V) that is 3.198 V to
    /// above full scale, i.e. raw 3638 upwards, which this divider reads
    /// as 9470 mV and more. Every one of those is above
    /// `PLAUSIBLE_CEILING_MV`, so the board says `implausible first
    /// reading` instead of publishing a pack that is not there.
    #[test]
    fn a_divider_left_disabled_reads_implausible_on_this_board() {
        let sn = solarnode_scale();
        assert_eq!(sn.raw_to_battery_mv(3638), 9470);
        for raw in 3638..=4095 {
            let mv = sn.raw_to_battery_mv(raw);
            assert!(
                !classify_cell_count(mv).plausible,
                "raw={raw} ({mv} mV) passed as a pack with the divider disabled"
            );
            assert_eq!(pack_percent(mv, 1), None, "raw={raw}");
        }
        // And the other way the polarity can fail: held inactive so hard
        // that nothing reaches the pin at all reads as zero, which is
        // also not a pack.
        assert_eq!(sn.raw_to_battery_mv(0), 0);
        assert!(!classify_cell_count(0).plausible);
    }

    /// What the divider settles: this board cannot see a series pack of
    /// more than two cells, whatever is in its enclosure. 10.66 V is the
    /// whole range, a 3S pack floors at 11.1 V nominal and a 4S at
    /// 14.8 V, so either would sit above full scale and read as a stuck
    /// input — and would put more than the ADC's 3.6 V on the pin on the
    /// way. The board file states the divider; the divider states this;
    /// nothing in either states the pack's topology, and this crate does
    /// not guess it.
    #[test]
    fn the_solar_nodes_divider_cannot_see_a_series_pack_above_two_cells() {
        let sn = solarnode_scale();
        let full_scale = sn.terminal_full_scale_mv() as u16;
        assert_eq!(full_scale, 10_660);
        // A 2S pack, charged, fits with room to spare.
        assert!(8400 < full_scale);
        assert_eq!(classify_cell_count(8400).cells, 2);
        // 3S and 4S nominal do not fit at all.
        assert!(11_100 > full_scale);
        assert!(14_800 > full_scale);
        // And the reading a pack that big produces is the saturated one,
        // which is already rejected.
        assert_eq!(sn.raw_to_battery_mv(4095), full_scale);
        assert!(!classify_cell_count(full_scale).plausible);
    }

    /// The acquisition table is the part's, row for row, and every
    /// variant is in it.
    #[test]
    fn the_acquisition_table_covers_every_window_the_part_has() {
        use AcquisitionTime::*;
        for (t, micros, max_ohms) in [
            (ThreeUs, 3, 10_000),
            (FiveUs, 5, 40_000),
            (TenUs, 10, 100_000),
            (FifteenUs, 15, 200_000),
            (TwentyUs, 20, 400_000),
            (FortyUs, 40, 800_000),
        ] {
            assert_eq!(t.micros(), micros, "{t:?}");
            assert_eq!(t.max_source_ohms(), max_ohms, "{t:?}");
        }
        // The shortest window that covers a source, not merely one that
        // does: a longer window holds the divider enabled for nothing.
        assert_eq!(AcquisitionTime::for_source_ohms(0), Some(ThreeUs));
        assert_eq!(AcquisitionTime::for_source_ohms(10_000), Some(ThreeUs));
        assert_eq!(AcquisitionTime::for_source_ohms(10_001), Some(FiveUs));
        assert_eq!(AcquisitionTime::for_source_ohms(100_000), Some(TenUs));
        assert_eq!(AcquisitionTime::for_source_ohms(100_001), Some(FifteenUs));
        assert_eq!(AcquisitionTime::for_source_ohms(800_000), Some(FortyUs));
        // Above the last row the part specifies nothing, and this says so
        // rather than handing back the longest and calling it covered.
        assert_eq!(AcquisitionTime::for_source_ohms(800_001), None);
    }

    /// The reason the resistors are in the board file and not just their
    /// ratio: this divider needs a longer acquisition window than the
    /// default every board got, and the T114's does not.
    #[test]
    fn the_solar_nodes_divider_needs_a_longer_window_than_the_t114s() {
        // 1 MΩ ∥ 510 kΩ.
        assert_eq!(SOLARNODE.source_ohms(), 337_748);
        assert_eq!(
            SOLARNODE.acquisition_time(),
            Some(AcquisitionTime::TwentyUs)
        );
        assert_eq!(
            solarnode_scale().acquisition_time(),
            AcquisitionTime::TwentyUs
        );
        // Above the 100 kΩ the 10 µs default is specified for, which is
        // the whole finding — sampled at 10 µs this divider is outside
        // what the part guarantees.
        assert!(SOLARNODE.source_ohms() > AcquisitionTime::TenUs.max_source_ohms());

        // The T114's divider, `AIN2 = VBAT * (100/490)` at the 490 kΩ
        // across the pack its own module doc states: 100 kΩ over 390 kΩ,
        // 79.6 kΩ of source. Inside the default's envelope, so nothing
        // about that board's sampling changes.
        let t114 = Divider::new(390_000, 100_000);
        assert_eq!(t114.source_ohms(), 79_591);
        assert_eq!(t114.acquisition_time(), Some(AcquisitionTime::TenUs));
    }

    /// The two boards that predate [`Divider`] keep the window they have
    /// always been sampled with, so its arrival changes no reading on
    /// either. A multiplier says nothing about source resistance, and
    /// this constructor does not pretend otherwise.
    #[test]
    fn a_multiplier_only_board_keeps_the_window_it_has_always_had() {
        assert_eq!(
            BatteryScale::for_board(4.916).acquisition_time(),
            AcquisitionTime::TenUs
        );
        assert_eq!(
            BatteryScale::for_board(1.73).acquisition_time(),
            AcquisitionTime::TenUs
        );
    }

    /// Stating the same divider both ways must produce the same
    /// conversion, or the resistors and the decimal have drifted.
    #[test]
    fn the_resistors_and_the_decimal_convert_identically() {
        let from_resistors = solarnode_scale();
        let from_decimal = BatteryScale::for_board(2.9608);
        assert_eq!(
            from_resistors.terminal_full_scale_mv(),
            from_decimal.terminal_full_scale_mv()
        );
        for raw in 0..=4095i16 {
            assert_eq!(
                from_resistors.raw_to_battery_mv(raw),
                from_decimal.raw_to_battery_mv(raw),
                "raw={raw}"
            );
        }
    }

    /// A divider no acquisition window covers still has to be sampled
    /// somehow, and the longest window is what the part has. The
    /// `Divider` says `None` so nothing can call it covered.
    #[test]
    fn a_divider_beyond_the_table_takes_the_longest_window_and_says_so() {
        let huge = Divider::new(10_000_000, 10_000_000);
        assert_eq!(huge.source_ohms(), 5_000_000);
        assert_eq!(huge.acquisition_time(), None);
        assert_eq!(
            BatteryScale::for_divider(huge).acquisition_time(),
            AcquisitionTime::FortyUs
        );
    }

    /// A cell count with no band renders an empty one rather than
    /// inventing edges.
    #[test]
    fn a_cell_count_without_a_band_renders_an_empty_band() {
        assert_eq!(
            BatteryPercentLine::new(3900, 0, false).to_string(),
            "BATTERY_PCT reportable=0 pack_mv=3900 cells=0S band_lo_mv=0 band_hi_mv=0"
        );
    }
}
