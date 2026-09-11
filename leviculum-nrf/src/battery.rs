//! Battery voltage monitor, on whichever board is asking (Codeberg #380).
//!
//! # What the board says now
//!
//! Once at boot and then every 30 s:
//!
//! ```text
//! BATTERY mv=<ewma> min_mv=<n> max_mv=<n> pct=<n|none> cells=<n>S t=<ms>
//! ```
//!
//! and, only when that `pct=` starts or stops being derivable, one line
//! of its own:
//!
//! ```text
//! BATTERY_PCT reportable=<0|1> pack_mv=<n> cells=<n>S band_lo_mv=<n> band_hi_mv=<n> t=<ms>
//! ```
//!
//! Before this the module sampled the pack, fed [`BATTERY_STATE`] for
//! the display, and said nothing at all — one `[BAT] init` line at
//! startup and then silence for the rest of the boot. A Pocket V2 that
//! restarted twice during a 90 minute field walk on battery produced
//! zero battery lines, so the one question worth asking afterwards
//! (*how close to the edge was the pack?*) had no data behind it at all.
//!
//! # What 1 Hz can and cannot catch
//!
//! The sampler runs at 1 Hz and the report holds the minimum and the
//! maximum across its period, so a sag that *lasts* — the pack drooping
//! under a long transmit, a cell aging into a high internal resistance
//! — shows up as a `min_mv` far below `mv` instead of being averaged
//! into invisibility.
//!
//! It does not catch the transient that actually resets a board. A
//! brownout is microseconds wide, the sampler is a second apart, and
//! the reset takes the log with it; the field Pocket's every boot came
//! up with `reset_reason=0x00000000` and `BOOT_TRACE prev_magic=absent`,
//! i.e. retained RAM had lost power and there was nothing to read. What
//! this buys is the *margin* — the distance between the pack and the
//! edge, minute by minute — which is the thing that has never once been
//! measured. Whether transmitting at 22 dBm closes that distance needs a
//! boot counter in non-volatile storage and is not this.
//!
//! # Why a percentage can be missing
//!
//! The cell count is one reading's worth of decision, taken at boot and
//! held for the boot; every per-cell voltage after it is the pack voltage
//! divided by it. `119067f1` caught the first reading that is not a pack
//! at all. What it could not catch is a first reading that is inside the
//! plausible band and still wrong, or a classification that was right at
//! boot and is not any more — and the percentage that goes on the air
//! carries neither a unit nor a cell count, so nothing a remote reader
//! has can tell a wrong one from a right one.
//!
//! So the percentage is reported only while the pack voltage stays inside
//! the band its classification implies
//! ([`leviculum_battery_scale::pack_percent`]). Outside it the board
//! publishes no percentage and says so once, on the transition rather
//! than once per sample. The voltage is never withheld: `mv=` is a
//! measurement and stays whatever the ADC said, and it is the number a
//! reader can still act on. Nor is the classification revised — that is a
//! boot-time decision and re-taking it at runtime is a different
//! question; this only declines to build on it.
//!
//! # Where the numbers live
//!
//! Nothing quantitative is in this file. The ADC gain, the conversion,
//! the cell classification and the line's bytes are all in
//! [`leviculum_battery_scale`], where a host test can hold them —
//! notably the full scale, which is *derived from* the gain this module
//! configures rather than written down beside it. The divider
//! multiplier and the pins come from the board file (`boards/t114.rs`,
//! `boards/rak4631.rs`), which stays their single source.

use embassy_executor::Spawner;
use embassy_nrf::gpio::{AnyPin, Level, Output, OutputDrive};
use embassy_nrf::peripherals;
use embassy_nrf::saadc::{self, AnyInput, ChannelConfig, Config, Reference, Resolution, Saadc};
use embassy_nrf::{bind_interrupts, Peri};
use embassy_time::{Duration, Timer};

use leviculum_battery_scale::{
    classify_cell_count, pack_percent, AdcGain, BatteryEwma, BatteryLine, BatteryPercentLine,
    BatteryScale, BatteryWindow,
};

use crate::baseboard::{BatteryState, BATTERY_STATE};

bind_interrupts!(pub struct BatteryIrqs {
    SAADC => saadc::InterruptHandler;
});

/// How often the ADC is read.
const SAMPLE_PERIOD: Duration = Duration::from_secs(1);

/// Samples between two [`BATTERY_STATE`] updates — 5 s, the cadence the
/// display has always been fed at. Deliberately unchanged: the status
/// screen quantises the pack voltage to 100 mV and the charge to 1 %
/// (`leviculum_screen::FrameKey`), and a transmit sag is larger than
/// 100 mV, so publishing every sample would have put a redraw on the
/// screen for every packet the node sends.
const SAMPLES_PER_PUBLISH: u32 = 5;

/// Samples between two `BATTERY` lines.
const SAMPLES_PER_REPORT: u32 = 30;

/// How long the divider needs after its enable pin goes high before the
/// voltage on the ADC pin is the pack's. Meshtastic waits the same 10 ms
/// on the same divider (`meshtastic/src/Power.cpp:243`).
const DIVIDER_SETTLE_MS: u64 = 10;

/// Translate the gain the arithmetic uses into the gain the channel is
/// configured with.
///
/// This `match` is the whole seam between the two: the scale carries one
/// gain, the register is written from it, and the full scale the
/// conversion divides by is derived from it
/// ([`AdcGain::full_scale_mv`]). A gain changed in one place therefore
/// cannot fail to change in the other — which is what happened before,
/// when the register value came from an embassy default
/// (`ChannelConfig::single_ended`, `embassy-nrf-0.9.0/src/saadc.rs:100`)
/// and the 3600 it implies was a hard-coded constant in the conversion.
fn saadc_gain(gain: AdcGain) -> saadc::Gain {
    match gain {
        AdcGain::OneSixth => saadc::Gain::GAIN1_6,
        AdcGain::OneFifth => saadc::Gain::GAIN1_5,
        AdcGain::OneQuarter => saadc::Gain::GAIN1_4,
        AdcGain::OneThird => saadc::Gain::GAIN1_3,
        AdcGain::OneHalf => saadc::Gain::GAIN1_2,
        AdcGain::Unity => saadc::Gain::GAIN1,
        AdcGain::Two => saadc::Gain::GAIN2,
        AdcGain::Four => saadc::Gain::GAIN4,
    }
}

/// Take one reading, in millivolts at the battery terminal.
///
/// The divider-enable pin, where the board has one, is high only for the
/// duration of the sample: on the T114 the divider is 490 kΩ across the
/// pack, and leaving it enabled between samples would drain the pack for
/// nothing 99 % of the time.
async fn sample_pack_mv(
    adc: &mut Saadc<'static, 1>,
    divider_enable: &mut Option<Output<'static>>,
    scale: &BatteryScale,
) -> u16 {
    if let Some(enable) = divider_enable.as_mut() {
        enable.set_high();
        Timer::after(Duration::from_millis(DIVIDER_SETTLE_MS)).await;
    }
    let mut buf = [0i16; 1];
    adc.sample(&mut buf).await;
    if let Some(enable) = divider_enable.as_mut() {
        enable.set_low();
    }
    scale.raw_to_battery_mv(buf[0])
}

/// Say, on a line of its own, that the percentage just started or
/// stopped being derivable.
///
/// Critical rather than gated: these lines are rare by construction — one
/// per transition, not one per sample — and each of them is what explains
/// a gap in the `pct=` series that a capture would otherwise have to
/// guess at. A field board has no host to open the `RUNTIME_DRAIN_OPEN`
/// gate, and this is precisely the run in which the gap appears.
fn log_percent_gate(pack_mv: u16, cell_count: u8, reportable: bool) {
    crate::log::log_fmt_critical(
        "[BAT] ",
        format_args!(
            "{}",
            BatteryPercentLine::new(pack_mv, cell_count, reportable)
        ),
    );
}

#[embassy_executor::task]
pub async fn battery_task(
    saadc_periph: Peri<'static, peripherals::SAADC>,
    adc_pin: AnyInput<'static>,
    divider_enable: Option<Peri<'static, AnyPin>>,
    scale: BatteryScale,
) {
    let mut config = Config::default();
    config.resolution = Resolution::_12BIT;
    let mut ch = ChannelConfig::single_ended(adc_pin);
    // Both of these are what `single_ended` would have defaulted to
    // today. They are written down because the conversion depends on
    // them and a default is not a statement: `leviculum_battery_scale`
    // derives the full-scale millivolts from this gain, so the two move
    // together or a test fails.
    ch.reference = Reference::INTERNAL;
    ch.gain = saadc_gain(scale.gain());
    let mut adc = Saadc::new(saadc_periph, BatteryIrqs, config, [ch]);

    // The enable pin starts LOW: the divider is switched on per sample,
    // not for the lifetime of the task.
    let mut divider_enable =
        divider_enable.map(|pin| Output::new(pin, Level::Low, OutputDrive::Standard));

    // One sample decides the cell count for the lifetime of the task.
    // Persisting it across boots waits for the flash store; redoing it
    // costs one ADC reading.
    let first_mv = sample_pack_mv(&mut adc, &mut divider_enable, &scale).await;
    let class = classify_cell_count(first_mv);
    // Critical, i.e. past the `RUNTIME_DRAIN_OPEN` gate, and this is the
    // one place in the module where that is right: the gate stays shut
    // until DTR-assert or 30 s of uptime, whichever comes first, and a
    // board on battery in a field has neither a host nor, if it resets
    // inside that window, a second chance. The boot statement is exactly
    // the one #380 went looking for and did not find. Everything after
    // it is routine and stays gated.
    crate::log::log_fmt_critical(
        "[BAT] ",
        format_args!(
            "init pack_mv={} cells={}S full_scale_mv={}",
            first_mv,
            class.cells,
            scale.terminal_full_scale_mv()
        ),
    );
    if !class.plausible {
        // Not a pack voltage: a floating input, a divider whose enable
        // pin is not doing what the board file says, or a multiplier
        // that does not match the board this image is running on. 1S is
        // the fallback because it cannot make an empty pack look full.
        crate::log::log_fmt_critical(
            "[WARN] ",
            format_args!(
                "[BAT] implausible first reading pack_mv={} (full scale {} mV) — assuming 1S",
                first_mv,
                scale.terminal_full_scale_mv()
            ),
        );
    }
    let cell_count = class.cells;

    let sender = BATTERY_STATE.sender();
    let mut ewma = BatteryEwma::new(first_mv);
    let mut window = BatteryWindow::new(first_mv);

    let publish = |mv: u16, percent: Option<u8>| {
        sender.send(BatteryState {
            voltage_mv: mv,
            percent,
            cell_count,
        });
    };
    let report = |mv: u16, percent: Option<u8>, window: &BatteryWindow, at_boot: bool| {
        let line = BatteryLine {
            mv,
            min_mv: window.min_mv(),
            max_mv: window.max_mv(),
            percent,
            cells: cell_count,
        };
        // Same gate argument as the `init` line above: the boot one
        // bypasses, the every-30-s ones do not.
        if at_boot {
            crate::log::log_fmt_critical("[BAT] ", format_args!("{}", line));
        } else {
            crate::log::log_fmt("[BAT] ", format_args!("{}", line));
        }
    };

    // Say it once at boot rather than 30 s in: a board that resets
    // before the first period is up would otherwise report nothing at
    // all, which is exactly the boot worth hearing about.
    let mut percent = pack_percent(first_mv, cell_count);
    let mut reportable = percent.is_some();
    publish(first_mv, percent);
    report(first_mv, percent, &window, true);
    if !reportable {
        // Only the withheld state earns a line at boot. A boot that CAN
        // derive a percentage has already said so, with `pct=` on the
        // `BATTERY` line immediately above; a boot that cannot is the
        // anomaly, and it is critical for the same reason `init` is — a
        // board on battery in a field has no host to open the drain gate.
        log_percent_gate(first_mv, cell_count, false);
    }

    let mut samples: u32 = 0;
    loop {
        Timer::after(SAMPLE_PERIOD).await;
        let pack_mv = sample_pack_mv(&mut adc, &mut divider_enable, &scale).await;
        let filtered = ewma.step(pack_mv);
        window.observe(pack_mv);
        samples = samples.wrapping_add(1);

        // Judged on the same number the percentage would be derived from,
        // in the same iteration it is derived in, so the line and the
        // published state can never disagree about which side of the band
        // the pack is on. Every sample, not every report: the transition
        // is the event, and waiting up to 30 s to mention it would put it
        // in the wrong place on a merged timeline.
        percent = pack_percent(filtered, cell_count);
        if percent.is_some() != reportable {
            reportable = percent.is_some();
            log_percent_gate(filtered, cell_count, reportable);
        }

        if samples.is_multiple_of(SAMPLES_PER_PUBLISH) {
            publish(filtered, percent);
        }
        if samples.is_multiple_of(SAMPLES_PER_REPORT) {
            report(filtered, percent, &window, false);
            // Seed the next window with the sample that closed this one,
            // so no sample falls between two windows.
            window.restart(pack_mv);
        }
    }
}

/// Convenience wrapper invoked from the bin file.
///
/// `adc_pin` is the board's `BatteryAdc`, `divider_enable` its
/// `AdcCtrl` where it has one (the T114 does, the RAK does not), and
/// `divider_multiplier` its `ADC_MULTIPLIER`. All three come from the
/// board file, which stays their single source — this module holds no
/// copy of any of them. The ADC side of the scale is not a per-board
/// choice and comes from [`leviculum_battery_scale::CONFIGURED_GAIN`].
pub fn init(
    spawner: &Spawner,
    saadc_periph: Peri<'static, peripherals::SAADC>,
    adc_pin: impl saadc::Input + 'static,
    divider_enable: Option<Peri<'static, AnyPin>>,
    divider_multiplier: f32,
) {
    spawner.must_spawn(battery_task(
        saadc_periph,
        adc_pin.degrade_saadc(),
        divider_enable,
        BatteryScale::for_board(divider_multiplier),
    ));
}
