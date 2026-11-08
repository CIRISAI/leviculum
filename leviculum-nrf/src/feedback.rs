//! The players of the one-button feedback grammar (#459, order 892): the
//! board's LED, its notification LED where it has one, and the T1000-E's
//! buzzer, each playing a [`leviculum_feedback::Pattern`] to its end.
//!
//! The patterns, their timings and the phase a pattern is in at a given
//! millisecond are `leviculum-feedback`'s, host-tested there. This module
//! owns the pins and the three signals that hand a pattern to a player. A
//! pattern raised while another is playing on the same sink replaces it:
//! the player starts the new one from its first step and the rest of the
//! old one is never played.
//!
//! | Board | `sink=led` | `sink=notify` | `sink=buzzer` |
//! |---|---|---|---|
//! | WisMesh Pocket V2 | green LED1, between its TX activity pulses | blue LED2, between its RX activity pulses | none |
//! | T1000-E | the green LED | none, `notify` plays on the LED | P0.25 at `BUZZER_TONE_HZ` |
//! | Heltec T114 | the green LED (active low) | none, `notify` plays on the LED | none |
//! | Solar Node | the green LED | the blue LED | none |
//!
//! Every play writes one line when the player starts it, through the
//! gated path, not the critical one:
//!
//! ```text
//! FEEDBACK pattern=<name> sink=led|notify|buzzer t=<ms>
//! ```
//!
//! A status press on a board with no battery percentage plays nothing and
//! writes `FEEDBACK pattern=none reason=no-percentage sink=led` instead.
//!
//! A sink a board does not have is never asked, so it writes no line.
//!
//! Which event raises which pattern on which sink is
//! `leviculum_firmware::feedback`'s since #469 973; this module is the
//! other side of its `Sinks` seam:
//! [`NrfSinks`](crate::feedback::NrfSinks), registered at boot with
//! `leviculum_firmware::feedback::set_sinks`, hands a pattern to the
//! player of its sink.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::Output;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use leviculum_feedback::{Pattern, Phase, Playback};
pub use leviculum_firmware::feedback::Sink;
use leviculum_firmware::seams::Sinks;

fn signal(sink: Sink) -> &'static Signal<CriticalSectionRawMutex, Pattern> {
    match sink {
        Sink::Led => &LED,
        Sink::Notify => &NOTIFY,
        Sink::Buzzer => &BUZZER,
    }
}

fn present(sink: Sink) -> &'static AtomicBool {
    match sink {
        Sink::Led => &HAS_LED,
        Sink::Notify => &HAS_NOTIFY,
        Sink::Buzzer => &HAS_BUZZER,
    }
}

// One signal per sink. A `Signal` holds one pending value, and a newer
// pattern overwriting one the player has not taken yet is exactly the
// replacement rule.
static LED: Signal<CriticalSectionRawMutex, Pattern> = Signal::new();
static NOTIFY: Signal<CriticalSectionRawMutex, Pattern> = Signal::new();
static BUZZER: Signal<CriticalSectionRawMutex, Pattern> = Signal::new();

// Set when the sink's player is spawned, so a request to a sink the board
// does not have goes nowhere and writes no line.
static HAS_LED: AtomicBool = AtomicBool::new(false);
static HAS_NOTIFY: AtomicBool = AtomicBool::new(false);
static HAS_BUZZER: AtomicBool = AtomicBool::new(false);

/// The board's [`Sinks`]: a sink is present once its player is spawned,
/// and a pattern played on it is handed to that player's signal.
#[derive(Clone, Copy, Debug, Default)]
pub struct NrfSinks;

impl Sinks for NrfSinks {
    fn present(&self, sink: Sink) -> bool {
        present(sink).load(Ordering::Relaxed)
    }

    fn play(&self, sink: Sink, pattern: Pattern) {
        signal(sink).signal(pattern);
    }
}

/// Play `pattern` on `sink` through `drive` (`true` = on) until it ends,
/// starting over with any pattern raised on the same sink meanwhile. One
/// `FEEDBACK` line per pattern started. Returns with the sink off.
async fn play(mut pattern: Pattern, sink: Sink, mut drive: impl FnMut(bool)) {
    let signal = signal(sink);
    loop {
        crate::log::log_fmt(
            "",
            format_args!("FEEDBACK pattern={} sink={}", pattern.name, sink.as_str()),
        );
        let playback = Playback::new(pattern, Instant::now().as_millis());
        let replaced = loop {
            let until_ms = match playback.phase(Instant::now().as_millis()) {
                Phase::On { until_ms } => {
                    drive(true);
                    until_ms
                }
                Phase::Off { until_ms } => {
                    drive(false);
                    until_ms
                }
                Phase::Done => {
                    drive(false);
                    break None;
                }
            };
            if let Either::Second(next) =
                select(Timer::at(Instant::from_millis(until_ms)), signal.wait()).await
            {
                break Some(next);
            }
        };
        match replaced {
            Some(next) => pattern = next,
            None => return,
        }
    }
}

/// The Pocket V2's activity pulse, unchanged from `crate::led` before it
/// shared the LEDs: 80 ms on, then 30 ms forced dark so a burst still
/// reads as separate flashes.
const ACTIVITY_ON_MS: u64 = 80;
const ACTIVITY_OFF_MS: u64 = 30;

/// How long the boot blink holds the LED lit. The length the spin it
/// replaced had on the T114 (12 M `nop`s, 2093 ms between `post-init` and
/// `blink_done` on d5981450, order 1055), now on a timer, so the executor
/// runs the USB task meanwhile instead of waiting for it.
pub const BOOT_BLINK_MS: u64 = 2_100;

/// An LED player. `activity`, where given, is the Pocket V2's LoRa TX or
/// RX pulse signal: the LED pulses on it while no pattern plays, and the
/// pulses raised during a pattern are dropped, so a pattern is never cut
/// into by traffic and never followed by a stale flash.
///
/// `boot_blink` lights the LED for [`BOOT_BLINK_MS`] before the player
/// starts, then stamps `[BOOT_TRACE] stage=blink_done`. A pattern raised
/// meanwhile waits in its signal and plays once the blink is over.
#[embassy_executor::task(pool_size = 2)]
pub async fn led_task(
    mut led: Output<'static>,
    active_high: bool,
    sink: Sink,
    activity: Option<&'static Signal<CriticalSectionRawMutex, ()>>,
    boot_blink: bool,
) {
    let mut set = move |on: bool| {
        if on == active_high {
            led.set_high();
        } else {
            led.set_low();
        }
    };
    if boot_blink {
        set(true);
        Timer::after(Duration::from_millis(BOOT_BLINK_MS)).await;
        set(false);
        leviculum_firmware::boot_trace::log_stage("blink_done");
    }
    set(false);
    let signal = signal(sink);
    loop {
        let pattern = match activity {
            Some(pulse) => match select(signal.wait(), pulse.wait()).await {
                Either::First(pattern) => pattern,
                Either::Second(()) => {
                    set(true);
                    Timer::after(Duration::from_millis(ACTIVITY_ON_MS)).await;
                    set(false);
                    Timer::after(Duration::from_millis(ACTIVITY_OFF_MS)).await;
                    continue;
                }
            },
            None => signal.wait().await,
        };
        play(pattern, sink, &mut set).await;
        if let Some(pulse) = activity {
            pulse.reset();
        }
    }
}

/// Spawn an LED player for `sink`. `active_high` is the board file's
/// polarity (the T114's LED is the one active low).
pub fn spawn_led(
    spawner: &Spawner,
    led: Output<'static>,
    active_high: bool,
    sink: Sink,
    activity: Option<&'static Signal<CriticalSectionRawMutex, ()>>,
) {
    spawner.must_spawn(led_task(led, active_high, sink, activity, false));
    present(sink).store(true, Ordering::Relaxed);
}

/// [`spawn_led`] for the board's main LED, opening with the boot blink
/// (see [`led_task`]). The blink runs on the executor, so `main` goes on
/// to the main loop and USB enumerates while the LED is still lit.
pub fn spawn_led_with_boot_blink(
    spawner: &Spawner,
    led: Output<'static>,
    active_high: bool,
    sink: Sink,
) {
    spawner.must_spawn(led_task(led, active_high, sink, None, true));
    present(sink).store(true, Ordering::Relaxed);
}

/// [`spawn_led_with_boot_blink`] for a main LED that also pulses on LoRa
/// activity (D4 of order 1085: the activity blink is a feature of a board
/// with a free LED, through its feedback sink). The Pocket V2's green LED1:
/// the boot blink first, then TX pulses between patterns.
pub fn spawn_activity_led_with_boot_blink(
    spawner: &Spawner,
    led: Output<'static>,
    active_high: bool,
    sink: Sink,
    activity: &'static Signal<CriticalSectionRawMutex, ()>,
) {
    spawner.must_spawn(led_task(led, active_high, sink, Some(activity), true));
    present(sink).store(true, Ordering::Relaxed);
}

/// The T1000-E buzzer's tone: B3, 247 Hz, the note Meshtastic's own beep
/// plays on this board (`playBeep`, `NOTE_B3`, `src/buzz/buzz.cpp`; its
/// T1000-E variant drives `PIN_BUZZER` P0.25 as "pwm output" with
/// `tone()`). Neither Seeed nor Meshtastic publishes the transducer's
/// resonance, so the tone is the one this part is known to have sounded in
/// the field; a louder one is a rig measurement, not a guess.
#[cfg(feature = "bsp-t1000e")]
pub const BUZZER_TONE_HZ: u32 = 247;

/// The buzzer player (T1000-E). A square wave at [`BUZZER_TONE_HZ`], 50 %
/// duty, while a step is on. The enable line is raised for the length of a
/// pattern and dropped after it, so the buzzer is unpowered whenever it is
/// silent, as it was before it had a player.
#[cfg(feature = "bsp-t1000e")]
#[embassy_executor::task]
pub async fn buzzer_task(
    mut pwm: embassy_nrf::pwm::SimplePwm<'static>,
    mut enable: Output<'static>,
) {
    use embassy_nrf::pwm::DutyCycle;
    pwm.set_period(BUZZER_TONE_HZ);
    let half = DutyCycle::normal(pwm.max_duty() / 2);
    pwm.disable();
    loop {
        let pattern = BUZZER.wait().await;
        enable.set_high();
        play(pattern, Sink::Buzzer, |on| {
            if on {
                // Enabling alone does not restart the waveform; loading the
                // duty starts the sequence (and waits one period for it).
                pwm.enable();
                pwm.set_duty(0, half);
            } else {
                // Disabled, the pin falls back to its idle level, low.
                pwm.disable();
            }
        })
        .await;
        enable.set_low();
    }
}

/// Spawn the buzzer player (T1000-E).
#[cfg(feature = "bsp-t1000e")]
pub fn spawn_buzzer(
    spawner: &Spawner,
    pwm: embassy_nrf::pwm::SimplePwm<'static>,
    enable: Output<'static>,
) {
    spawner.must_spawn(buzzer_task(pwm, enable));
    HAS_BUZZER.store(true, Ordering::Relaxed);
}
